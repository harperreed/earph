// ABOUTME: Verifies exact redacted preview bytes from the real Session query path.
// ABOUTME: Separates retained reply replay from current candidate authority and build evidence.
use earph_core::{control::*, ids::*, session::Session};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("private-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn request(id: &str) -> CommandEnvelope {
    decode_command(&serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"private-zone","owner_id":"private-owner","session_id":null,"expected_revision":null,"payload":{"method":"diagnostics.preview"}})).unwrap(),&ControlLimits::default()).expect("diagnostics.preview must be supported")
}
fn value(r: &Receipt) -> Value {
    assert_eq!(r.status, ReceiptStatus::Applied, "{:?}", r.error);
    serde_json::to_value(r).unwrap()["result"]["value"].clone()
}
#[test]
fn preview_exact_bytes_digest_cut_and_missing_evidence_are_honest() {
    let mut s = owner();
    let r = s.submit_with_graph_effects(
        request("preview"),
        7,
        |_| unreachable!(),
        |_| panic!("preview must not persist state"),
    );
    let v = value(&r);
    let p = &v["report"];
    let bytes = p["bytes"].as_str().unwrap().as_bytes();
    assert_eq!(p["byte_len"], bytes.len());
    assert_eq!(
        p["sha256"],
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    let bundle: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(bundle["cut"], bundle["signal_path"]["cut"]);
    assert_eq!(bundle["metrics"]["cut"], bundle["cut"]);
    assert_eq!(bundle["history"]["snapshot"]["cut"], bundle["cut"]);
    assert_eq!(
        bundle["running_build"]["unavailable_reason"],
        "not_provided"
    );
    assert_eq!(bundle["test_inventory"], "not_attached");
    assert_eq!(bundle["reproduction"], "incomplete");
    for secret in ["private-zone", "private-owner"] {
        assert!(!std::str::from_utf8(bytes).unwrap().contains(secret));
    }
    assert_eq!(p["created_monotonic_ns"], 7);
    assert_eq!(p["expires_monotonic_ns"], 30_000_000_007u64);
    let replay = s.submit_with_graph_effects(
        request("preview"),
        31_000_000_000,
        |_| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(replay, r);
    let fresh = s.submit_with_graph_effects(
        request("fresh"),
        31_000_000_001,
        |_| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(
        value(&fresh)["report"]["created_monotonic_ns"],
        31_000_000_001u64
    );
    assert_eq!(fresh.revision, r.revision);
}
#[test]
fn preview_clock_overflow_rejects_without_product_mutation() {
    let mut s = owner();
    let before = serde_json::to_value(s.snapshot()).unwrap();
    let r = s.submit_with_graph_effects(
        request("overflow"),
        u64::MAX,
        |_| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(
        r.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), before);
}
#[test]
fn build_initialization_is_engine_validated_once_and_not_checkpointed() {
    use earph_core::provenance::{BuildUnavailableReason, RunningBuildProvenance};
    use std::sync::Arc;
    let stamp = Arc::new(RunningBuildProvenance::unavailable(
        BuildUnavailableReason::NotProvided,
    ));
    let initialized = owner().with_diagnostic_build(stamp.clone()).unwrap();
    assert!(
        initialized
            .clone()
            .with_diagnostic_build(stamp.clone())
            .is_err()
    );
    let mut observed = owner();
    value(&observed.submit(request("before-build")));
    assert!(observed.with_diagnostic_build(stamp.clone()).is_err());
    let checkpoint = initialized.checkpoint();
    assert!(
        !serde_json::to_string(&checkpoint)
            .unwrap()
            .contains("running_build")
    );
    let mut restored = Session::restore_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("private-owner").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap()
    .with_diagnostic_build(stamp)
    .unwrap();
    let v = value(&restored.submit(request("restored")));
    let bundle: Value = serde_json::from_str(v["report"]["bytes"].as_str().unwrap()).unwrap();
    assert_eq!(
        bundle["running_build"]["unavailable_reason"],
        "not_provided"
    );
}
#[test]
fn preview_shared_decoder_rejects_tampering_and_missing_nulls() {
    use earph_core::diagnostics::PreviewRead;
    let mut s = owner();
    let original = value(&s.submit(request("decode")));
    PreviewRead::decode(&serde_json::to_vec(&original).unwrap()).unwrap();
    for key in [
        "sha256",
        "byte_len",
        "policy_version",
        "expires_monotonic_ns",
    ] {
        let mut bad = original.clone();
        bad["report"][key] = json!(0);
        assert!(
            PreviewRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "{key}"
        );
    }
    let mut bad = original.clone();
    bad["binding"]["event_sequence"] = json!(19);
    assert!(PreviewRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    let mut bundle: Value =
        serde_json::from_str(original["report"]["bytes"].as_str().unwrap()).unwrap();
    bundle["running_build"]
        .as_object_mut()
        .unwrap()
        .remove("record");
    assert!(
        earph_core::diagnostics::DiagnosticBundle::decode(&serde_json::to_vec(&bundle).unwrap())
            .is_err()
    );
    bundle["running_build"]["record"] = Value::Null;
    bundle["metrics"]["underruns"]["value"] = json!(0);
    bundle["metrics"]["underruns"]["unavailable_reason"] = Value::Null;
    assert!(
        earph_core::diagnostics::DiagnosticBundle::decode(&serde_json::to_vec(&bundle).unwrap())
            .is_err()
    );
}
#[test]
fn typed_preview_size_refusal_precedes_digest_validation() {
    use earph_core::diagnostics::{MAX_PREVIEW_BYTES, PreviewRead};
    use std::sync::Arc;
    let mut s = owner();
    let original = value(&s.submit(request("typed-bound")));
    let mut read = PreviewRead::decode(&serde_json::to_vec(&original).unwrap()).unwrap();
    read.report.bytes = Arc::from(" ".repeat(MAX_PREVIEW_BYTES + 1));
    read.report.byte_len = read.report.bytes.len();
    assert_eq!(
        read.validate().unwrap_err().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn fresh_preview_observes_new_state_while_replay_keeps_the_original_cut() {
    let mut s = owner();
    let first = s.submit(request("historical-cut"));
    let first_value = value(&first);
    let state = s.snapshot();
    let changed = s.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("safe-mute").unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: None,
        expected_revision: None,
        payload: Command::Mute { muted: true },
    });
    assert_eq!(changed.status, ReceiptStatus::Applied);
    let fresh = value(&s.submit(request("current-cut")));
    assert_eq!(fresh["report"]["cut"]["revision"], first.revision + 1);
    assert_ne!(fresh["report"]["sha256"], first_value["report"]["sha256"]);
    assert_eq!(s.submit(request("historical-cut")), first);
}

#[test]
fn session_initializer_rejects_a_valid_harness_record_as_owner_provenance() {
    use earph_core::{profiles::ProfileObjectHash, provenance::*};
    use std::sync::Arc;
    // Negative DTO input only; these calculated fixture digests describe no product build.
    fn fixture_hash(bytes: &[u8]) -> ProfileObjectHash {
        ProfileObjectHash::try_from(
            Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        )
        .unwrap()
    }
    let record = CompiledBuildRecord {
        producer: BuildProducer::Harness,
        input_set_version: BUILD_INPUT_SET_VERSION,
        commit: BuildField::unavailable(BuildFieldReason::NotProvided),
        source_dirty: BuildField::unavailable(BuildFieldReason::NotProvided),
        dependency_lock_sha256: fixture_hash(b"negative harness fixture lock"),
        source_inputs_sha256: fixture_hash(b"negative harness fixture source"),
        compiler: BuildField::unavailable(BuildFieldReason::NotProvided),
        target: BuildField::unavailable(BuildFieldReason::NotProvided),
        cargo_profile_class: BuildField::unavailable(BuildFieldReason::NotProvided),
    };
    let stamp = RunningBuildProvenance::available(record).unwrap();
    assert!(owner().with_diagnostic_build(Arc::new(stamp)).is_err());
}

#[test]
fn preview_keeps_retained_history_when_core_session_has_no_owner_identity() {
    let mut s = Session::new(
        ZoneId::new("private-zone").unwrap(),
        ControlLimits::default(),
    );
    let mut mute = request("ownerless-mute");
    mute.owner_id = None;
    mute.payload = Command::Mute { muted: true };
    assert_eq!(s.submit(mute).status, ReceiptStatus::Applied);
    let mut c = request("ownerless-preview");
    c.owner_id = None;
    let read = value(&s.submit(c));
    let bundle: Value = serde_json::from_str(read["report"]["bytes"].as_str().unwrap()).unwrap();
    assert_eq!(bundle["history"]["events"].as_array().unwrap().len(), 1);
    assert!(read["binding"]["owner_id"].is_null());
}
