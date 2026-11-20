// ABOUTME: Checks desktop projections against real Core-generated reply documents.
// ABOUTME: Catches missing fields, invented null defaults, and malformed control replies.
use earph_core::{
    control::{Command, CommandEnvelope, ControlLimits},
    ids::{OwnerId, RequestId, ZoneId},
    session::Session,
};
use earph_desktop::{Reply, ReplyStatus, decode_reply};
use serde_json::{Value, json};

fn state_reply() -> Vec<u8> {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner-test").unwrap(),
        ControlLimits::default(),
    );
    let receipt = session.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("state-test").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::State,
    });
    serde_json::to_vec(&json!({"receipt":receipt})).unwrap()
}

#[test]
fn actual_owner_state_keeps_unknown_observations_and_exact_reply_bytes() {
    let bytes = state_reply();
    let reply = decode_reply(bytes.clone()).expect("real Core state reply must project");
    assert_eq!(reply.bytes(), bytes);
    let Reply::Receipt(receipt) = reply else {
        panic!("expected receipt")
    };
    assert_eq!(receipt.status, ReplyStatus::Applied);
    let state = receipt.state().expect("state result");
    assert_eq!(state.owner_id.as_ref().unwrap().as_str(), "owner-test");
    assert_eq!(state.revision, 0);
    assert!(state.muted);
    assert_eq!(state.software_gain_db, -40.0);
    assert_eq!(state.audible_position_frames, None);
    assert_eq!(state.playback, None);
    assert_eq!(state.output, None);
    assert!(!state.output_consented);
}

#[test]
fn missing_consumed_fields_are_not_deserialized_as_null_or_zero() {
    let original: Value = serde_json::from_slice(&state_reply()).unwrap();
    for field in [
        "owner_id",
        "session_id",
        "audible_position_frames",
        "output",
        "playback",
        "muted",
        "software_gain_zero",
        "revision",
        "output_consented",
    ] {
        let mut value = original.clone();
        value["receipt"]["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            decode_reply(serde_json::to_vec(&value).unwrap()).is_err(),
            "missing {field}"
        );
    }
    for field in ["session_id", "activation_frame", "result", "error"] {
        let mut value = original.clone();
        value["receipt"].as_object_mut().unwrap().remove(field);
        assert!(
            decode_reply(serde_json::to_vec(&value).unwrap()).is_err(),
            "missing receipt {field}"
        );
    }
}

#[test]
fn missing_hardware_cap_and_null_receipt_keys_are_not_valid_owner_documents() {
    let mut value: Value = serde_json::from_slice(&state_reply()).unwrap();
    value["receipt"]["result"]["value"]["volume_policy"]
        .as_object_mut()
        .unwrap()
        .remove("hardware_cap_db");
    assert!(decode_reply(serde_json::to_vec(&value).unwrap()).is_err());
    let owner_error =
        br#"{"receipt":null,"error":{"code":"ASSET_INVALID","message":"bad"}}"#.to_vec();
    assert!(decode_reply(owner_error).is_err());
}

#[test]
fn malformed_or_ambiguous_envelopes_never_become_successful_receipts() {
    let mut state: Value = serde_json::from_slice(&state_reply()).unwrap();
    state["receipt"]["result"]["value"]["revision"] = json!("0");
    let mut bad = vec![
        b"null".to_vec(),
        b"{}".to_vec(),
        b"{".to_vec(),
        serde_json::to_vec(&state).unwrap(),
    ];
    let mut both: Value = serde_json::from_slice(&state_reply()).unwrap();
    both["error"] = json!({"code":"ASSET_INVALID","message":"ambiguous"});
    bad.push(serde_json::to_vec(&both).unwrap());
    for bytes in bad {
        assert!(decode_reply(bytes).is_err());
    }
}

#[test]
fn top_level_owner_uncertainty_stays_distinct_from_a_retained_receipt() {
    let bytes = br#"{"error":{"code":"DEVICE_UNAVAILABLE","message":"control result is uncertain; reconcile through fresh state"}}"#.to_vec();
    let reply = decode_reply(bytes.clone()).unwrap();
    assert_eq!(reply.bytes(), bytes);
    let Reply::OwnerError(error) = reply else {
        panic!("must preserve top-level error")
    };
    assert_eq!(error.error.code, earph_core::ErrorCode::DeviceUnavailable);
}

#[test]
fn contradictory_snapshot_metadata_cannot_replace_an_observed_owner_state() {
    let original: Value = serde_json::from_slice(&state_reply()).unwrap();
    for (field, value) in [
        ("revision", json!(8)),
        ("zone_id", json!("other-zone")),
        ("epochs", json!({"session":7,"stream":0})),
    ] {
        let mut reply = original.clone();
        reply["receipt"]["result"]["value"][field] = value;
        assert!(
            decode_reply(serde_json::to_vec(&reply).unwrap()).is_err(),
            "contradictory {field}"
        );
    }
    let mut pending = original;
    pending["receipt"]["status"] = json!("accepted");
    assert!(decode_reply(serde_json::to_vec(&pending).unwrap()).is_err());
}
