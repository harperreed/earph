// ABOUTME: Reconciles worker observations through the authoritative session reducer.
// ABOUTME: Commits operational state before publishing queue changes or worker controls.
use crate::{
    directory,
    graph::{GraphCancellation, GraphService, PreparedPipeline},
    ipc, runtime,
    worker::{Action, Event, Settings, SourceTerminalToken, StopRequest, Worker},
};
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, ReceiptStatus, SourceKind},
    graph_control::{GraphActivationAck, GraphServiceObservation},
    ids::{Epochs, RequestId},
    session::{EngineEvent, EngineEventKind, Session},
};
use std::{
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{RecvTimeoutError, TryRecvError},
    },
    time::{Duration, Instant},
};

const IO: EngineError = EngineError::new(
    ErrorCode::InternalFault,
    "cannot persist local playback state",
);
pub(crate) fn serve(
    mut session: Session,
    listener: UnixListener,
    directory: PathBuf,
    token: String,
    device_helper: Option<crate::DeviceDiscoveryHelper>,
) -> Result<(), EngineError> {
    let server = ipc::Server::spawn(listener)?;
    let mut graphs = GraphJobs::spawn(directory.clone())?;
    let mut profiles = crate::profiles::ProfileJobs::spawn(directory.clone());
    let mut devices = crate::devices::DeviceJobs::spawn(device_helper)?;
    // Worker pipelines must retire before the preparation service shuts down.
    let mut worker = Worker::spawn();
    let mut terminal_acknowledgement = None;
    let clock = Instant::now();
    loop {
        loop {
            match worker.events.try_recv() {
                Ok(Event::GraphActivated(acknowledgement)) => {
                    if let Err(error) = reconcile_graph_activation(
                        &mut session,
                        acknowledgement,
                        monotonic(&clock),
                        |staged| {
                            let bytes = serde_json::to_vec(&staged.checkpoint()).map_err(|_| IO)?;
                            runtime::write_checkpoint(
                                &directory,
                                &format!("{token}-graph-{}", staged.snapshot().revision),
                                &bytes,
                            )
                        },
                    ) {
                        worker.cancel_active();
                        return Err(error);
                    }
                }
                Ok(Event::GraphFailed {
                    epochs,
                    request_id,
                    error,
                }) => {
                    if session
                        .graph_activation()
                        .is_some_and(|a| a.request_id == request_id && a.epochs == epochs)
                    {
                        session.fail_graph_activation(&request_id, error);
                        worker.cancel_active();
                    }
                }
                Ok(event) => {
                    if let Ok(Some(token)) = observation(&mut session, event, &clock) {
                        terminal_acknowledgement = Some(token);
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if session.snapshot().session_id.is_some() {
                        advance(
                            &mut session,
                            EngineEventKind::LocalFailed {
                                code: ErrorCode::OutputFault,
                            },
                            None,
                            &clock,
                        );
                    }
                    break;
                }
            }
        }
        if let Some(token) = terminal_acknowledgement.as_ref()
            && worker
                .send(Action::AcknowledgeSourceTerminal(token.clone()))
                .is_ok()
        {
            terminal_acknowledgement = None;
        }
        devices.poll(&mut session, monotonic(&clock));
        if session.shutdown_requested() {
            devices.cancel();
            if devices.is_idle() {
                return Ok(());
            }
        }
        let progress = worker
            .progress
            .try_lock()
            .ok()
            .and_then(|mut latest| latest.take());
        if let Some((epochs, observation)) = progress {
            advance(
                &mut session,
                EngineEventKind::LocalProgress { observation },
                Some(epochs),
                &clock,
            );
        }
        graphs.poll(&mut session, monotonic(&clock))?;
        profiles.poll(&mut session, monotonic(&clock), &directory, &token)?;
        match server.requests.recv_timeout(Duration::from_millis(1)) {
            Ok(pending) => {
                let command = pending.command;
                let before = session.snapshot();
                let actual_check = worker
                    .view
                    .lock()
                    .map_err(|_| IO)?
                    .validate_edit(&command.payload);
                let needs_worker = action_required(&command.payload, before.source);
                let safety = safety_control(&command.payload);
                let capacity = !needs_worker || worker.has_capacity(safety);
                let view = worker.view.clone();
                let mut persistence_error = None;
                let mut dispatched_early = false;
                let mut admission_failure = None;
                let mut receipt = session.submit_with_graph_effects(
                    command.clone(),
                    monotonic(&clock),
                    directory::paths,
                    |staged| {
                        actual_check?;
                        if matches!(
                            command.payload,
                            Command::DevicesList { .. } | Command::DevicesInspect { .. }
                        ) {
                            return devices.submit(staged);
                        }
                        if command.payload.is_profile_save() {
                            return profiles.submit(staged);
                        }
                        if command.payload.prepares_graph() {
                            return graphs.submit(staged, &worker);
                        }
                        if matches!(command.payload, Command::Stop | Command::OwnerShutdown) {
                            worker.cancel_active();
                        }
                        if !capacity {
                            if safety {
                                worker.cancel_active();
                                admission_failure = Some(ErrorCode::SourceBusy);
                            }
                            return Err(EngineError::new(
                                ErrorCode::SourceBusy,
                                "playback control queue is full",
                            ));
                        }
                        if command.payload.activates_graph() {
                            graphs.validate_activation(staged)?;
                        }
                        if before.source == Some(SourceKind::LocalFile) {
                            let settings = Settings {
                                snapshot: staged.snapshot(),
                                playing: staged.wants_playback(),
                            };
                            let safety_action = match command.payload {
                                Command::Pause => Some(Action::Pause(settings)),
                                Command::Mute { muted: true } | Command::VolumeCaps { .. } => {
                                    Some(Action::Settings(settings))
                                }
                                _ => None,
                            };
                            if let Some(action) = safety_action {
                                if let Err(error) = worker.send(action) {
                                    admission_failure = Some(error.code);
                                    return Err(error);
                                }
                                dispatched_early = true;
                            }
                        }
                        let bytes = serde_json::to_vec(&staged.checkpoint()).map_err(|_| IO)?;
                        let mut gate = None;
                        let persisted = runtime::checkpoint_transaction(
                            &directory,
                            &format!("{token}-{}", staged.snapshot().revision),
                            &bytes,
                            || {
                                let acquired = view.lock().map_err(|_| IO)?;
                                acquired.validate_edit(&command.payload)?;
                                gate = Some(acquired);
                                Ok(())
                            },
                        );
                        if let Err(error) = persisted {
                            if let runtime::CheckpointError::Storage(error) = error {
                                persistence_error = Some(error);
                            }
                            return Err(error.error());
                        }
                        let mut gate = gate.ok_or(IO)?;
                        if matches!(
                            command.payload,
                            Command::Start {
                                source: SourceKind::LocalFile,
                                ..
                            }
                        ) {
                            gate.active = staged.snapshot().active_entry_id;
                            gate.reserved = staged.snapshot().reserved_next_entry_id;
                            gate.epochs = Some(staged.snapshot().epochs);
                        }
                        if matches!(command.payload, Command::Stop | Command::OwnerShutdown) {
                            gate.active = None;
                            gate.reserved = None;
                        }
                        gate.publish(staged.queue_snapshot());
                        Ok(())
                    },
                );
                cancel_rejected_safety(&mut session, &worker, &command, &receipt);
                if let Some(code) = admission_failure {
                    // Cancellation protects output even when safety traffic filled the reserve.
                    // Report failure until an explicit control restores a usable stream.
                    advance(
                        &mut session,
                        EngineEventKind::LocalFailed { code },
                        None,
                        &clock,
                    );
                }
                let after = session.snapshot();
                if matches!(
                    receipt.status,
                    ReceiptStatus::Applied | ReceiptStatus::Accepted
                ) && after.revision != before.revision
                    && !dispatched_early
                    && let Err(error) = if command.payload.activates_graph() {
                        graphs.activate(&mut worker, &session)
                    } else {
                        dispatch(&command, &mut worker, &session)
                    }
                {
                    if command.payload.activates_graph() {
                        session.fail_graph_activation(&command.request_id, error);
                        worker.cancel_active();
                        receipt = session.submit_graph(command.clone(), monotonic(&clock));
                    } else {
                        advance(
                            &mut session,
                            EngineEventKind::LocalFailed { code: error.code },
                            None,
                            &clock,
                        );
                    }
                }
                let _ = pending.reply.try_send(receipt);
                if let Some(error) = persistence_error {
                    return Err(error);
                }
                if session.shutdown_requested() {
                    devices.cancel();
                    if devices.is_idle() {
                        return Ok(());
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
fn safety_control(command: &Command) -> bool {
    matches!(
        command,
        Command::Stop
            | Command::OwnerShutdown
            | Command::Pause
            | Command::Mute { muted: true }
            | Command::VolumeCaps { .. }
    )
}
fn reconcile_graph_activation(
    session: &mut Session,
    acknowledgement: GraphActivationAck,
    now_ns: u64,
    persist: impl FnOnce(&Session) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let current = session.graph_activation().is_some_and(|activation| {
        activation.request_id == acknowledgement.request_id
            && activation.epochs == acknowledgement.epochs
    });
    let applied = session.complete_graph_activation(acknowledgement, now_ns, persist);
    // Only a matching activation that reached a terminal failure can stop the owner.
    if current
        && session.graph_activation().is_none()
        && session.graph_inspection().recovery_required
    {
        return applied;
    }
    Ok(())
}
fn cancel_rejected_safety(
    session: &mut Session,
    worker: &Worker,
    command: &CommandEnvelope,
    receipt: &earph_core::control::Receipt,
) {
    if receipt.status != ReceiptStatus::Rejected || !safety_control(&command.payload) {
        return;
    }
    let Some(error) = receipt
        .error
        .filter(|error| error.code == ErrorCode::ResourceBudgetExceeded)
    else {
        return;
    };
    let Some(activation) = session.graph_activation().filter(|activation| {
        activation.epochs == receipt.epochs && receipt.request_id == command.request_id
    }) else {
        return;
    };
    worker.cancel_active();
    session.fail_graph_activation(&activation.request_id, error);
}
fn action_required(command: &Command, source: Option<SourceKind>) -> bool {
    matches!(
        command,
        Command::Start {
            source: SourceKind::LocalFile,
            ..
        }
    ) || source == Some(SourceKind::LocalFile)
        && matches!(
            command,
            Command::Pause
                | Command::GraphCommit { .. }
                | Command::ProfilesApply { .. }
                | Command::Resume
                | Command::Stop
                | Command::Seek { .. }
                | Command::Mute { .. }
                | Command::VolumeSet { .. }
                | Command::VolumeCaps { .. }
                | Command::OwnerShutdown
        )
}
fn dispatch(
    command: &CommandEnvelope,
    worker: &mut Worker,
    session: &Session,
) -> Result<(), EngineError> {
    let settings = Settings {
        snapshot: session.snapshot(),
        playing: session.wants_playback(),
    };
    let action = match command.payload {
        Command::Start {
            source: SourceKind::LocalFile,
            ..
        } => Some(Action::Start {
            settings,
            entries: session.queue_snapshot().entries,
            cancel: Arc::new(AtomicBool::new(false)),
        }),
        Command::OwnerShutdown | Command::Stop => Some(Action::Stop(StopRequest {
            request_id: command.request_id.clone(),
            settings,
        })),
        Command::Pause if settings.snapshot.source == Some(SourceKind::LocalFile) => {
            Some(Action::Pause(settings))
        }
        Command::Seek { position_frames }
            if settings.snapshot.source == Some(SourceKind::LocalFile) =>
        {
            Some(Action::Seek {
                settings,
                position: position_frames,
            })
        }
        Command::Resume
        | Command::Mute { .. }
        | Command::VolumeSet { .. }
        | Command::VolumeCaps { .. }
            if settings.snapshot.source == Some(SourceKind::LocalFile) =>
        {
            Some(Action::Settings(settings))
        }
        _ => None,
    };
    if let Some(action) = action {
        worker.send(action)?;
    }
    Ok(())
}
fn advance(session: &mut Session, kind: EngineEventKind, epochs: Option<Epochs>, clock: &Instant) {
    let epochs = epochs.unwrap_or_else(|| session.snapshot().epochs);
    let _ = session.advance(EngineEvent {
        epochs,
        monotonic_ns: clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
        kind,
    });
}
fn observation(
    session: &mut Session,
    event: Event,
    clock: &Instant,
) -> Result<Option<SourceTerminalToken>, EngineError> {
    match event {
        Event::GraphActivated(_) | Event::GraphFailed { .. } => {
            unreachable!("graph events reconcile with persistence in the owner loop")
        }
        Event::Item { epochs, id, status } => {
            let _ = session.set_queue_preparation(epochs, &id, status);
        }
        Event::Ready {
            epochs,
            observation,
        } => advance(
            session,
            EngineEventKind::LocalPrepared { observation },
            Some(epochs),
            clock,
        ),
        Event::Started { epochs } => advance(
            session,
            EngineEventKind::Started {
                activation_frame: 0,
            },
            Some(epochs),
            clock,
        ),
        Event::Reserved {
            epochs,
            active,
            next,
        } => advance(
            session,
            EngineEventKind::LocalReserved { active, next },
            Some(epochs),
            clock,
        ),
        Event::Paused {
            epochs,
            observation,
        } => advance(
            session,
            EngineEventKind::LocalPaused { observation },
            Some(epochs),
            clock,
        ),
        Event::Stopped {
            epochs,
            observation,
        } => advance(
            session,
            EngineEventKind::LocalStopped { observation },
            Some(epochs),
            clock,
        ),
        Event::End { epochs } => advance(session, EngineEventKind::EndOfInput, Some(epochs), clock),
        Event::Drained {
            epochs,
            token,
            observation,
        } => {
            let snapshot = session.snapshot();
            if !token.matches_source(&snapshot)
                || epochs != snapshot.epochs
                || epochs.stream < token.origin_epochs.stream
                || observation.source_entry_id != token.final_entry_id
            {
                return Err(EngineError::new(
                    ErrorCode::StaleSession,
                    "native completion no longer matches the source",
                ));
            }
            session.advance(EngineEvent {
                epochs,
                monotonic_ns: monotonic(clock),
                kind: EngineEventKind::LocalCompleted { observation },
            })?;
            return Ok(Some(token));
        }
        Event::Failed {
            epochs,
            token,
            error,
            observation,
        } => {
            if let Some(token) = &token {
                let snapshot = session.snapshot();
                if !token.matches_source(&snapshot)
                    || epochs != snapshot.epochs
                    || epochs.stream < token.origin_epochs.stream
                    || observation
                        .as_ref()
                        .is_some_and(|value| value.source_entry_id != token.final_entry_id)
                {
                    return Err(EngineError::new(
                        ErrorCode::StaleSession,
                        "source failure no longer matches the source",
                    ));
                }
            }
            if token.is_some() || error.code != ErrorCode::OperationCancelled {
                if let Some(observation) = observation {
                    advance(
                        session,
                        EngineEventKind::LocalProgress { observation },
                        Some(epochs),
                        clock,
                    );
                }
                session.advance(EngineEvent {
                    epochs,
                    monotonic_ns: monotonic(clock),
                    kind: EngineEventKind::LocalFailed { code: error.code },
                })?;
                return Ok(token);
            }
        }
        Event::StopFailed {
            request,
            token,
            error,
            observation,
        } => {
            let snapshot = session.snapshot();
            if session.pending_local_stop_request() != Some(&request.request_id)
                || !request.matches_snapshot(&snapshot)
                || !request.matches_source(&token)
                || observation
                    .as_ref()
                    .is_some_and(|value| value.source_entry_id != token.final_entry_id)
            {
                return Err(EngineError::new(
                    ErrorCode::StaleSession,
                    "Stop failure no longer matches the pending source Stop",
                ));
            }
            if let Some(observation) = observation {
                advance(
                    session,
                    EngineEventKind::LocalProgress { observation },
                    Some(snapshot.epochs),
                    clock,
                );
            }
            session.advance(EngineEvent {
                epochs: snapshot.epochs,
                monotonic_ns: monotonic(clock),
                kind: EngineEventKind::LocalFailed { code: error.code },
            })?;
            return Ok(Some(token));
        }
    }
    Ok(None)
}

fn monotonic(clock: &Instant) -> u64 {
    clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

struct GraphJobs {
    service: GraphService,
    job: Option<(RequestId, GraphCancellation)>,
    ready: Option<PreparedPipeline>,
}
impl GraphJobs {
    fn spawn(asset_store: PathBuf) -> Result<Self, EngineError> {
        Ok(Self {
            service: GraphService::spawn(Some(asset_store))?,
            job: None,
            ready: None,
        })
    }
    fn poll(&mut self, session: &mut Session, now_ns: u64) -> Result<(), EngineError> {
        session.expire_graph_work(now_ns);
        if self
            .job
            .as_ref()
            .is_some_and(|(id, _)| session.graph_job().is_none_or(|job| &job.request_id != id))
            && let Some((_, cancel)) = self.job.take()
        {
            cancel.cancel();
        }
        if self.ready.as_ref().is_some_and(|p| {
            session
                .graph_inspection()
                .prepared
                .as_ref()
                .is_none_or(|r| {
                    r.preview.binding != *p.binding()
                        || r.preview.binding.expected_revision != session.snapshot().revision
                })
        }) {
            self.ready = None;
        }
        while let Some(completion) = self.service.try_completion() {
            let current = self
                .job
                .as_ref()
                .is_some_and(|(id, _)| id == &completion.request_id);
            if !current {
                continue;
            }
            self.job = None;
            match completion.result {
                Ok(success) => {
                    if session
                        .complete_graph_preparation(
                            &completion.request_id,
                            &completion.binding,
                            now_ns,
                            Ok(success.preview),
                        )
                        .is_ok()
                    {
                        self.ready = success.pipeline;
                    }
                }
                Err(diagnostic) => {
                    let _ = session.complete_graph_preparation(
                        &completion.request_id,
                        &completion.binding,
                        now_ns,
                        Err(diagnostic),
                    );
                }
            }
        }
        let observed = self.service.resources();
        session.observe_graph_resources(
            GraphServiceObservation {
                resident_bytes: observed.resident_bytes,
                pending_retirements: observed.pending_retirements,
                preparing_or_candidate: observed.preparing_or_candidate,
            },
            now_ns,
        )
    }
    fn submit(&mut self, session: &Session, worker: &Worker) -> Result<(), EngineError> {
        let job = session.graph_job().ok_or(IO)?;
        let id = job.request_id.clone();
        let cancel = self
            .service
            .submit(job, session.snapshot(), worker.source_cancellation()?)?;
        self.job = Some((id, cancel));
        Ok(())
    }
    fn validate_activation(&self, session: &Session) -> Result<(), EngineError> {
        let activation = session.graph_activation().ok_or(IO)?;
        if self.ready.as_ref().is_none_or(|p| {
            p.binding() != &activation.previous_binding
                || p.preview().graph_hash != activation.prepared.preview.graph_hash
        }) {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "prepared graph owner is unavailable",
            ));
        }
        Ok(())
    }
    fn activate(&mut self, worker: &mut Worker, session: &Session) -> Result<(), EngineError> {
        self.validate_activation(session)?;
        let activation = session.graph_activation().ok_or(IO)?;
        let pipeline = self.ready.take().ok_or(IO)?;
        worker.send(Action::ActivateGraph {
            settings: Settings {
                snapshot: session.snapshot(),
                playing: session.wants_playback(),
            },
            activation,
            pipeline,
        })
    }
}
impl Drop for GraphJobs {
    fn drop(&mut self) {
        if let Some((_, cancel)) = self.job.take() {
            cancel.cancel();
        }
        self.ready = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
        control::{ControlLimits, OutputBackend, OutputConfiguration, OutputRoute, ReceiptStatus},
        graph::{GraphDocument, GraphProcessingRequest, QuantizationPolicy},
        ids::{DeviceId, OwnerId, ProfileId, QueueEntryId, SessionId, ZoneId},
        safety::{FidelityPolicy, VolumeMode},
        session::{PlaybackObservation, PlaybackProcessingObservation, TransportState},
    };
    use serde_json::json;

    fn command(session: &Session, id: &str, payload: Command) -> CommandEnvelope {
        let state = session.snapshot();
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(id).unwrap(),
            zone_id: state.zone_id,
            owner_id: state.owner_id,
            session_id: state.session_id,
            expected_revision: Some(state.revision),
            payload,
        }
    }
    fn activation() -> (Session, GraphActivationAck) {
        activation_at_revision(0)
    }
    fn activation_at_revision(revision: u64) -> (Session, GraphActivationAck) {
        let (session, ack, _) = activation_resources(revision, "fixture.wav", false);
        (session, ack)
    }
    fn activation_resources(
        revision: u64,
        path: &str,
        observe_backend: bool,
    ) -> (Session, GraphActivationAck, PreparedPipeline) {
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owner").unwrap(),
            ControlLimits::default(),
        );
        if revision != 0 {
            let mut checkpoint = session.checkpoint();
            let restored_revision = revision.min(u64::MAX - 64);
            checkpoint.revision = restored_revision;
            session = Session::restore_owner(
                ZoneId::new("main").unwrap(),
                OwnerId::new("owner").unwrap(),
                ControlLimits::default(),
                checkpoint,
            )
            .unwrap();
            for index in 0..revision - restored_revision {
                let request = command(
                    &session,
                    &format!("startup-padding-{index}"),
                    Command::Mute { muted: true },
                );
                assert_eq!(
                    session.submit_graph(request, 0).status,
                    ReceiptStatus::Applied
                );
            }
        }
        session.enable_local_playback();
        let output = OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "null".into(),
            device_id: DeviceId::new("null").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        };
        for (id, payload) in [
            (
                "output",
                Command::OutputSelect {
                    output: output.clone(),
                },
            ),
            (
                "consent",
                Command::OutputConsent {
                    device_id: output.device_id.clone(),
                    profile_id: output.profile_id.clone(),
                },
            ),
            ("queue", Command::QueueAppend { path: path.into() }),
        ] {
            let request = command(&session, id, payload);
            assert_eq!(
                session.submit_graph(request, 1).status,
                ReceiptStatus::Applied
            );
        }
        let mut start = command(
            &session,
            "start",
            Command::Start {
                source: SourceKind::LocalFile,
                device_id: output.device_id.clone(),
                seekable: true,
                takeover: false,
            },
        );
        start.session_id = Some(SessionId::new("session").unwrap());
        assert_eq!(
            session.submit_graph(start, 2).status,
            ReceiptStatus::Accepted
        );
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
        let backend = if observe_backend {
            let mut native =
                crate::output::open(&output, format, session.snapshot().epochs).unwrap();
            let observed = native.observation().unwrap();
            native.stop().unwrap();
            Some(observed)
        } else {
            None
        };
        let observation = PlaybackObservation {
            output_activity: None,
            processing: None,
            format,
            submitted_frames: 0,
            consumed_frames: None,
            backend_delay_frames: None,
            consumption_basis: "unit activation contract",
            source_entry_id: session.snapshot().active_entry_id.unwrap(),
            source_position_frames: 0,
            audible_entry_id: None,
            audible_source_position_frames: None,
            input_peak: None,
            output_peak: None,
            modified: false,
            backend,
        };
        session
            .advance(EngineEvent {
                epochs: session.snapshot().epochs,
                monotonic_ns: 3,
                kind: EngineEventKind::LocalPrepared {
                    observation: observation.clone(),
                },
            })
            .unwrap();
        let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo",
            "precision":"float64","max_quantum_frames":64});
        let graph: GraphDocument = serde_json::from_value(json!({"schema_version":1,
            "nodes":[{"id":"input","kind":{"kind":"input"},"format":port},
                {"id":"output","kind":{"kind":"output"},"format":port}],
            "edges":[{"from":"input","to":"output","from_port":0,"to_port":0}],"output":"output"}))
        .unwrap();
        let prepare = command(
            &session,
            "prepare",
            Command::GraphPrepare {
                graph: Arc::new(graph),
                processing: GraphProcessingRequest::Float64 {
                    quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
                },
            },
        );
        assert_eq!(
            session.submit_graph(prepare.clone(), 4).status,
            ReceiptStatus::Accepted
        );
        let mut service = GraphService::spawn(None).unwrap();
        let job = session.graph_job().unwrap();
        service
            .submit(
                job.clone(),
                session.snapshot(),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = service.try_completion() {
                break result.result.unwrap();
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        };
        let preview = result.preview.clone();
        session
            .complete_graph_preparation(&prepare.request_id, &job.binding, 5, Ok(preview.clone()))
            .unwrap();
        let pipeline = result.pipeline.unwrap();
        drop(service);
        let prepared = session.graph_inspection().prepared.unwrap();
        let commit = command(
            &session,
            "commit",
            Command::GraphCommit {
                token: prepared.token,
                graph_hash: preview.graph_hash.clone(),
            },
        );
        assert_eq!(
            session.submit_graph(commit.clone(), 6).status,
            ReceiptStatus::Accepted
        );
        let mut observation = observation;
        observation.processing = Some(PlaybackProcessingObservation {
            streaming: None,
            source_boundary: None,
            graph_hash: preview.graph_hash.clone(),
            resident_bytes: preview.resources.pipeline_bytes,
            audio_payload_bytes: Some(0),
            failure_node: None,
            timing: earph_core::graph_control::PlaybackTiming::SameRate {
                latency_frames: 0,
                tail_frames: 0,
            },
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
        });
        let acknowledgement = GraphActivationAck {
            source_total_frames: 48000,
            epochs: session.snapshot().epochs,
            request_id: commit.request_id,
            graph_hash: preview.graph_hash.clone(),
            activation_frame: 0,
            observation,
        };
        (session, acknowledgement, pipeline)
    }
    #[test]
    fn stale_acknowledgement_during_recovery_keeps_the_owner_and_stop_alive() {
        let (mut session, acknowledgement) = activation();
        session.fail_graph_activation(&acknowledgement.request_id, IO);
        assert!(session.graph_inspection().recovery_required);
        let stop = command(&session, "stop", Command::Stop);
        assert_eq!(
            session.submit_graph(stop.clone(), 7).status,
            ReceiptStatus::Accepted
        );
        let before = session.snapshot();
        reconcile_graph_activation(&mut session, acknowledgement, 8, |_| {
            panic!("stale ack must not persist")
        })
        .unwrap();
        assert_eq!(session.snapshot(), before);
        assert_eq!(
            session.submit_graph(stop, 9).status,
            ReceiptStatus::Accepted
        );
    }
    #[test]
    fn current_checkpoint_failure_still_requests_owner_shutdown_and_recovery() {
        let (mut session, acknowledgement) = activation();
        let error =
            reconcile_graph_activation(&mut session, acknowledgement, 7, |_| Err(IO)).unwrap_err();
        assert_eq!(error, IO);
        assert!(session.graph_inspection().recovery_required);
        assert_eq!(session.snapshot().transport, TransportState::Failed);
        assert!(!session.snapshot().output_consented);
    }
    #[test]
    fn current_terminal_completion_failure_still_requests_worker_stop() {
        let (mut session, acknowledgement) = activation_at_revision(u64::MAX - 8);
        for (now, id) in [(7, "first-mute"), (8, "last-mute")] {
            let mute = command(&session, id, Command::Mute { muted: true });
            assert_eq!(
                session.submit_graph(mute, now).status,
                ReceiptStatus::Applied
            );
        }
        assert_eq!(session.snapshot().revision, u64::MAX);
        let error = reconcile_graph_activation(&mut session, acknowledgement, 9, |_| {
            panic!("exhausted completion cannot persist");
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ResourceBudgetExceeded);
        assert!(session.graph_inspection().recovery_required);
        assert!(session.graph_activation().is_none());
    }
    #[test]
    fn rejected_safety_budget_cancels_the_matching_activation_and_worker() {
        use std::sync::atomic::Ordering;
        for payload in [
            Command::Mute { muted: true },
            Command::VolumeCaps {
                software_millidecibels: -48000,
                hardware_millidecibels: None,
            },
        ] {
            let (mut session, acknowledgement) = activation();
            let mut worker = Worker::spawn();
            let cancel = Arc::new(AtomicBool::new(false));
            worker
                .send(Action::Start {
                    settings: Settings {
                        snapshot: session.snapshot(),
                        playing: false,
                    },
                    entries: vec![],
                    cancel: cancel.clone(),
                })
                .unwrap();
            let command = command(&session, "safety-budget", payload);
            let receipt = earph_core::control::Receipt {
                request_id: command.request_id.clone(),
                status: ReceiptStatus::Rejected,
                zone_id: session.snapshot().zone_id,
                session_id: session.snapshot().session_id,
                revision: session.snapshot().revision,
                epochs: session.snapshot().epochs,
                error: Some(EngineError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "test reserved event slot",
                )),
                result: None,
                activation_frame: None,
            };
            cancel_rejected_safety(&mut session, &worker, &command, &receipt);
            assert!(cancel.load(Ordering::Acquire));
            assert!(session.graph_activation().is_none());
            assert!(session.graph_inspection().recovery_required);
            assert_eq!(session.snapshot().transport, TransportState::Failed);
            assert_eq!(
                session.snapshot().error,
                Some(ErrorCode::ResourceBudgetExceeded)
            );
            assert_eq!(
                session
                    .events_since(0)
                    .events
                    .last()
                    .unwrap()
                    .request_id
                    .as_ref(),
                Some(&acknowledgement.request_id)
            );
        }
    }
    #[test]
    fn ended_source_failure_settles_the_pending_reposition_receipt() {
        let (mut session, acknowledgement) = activation();
        reconcile_graph_activation(&mut session, acknowledgement, 7, |_| Ok(())).unwrap();
        let pause = command(&session, "pause-ended", Command::Pause);
        assert_eq!(
            session.submit_graph(pause.clone(), 8).status,
            ReceiptStatus::Accepted
        );
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let epochs = session.snapshot().epochs;
        observation(
            &mut session,
            Event::Failed {
                epochs,
                token: None,
                error: EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "source ended before reposition",
                ),
                observation: None,
            },
            &clock,
        )
        .unwrap();
        let replay = session.submit_graph(pause, monotonic(&clock) + 1);
        assert_eq!(replay.status, ReceiptStatus::Failed);
        assert_eq!(replay.error.unwrap().code, ErrorCode::CapabilityUnsupported);
        assert!(session.snapshot().operation.is_none());
    }
    fn terminal_token(
        session: &Session,
        origin: Epochs,
        observation: &PlaybackObservation,
    ) -> SourceTerminalToken {
        let snapshot = session.snapshot();
        SourceTerminalToken {
            owner_id: snapshot.owner_id,
            session_id: snapshot.session_id.unwrap(),
            origin_epochs: origin,
            graph_epoch: snapshot.graph_epoch,
            device_epoch: snapshot.device_epoch,
            final_entry_id: observation.source_entry_id.clone(),
        }
    }
    #[test]
    fn proven_native_completion_cancels_graph_commit_without_an_output_fault() {
        let (mut session, acknowledgement) = activation();
        let activation = session.graph_activation().unwrap();
        let mut commit = command(
            &session,
            "commit",
            Command::GraphCommit {
                token: activation.prepared.token.clone(),
                graph_hash: activation.prepared.preview.graph_hash.clone(),
            },
        );
        commit.expected_revision = Some(activation.previous_binding.expected_revision);
        let epochs = session.snapshot().epochs;
        let mut final_observation = acknowledgement.observation;
        final_observation.processing = None;
        final_observation.source_position_frames = 65;
        let token = terminal_token(
            &session,
            activation.previous_binding.epochs,
            &final_observation,
        );
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let accepted = observation(
            &mut session,
            Event::Drained {
                epochs,
                token: token.clone(),
                observation: final_observation.clone(),
            },
            &clock,
        )
        .unwrap();
        assert_eq!(session.snapshot().transport, TransportState::Idle);
        assert!(session.snapshot().output_consented);
        assert!(!session.graph_inspection().recovery_required);
        assert!(session.graph_activation().is_none());
        assert_eq!(session.snapshot().playback.unwrap(), final_observation);
        assert_eq!(accepted, Some(token));
        let replay = session.submit_graph(commit, monotonic(&clock) + 1);
        assert_eq!(replay.status, ReceiptStatus::Failed);
        assert_eq!(replay.error.unwrap().code, ErrorCode::CapabilityUnsupported);
    }
    #[test]
    fn proven_completion_settles_pause_and_seek_without_revoking_consent() {
        for payload in [Command::Pause, Command::Seek { position_frames: 0 }] {
            let (mut session, acknowledgement) = activation();
            reconcile_graph_activation(&mut session, acknowledgement, 7, |_| Ok(())).unwrap();
            let before = session.snapshot();
            let final_observation = before.playback.clone().unwrap();
            session
                .set_queue_preparation(
                    before.epochs,
                    &final_observation.source_entry_id,
                    earph_core::queue::PreparationStatus::Ready {
                        format: final_observation.format,
                        total_frames: 65,
                    },
                )
                .unwrap();
            let token = terminal_token(&session, before.epochs, &final_observation);
            let position = command(&session, "position-completed", payload);
            assert_eq!(
                session.submit_graph(position.clone(), 8).status,
                ReceiptStatus::Accepted
            );
            let epochs = session.snapshot().epochs;
            let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            let accepted = observation(
                &mut session,
                Event::Drained {
                    epochs,
                    token: token.clone(),
                    observation: final_observation,
                },
                &clock,
            )
            .unwrap();
            assert_eq!(accepted, Some(token));
            assert_eq!(session.snapshot().transport, TransportState::Idle);
            assert!(session.snapshot().output_consented);
            assert!(!session.graph_inspection().recovery_required);
            let replay = session.submit_graph(position, monotonic(&clock) + 1);
            assert_eq!(replay.status, ReceiptStatus::Failed);
            assert_eq!(replay.error.unwrap().code, ErrorCode::CapabilityUnsupported);
            assert!(session.snapshot().operation.is_none());
        }
    }
    #[test]
    fn a_stale_completion_does_not_acknowledge_or_discard_the_current_activation() {
        let (mut session, acknowledgement) = activation();
        let activation = session.graph_activation().unwrap();
        let observation_value = acknowledgement.observation;
        let token = terminal_token(
            &session,
            activation.previous_binding.epochs,
            &observation_value,
        );
        let before = session.snapshot();
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let result = observation(
            &mut session,
            Event::Drained {
                epochs: activation.previous_binding.epochs,
                token,
                observation: observation_value,
            },
            &clock,
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::StaleSession);
        assert_eq!(session.snapshot(), before);
        assert!(session.graph_activation().is_some());
    }
    #[test]
    fn proven_completion_at_revision_exhaustion_closes_the_source_and_retires_the_owner() {
        let (mut session, acknowledgement) = activation_at_revision(u64::MAX - 8);
        let activation = session.graph_activation().unwrap();
        for (now, id) in [(7, "first-mute"), (8, "last-mute")] {
            let mute = command(&session, id, Command::Mute { muted: true });
            assert_eq!(
                session.submit_graph(mute, now).status,
                ReceiptStatus::Applied
            );
        }
        assert_eq!(session.snapshot().revision, u64::MAX);
        let epochs = session.snapshot().epochs;
        let value = acknowledgement.observation;
        let token = terminal_token(&session, activation.previous_binding.epochs, &value);
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(
            observation(
                &mut session,
                Event::Drained {
                    epochs,
                    token: token.clone(),
                    observation: value
                },
                &clock
            )
            .unwrap(),
            Some(token)
        );
        assert_eq!(session.snapshot().revision, u64::MAX);
        assert_eq!(session.snapshot().transport, TransportState::Idle);
        assert_eq!(
            session.snapshot().error,
            Some(ErrorCode::ResourceBudgetExceeded)
        );
        assert!(session.shutdown_requested());
        assert!(session.snapshot().output_consented);
        assert!(!session.graph_inspection().recovery_required);
        assert!(session.graph_activation().is_none());
    }
    #[test]
    fn retained_pause_fault_settles_after_mute_advances_the_stream() {
        let (mut session, acknowledgement) = activation();
        reconcile_graph_activation(&mut session, acknowledgement, 7, |_| Ok(())).unwrap();
        let unmute = command(
            &session,
            "unmute-before-pause",
            Command::Mute { muted: false },
        );
        assert_eq!(
            session.submit_graph(unmute, 8).status,
            ReceiptStatus::Applied
        );
        let pause = command(&session, "pause-before-fault", Command::Pause);
        assert_eq!(
            session.submit_graph(pause.clone(), 9).status,
            ReceiptStatus::Accepted
        );
        let pause_epochs = session.snapshot().epochs;
        let value = session.snapshot().playback.unwrap();
        let token = terminal_token(&session, pause_epochs, &value);
        let mute = command(&session, "mute-during-pause", Command::Mute { muted: true });
        assert_eq!(
            session.submit_graph(mute, 10).status,
            ReceiptStatus::Applied
        );
        let epochs = session.snapshot().epochs;
        assert_eq!(epochs.stream, pause_epochs.stream + 1);
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let fault = EngineError::new(ErrorCode::OutputFault, "native suspend failed");
        let before = session.snapshot();
        assert_eq!(
            observation(
                &mut session,
                Event::Failed {
                    epochs: pause_epochs,
                    token: Some(token.clone()),
                    error: fault,
                    observation: Some(value.clone())
                },
                &clock
            )
            .unwrap_err()
            .code,
            ErrorCode::StaleSession
        );
        assert_eq!(session.snapshot(), before);
        let accepted = observation(
            &mut session,
            Event::Failed {
                epochs,
                token: Some(token.clone()),
                error: fault,
                observation: Some(value),
            },
            &clock,
        )
        .unwrap();
        assert_eq!(accepted, Some(token));
        assert_eq!(session.snapshot().transport, TransportState::Failed);
        assert!(session.snapshot().muted && !session.snapshot().output_consented);
        let replay = session.submit_graph(pause, monotonic(&clock) + 1);
        assert_eq!(replay.status, ReceiptStatus::Failed);
        assert_eq!(replay.error.unwrap().code, ErrorCode::OutputFault);
        assert!(session.snapshot().operation.is_none());
    }
    #[test]
    fn source_fault_settles_a_queued_graph_activation_with_one_failure_fact() {
        let (mut session, acknowledgement) = activation();
        let activation = session.graph_activation().unwrap();
        let token = terminal_token(
            &session,
            activation.previous_binding.epochs,
            &acknowledgement.observation,
        );
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let cursor = session.snapshot().event_sequence;
        let epochs = session.snapshot().epochs;
        let accepted = observation(
            &mut session,
            Event::Failed {
                epochs,
                token: Some(token.clone()),
                error: EngineError::new(ErrorCode::OutputFault, "native output failed"),
                observation: Some(acknowledgement.observation),
            },
            &clock,
        )
        .unwrap();
        assert_eq!(accepted, Some(token));
        assert!(session.graph_activation().is_none());
        assert!(session.graph_inspection().recovery_required);
        assert!(!session.snapshot().output_consented);
        assert!(
            session
                .events_since(cursor)
                .events
                .iter()
                .any(|event| event.request_id.as_ref() == Some(&activation.request_id))
        );
    }
    #[test]
    fn a_failure_from_a_replaced_source_cannot_close_the_current_source() {
        let (mut session, acknowledgement) = activation();
        reconcile_graph_activation(&mut session, acknowledgement, 7, |_| Ok(())).unwrap();
        let value = session.snapshot().playback.unwrap();
        let token = terminal_token(&session, session.snapshot().epochs, &value);
        let stop = command(&session, "replace-stop", Command::Stop);
        assert_eq!(
            session.submit_graph(stop, 8).status,
            ReceiptStatus::Accepted
        );
        session
            .advance(EngineEvent {
                epochs: session.snapshot().epochs,
                monotonic_ns: 9,
                kind: EngineEventKind::LocalStopped { observation: None },
            })
            .unwrap();
        let mut start = command(
            &session,
            "replace-start",
            Command::Start {
                source: SourceKind::LocalFile,
                device_id: DeviceId::new("null").unwrap(),
                seekable: true,
                takeover: false,
            },
        );
        start.session_id = Some(SessionId::new("replacement-source").unwrap());
        assert_eq!(
            session.submit_graph(start, 10).status,
            ReceiptStatus::Accepted
        );
        let before = session.snapshot();
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert_eq!(
            observation(
                &mut session,
                Event::Failed {
                    epochs: before.epochs,
                    token: Some(token),
                    error: EngineError::new(ErrorCode::OutputFault, "old source fault"),
                    observation: Some(value),
                },
                &clock
            )
            .unwrap_err()
            .code,
            ErrorCode::StaleSession
        );
        assert_eq!(session.snapshot(), before);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn native_activation_recovers_readiness_from_the_reused_open_file() {
        use std::{fs, time::SystemTime};
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/activation-readiness")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("source.wav");
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(296_u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(2_u16.to_le_bytes());
        bytes.extend(48000_u32.to_le_bytes());
        bytes.extend(192000_u32.to_le_bytes());
        bytes.extend(4_u16.to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(260_u32.to_le_bytes());
        bytes.extend([0_u8; 260]);
        fs::write(&path, bytes).unwrap();
        let (mut session, _, pipeline) = activation_resources(0, path.to_str().unwrap(), true);
        let activation = session.graph_activation().unwrap();
        let mut settings = Settings {
            snapshot: session.snapshot(),
            playing: false,
        };
        settings.snapshot.epochs = activation.previous_binding.epochs;
        let entries = session.queue_snapshot().entries;
        let id = entries[0].id.clone();
        let mut worker = Worker::spawn();
        worker
            .view
            .lock()
            .unwrap()
            .publish(session.queue_snapshot());
        worker
            .send(Action::Start {
                settings,
                entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        loop {
            let event = worker.events.recv_timeout(Duration::from_secs(5)).unwrap();
            if let Event::Failed { error, .. } = &event {
                panic!("real output preparation failed: {error:?}");
            }
            let ready = matches!(event, Event::Ready { .. });
            observation(&mut session, event, &clock).unwrap();
            if ready {
                break;
            }
        }
        assert!(matches!(
            session.queue_snapshot().entries[0].status,
            earph_core::queue::PreparationStatus::Unprepared
        ));
        worker
            .send(Action::ActivateGraph {
                settings: Settings {
                    snapshot: session.snapshot(),
                    playing: false,
                },
                activation,
                pipeline,
            })
            .unwrap();
        loop {
            let event = worker.events.recv_timeout(Duration::from_secs(5)).unwrap();
            if let Event::Failed { error, .. } = &event {
                panic!("real graph activation failed: {error:?}");
            }
            if let Event::GraphActivated(ack) = event {
                assert_eq!(ack.source_total_frames, 65);
                reconcile_graph_activation(&mut session, ack, monotonic(&clock), |staged| {
                    assert_eq!(staged.snapshot().active_entry_id.as_ref(), Some(&id));
                    assert!(matches!(
                        staged.queue_snapshot().entries[0].status,
                        earph_core::queue::PreparationStatus::Ready {
                            total_frames: 65,
                            ..
                        }
                    ));
                    Ok(())
                })
                .unwrap();
                break;
            }
            let before = session.snapshot();
            observation(&mut session, event, &clock).unwrap();
            assert_eq!(session.snapshot().active_entry_id, before.active_entry_id);
        }
        let seek = command(
            &session,
            "seek-reused-open-file",
            Command::Seek {
                position_frames: 64,
            },
        );
        assert_eq!(
            session.submit_graph(seek, monotonic(&clock)).status,
            ReceiptStatus::Accepted
        );
    }
    #[test]
    fn a_native_stop_fault_settles_stop_after_its_graph_acknowledgement_is_cancelled() {
        for revision in [0, u64::MAX - 7] {
            let (mut session, ack) = activation_at_revision(revision);
            let stop = command(
                &session,
                "stop-installed-unacknowledged-graph",
                Command::Stop,
            );
            assert_eq!(
                session.submit_graph(stop.clone(), 8).status,
                ReceiptStatus::Accepted
            );
            assert!(session.graph_activation().is_none());
            let before = session.snapshot();
            reconcile_graph_activation(&mut session, ack.clone(), 9, |_| {
                panic!("cancelled acknowledgement cannot persist")
            })
            .unwrap();
            assert_eq!(session.snapshot(), before);
            let snapshot = session.snapshot();
            let mut token = terminal_token(&session, ack.epochs, &ack.observation);
            token.graph_epoch = snapshot.graph_epoch + 1;
            let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            let request = StopRequest {
                request_id: stop.request_id.clone(),
                settings: Settings {
                    snapshot,
                    playing: false,
                },
            };
            observation(
                &mut session,
                Event::StopFailed {
                    request: Box::new(request),
                    token,
                    error: EngineError::new(ErrorCode::OutputFault, "native Stop failed"),
                    observation: Some(ack.observation),
                },
                &clock,
            )
            .unwrap();
            assert_eq!(
                session.submit_graph(stop, monotonic(&clock) + 1).status,
                ReceiptStatus::Failed
            );
            assert!(!session.snapshot().output_consented);
            if revision != 0 {
                assert_eq!(session.snapshot().revision, u64::MAX);
                assert_eq!(session.snapshot().error, Some(ErrorCode::OutputFault));
                assert!(session.shutdown_requested());
            }
        }
    }
    #[test]
    fn stale_stop_fault_bindings_cannot_settle_an_unrelated_source_or_request() {
        for invalid in [
            "request",
            "owner",
            "captured_graph",
            "captured_epoch",
            "token_owner",
            "token_session",
            "token_device",
            "future_origin",
            "future_graph",
            "entry",
            "replacement",
        ] {
            let (mut session, ack) = activation();
            let stop = command(&session, "bound-stop", Command::Stop);
            assert_eq!(
                session.submit_graph(stop.clone(), 8).status,
                ReceiptStatus::Accepted
            );
            let snapshot = session.snapshot();
            let mut request = StopRequest {
                request_id: stop.request_id,
                settings: Settings {
                    snapshot: snapshot.clone(),
                    playing: false,
                },
            };
            let mut token = terminal_token(&session, ack.epochs, &ack.observation);
            let value = ack.observation;
            match invalid {
                "request" => request.request_id = RequestId::new("different-stop").unwrap(),
                "owner" => {
                    request.settings.snapshot.owner_id =
                        Some(OwnerId::new("another-owner").unwrap())
                }
                "captured_graph" => request.settings.snapshot.graph_epoch += 1,
                "captured_epoch" => request.settings.snapshot.epochs.stream -= 1,
                "token_owner" => token.owner_id = Some(OwnerId::new("another-owner").unwrap()),
                "token_session" => token.session_id = SessionId::new("another-session").unwrap(),
                "token_device" => token.device_epoch += 1,
                "future_origin" => token.origin_epochs.stream = snapshot.epochs.stream + 1,
                "future_graph" => token.graph_epoch = snapshot.graph_epoch + 2,
                "entry" => token.final_entry_id = QueueEntryId::new("different-entry").unwrap(),
                "replacement" => {
                    let mut start = command(
                        &session,
                        "replacement-before-old-stop-fault",
                        Command::Start {
                            source: SourceKind::LocalFile,
                            device_id: DeviceId::new("null").unwrap(),
                            seekable: true,
                            takeover: true,
                        },
                    );
                    start.session_id = Some(SessionId::new("replacement-stop-source").unwrap());
                    assert_eq!(
                        session.submit_graph(start, 9).status,
                        ReceiptStatus::Accepted
                    );
                }
                _ => unreachable!(),
            }
            let before = session.snapshot();
            let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
            assert_eq!(
                observation(
                    &mut session,
                    Event::StopFailed {
                        request: Box::new(request),
                        token,
                        error: EngineError::new(ErrorCode::OutputFault, "stale native Stop fault"),
                        observation: Some(value)
                    },
                    &clock
                )
                .unwrap_err()
                .code,
                ErrorCode::StaleSession,
                "{invalid}"
            );
            assert_eq!(session.snapshot(), before, "{invalid}");
        }
    }
    #[test]
    fn a_stop_failure_cannot_settle_a_pending_pause_with_the_same_request_id() {
        let (mut session, ack) = activation();
        reconcile_graph_activation(&mut session, ack.clone(), 7, |_| Ok(())).unwrap();
        let pause = command(&session, "pending-pause-not-stop", Command::Pause);
        assert_eq!(
            session.submit_graph(pause.clone(), 8).status,
            ReceiptStatus::Accepted
        );
        assert!(session.pending_local_stop_request().is_none());
        let snapshot = session.snapshot();
        let token = terminal_token(&session, snapshot.epochs, &ack.observation);
        let request = StopRequest {
            request_id: pause.request_id,
            settings: Settings {
                snapshot: snapshot.clone(),
                playing: false,
            },
        };
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(
            observation(
                &mut session,
                Event::StopFailed {
                    request: Box::new(request),
                    token,
                    error: EngineError::new(ErrorCode::OutputFault, "not this operation"),
                    observation: None
                },
                &clock
            )
            .is_err()
        );
        assert_eq!(session.snapshot(), snapshot);
    }
    #[test]
    fn successful_stop_after_normal_eof_keeps_consent_and_its_applied_receipt() {
        let (mut session, ack) = activation();
        let stop = command(&session, "stop-after-native-finish", Command::Stop);
        assert_eq!(
            session.submit_graph(stop.clone(), 8).status,
            ReceiptStatus::Accepted
        );
        let snapshot = session.snapshot();
        let stale_token = terminal_token(&session, ack.epochs, &ack.observation);
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        observation(
            &mut session,
            Event::Stopped {
                epochs: snapshot.epochs,
                observation: Some(ack.observation.clone()),
            },
            &clock,
        )
        .unwrap();
        assert!(session.snapshot().output_consented);
        assert_eq!(session.snapshot().transport, TransportState::Idle);
        assert_eq!(
            session.submit_graph(stop, monotonic(&clock) + 1).status,
            ReceiptStatus::Applied
        );
        assert!(session.pending_local_stop_request().is_none());
        let before = session.snapshot();
        assert!(
            observation(
                &mut session,
                Event::Drained {
                    epochs: ack.epochs,
                    token: stale_token,
                    observation: ack.observation
                },
                &clock
            )
            .is_err()
        );
        assert_eq!(session.snapshot(), before);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_cancelled_preparation_settles_without_revoking_consent() {
        let path = crate::worker::tests::owned_silence_waves(1).pop().unwrap();
        let (mut session, _, pipeline) = activation_resources(0, path.to_str().unwrap(), false);
        drop(pipeline);
        let origin = session.graph_activation().unwrap().previous_binding.epochs;
        let mut settings = Settings {
            snapshot: session.snapshot(),
            playing: false,
        };
        settings.snapshot.epochs = origin;
        let stop = command(&session, "stop-before-output-opens", Command::Stop);
        assert_eq!(
            session.submit_graph(stop.clone(), 8).status,
            ReceiptStatus::Accepted
        );
        let mut worker = Worker::spawn();
        let entries = session.queue_snapshot().entries;
        worker
            .view
            .lock()
            .unwrap()
            .publish(session.queue_snapshot());
        worker
            .send(Action::Start {
                settings,
                entries,
                cancel: Arc::new(AtomicBool::new(true)),
            })
            .unwrap();
        let clock = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        let event = worker.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(
            event,
            Event::Failed {
                error: EngineError {
                    code: ErrorCode::OperationCancelled,
                    ..
                },
                ..
            }
        ));
        assert!(observation(&mut session, event, &clock).is_err());
        worker
            .send(Action::Stop(StopRequest {
                request_id: stop.request_id.clone(),
                settings: Settings {
                    snapshot: session.snapshot(),
                    playing: false,
                },
            }))
            .unwrap();
        let event = worker.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(event, Event::Stopped { .. }));
        observation(&mut session, event, &clock).unwrap();
        assert!(session.snapshot().output_consented);
        assert!(session.snapshot().error.is_none());
        assert_eq!(
            session.submit_graph(stop, monotonic(&clock)).status,
            ReceiptStatus::Applied
        );
    }
}
