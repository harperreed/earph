// ABOUTME: Exercises graph commands through the shared typed control contract.
// ABOUTME: Checks transaction authority, stale work, safety, and retained request identity.
use earph_core::{control::*, ids::*, session::Session};

fn digest_command(hash: String) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("digest-storage").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: Some(0),
        payload: Command::GraphCommit {
            token: PreparedGraphId::new("candidate").unwrap(),
            graph_hash: hash,
        },
    }
}

#[test]
fn oversized_digest_storage_rejects_wire_admission() {
    let bytes = serde_json::to_vec(&digest_command("x".repeat(129))).unwrap();
    assert_eq!(
        decode_command(&bytes, &ControlLimits::default())
            .unwrap_err()
            .code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    for length in [64, 65, 128] {
        assert!(
            decode_command(
                &serde_json::to_vec(&digest_command("a".repeat(length))).unwrap(),
                &ControlLimits::default()
            )
            .is_ok()
        );
    }
}

#[test]
fn oversized_typed_digest_does_not_reserve_a_receipt() {
    let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    let mut command = digest_command("x".repeat(129));
    let rejected = session.submit(command.clone());
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    command.payload = Command::State;
    assert_eq!(session.submit(command).status, ReceiptStatus::Applied);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn typed_digest_storage_counts_reserved_string_capacity() {
    let mut hash = String::with_capacity(129);
    hash.push_str(&"a".repeat(64));
    let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    let rejected = session.submit(digest_command(hash));
    let mut reused = digest_command(String::new());
    reused.payload = Command::State;
    assert_eq!(session.submit(reused).status, ReceiptStatus::Applied);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn bounded_invalid_graph_digests_retain_their_request_identity() {
    for hash in [
        String::new(),
        "a".repeat(63),
        "g".repeat(64),
        "a".repeat(65),
    ] {
        let frame = serde_json::json!({
            "schema_version":1,"request_id":"digest","zone_id":"main",
            "owner_id":null,"session_id":null,"expected_revision":0,
            "payload":{"method":"graph.commit","token":"candidate","graph_hash":hash}
        });
        let mut command = decode_command(
            &serde_json::to_vec(&frame).unwrap(),
            &ControlLimits::default(),
        )
        .unwrap();
        let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
        let receipt = session.submit(command.clone());
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(
            receipt.error.unwrap().code,
            earph_core::ErrorCode::AssetInvalid
        );
        assert_eq!(session.submit(command.clone()).error, receipt.error);
        command.payload = Command::State;
        let reused = session.submit(command);
        assert_eq!(reused.status, ReceiptStatus::Rejected);
        assert_eq!(
            reused.error.unwrap().message,
            "request ID reused for different content"
        );
    }
}

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
