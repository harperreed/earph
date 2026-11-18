// ABOUTME: Reconciles worker observations through the authoritative session reducer.
// ABOUTME: Commits operational state before publishing queue changes or worker controls.
use crate::{
    directory,
    graph::{GraphCancellation, GraphService, PreparedPipeline},
    ipc, runtime,
    worker::{Action, Event, Settings, Worker},
};
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, ReceiptStatus, SourceKind},
    graph_control::GraphServiceObservation,
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
) -> Result<(), EngineError> {
    let server = ipc::Server::spawn(listener)?;
    let mut graphs = GraphJobs::spawn()?;
    // Worker pipelines must retire before the preparation service shuts down.
    let mut worker = Worker::spawn();
    let clock = Instant::now();
    loop {
        loop {
            match worker.events.try_recv() {
                Ok(Event::GraphActivated {
                    epochs,
                    request_id,
                    graph_hash,
                    activation_frame,
                }) => {
                    let applied = session.complete_graph_activation(
                        &request_id,
                        epochs,
                        &graph_hash,
                        monotonic(&clock),
                        activation_frame,
                        |staged| {
                            let bytes = serde_json::to_vec(&staged.checkpoint()).map_err(|_| IO)?;
                            runtime::write_atomic(
                                &directory,
                                "queue.json",
                                &format!("{token}-graph-{}", staged.snapshot().revision),
                                &bytes,
                            )
                        },
                    );
                    if let Err(error) = applied
                        && session.graph_inspection().recovery_required
                    {
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
                Ok(event) => observation(&mut session, event, &clock),
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
        match server.requests.recv_timeout(Duration::from_millis(1)) {
            Ok(pending) => {
                let command = pending.command;
                let before = session.snapshot();
                let old_checkpoint = serde_json::to_vec(&session.checkpoint()).map_err(|_| IO)?;
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
                            Command::GraphPrepare { .. } | Command::GraphValidate { .. }
                        ) {
                            return graphs.submit(staged);
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
                        if matches!(command.payload, Command::GraphCommit { .. }) {
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
                        if let Err(error) = runtime::write_atomic(
                            &directory,
                            "queue.json",
                            &format!("{token}-{}", staged.snapshot().revision),
                            &bytes,
                        ) {
                            persistence_error = Some(error);
                            return Err(error);
                        }
                        let mut gate = view.lock().map_err(|_| IO)?;
                        if let Err(error) = gate.validate_edit(&command.payload) {
                            drop(gate);
                            if let Err(failure) = runtime::write_atomic(
                                &directory,
                                "queue.json",
                                &format!("{token}-rollback-{}", staged.snapshot().revision),
                                &old_checkpoint,
                            ) {
                                persistence_error = Some(failure);
                                return Err(failure);
                            }
                            return Err(error);
                        }
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
                    && let Err(error) = if matches!(command.payload, Command::GraphCommit { .. }) {
                        graphs.activate(&mut worker, &session)
                    } else {
                        dispatch(&command, &mut worker, &session)
                    }
                {
                    if matches!(command.payload, Command::GraphCommit { .. }) {
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
                    return Ok(());
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
        Command::OwnerShutdown | Command::Stop => Some(Action::Stop {
            epochs: settings.snapshot.epochs,
        }),
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
fn observation(session: &mut Session, event: Event, clock: &Instant) {
    match event {
        Event::GraphActivated { .. } | Event::GraphFailed { .. } => {
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
            observation,
        } => {
            advance(
                session,
                EngineEventKind::LocalProgress { observation },
                Some(epochs),
                clock,
            );
            advance(session, EngineEventKind::Drained, Some(epochs), clock);
        }
        Event::Failed {
            epochs,
            error,
            observation,
        } => {
            if error.code != ErrorCode::OperationCancelled {
                if let Some(observation) = observation {
                    advance(
                        session,
                        EngineEventKind::LocalProgress { observation },
                        Some(epochs),
                        clock,
                    );
                }
                advance(
                    session,
                    EngineEventKind::LocalFailed { code: error.code },
                    Some(epochs),
                    clock,
                );
            }
        }
    }
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
    fn spawn() -> Result<Self, EngineError> {
        Ok(Self {
            service: GraphService::spawn()?,
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
    fn submit(&mut self, session: &Session) -> Result<(), EngineError> {
        let job = session.graph_job().ok_or(IO)?;
        let id = job.request_id.clone();
        let cancel = self.service.submit(job, session.snapshot())?;
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
