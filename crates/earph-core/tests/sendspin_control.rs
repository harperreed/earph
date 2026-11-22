// ABOUTME: Tests the Sendspin player control surface through wire commands and source events.
// ABOUTME: Covers start admission, the unmute gate, event fencing, close handling, and restore.
use earph_core::{
    ErrorCode,
    control::{CommandEnvelope, ControlLimits, Receipt, ReceiptStatus, decode_command},
    ids::{OwnerId, ZoneId},
    sendspin_source::{SendspinSourceEvent, SendspinSourceObservation, SendspinTransportPhase},
    session::{Session, SessionCheckpoint, TransportState},
};
use serde_json::{Value, json};

const OWNER: &str = "sendspin-owner";

fn owner() -> Session {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new(OWNER).unwrap(),
        ControlLimits::default(),
    );
    session.enable_local_playback();
    session
}
fn configured() -> Session {
    let mut session = owner();
    session.enable_sendspin();
    session
}
/// Builds one wire command against the current revision and the given target session.
fn wire(session: &Session, id: &str, target: Option<&str>, payload: Value) -> CommandEnvelope {
    let bytes = serde_json::to_vec(&json!({
        "schema_version":1,"request_id":id,"zone_id":"main","owner_id":OWNER,
        "session_id":target,"expected_revision":session.snapshot().revision,"payload":payload
    }))
    .unwrap();
    decode_command(&bytes, &ControlLimits::default()).unwrap()
}
fn submit(session: &mut Session, id: &str, target: Option<&str>, payload: Value) -> Receipt {
    let command = wire(session, id, target, payload);
    session.submit(command)
}
fn applied(session: &mut Session, id: &str, target: Option<&str>, payload: Value) {
    let receipt = submit(session, id, target, payload);
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{id}: {receipt:?}");
}
fn rejected(
    session: &mut Session,
    id: &str,
    target: Option<&str>,
    payload: Value,
    code: ErrorCode,
) {
    let before = session.snapshot();
    let receipt = submit(session, id, target, payload);
    assert_eq!(receipt.status, ReceiptStatus::Rejected, "{id}");
    assert_eq!(receipt.error.map(|error| error.code), Some(code), "{id}");
    assert_eq!(session.snapshot(), before, "{id} must change no state");
}
fn output() -> Value {
    json!({"method":"output.select","output":{
        "backend":"alsa","route":"system","endpoint":"capture","device_id":"dac",
        "profile_id":"profile","period_frames":64,"buffer_frames":256,
        "volume_mode":"software","fidelity":"compatible_conversion"}})
}
fn consent() -> Value {
    json!({"method":"output.consent","device_id":"dac","profile_id":"profile"})
}
fn start() -> Value {
    json!({"method":"sources.sendspin.start","device_id":"dac"})
}
/// Returns a configured owner with an approved output, and its accepted Sendspin start.
fn started_by() -> (Session, CommandEnvelope) {
    let mut session = configured();
    applied(&mut session, "select", None, output());
    applied(&mut session, "consent", None, consent());
    let command = wire(&session, "start", Some("player-1"), start());
    let receipt = session.submit(command.clone());
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    (session, command)
}
fn started() -> Session {
    started_by().0
}
fn sendspin(session: &mut Session, id: &str) -> Value {
    let receipt = submit(session, id, None, json!({"method":"sources.sendspin.get"}));
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    serde_json::to_value(receipt).unwrap()["result"].clone()
}
/// Delivers one source event that carries the current connection's fences.
fn report(
    session: &mut Session,
    phase: SendspinTransportPhase,
    frames: u64,
    error: Option<ErrorCode>,
) {
    let connection = session.sendspin_connection().unwrap();
    let now = session.snapshot().monotonic_ns + 1;
    session
        .advance_sendspin(
            connection.generation,
            SendspinSourceEvent {
                epochs: connection.epochs,
                observation: SendspinSourceObservation::new(phase, frames),
                error,
            },
            now,
        )
        .unwrap();
}

#[test]
fn start_requires_a_configured_owner_an_approved_matching_output_and_no_pipeline() {
    let mut bare = owner();
    applied(&mut bare, "select", None, output());
    applied(&mut bare, "consent", None, consent());
    rejected(
        &mut bare,
        "unconfigured",
        Some("player-1"),
        start(),
        ErrorCode::CapabilityUnsupported,
    );
    let capabilities = |session: &mut Session| {
        let receipt = submit(session, "caps", None, json!({"method":"capabilities.get"}));
        serde_json::to_value(receipt).unwrap()["result"]["value"]["sources"].clone()
    };
    assert_eq!(capabilities(&mut bare), json!(["local_file"]));

    let mut session = configured();
    assert_eq!(
        capabilities(&mut session),
        json!(["local_file", "sendspin"])
    );
    rejected(
        &mut session,
        "no-output",
        Some("player-1"),
        start(),
        ErrorCode::SafetyPolicyBlocked,
    );
    applied(&mut session, "select", None, output());
    rejected(
        &mut session,
        "no-consent",
        Some("player-1"),
        start(),
        ErrorCode::SafetyPolicyBlocked,
    );
    applied(&mut session, "consent", None, consent());
    rejected(
        &mut session,
        "other-device",
        Some("player-1"),
        json!({"method":"sources.sendspin.start","device_id":"other"}),
        ErrorCode::StaleSession,
    );
    rejected(
        &mut session,
        "no-session-id",
        None,
        start(),
        ErrorCode::StaleSession,
    );

    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    let mut checkpoint = session.checkpoint();
    checkpoint.graph_intent = Some(std::sync::Arc::new(
        serde_json::from_value(json!({
            "graph":{"schema_version":1,"output":"out","nodes":[
                {"id":"in","kind":{"kind":"input"},"format":port},
                {"id":"out","kind":{"kind":"output"},"format":port}],
                "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},
            "processing":{"kind":"integer_bypass"}
        }))
        .unwrap(),
    ));
    let mut piped = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new(OWNER).unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    piped.enable_local_playback();
    piped.enable_sendspin();
    applied(&mut piped, "consent", None, consent());
    rejected(
        &mut piped,
        "pipeline",
        Some("player-1"),
        start(),
        ErrorCode::FidelityPolicyConflict,
    );
}

#[test]
fn an_accepted_start_owns_a_muted_preparing_session_and_refuses_a_second_start() {
    let mut session = started();
    let state = session.snapshot();
    assert_eq!(
        state.session_id.as_ref().map(|id| id.as_str()),
        Some("player-1")
    );
    assert_eq!(
        serde_json::to_value(&state).unwrap()["source"],
        json!("sendspin")
    );
    assert_eq!(state.transport, TransportState::Preparing);
    assert!(state.muted);
    assert!(!state.seekable);
    assert_eq!(state.audible_position_frames, None);
    let view = sendspin(&mut session, "get-connecting");
    assert_eq!(view["kind"], "sendspin");
    assert_eq!(view["value"]["phase"], "connecting");
    assert_eq!(view["value"]["connection_generation"], 1);
    rejected(
        &mut session,
        "second",
        Some("player-2"),
        start(),
        ErrorCode::SourceBusy,
    );
}

#[test]
fn unmute_waits_for_the_prepared_source_and_respects_the_volume_cap() {
    let mut session = started();
    let unmute = json!({"method":"mute.set","muted":false});
    rejected(
        &mut session,
        "early",
        Some("player-1"),
        unmute.clone(),
        ErrorCode::SafetyPolicyBlocked,
    );
    report(&mut session, SendspinTransportPhase::Preparing, 0, None);
    rejected(
        &mut session,
        "output-open",
        Some("player-1"),
        unmute.clone(),
        ErrorCode::SafetyPolicyBlocked,
    );
    report(
        &mut session,
        SendspinTransportPhase::AwaitingStream,
        0,
        None,
    );
    assert_eq!(session.snapshot().transport, TransportState::Buffering);
    assert!(session.snapshot().muted, "preparing never unmutes");
    assert_eq!(session.snapshot().operation, None, "the start settled");
    applied(
        &mut session,
        "caps",
        Some("player-1"),
        json!({"method":"volume.caps.set","software_millidecibels":-20000,"hardware_millidecibels":null}),
    );
    rejected(
        &mut session,
        "loud",
        Some("player-1"),
        json!({"method":"volume.set","millidecibels":-10000,"ramp_frames":0}),
        ErrorCode::SafetyPolicyBlocked,
    );
    applied(&mut session, "unmute", Some("player-1"), unmute);
    assert!(!session.snapshot().muted);
}

#[test]
fn server_streams_move_transport_inside_the_session_without_new_epochs_or_authority() {
    let mut session = started();
    report(
        &mut session,
        SendspinTransportPhase::AwaitingStream,
        0,
        None,
    );
    let before = session.snapshot();
    report(&mut session, SendspinTransportPhase::Streaming, 0, None);
    let playing = session.snapshot();
    assert_eq!(playing.transport, TransportState::Playing);
    report(
        &mut session,
        SendspinTransportPhase::AwaitingStream,
        4800,
        None,
    );
    let after = session.snapshot();
    assert_eq!(after.transport, TransportState::Buffering);
    for state in [&playing, &after] {
        assert_eq!(state.epochs, before.epochs);
        assert_eq!(state.session_id, before.session_id);
        assert!(state.muted, "the server never unmutes");
        assert_eq!(state.software_gain_db, before.software_gain_db);
        assert_eq!(state.audible_position_frames, None);
    }
    for method in ["signal_path.get", "metrics.get"] {
        applied(&mut session, method, None, json!({"method":method}));
    }
    let value = serde_json::to_value(&after).unwrap();
    assert_eq!(
        value["sendspin_playback"],
        json!({"phase":"awaiting_stream","submitted_frames":4800,"timing":{"qualification":"unqualified"}})
    );
    assert_eq!(
        sendspin(&mut session, "get-waiting")["value"]["phase"],
        "awaiting_stream"
    );
}

#[test]
fn stale_source_events_change_nothing() {
    let mut session = started();
    let connection = session.sendspin_connection().unwrap();
    let event = |epochs| SendspinSourceEvent {
        epochs,
        observation: SendspinSourceObservation::new(SendspinTransportPhase::Streaming, 0),
        error: None,
    };
    let before = session.snapshot();
    let mut old_session = connection.epochs;
    old_session.session -= 1;
    let mut old_stream = connection.epochs;
    old_stream.stream += 1;
    for (generation, epochs) in [
        (connection.generation + 1, connection.epochs),
        (connection.generation, old_session),
        (connection.generation, old_stream),
    ] {
        let error = session
            .advance_sendspin(generation, event(epochs), before.monotonic_ns + 1)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::StaleSession);
        assert_eq!(session.snapshot(), before);
    }
    // A stopped session no longer accepts events from its own source.
    applied(
        &mut session,
        "stop",
        Some("player-1"),
        json!({"method":"sessions.stop"}),
    );
    let stopped = session.snapshot();
    session
        .advance_sendspin(
            connection.generation,
            event(connection.epochs),
            stopped.monotonic_ns + 1,
        )
        .unwrap_err();
    assert_eq!(session.snapshot(), stopped);
    assert!(
        session.sendspin_connection().is_none(),
        "a stopped session owns no connection"
    );
}

#[test]
fn stop_clears_the_session_and_blocks_a_new_start_until_retirement() {
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 960, None);
    let generation = session.sendspin_connection().unwrap().generation;
    applied(
        &mut session,
        "stop",
        Some("player-1"),
        json!({"method":"sessions.stop"}),
    );
    let state = session.snapshot();
    assert_eq!(state.session_id, None);
    assert_eq!(state.source, None);
    assert_eq!(state.transport, TransportState::Idle);
    assert!(state.muted);
    assert_eq!(state.error, None);
    assert_eq!(
        sendspin(&mut session, "closing")["value"]["phase"],
        "closing"
    );
    assert_eq!(session.retiring_sendspin_connection(), Some(generation));
    rejected(
        &mut session,
        "too-soon",
        Some("player-2"),
        start(),
        ErrorCode::SourceBusy,
    );
    session
        .complete_sendspin_retirement(generation + 1, state.monotonic_ns + 1)
        .unwrap_err();
    session
        .complete_sendspin_retirement(generation, state.monotonic_ns + 1)
        .unwrap();
    assert_eq!(
        sendspin(&mut session, "idle")["value"]["phase"],
        "disconnected"
    );
    let receipt = submit(&mut session, "again", Some("player-2"), start());
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    assert_eq!(
        session.sendspin_connection().unwrap().generation,
        generation + 1
    );
}

#[test]
fn a_clean_close_clears_the_session_and_a_failed_close_fails_it() {
    let mut session = started();
    report(
        &mut session,
        SendspinTransportPhase::AwaitingStream,
        0,
        None,
    );
    let epochs = session.snapshot().epochs;
    report(&mut session, SendspinTransportPhase::Closed, 480, None);
    let state = session.snapshot();
    assert_eq!(
        (state.session_id.clone(), state.source, state.error),
        (None, None, None)
    );
    assert_eq!(state.transport, TransportState::Idle);
    assert!(state.muted);
    assert_eq!(state.epochs.session, epochs.session);
    assert_eq!(state.epochs.stream, epochs.stream + 1);
    assert_eq!(
        sendspin(&mut session, "closing")["value"]["phase"],
        "closing"
    );

    let (mut session, command) = started_by();
    report(
        &mut session,
        SendspinTransportPhase::Closed,
        0,
        Some(ErrorCode::AuthDenied),
    );
    let state = session.snapshot();
    assert_eq!(state.transport, TransportState::Failed);
    assert_eq!(state.error, Some(ErrorCode::AuthDenied));
    assert_eq!((state.session_id.clone(), state.source), (None, None));
    assert!(state.muted);
    assert!(
        state.output_consented,
        "a denied server keeps output consent"
    );
    let replay = session.submit(command);
    assert_eq!(
        replay.status,
        ReceiptStatus::Failed,
        "the start receipt fails"
    );
    assert_eq!(
        replay.error.map(|error| error.code),
        Some(ErrorCode::AuthDenied)
    );
    assert_eq!(
        sendspin(&mut session, "failed")["value"]["error"],
        "AUTH_DENIED"
    );

    let mut session = started();
    report(
        &mut session,
        SendspinTransportPhase::Closed,
        0,
        Some(ErrorCode::OutputFault),
    );
    assert!(
        !session.snapshot().output_consented,
        "an output fault revokes consent"
    );
}

#[test]
fn resume_and_seek_are_rejected_while_mute_and_volume_still_apply() {
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 0, None);
    for (id, payload) in [
        ("resume", json!({"method":"sessions.resume"})),
        (
            "seek",
            json!({"method":"sessions.seek","position_frames":0}),
        ),
    ] {
        rejected(
            &mut session,
            id,
            Some("player-1"),
            payload,
            ErrorCode::CapabilityUnsupported,
        );
    }
    rejected(
        &mut session,
        "takeover",
        Some("local-1"),
        json!({"method":"sessions.start","source":"local_file","device_id":"dac","takeover":true,"seekable":true}),
        ErrorCode::SourceBusy,
    );
    applied(
        &mut session,
        "volume",
        Some("player-1"),
        json!({"method":"volume.set","millidecibels":-30000,"ramp_frames":0}),
    );
    applied(
        &mut session,
        "unmute",
        Some("player-1"),
        json!({"method":"mute.set","muted":false}),
    );
    applied(
        &mut session,
        "mute",
        Some("player-1"),
        json!({"method":"mute.set","muted":true}),
    );
    assert_eq!(session.snapshot().transport, TransportState::Playing);
    applied(
        &mut session,
        "shutdown",
        Some("player-1"),
        json!({"method":"owner.shutdown"}),
    );
    let state = session.snapshot();
    assert!(state.shutdown_requested);
    assert_eq!((state.session_id.clone(), state.source), (None, None));
    assert!(session.sendspin_connection().is_none());
}

#[test]
fn a_restored_owner_has_no_sendspin_session_and_a_disconnected_source() {
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 0, None);
    let checkpoint: SessionCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&session.checkpoint()).unwrap()).unwrap();
    let mut restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new(OWNER).unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    restored.enable_local_playback();
    restored.enable_sendspin();
    let state = restored.snapshot();
    assert_eq!((state.session_id.clone(), state.source), (None, None));
    assert!(state.muted);
    assert!(
        serde_json::to_value(&state)
            .unwrap()
            .get("sendspin_playback")
            .is_none()
    );
    let view = sendspin(&mut restored, "restored");
    assert_eq!(view["value"]["phase"], "disconnected");
    assert_eq!(view["value"]["configured"], true);
    assert!(restored.sendspin_connection().is_none());
}

#[test]
fn pause_ends_a_streaming_session_like_stop() {
    let mut session = started();
    report(
        &mut session,
        SendspinTransportPhase::AwaitingStream,
        0,
        None,
    );
    applied(
        &mut session,
        "unmute",
        Some("player-1"),
        json!({"method":"mute.set","muted":false}),
    );
    report(&mut session, SendspinTransportPhase::Streaming, 960, None);
    let before = session.snapshot();
    let generation = session.sendspin_connection().unwrap().generation;
    let pause = json!({"method":"sessions.pause"});
    // Pause keeps its ordinary fences: the exact session and the exact revision.
    rejected(
        &mut session,
        "other-session",
        Some("player-9"),
        pause.clone(),
        ErrorCode::StaleSession,
    );
    let mut stale = wire(&session, "old-revision", Some("player-1"), pause.clone());
    stale.expected_revision = Some(before.revision - 1);
    let receipt = session.submit(stale);
    assert_eq!(
        receipt.error.map(|error| error.code),
        Some(ErrorCode::StaleRevision)
    );
    assert_eq!(session.snapshot(), before);
    applied(&mut session, "pause", Some("player-1"), pause);
    let state = session.snapshot();
    assert_eq!((state.session_id.clone(), state.source), (None, None));
    assert_eq!(state.transport, TransportState::Idle);
    assert!(state.muted);
    assert_eq!(state.error, None);
    assert_eq!(state.epochs.session, before.epochs.session);
    assert_eq!(state.epochs.stream, before.epochs.stream + 1);
    assert!(session.sendspin_connection().is_none());
    assert_eq!(session.retiring_sendspin_connection(), Some(generation));
    assert_eq!(
        sendspin(&mut session, "closing")["value"]["phase"],
        "closing"
    );
}

#[test]
fn the_credential_recovery_fact_latches_until_the_owner_restarts() {
    let mut session = started();
    assert_eq!(
        sendspin(&mut session, "clear")["value"]["credential_recovery_required"],
        false
    );
    let revision = session.snapshot().revision;
    session.require_sendspin_credential_recovery();
    assert_eq!(session.snapshot().revision, revision);
    report(
        &mut session,
        SendspinTransportPhase::Closed,
        0,
        Some(ErrorCode::AuthDenied),
    );
    assert_eq!(
        sendspin(&mut session, "latched")["value"]["credential_recovery_required"],
        true
    );
    let checkpoint: SessionCheckpoint =
        serde_json::from_slice(&serde_json::to_vec(&session.checkpoint()).unwrap()).unwrap();
    let mut restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new(OWNER).unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    restored.enable_sendspin();
    assert_eq!(
        sendspin(&mut restored, "restarted")["value"]["credential_recovery_required"],
        false
    );
}
