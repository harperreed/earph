// ABOUTME: Tests session lifecycle and controller safety through public contracts.
// ABOUTME: Covers stale work, receipt replay, bounded history, and startup mute.
use earph_core::{ErrorCode, control::*, ids::*, session::*};

fn session() -> Session {
    Session::new(ZoneId::new("main").unwrap(), ControlLimits::default())
}
fn command(session: &Session, id: &str, payload: Command) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        owner_id: None,
        request_id: RequestId::new(id).unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        session_id: session.snapshot().session_id,
        expected_revision: Some(session.snapshot().revision),
        payload,
    }
}
fn start(session: &Session, id: &str, takeover: bool) -> CommandEnvelope {
    let mut c = command(
        session,
        id,
        Command::Start {
            source: SourceKind::Virtual,
            device_id: DeviceId::new("virtual").unwrap(),
            takeover,
            seekable: true,
        },
    );
    c.session_id = Some(SessionId::new(id).unwrap());
    c
}
fn event(session: &mut Session, kind: EngineEventKind) {
    session
        .advance(EngineEvent {
            epochs: session.snapshot().epochs,
            monotonic_ns: session.snapshot().monotonic_ns + 1,
            kind,
        })
        .unwrap();
}
fn playing(session: &mut Session) {
    assert_eq!(
        session.submit(start(session, "start", false)).status,
        ReceiptStatus::Accepted
    );
    event(session, EngineEventKind::Prepared);
    event(
        session,
        EngineEventKind::Started {
            activation_frame: 0,
        },
    );
}

#[test]
fn startup_is_muted_and_acceptance_is_not_application() {
    let mut s = session();
    assert!(s.snapshot().muted);
    assert_eq!(s.snapshot().software_gain_db, -40.0);
    let start = start(&s, "start", false);
    assert_eq!(s.submit(start.clone()).status, ReceiptStatus::Accepted);
    assert_eq!(s.snapshot().transport, TransportState::Preparing);
    event(&mut s, EngineEventKind::Prepared);
    event(
        &mut s,
        EngineEventKind::Started {
            activation_frame: 4,
        },
    );
    assert_eq!(s.submit(start).status, ReceiptStatus::Applied);
    assert!(s.snapshot().muted);
    let before = s.snapshot();
    let receipt = s.submit(command(&s, "bad", Command::Unsupported));
    assert_eq!(
        receipt.error.unwrap().code,
        ErrorCode::CapabilityUnsupported
    );
    assert_eq!(s.snapshot(), before);
}

#[test]
fn output_position_pause_seek_and_drain_follow_real_consumption() {
    let mut s = session();
    playing(&mut s);
    event(
        &mut s,
        EngineEventKind::Decoded {
            position_frames: 100,
        },
    );
    event(&mut s, EngineEventKind::Consumed { frames: 7 });
    assert_eq!(s.snapshot().audible_position_frames, Some(7));
    assert_eq!(
        s.submit(command(&s, "pause", Command::Pause)).status,
        ReceiptStatus::Applied
    );
    assert!(
        s.advance(EngineEvent {
            epochs: s.snapshot().epochs,
            monotonic_ns: 20,
            kind: EngineEventKind::Consumed { frames: 1 }
        })
        .is_err()
    );
    let old = s.snapshot().epochs;
    assert_eq!(
        s.submit(command(
            &s,
            "seek",
            Command::Seek {
                position_frames: 40
            }
        ))
        .status,
        ReceiptStatus::Accepted
    );
    assert_ne!(s.snapshot().epochs, old);
    assert_eq!(s.snapshot().audible_position_frames, Some(40));
    assert_eq!(
        s.advance(EngineEvent {
            epochs: old,
            monotonic_ns: 50,
            kind: EngineEventKind::Started {
                activation_frame: 7
            }
        })
        .unwrap_err()
        .code,
        ErrorCode::StaleSession
    );
    event(
        &mut s,
        EngineEventKind::Started {
            activation_frame: 40,
        },
    );
    assert_eq!(s.snapshot().transport, TransportState::Paused);
    s.submit(command(&s, "resume", Command::Resume));
    event(
        &mut s,
        EngineEventKind::Started {
            activation_frame: 40,
        },
    );
    event(&mut s, EngineEventKind::EndOfInput);
    assert_eq!(s.snapshot().transport, TransportState::Draining);
    event(&mut s, EngineEventKind::Drained);
    assert_eq!(s.snapshot().transport, TransportState::Idle);
    assert!(
        s.advance(EngineEvent {
            epochs: s.snapshot().epochs,
            monotonic_ns: 100,
            kind: EngineEventKind::Drained
        })
        .is_err()
    );
}

#[test]
fn takeover_is_explicit_and_stale_commands_cannot_mutate_new_owner() {
    let mut s = session();
    playing(&mut s);
    let old = command(&s, "old-pause", Command::Pause);
    let before = s.snapshot();
    assert_eq!(
        s.submit(start(&s, "other", false)).error.unwrap().code,
        ErrorCode::SourceBusy
    );
    assert_eq!(s.snapshot(), before);
    let mut takeover = start(&s, "other", true);
    takeover.request_id = RequestId::new("takeover").unwrap();
    assert_eq!(s.submit(takeover).status, ReceiptStatus::Accepted);
    assert_ne!(s.snapshot().epochs, before.epochs);
    assert_eq!(s.submit(old).error.unwrap().code, ErrorCode::StaleSession);
    assert_eq!(s.snapshot().session_id.unwrap().as_str(), "other");
    assert!(s.snapshot().muted);
}

#[test]
fn retries_stale_revisions_and_request_id_conflicts_are_atomic() {
    let mut s = session();
    playing(&mut s);
    let pause = command(&s, "pause", Command::Pause);
    let stale = command(&s, "stale", Command::Resume);
    let receipt = s.submit(pause.clone());
    let after = s.snapshot();
    assert_eq!(s.submit(pause), receipt);
    assert_eq!(s.snapshot(), after);
    assert_eq!(
        s.submit(stale).error.unwrap().code,
        ErrorCode::StaleRevision
    );
    assert_eq!(
        s.submit(command(&s, "pause", Command::Stop))
            .error
            .unwrap()
            .code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(s.snapshot(), after);
    let mut mute = command(&s, "emergency", Command::Mute { muted: true });
    mute.expected_revision = Some(0);
    assert_eq!(s.submit(mute).status, ReceiptStatus::Applied);
}

#[test]
fn output_faults_cancellation_and_replacement_invalidate_pending_work() {
    let mut s = session();
    let start = start(&s, "pending", false);
    s.submit(start.clone());
    event(
        &mut s,
        EngineEventKind::Cancelled {
            request_id: start.request_id.clone(),
        },
    );
    assert_eq!(s.submit(start).status, ReceiptStatus::Cancelled);
    assert!(s.snapshot().muted);
    playing(&mut s);
    let old = s.snapshot().epochs;
    event(&mut s, EngineEventKind::OutputFailed);
    assert_eq!(s.snapshot().transport, TransportState::Failed);
    assert!(s.snapshot().muted);
    assert_ne!(s.snapshot().epochs, old);
    let old = s.snapshot().epochs;
    event(
        &mut s,
        EngineEventKind::OutputReplaced {
            device_id: DeviceId::new("virtual-other").unwrap(),
        },
    );
    assert_ne!(s.snapshot().epochs, old);
    assert_eq!(s.snapshot().transport, TransportState::Suspended);
    assert!(s.snapshot().muted);
}

#[test]
fn slow_consumers_get_a_gap_and_receipt_retention_is_bounded() {
    let mut s = session();
    let first = command(&s, "first", Command::Mute { muted: false });
    s.submit(first.clone());
    for index in 0..1030 {
        s.submit(command(
            &s,
            &format!("mute-{index}"),
            Command::Mute { muted: true },
        ));
    }
    let batch = s.events_since(0);
    assert!(batch.gap);
    assert_eq!(batch.events.len(), 1024);
    assert_eq!(batch.snapshot, s.snapshot());
    let mut refreshed = first;
    refreshed.expected_revision = Some(s.snapshot().revision);
    assert_eq!(s.submit(refreshed).status, ReceiptStatus::Applied);
    assert!(!s.snapshot().muted);
}

#[test]
fn stale_session_cannot_unmute_and_queries_return_actual_state() {
    let mut s = session();
    playing(&mut s);
    let mut unmute = command(&s, "old-unmute", Command::Mute { muted: false });
    s.submit(start(&s, "replacement", true));
    unmute.expected_revision = Some(s.snapshot().revision);
    assert_eq!(
        s.submit(unmute).error.unwrap().code,
        ErrorCode::StaleSession
    );
    assert!(s.snapshot().muted);
    let state = s.submit(command(&s, "state", Command::State));
    assert!(matches!(state.result,Some(QueryResult::State(snapshot)) if *snapshot==s.snapshot()));
    let capabilities = s.submit(command(&s, "caps", Command::Capabilities));
    assert!(
        matches!(capabilities.result,Some(QueryResult::Capabilities(caps)) if !caps.physical_output && caps.sources==vec![SourceKind::Virtual])
    );
}

#[test]
fn repeated_seeks_preserve_paused_intent_until_activation() {
    let mut s = session();
    playing(&mut s);
    s.submit(command(&s, "pause-before-seeks", Command::Pause));
    s.submit(command(
        &s,
        "seek-one",
        Command::Seek {
            position_frames: 10,
        },
    ));
    s.submit(command(
        &s,
        "seek-two",
        Command::Seek {
            position_frames: 20,
        },
    ));
    event(
        &mut s,
        EngineEventKind::Started {
            activation_frame: 20,
        },
    );
    assert_eq!(s.snapshot().transport, TransportState::Paused);
    assert_eq!(s.snapshot().audible_position_frames, Some(20));
}

#[test]
fn output_progress_does_not_invalidate_a_transport_revision() {
    let mut s = session();
    playing(&mut s);
    let pause = command(&s, "pause-after-progress", Command::Pause);
    let before = s.snapshot();
    event(
        &mut s,
        EngineEventKind::Decoded {
            position_frames: 100,
        },
    );
    event(&mut s, EngineEventKind::Consumed { frames: 1 });
    assert!(s.snapshot().event_sequence > before.event_sequence);
    assert_eq!(s.snapshot().revision, before.revision);
    assert_eq!(s.submit(pause).status, ReceiptStatus::Applied);
}

#[test]
fn paused_intent_survives_discontinuity_and_rebuffering() {
    let mut s = session();
    playing(&mut s);
    s.submit(command(&s, "pause-before-discontinuity", Command::Pause));
    event(&mut s, EngineEventKind::Discontinuity);
    event(&mut s, EngineEventKind::Prepared);
    event(
        &mut s,
        EngineEventKind::Started {
            activation_frame: 0,
        },
    );
    assert_eq!(s.snapshot().transport, TransportState::Paused);
    assert!(s.snapshot().muted);
}

#[test]
fn terminal_output_states_reject_source_events_until_explicit_restart() {
    for terminal in [
        EngineEventKind::OutputFailed,
        EngineEventKind::OutputReplaced {
            device_id: DeviceId::new("replacement").unwrap(),
        },
    ] {
        let mut s = session();
        playing(&mut s);
        event(&mut s, terminal);
        let before = s.snapshot();
        for kind in [
            EngineEventKind::Decoded {
                position_frames: 100,
            },
            EngineEventKind::Discontinuity,
        ] {
            assert!(
                s.advance(EngineEvent {
                    epochs: before.epochs,
                    monotonic_ns: before.monotonic_ns + 1,
                    kind
                })
                .is_err()
            );
            assert_eq!(s.snapshot(), before);
        }
        assert_eq!(
            s.submit(start(&s, "explicit-restart", true)).status,
            ReceiptStatus::Accepted
        );
        assert!(s.snapshot().muted);
    }
}

#[test]
fn superseded_operations_report_the_revision_and_epochs_that_cancelled_them() {
    for replacement in [
        Command::Stop,
        Command::Start {
            source: SourceKind::Virtual,
            device_id: DeviceId::new("virtual").unwrap(),
            takeover: true,
            seekable: true,
        },
    ] {
        let mut s = session();
        let original = start(&s, "pending-original", false);
        s.submit(original.clone());
        let mut next = command(&s, "replacement", replacement);
        if matches!(next.payload, Command::Start { .. }) {
            next.session_id = Some(SessionId::new("replacement").unwrap());
        }
        s.submit(next);
        let terminal = s.submit(original);
        assert_eq!(terminal.status, ReceiptStatus::Cancelled);
        assert_eq!(terminal.revision, s.snapshot().revision);
        assert_eq!(terminal.epochs, s.snapshot().epochs);
    }
}
