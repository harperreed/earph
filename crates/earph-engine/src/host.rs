// ABOUTME: Reconciles worker observations through the authoritative session reducer.
// ABOUTME: Commits operational state before publishing queue changes or worker controls.
use crate::{
    directory,
    graph::{GraphCancellation, GraphService, PreparedPipeline},
    ipc, runtime,
    worker::{
        Action, DsdRetirementFailure, Event, Settings, SourceTerminalToken, StopRequest, Worker,
    },
};
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, ReceiptStatus, SourceKind},
    graph_control::{GraphActivationAck, GraphServiceObservation},
    ids::{Epochs, RequestId},
    renderer::{RendererAction, RendererGraphBinding},
    session::{EngineEvent, EngineEventKind, Session},
};
use std::{
    os::unix::net::UnixListener,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
    mut renderer: Option<crate::renderer::RendererRuntime>,
) -> Result<(), EngineError> {
    let server = ipc::Server::spawn(listener)?;
    let mut graphs = GraphJobs::spawn(directory.clone())?;
    let mut artifacts = crate::profiles::ArtifactJobs::spawn(directory.clone());
    let mut devices = crate::devices::DeviceJobs::spawn(device_helper)?;
    // Worker pipelines must retire before the preparation service shuts down.
    let mut worker = Worker::spawn();
    let mut terminal_acknowledgement = None;
    let mut retirement_acknowledgement = None;
    let mut dsd_retirement = DsdRetirementEvidence::default();
    let clock = Instant::now();
    let live_binding = Arc::new(std::sync::Mutex::new(None));
    let mut network_retirement = None;
    loop {
        session.expire_network_authority(monotonic(&clock))?;
        if let Some(renderer) = renderer.as_mut() {
            renderer.poll(&mut session, monotonic(&clock))?;
        }
        synchronize_network(
            &session,
            &mut worker,
            &live_binding,
            &mut network_retirement,
        )?;
        loop {
            match worker.events.try_recv() {
                Ok(Event::DsdRetirementFailed(failure)) => {
                    if dsd_retirement.adopt(failure) {
                        retirement_acknowledgement = dsd_retirement
                            .failure
                            .as_ref()
                            .map(|failure| failure.token.clone());
                    }
                }
                Ok(Event::NetworkGraphFailed {
                    request_id,
                    error,
                    actual,
                }) => {
                    let _ = session.fail_network_graph_activation(&request_id, error, actual);
                }
                Ok(Event::Network(mut event)) => {
                    event.monotonic_ns = monotonic(&clock);
                    let _ = session.advance_network(event);
                }
                Ok(Event::NetworkRetired(ack)) => {
                    if session
                        .complete_network_retirement(ack, monotonic(&clock))
                        .is_ok()
                    {
                        network_retirement = None;
                    }
                }
                Ok(Event::NetworkGraphActivated(ack)) => {
                    if let Err(error) = reconcile_network_graph_activation(
                        &mut session,
                        ack,
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
        if let Some(token) = retirement_acknowledgement.as_ref()
            && worker
                .send(Action::AcknowledgeDsdRetirement(token.clone()))
                .is_ok()
        {
            retirement_acknowledgement = None;
        }
        // Adoption acknowledges evidence ownership, never successful native closure.
        // Keep failed writes in this bounded slot without holding shutdown on stderr.
        if dsd_retirement.failure.is_some() && !dsd_retirement.reported {
            let _ = dsd_retirement.report(&mut std::io::stderr().lock());
        }
        devices.poll(&mut session, monotonic(&clock));
        if session.shutdown_requested() && session.closing_network_source().is_none() {
            devices.cancel();
            artifacts.cancel(&mut session);
            artifacts.poll(&mut session, monotonic(&clock), &directory, &token)?;
            let resources = graphs.service.resources();
            if session.pending_local_stop_request().is_none()
                && devices.is_idle()
                && artifacts.is_idle()
                && resources.resident_bytes == 0
                && resources.pending_retirements == 0
                && !resources.preparing_or_candidate
                && renderer
                    .as_ref()
                    .is_none_or(|renderer| renderer.is_idle(&session))
            {
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
        artifacts.poll(&mut session, monotonic(&clock), &directory, &token)?;
        if let Some(renderer) = renderer.as_mut()
            && let Some((command, binding)) =
                renderer.next_graph_command(&session, monotonic(&clock))
        {
            let executed = OwnerControl {
                session: &mut session,
                worker: &mut worker,
                graphs: &mut graphs,
                artifacts: &mut artifacts,
                devices: &mut devices,
                directory: &directory,
                token: &token,
                clock: &clock,
                live_binding: &live_binding,
                network_retirement: &mut network_retirement,
            }
            .execute(command, Ingress::Graph(binding))?;
            if let Some(error) = executed.persistence_error {
                return Err(error);
            }
        }
        if let Some(renderer) = renderer.as_mut()
            && let Some(pending) = renderer.try_recv()
        {
            match pending {
                earph_network::upnp::IncomingRequest::Action(pending) => {
                    match renderer.admit(&mut session, &pending, monotonic(&clock)) {
                        Ok(crate::renderer::Admission::Read(values)) => {
                            let values: Vec<_> =
                                values.iter().map(|(k, v)| (*k, v.as_str())).collect();
                            pending.respond(Ok(&values));
                        }
                        Ok(crate::renderer::Admission::Command(command)) => {
                            let peer = pending.peer().into();
                            let executed = OwnerControl {
                                session: &mut session,
                                worker: &mut worker,
                                graphs: &mut graphs,
                                artifacts: &mut artifacts,
                                devices: &mut devices,
                                directory: &directory,
                                token: &token,
                                clock: &clock,
                                live_binding: &live_binding,
                                network_retirement: &mut network_retirement,
                            }
                            .execute(*command, Ingress::Peer(peer))?;
                            renderer.poll(&mut session, monotonic(&clock))?;
                            let result = crate::renderer::receipt_result(
                                &executed.receipt,
                                pending.request().service,
                            );
                            pending.respond(result.map(|()| &[][..]));
                            if let Some(error) = executed.persistence_error {
                                return Err(error);
                            }
                        }
                        Err(fault) => {
                            pending.respond(Err(fault));
                        }
                    }
                }
                earph_network::upnp::IncomingRequest::Subscription(pending) => {
                    let mut persistence_error = None;
                    renderer.subscription(&mut session, pending, monotonic(&clock), |staged| {
                        let bytes = serde_json::to_vec(&staged.checkpoint()).map_err(|_| IO)?;
                        runtime::checkpoint_transaction(
                            &directory,
                            &format!("{token}-{}", staged.snapshot().revision),
                            &bytes,
                            || Ok(()),
                        )
                        .map_err(|error| {
                            if let runtime::CheckpointError::Storage(error) = error {
                                persistence_error = Some(error);
                            }
                            error.error()
                        })
                    });
                    renderer.poll(&mut session, monotonic(&clock))?;
                    if let Some(error) = persistence_error {
                        return Err(error);
                    }
                }
            }
        }
        match server.requests.recv_timeout(Duration::from_millis(1)) {
            Ok(pending) => {
                let command = pending.command;
                let executed = OwnerControl {
                    session: &mut session,
                    worker: &mut worker,
                    graphs: &mut graphs,
                    artifacts: &mut artifacts,
                    devices: &mut devices,
                    directory: &directory,
                    token: &token,
                    clock: &clock,
                    live_binding: &live_binding,
                    network_retirement: &mut network_retirement,
                }
                .execute(command, Ingress::Local)?;
                if let Some(renderer) = renderer.as_mut() {
                    renderer.poll(&mut session, monotonic(&clock))?;
                }
                let _ = pending.reply.try_send(executed.receipt);
                if let Some(error) = executed.persistence_error {
                    return Err(error);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
enum Ingress {
    Local,
    Peer(std::net::IpAddr),
    Graph(Arc<RendererGraphBinding>),
}
struct OwnerExecution {
    receipt: earph_core::control::Receipt,
    persistence_error: Option<EngineError>,
}
struct OwnerControl<'a> {
    session: &'a mut Session,
    worker: &'a mut Worker,
    graphs: &'a mut GraphJobs,
    artifacts: &'a mut crate::profiles::ArtifactJobs,
    devices: &'a mut crate::devices::DeviceJobs,
    directory: &'a std::path::Path,
    token: &'a str,
    clock: &'a Instant,
    live_binding: &'a crate::network_source::LiveBinding,
    network_retirement: &'a mut Option<earph_core::network_source::NetworkSourceBinding>,
}
impl OwnerControl<'_> {
    fn execute(
        &mut self,
        command: CommandEnvelope,
        ingress: Ingress,
    ) -> Result<OwnerExecution, EngineError> {
        let session = &mut *self.session;
        let worker = &mut *self.worker;
        let graphs = &mut *self.graphs;
        let artifacts = &mut *self.artifacts;
        let devices = &mut *self.devices;
        let directory = self.directory;
        let token = self.token;
        let clock = self.clock;
        let live_binding = self.live_binding;
        let network_retirement = &mut *self.network_retirement;
        let before = session.snapshot();
        let actual_check = worker
            .view
            .lock()
            .map_err(|_| IO)?
            .validate_edit(&command.payload);
        let needs_worker = action_required(&command.payload, before.source);
        let safety = safety_control(&command.payload);
        // This actor is the sole command producer. Replacement queues retirement before start.
        let replacing_network = before.source == Some(SourceKind::HttpFlac)
            && matches!(
                command.payload,
                Command::NetworkStartHttpFlac { .. }
                    | Command::RendererRequest {
                        action: RendererAction::Play { .. },
                        ..
                    }
                    | Command::Start {
                        source: SourceKind::LocalFile,
                        ..
                    }
            );
        let capacity =
            !needs_worker || worker.has_capacity_for(safety, 1 + usize::from(replacing_network));
        let view = worker.view.clone();
        let mut persistence_error = None;
        let mut dispatched_early = false;
        let mut admission_failure = None;
        let submitted_ns = monotonic(clock);
        let commit = |staged: &Session| {
            actual_check?;
            if matches!(
                command.payload,
                Command::DevicesList { .. } | Command::DevicesInspect { .. }
            ) {
                return devices.submit(staged);
            }
            if command.payload.is_profile_save() || command.payload.starts_diagnostic_work() {
                return artifacts.submit(staged, monotonic(clock));
            }
            if command.payload.prepares_graph() {
                return graphs.submit(staged, worker);
            }
            if matches!(
                command.payload,
                Command::Stop
                    | Command::OwnerShutdown
                    | Command::RendererRequest {
                        action: RendererAction::Stop { binding: Some(_) },
                        ..
                    }
            ) || before.source == Some(SourceKind::HttpFlac)
                && matches!(command.payload, Command::NetworkGrantsRevoke { .. })
            {
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
            if matches!(
                before.source,
                Some(SourceKind::LocalFile | SourceKind::HttpFlac)
            ) {
                let settings = Settings {
                    pcm_processing: staged.has_pcm_processing(),
                    snapshot: staged.snapshot(),
                    playing: staged.wants_playback(),
                };
                let safety_action = match command.payload {
                    Command::Pause => Some(Action::Pause(settings)),
                    Command::Mute { muted: true }
                    | Command::VolumeSilence
                    | Command::VolumeFactoryDefaults
                    | Command::VolumeCaps { .. } => Some(Action::Settings(settings)),
                    Command::RendererRequest { ref action, .. } => match action {
                        RendererAction::Pause { .. } => Some(Action::Pause(settings)),
                        RendererAction::Stop { .. } => None,
                        action if action.is_safety() => Some(Action::Settings(settings)),
                        _ => None,
                    },
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
                directory,
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
        };
        let mut receipt = match ingress {
            Ingress::Local => session.submit_with_network_effects(
                command.clone(),
                submitted_ns,
                directory::paths,
                |s, c| crate::network_source::validate(s, c, clock),
                commit,
            ),
            Ingress::Peer(peer) => session.submit_renderer_with_network_effects(
                command.clone(),
                peer,
                submitted_ns,
                |s, c| crate::network_source::validate(s, c, clock),
                commit,
            ),
            Ingress::Graph(binding) => session.submit_renderer_graph_with_effects(
                command.clone(),
                binding,
                submitted_ns,
                commit,
            ),
        };
        cancel_rejected_safety(session, worker, &command, &receipt);
        if let Some(code) = admission_failure {
            // Cancellation protects output even when safety traffic filled the reserve.
            // Report failure until an explicit control restores a usable stream.
            advance(session, EngineEventKind::LocalFailed { code }, None, clock);
        }
        let after = session.snapshot();
        synchronize_network(session, worker, live_binding, network_retirement)?;
        if matches!(
            receipt.status,
            ReceiptStatus::Applied | ReceiptStatus::Accepted
        ) && after.revision != before.revision
            && !dispatched_early
            && let Err(error) = if command.payload.activates_graph() {
                graphs.activate(worker, session)
            } else {
                dispatch_with_network(&command, worker, session, live_binding, clock)
            }
        {
            if command.payload.activates_graph() {
                session.fail_graph_activation(&command.request_id, error);
                worker.cancel_active();
                receipt = session.submit_graph(command.clone(), monotonic(clock));
            } else {
                advance(
                    session,
                    EngineEventKind::LocalFailed { code: error.code },
                    None,
                    clock,
                );
            }
        }

        Ok(OwnerExecution {
            receipt,
            persistence_error,
        })
    }
}

fn safety_control(command: &Command) -> bool {
    if let Command::RendererRequest { action, .. } = command {
        return action.is_safety();
    }
    matches!(
        command,
        Command::Stop
            | Command::OwnerShutdown
            | Command::Pause
            | Command::Mute { muted: true }
            | Command::VolumeSilence
            | Command::VolumeFactoryDefaults
            | Command::VolumeCaps { .. }
            | Command::NetworkGrantsRevoke { .. }
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
fn reconcile_network_graph_activation(
    session: &mut Session,
    ack: earph_core::network_source::NetworkGraphActivationAck,
    now_ns: u64,
    persist: impl FnOnce(&Session) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let current = session.graph_activation().is_some_and(|activation| {
        activation.request_id == ack.request_id && activation.epochs == ack.epochs
    }) && session
        .network_source()
        .is_some_and(|source| source.binding == ack.observation.binding);
    let result = session.complete_network_graph_activation(ack, now_ns, persist);
    if current
        && session.graph_activation().is_none()
        && session.graph_inspection().recovery_required
    {
        return result;
    }
    Ok(())
}
fn cancel_rejected_safety(
    session: &mut Session,
    worker: &Worker,
    command: &CommandEnvelope,
    receipt: &earph_core::control::Receipt,
) {
    if matches!(command.payload, Command::NetworkGrantsRevoke { .. })
        && session.snapshot().source != Some(SourceKind::HttpFlac)
    {
        return;
    }
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
    if let Command::RendererRequest { action, .. } = command {
        return match action {
            RendererAction::StageUri { .. } => false,
            RendererAction::Play { .. } => true,
            RendererAction::Stop { binding } => binding.is_some(),
            _ => source == Some(SourceKind::HttpFlac),
        };
    }
    if matches!(command, Command::NetworkStartHttpFlac { .. }) {
        return true;
    }
    if source == Some(SourceKind::HttpFlac)
        && matches!(
            command,
            Command::NetworkGrantsReplace { .. }
                | Command::NetworkGrantsRevoke { .. }
                | Command::Pause
                | Command::GraphCommit { .. }
                | Command::Resume
                | Command::Stop
                | Command::Mute { .. }
                | Command::VolumeSet { .. }
                | Command::VolumeSilence
                | Command::VolumeFactoryDefaults
                | Command::VolumeCaps { .. }
                | Command::OwnerShutdown
        )
    {
        return true;
    }
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
                | Command::SeekDsd { .. }
                | Command::Mute { .. }
                | Command::VolumeSet { .. }
                | Command::VolumeSilence
                | Command::VolumeFactoryDefaults
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
        pcm_processing: session.has_pcm_processing(),
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
        Command::SeekDsd { position_bits }
            if settings.snapshot.source == Some(SourceKind::LocalFile) =>
        {
            Some(Action::SeekDsd {
                settings,
                position_bits,
            })
        }
        Command::Resume
        | Command::Mute { .. }
        | Command::VolumeSet { .. }
        | Command::VolumeSilence
        | Command::VolumeFactoryDefaults
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
fn synchronize_network(
    session: &Session,
    worker: &mut Worker,
    live: &crate::network_source::LiveBinding,
    retirement: &mut Option<earph_core::network_source::NetworkSourceBinding>,
) -> Result<(), EngineError> {
    let binding = session
        .network_source()
        .map(|source| source.binding.clone());
    {
        let mut current = live.lock().map_err(|_| IO)?;
        if *current != binding {
            worker.cancel_active();
            *current = binding;
        }
    }
    if let Some(closing) = session.closing_network_source()
        && retirement.as_ref() != Some(&closing.context.binding)
    {
        worker.cancel_active();
        if worker.send(Action::RetireNetwork(closing.clone())).is_ok() {
            *retirement = Some(closing.context.binding.clone());
        }
    }
    Ok(())
}
fn dispatch_with_network(
    command: &CommandEnvelope,
    worker: &mut Worker,
    session: &Session,
    live: &crate::network_source::LiveBinding,
    clock: &Instant,
) -> Result<(), EngineError> {
    let settings = Settings {
        pcm_processing: session.has_pcm_processing(),
        snapshot: session.snapshot(),
        playing: session.wants_playback(),
    };
    if matches!(
        command.payload,
        Command::NetworkStartHttpFlac { .. }
            | Command::RendererRequest {
                action: RendererAction::Play { .. },
                ..
            }
    ) {
        // Revocation precedes fallible permit construction after a successful commit.
        worker.cancel_active();
        let context = session.network_source().ok_or(IO)?;
        let permit = crate::network_source::permit(session, &context, clock)?;
        return worker.send(Action::StartNetwork {
            settings,
            context,
            permit,
            live: live.clone(),
            clock: *clock,
            cancel: Arc::new(AtomicBool::new(false)),
        });
    }
    if settings.snapshot.source == Some(SourceKind::HttpFlac)
        || session.closing_network_source().is_some()
    {
        return match command.payload {
            Command::Pause => worker.send(Action::Pause(settings)),
            Command::RendererRequest {
                action: RendererAction::Pause { .. },
                ..
            } => worker.send(Action::Pause(settings)),
            Command::RendererRequest {
                action:
                    RendererAction::Resume { .. }
                    | RendererAction::SetMute { .. }
                    | RendererAction::SetVolume { .. }
                    | RendererAction::SetVolumeDb { .. }
                    | RendererAction::FactoryDefaults { .. },
                ..
            } => worker.send(Action::Settings(settings)),
            Command::Resume
            | Command::Mute { .. }
            | Command::VolumeSet { .. }
            | Command::VolumeSilence
            | Command::VolumeFactoryDefaults
            | Command::VolumeCaps { .. } => worker.send(Action::Settings(settings)),
            Command::Start {
                source: SourceKind::LocalFile,
                ..
            } => dispatch(command, worker, session),
            _ => Ok(()),
        };
    }
    dispatch(command, worker, session)
}
fn advance(session: &mut Session, kind: EngineEventKind, epochs: Option<Epochs>, clock: &Instant) {
    if let EngineEventKind::LocalFailed { code } = &kind
        && let Some(context) = session.network_source()
    {
        let snapshot = session.snapshot();
        let _ = session.advance_network(earph_core::network_source::NetworkEvent {
            binding: context.binding.clone(),
            source_epochs: context.source_epochs,
            output_epochs: snapshot.epochs,
            graph_epoch: snapshot.graph_epoch,
            device_epoch: snapshot.device_epoch,
            monotonic_ns: monotonic(clock),
            kind: earph_core::network_source::NetworkEventKind::Failed { code: *code },
        });
        return;
    }
    let epochs = epochs.unwrap_or_else(|| session.snapshot().epochs);
    let _ = session.advance(EngineEvent {
        epochs,
        monotonic_ns: clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
        kind,
    });
}
#[derive(Default)]
struct DsdRetirementEvidence {
    failure: Option<Arc<DsdRetirementFailure>>,
    reported: bool,
}
impl DsdRetirementEvidence {
    fn adopt(&mut self, failure: Arc<DsdRetirementFailure>) -> bool {
        if failure.validate().is_err() {
            return false;
        }
        if let Some(current) = &self.failure {
            if current.token == failure.token {
                return Arc::ptr_eq(current, &failure);
            }
            if !self.reported {
                return false;
            }
        }
        self.failure = Some(failure);
        self.reported = false;
        true
    }
    fn report(&mut self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        if self.reported {
            return Ok(());
        }
        if let Some(failure) = &self.failure {
            let mut bytes = serde_json::to_vec(&serde_json::json!({
                "dsd_retirement_failure": failure,
            }))?;
            bytes.push(b'\n');
            writer.write_all(&bytes)?;
            writer.flush()?;
            self.reported = true;
        }
        Ok(())
    }
}
fn observation(
    session: &mut Session,
    event: Event,
    clock: &Instant,
) -> Result<Option<SourceTerminalToken>, EngineError> {
    match event {
        Event::DsdRetirementFailed(_) => {
            unreachable!("retirement evidence is adopted independently from current playback")
        }
        Event::GraphActivated(_)
        | Event::GraphFailed { .. }
        | Event::Network(_)
        | Event::NetworkRetired(_)
        | Event::NetworkGraphActivated(_)
        | Event::NetworkGraphFailed { .. } => {
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
        Event::Completed {
            epochs,
            token,
            observation,
        } => {
            let snapshot = session.snapshot();
            if !token.matches_source(&snapshot)
                || epochs != snapshot.epochs
                || epochs.stream < token.origin_epochs.stream
                || observation.entry_id() != &token.final_entry_id
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
                        .is_some_and(|value| value.entry_id() != &token.final_entry_id)
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
                    .is_some_and(|value| value.entry_id() != &token.final_entry_id)
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
    pending: Option<PendingGraph>,
    ready: Option<PreparedPipeline>,
}
struct PendingGraph {
    request: earph_core::graph_control::GraphJobRequest,
    snapshot: earph_core::session::SessionSnapshot,
    source_cancel: Arc<AtomicBool>,
}
impl GraphJobs {
    fn spawn(asset_store: PathBuf) -> Result<Self, EngineError> {
        Ok(Self {
            service: GraphService::spawn(Some(asset_store))?,
            job: None,
            pending: None,
            ready: None,
        })
    }
    fn poll(&mut self, session: &mut Session, now_ns: u64) -> Result<(), EngineError> {
        session.expire_graph_work(now_ns);
        if self.pending.as_ref().is_some_and(|pending| {
            session.graph_job().is_none_or(|job| {
                job.request_id != pending.request.request_id
                    || job.binding != pending.request.binding
                    || job.renderer_application != pending.request.renderer_application
            })
        }) {
            self.pending = None;
        }
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
        if let Some(pending) = self.pending.take() {
            let state = session.snapshot();
            let binding = &pending.request.binding;
            let current = state.owner_id.as_ref() == Some(&binding.owner_id)
                && state.session_id.as_ref() == Some(&binding.session_id)
                && state.epochs == binding.epochs
                && state.device_epoch == binding.device_epoch
                && state.revision == binding.expected_revision
                && !pending.source_cancel.load(Ordering::Acquire)
                && pending
                    .request
                    .renderer_application
                    .as_ref()
                    .is_none_or(|renderer| session.renderer_graph_binding_valid(renderer, now_ns));
            let submitted = if current {
                self.start(&pending)
            } else {
                Err(EngineError::new(
                    ErrorCode::StaleSession,
                    "queued graph authority changed",
                ))
            };
            match submitted {
                Ok(cancel) => self.job = Some((pending.request.request_id, cancel)),
                Err(error) if error.code == ErrorCode::SourceBusy => self.pending = Some(pending),
                Err(error) => {
                    let _ = session.complete_graph_preparation(
                        &pending.request.request_id,
                        &pending.request.binding,
                        now_ns,
                        Err(error.into()),
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
        let pending = PendingGraph {
            request: session.graph_job().ok_or(IO)?,
            snapshot: session.snapshot(),
            source_cancel: worker.source_cancellation()?,
        };
        match self.start(&pending) {
            Ok(cancel) => {
                self.job = Some((pending.request.request_id, cancel));
                self.pending = None;
                Ok(())
            }
            Err(error) if error.code == ErrorCode::SourceBusy => {
                // Core adopts replacement before poll retires its displaced candidate.
                self.pending = Some(pending);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
    fn start(&mut self, pending: &PendingGraph) -> Result<GraphCancellation, EngineError> {
        self.service.submit(
            pending.request.clone(),
            pending.snapshot.clone(),
            pending.source_cancel.clone(),
        )
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
                pcm_processing: session.has_pcm_processing(),
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

#[cfg(all(test, target_os = "linux"))]
#[path = "host_renderer_tests.rs"]
mod renderer_ownership;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    pub(crate) fn assert_dsd_retirement_evidence(
        failure: Arc<DsdRetirementFailure>,
        directory: &std::path::Path,
    ) {
        let mut history = DsdRetirementEvidence::default();
        assert!(history.adopt(failure.clone()));
        assert!(Arc::ptr_eq(history.failure.as_ref().unwrap(), &failure));
        // Adoption transfers evidence ownership even when the diagnostic sink cannot write.
        let mut full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        assert!(history.report(&mut full).is_err());
        assert!(!history.reported);
        assert_eq!(history.failure.as_ref().unwrap().error, failure.error);
        assert!(history.adopt(failure.clone()));
        let next: DsdRetirementFailure = DsdRetirementFailure {
            token: SourceTerminalToken {
                origin_epochs: Epochs {
                    session: 2,
                    stream: 0,
                },
                ..failure.token.clone()
            },
            error: failure.error,
            observation: Box::new(earph_core::dop::DsdPlaybackObservation {
                source_epochs: Epochs {
                    session: 2,
                    stream: 0,
                },
                last_boundary: failure.observation.last_boundary.as_ref().map(|boundary| {
                    earph_core::dop::DsdOutputBoundary {
                        output_epochs: Epochs {
                            session: 2,
                            stream: 0,
                        },
                        ..boundary.clone()
                    }
                }),
                ..(*failure.observation).clone()
            }),
        };
        let next = Arc::new(next);
        assert!(!history.adopt(next.clone()));
        assert!(Arc::ptr_eq(history.failure.as_ref().unwrap(), &failure));

        let path = directory.join("retirement-evidence.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        history.report(&mut file).unwrap();
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let reported = &report["dsd_retirement_failure"];
        assert_eq!(reported["token"]["session_id"], "dsd-session");
        assert_eq!(reported["token"]["origin_epochs"]["session"], 1);
        assert_eq!(reported["observation"]["source_position_bits"], 496);
        assert_eq!(
            reported["observation"]["last_boundary"]["submitted_carrier_frames"],
            31
        );
        assert_eq!(reported["observation"]["last_boundary"]["reason"], "failed");
        assert_eq!(reported["error"]["code"], "OUTPUT_FAULT");
        assert!(history.adopt(next.clone()));
        assert!(Arc::ptr_eq(history.failure.as_ref().unwrap(), &next));
        assert!(!history.reported);
    }
    #[test]
    fn stale_network_graph_ack_does_not_stop_or_relabel_a_local_activation() {
        use earph_core::network_source::{
            NetworkGraphActivationAck, NetworkLifecycle, NetworkPlaybackObservation,
            NetworkSourceBinding,
        };
        let (mut session, local) = activation();
        let before = session.snapshot();
        let observation = local.observation;
        let ack = NetworkGraphActivationAck {
            request_id: local.request_id,
            epochs: local.epochs,
            graph_hash: local.graph_hash,
            activation_frame: local.activation_frame,
            observation: NetworkPlaybackObservation {
                binding: NetworkSourceBinding {
                    owner_id: before.owner_id.clone().unwrap(),
                    session_id: before.session_id.clone().unwrap(),
                    media_id: earph_core::ids::StagedMediaId::new("stale-network-media").unwrap(),
                    source_generation: 1,
                    grant_generation: 1,
                },
                source_epochs: local.epochs,
                output_epochs: local.epochs,
                graph_epoch: before.graph_epoch + 1,
                device_epoch: before.device_epoch,
                lifecycle: NetworkLifecycle::Ready,
                format: observation.format,
                output_format: observation.format,
                total_frames: Some(local.source_total_frames),
                source_position_frames: 0,
                audible_source_position_frames: None,
                submitted_frames: 0,
                consumed_frames: None,
                backend_delay_frames: None,
                consumption_basis: "alsa_client_estimate",
                input_peak: None,
                output_peak: None,
                modified: true,
                backend: observation.backend,
                output_activity: observation.output_activity,
                processing: None,
                replacement_boundary: None,
            },
        };
        reconcile_network_graph_activation(&mut session, ack, 7, |_| {
            panic!("stale network graph cannot persist")
        })
        .unwrap();
        assert_eq!(session.snapshot(), before);
        assert!(session.graph_activation().is_some());
    }
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
                    observation: (observation.clone()).into(),
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
            Command::VolumeSilence,
            Command::VolumeFactoryDefaults,
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
                        pcm_processing: session.has_pcm_processing(),
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
            Event::Completed {
                epochs,
                token: token.clone(),
                observation: earph_core::session::LocalPlaybackObservation::from(
                    final_observation.clone(),
                ),
            },
            &clock,
        )
        .unwrap();
        assert_eq!(session.snapshot().transport, TransportState::Idle);
        assert!(session.snapshot().output_consented);
        assert!(!session.graph_inspection().recovery_required);
        assert!(session.graph_activation().is_none());
        assert_eq!(
            session.snapshot().observation.pcm().cloned().unwrap(),
            final_observation
        );
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
            let final_observation = before.observation.pcm().cloned().unwrap();
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
                Event::Completed {
                    epochs,
                    token: token.clone(),
                    observation: earph_core::session::LocalPlaybackObservation::from(
                        final_observation.clone(),
                    ),
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
            Event::Completed {
                epochs: activation.previous_binding.epochs,
                token,
                observation: earph_core::session::LocalPlaybackObservation::from(
                    observation_value.clone(),
                ),
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
                Event::Completed {
                    epochs,
                    token: token.clone(),
                    observation: earph_core::session::LocalPlaybackObservation::from(value.clone())
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
        let value = session.snapshot().observation.pcm().cloned().unwrap();
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
                    observation: Some(earph_core::session::LocalPlaybackObservation::from(
                        value.clone()
                    ))
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
                observation: Some(earph_core::session::LocalPlaybackObservation::from(
                    value.clone(),
                )),
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
                observation: Some(earph_core::session::LocalPlaybackObservation::from(
                    acknowledgement.observation,
                )),
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
        let value = session.snapshot().observation.pcm().cloned().unwrap();
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
                    observation: Some(earph_core::session::LocalPlaybackObservation::from(
                        value.clone()
                    )),
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
            pcm_processing: session.has_pcm_processing(),
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
                    pcm_processing: session.has_pcm_processing(),
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
                    pcm_processing: false,
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
                    observation: Some(earph_core::session::LocalPlaybackObservation::from(
                        ack.observation,
                    )),
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
                    pcm_processing: false,
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
                        observation: Some(earph_core::session::LocalPlaybackObservation::from(
                            value.clone()
                        ))
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
                pcm_processing: false,
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
                observation: Some(earph_core::session::LocalPlaybackObservation::from(
                    ack.observation.clone(),
                )),
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
                Event::Completed {
                    epochs: ack.epochs,
                    token: stale_token,
                    observation: earph_core::session::LocalPlaybackObservation::from(
                        ack.observation
                    )
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
            pcm_processing: session.has_pcm_processing(),
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
                    pcm_processing: session.has_pcm_processing(),
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
