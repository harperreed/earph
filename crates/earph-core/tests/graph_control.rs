// ABOUTME: Exercises graph commands through the shared typed control contract.
// ABOUTME: Checks transaction authority, stale work, safety, and retained request identity.
use earph_core::{control::*, ids::*, session::Session};

#[test]
fn bounded_semantic_rejection_reaches_retained_owner_history() {
    let frame = br#"{"schema_version":1,"request_id":"semantic","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":0,"payload":{"method":"graph.prepare","graph":{"schema_version":1,"nodes":[{"id":"matrix","kind":{"kind":"matrix","input_channels":"stereo","coefficients":[1,0,1]},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64}}],"edges":[],"output":"matrix"},"processing":{"kind":"integer_bypass"}}}"#;
    let mut command = decode_command(frame, &ControlLimits::default()).unwrap();
    let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    let receipt = session.submit(command.clone());
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::GraphInvalid
    );
    command.payload = Command::State;
    let receipt = session.submit(command);
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
}

#[test]
fn graph_inspection_is_a_read_only_shared_command() {
    let command = decode_command(
        br#"{"schema_version":1,"request_id":"inspect","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"graph.inspect"}}"#,
        &ControlLimits::default(),
    ).unwrap();
    assert!(command.payload.is_query());
    let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    let receipt = session.submit(command);
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    assert_eq!(receipt.revision, 0);
    let result = serde_json::to_value(receipt.result).unwrap();
    assert_eq!(result["kind"], "graph_inspection");
    assert_eq!(result["value"]["graph_epoch"], 0);
    assert!(result["value"]["active"].is_null());
}

#[test]
fn inspection_rejects_ignored_graph_parameters() {
    assert!(decode_command(
        br#"{"schema_version":1,"request_id":"inspect","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"graph.inspect","graph_hash":"ignored"}}"#,
        &ControlLimits::default(),
    ).is_err());
}

#[test]
fn graph_byte_limit_includes_the_original_frame_whitespace() {
    let command = br#"{"schema_version":1,"request_id":"prepare","zone_id":"main","owner_id":"owner","session_id":"session","expected_revision":0,"payload":{"method":"graph.prepare","graph":{"schema_version":1,"nodes":[],"edges":[],"output":"out"},"processing":{"kind":"integer_bypass"}}}"#;
    let mut frame = vec![b' '; 128 * 1024 - command.len()];
    frame.extend_from_slice(command);
    assert!(decode_command(&frame, &ControlLimits::default()).is_ok());
    frame.push(b' ');
    assert_eq!(
        decode_command(&frame, &ControlLimits::default())
            .unwrap_err()
            .code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    let escaped = String::from_utf8(frame)
        .unwrap()
        .replace("graph.prepare", r"graph.\u0070repare");
    assert_eq!(
        decode_command(escaped.as_bytes(), &ControlLimits::default())
            .unwrap_err()
            .code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn oversized_graph_is_rejected_before_its_invalid_parameters() {
    let command = br#"{"schema_version":1,"request_id":"prepare","zone_id":"main","owner_id":"owner","session_id":"session","expected_revision":0,"payload":{"method":"graph.prepare","graph":{"schema_version":1,"nodes":[],"edges":[],"output":"out","invalid":true},"processing":{"kind":"integer_bypass"}}}"#;
    let mut frame = vec![b' '; 128 * 1024 + 1];
    frame.extend_from_slice(command);
    assert_eq!(
        decode_command(&frame, &ControlLimits::default())
            .unwrap_err()
            .code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}
