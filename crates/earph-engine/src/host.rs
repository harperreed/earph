// ABOUTME: Reconciles worker observations through the authoritative session reducer.
// ABOUTME: Commits operational state before publishing queue changes or worker controls.
use crate::{
    directory, ipc, runtime,
    worker::{Action, Event, Settings, Worker},
};
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, ReceiptStatus, SourceKind},
    ids::Epochs,
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
    let mut worker = Worker::spawn();
    let clock = Instant::now();
    loop {
        loop {
            match worker.events.try_recv() {
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
                let receipt = session.submit_with_queue_expansion(
                    command.clone(),
                    directory::paths,
                    |staged| {
                        actual_check?;
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
                    && let Err(error) = dispatch(&command, &mut worker, &session)
                {
                    advance(
                        &mut session,
                        EngineEventKind::LocalFailed { code: error.code },
                        None,
                        &clock,
                    );
                }
                let reply =
                    serde_json::to_vec(&serde_json::json!({"receipt":receipt})).map_err(|_| IO)?;
                let _ = pending.reply.try_send(reply);
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
