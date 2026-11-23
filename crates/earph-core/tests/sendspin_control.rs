// ABOUTME: Tests the Sendspin player control surface through wire commands and source events.
// ABOUTME: Covers start admission, the unmute gate, event fencing, close handling, and restore.
use earph_core::{
    ErrorCode,
    control::{CommandEnvelope, ControlLimits, Receipt, ReceiptStatus, decode_command},
    ids::{OwnerId, ZoneId},
    sendspin_source::{SendspinSourceEvent, SendspinSourceObservation, SendspinTransportPhase},
    session::{EngineEvent, EngineEventKind, Session, SessionCheckpoint, TransportState},
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
fn takeover_start() -> Value {
    json!({"method":"sources.sendspin.start","device_id":"dac","takeover":true})
}
fn local_start(takeover: bool) -> Value {
    json!({"method":"sessions.start","source":"local_file","device_id":"dac","takeover":takeover,"seekable":true})
}
/// Returns a configured owner with an approved output, and its accepted Sendspin start.
fn started_by() -> (Session, CommandEnvelope) {
    let mut session = configured();
    applied(&mut session, "select", None, output());
    applied(&mut session, "consent", None, consent());
    applied(
        &mut session,
        "append",
        None,
        json!({"method":"queue.append","path":"/music/track.flac"}),
    );
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
    // One server serves this source, so a second connection cannot take over the first.
    rejected(
        &mut session,
        "second-takeover",
        Some("player-2"),
        takeover_start(),
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
    // No chunk was scheduled, so the whole block is the unanchored, unqualified observation.
    let waiting = SendspinSourceObservation::new(SendspinTransportPhase::AwaitingStream, 4800);
    assert_eq!(
        value["sendspin_playback"],
        serde_json::to_value(waiting).unwrap()
    );
    assert_eq!(
        value["sendspin_playback"]["timing"]["qualification"],
        "unqualified"
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

/// The worker's own closed observation arrives after Core released the session, so Core refuses it.
#[test]
fn a_stopped_stream_reports_a_closed_source_without_an_anchor() {
    use earph_core::sendspin_source::SendspinTiming;
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 960, None);
    let generation = session.sendspin_connection().unwrap().generation;
    let mut streaming = SendspinSourceObservation::new(SendspinTransportPhase::Streaming, 4800);
    streaming.timing = SendspinTiming {
        anchored: true,
        start_error_us: Some(-40),
        last_error_us: Some(125),
        max_abs_error_us: 125,
        inserted_silence_frames: 480,
        ..SendspinTiming::UNANCHORED
    };
    session.observe_sendspin(generation, streaming).unwrap();
    applied(
        &mut session,
        "stop",
        Some("player-1"),
        json!({"method":"sessions.stop"}),
    );
    let playback = serde_json::to_value(session.snapshot()).unwrap()["sendspin_playback"].clone();
    assert_eq!(playback["phase"], "closed");
    assert_eq!(playback["timing"]["anchored"], false);
    assert_eq!(playback["timing"]["start_error_us"], Value::Null);
    // Cumulative facts stay readable.
    assert_eq!(playback["submitted_frames"], 4800);
    assert_eq!(playback["timing"]["last_error_us"], 125);
    assert_eq!(playback["timing"]["inserted_silence_frames"], 480);
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

/// The Host reports a lost server as `reconnecting`. The session and its unmute stay.
#[test]
fn a_reconnecting_source_keeps_the_session_and_reports_its_reconnects() {
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 960, None);
    applied(
        &mut session,
        "unmute",
        Some("player-1"),
        json!({"method":"mute.set","muted":false}),
    );
    let playing = session.snapshot();
    report(&mut session, SendspinTransportPhase::Reconnecting, 0, None);
    let lost = session.snapshot();
    assert_eq!(lost.transport, TransportState::Buffering);
    assert_eq!(lost.session_id, playing.session_id);
    assert_eq!(lost.epochs, playing.epochs);
    assert_eq!(lost.error, None);
    assert!(!lost.muted, "a lost server changes no mute");
    assert!(session.sendspin_connection().is_some());
    assert_eq!(
        sendspin(&mut session, "get-lost")["value"]["phase"],
        "reconnecting"
    );
    // The new connection reports its count of completed reconnects.
    let connection = session.sendspin_connection().unwrap();
    let mut observation = SendspinSourceObservation::new(SendspinTransportPhase::Streaming, 480);
    observation.reconnects = 1;
    session
        .advance_sendspin(
            connection.generation,
            SendspinSourceEvent {
                epochs: connection.epochs,
                observation,
                error: None,
            },
            session.snapshot().monotonic_ns + 1,
        )
        .unwrap();
    let state = session.snapshot();
    assert_eq!(state.transport, TransportState::Playing);
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(value["sendspin_playback"]["phase"], "streaming");
    assert_eq!(value["sendspin_playback"]["reconnects"], 1);
}

#[test]
fn a_local_start_takes_the_output_from_sendspin_only_with_takeover() {
    let mut session = started();
    report(&mut session, SendspinTransportPhase::Streaming, 960, None);
    applied(
        &mut session,
        "unmute",
        Some("player-1"),
        json!({"method":"mute.set","muted":false}),
    );
    let old = session.sendspin_connection().unwrap();
    rejected(
        &mut session,
        "implicit",
        Some("local-1"),
        local_start(false),
        ErrorCode::SourceBusy,
    );
    let receipt = submit(&mut session, "explicit", Some("local-1"), local_start(true));
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");

    let state = session.snapshot();
    assert_eq!(
        state.session_id.as_ref().map(|id| id.as_str()),
        Some("local-1")
    );
    let wire = serde_json::to_value(&state).unwrap();
    assert_eq!(wire["source"], json!("local_file"));
    assert!(
        wire.get("sendspin_playback").is_none(),
        "the new session shows no Sendspin facts"
    );
    assert_eq!(state.transport, TransportState::Preparing);
    assert!(state.muted, "a takeover starts muted");
    assert_eq!(state.epochs.session, old.epochs.session + 1);
    assert!(session.sendspin_connection().is_none());
    assert_eq!(session.retiring_sendspin_connection(), Some(old.generation));
    assert_eq!(
        sendspin(&mut session, "closing")["value"]["phase"],
        "closing"
    );

    // The retired stream's chunks, progress, and failure reach Core under the old fences.
    let before = session.snapshot();
    for (phase, error) in [
        (SendspinTransportPhase::Streaming, None),
        (SendspinTransportPhase::Closed, None),
        (SendspinTransportPhase::Closed, Some(ErrorCode::OutputFault)),
    ] {
        let stale = session
            .advance_sendspin(
                old.generation,
                SendspinSourceEvent {
                    epochs: old.epochs,
                    observation: SendspinSourceObservation::new(phase, 1920),
                    error,
                },
                before.monotonic_ns + 1,
            )
            .unwrap_err();
        assert_eq!(stale.code, ErrorCode::StaleSession);
        assert_eq!(session.snapshot(), before);
    }
    session
        .observe_sendspin(
            old.generation,
            SendspinSourceObservation::new(SendspinTransportPhase::Streaming, 1920),
        )
        .unwrap_err();
    assert_eq!(session.snapshot(), before);

    // The server cannot return without a new explicit start, and that start waits for retirement.
    rejected(
        &mut session,
        "seize-back",
        Some("player-2"),
        takeover_start(),
        ErrorCode::SourceBusy,
    );
    session
        .complete_sendspin_retirement(old.generation, before.monotonic_ns + 1)
        .unwrap();
    assert_eq!(
        sendspin(&mut session, "idle")["value"]["phase"],
        "disconnected"
    );
    let state = session.snapshot();
    assert_eq!(
        state.session_id.as_ref().map(|id| id.as_str()),
        Some("local-1")
    );
    assert_eq!(state.source, before.source);
    assert!(state.muted);
}

#[test]
fn a_sendspin_start_takes_the_output_from_a_local_session_only_with_takeover() {
    let mut session = configured();
    applied(&mut session, "select", None, output());
    applied(&mut session, "consent", None, consent());
    applied(
        &mut session,
        "append",
        None,
        json!({"method":"queue.append","path":"/music/track.flac"}),
    );
    let receipt = submit(&mut session, "local", Some("local-1"), local_start(false));
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    let local = session.snapshot();
    assert!(local.active_entry_id.is_some());

    // A start without the field asks for no takeover.
    for (id, payload) in [
        ("absent", start()),
        ("implicit", {
            let mut payload = takeover_start();
            payload["takeover"] = json!(false);
            payload
        }),
    ] {
        rejected(
            &mut session,
            id,
            Some("player-1"),
            payload,
            ErrorCode::SourceBusy,
        );
    }
    let receipt = submit(&mut session, "explicit", Some("player-1"), takeover_start());
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");

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
    assert_eq!(state.active_entry_id, None);
    assert_eq!(state.epochs.session, local.epochs.session + 1);
    let connection = session.sendspin_connection().unwrap();
    assert_eq!(connection.epochs, state.epochs);

    // The retired local source can no longer move this session.
    for epochs in [local.epochs, state.epochs] {
        session
            .advance(EngineEvent {
                epochs,
                monotonic_ns: state.monotonic_ns + 1,
                kind: EngineEventKind::Started {
                    activation_frame: 0,
                },
            })
            .unwrap_err();
        assert_eq!(session.snapshot(), state);
    }
    rejected(
        &mut session,
        "early-unmute",
        Some("player-1"),
        json!({"method":"mute.set","muted":false}),
        ErrorCode::SafetyPolicyBlocked,
    );
    report(&mut session, SendspinTransportPhase::Streaming, 0, None);
    assert_eq!(session.snapshot().transport, TransportState::Playing);
}
