// ABOUTME: Tests metadata-only URI staging through the canonical command and receipt contracts.
// ABOUTME: Keeps untrusted media separate from playback, persistence, and source admission.
use earph_core::{
    control::{ControlLimits, ReceiptStatus, decode_command},
    ids::{OwnerId, ZoneId},
    session::Session,
};
use serde_json::json;

#[test]
fn wire_staging_returns_an_unadmitted_identity_without_exposing_the_uri() {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("source-owner").unwrap(),
        ControlLimits::default(),
    );
    let bytes = serde_json::to_vec(&json!({
        "schema_version":1,
        "request_id":"stage-one",
        "zone_id":"main",
        "owner_id":"source-owner",
        "session_id":null,
        "expected_revision":0,
        "payload":{
            "method":"sources.stage_uri",
            "uri":"http://example.invalid/a?token=private",
            "metadata":"<DIDL-Lite/>"
        }
    }))
    .unwrap();
    let command = decode_command(&bytes, &ControlLimits::default())
        .expect("metadata staging must decode without authorizing playback");
    let receipt = session.submit(command);
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    let reply = serde_json::to_value(receipt).unwrap();
    assert_eq!(reply["result"]["kind"], "sources");
    assert_eq!(reply["result"]["value"]["generation"], 1);
    assert_eq!(reply["result"]["value"]["staged"]["media_id"], "staged-1");
    assert_eq!(
        reply["result"]["value"]["staged"]["admission"],
        "unadmitted"
    );
    assert!(!reply.to_string().contains("private"));
    assert!(!reply.to_string().contains("DIDL"));
    assert!(session.snapshot().source.is_none());
    assert!(session.snapshot().muted);
}

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("source-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn wire(session: &Session, id: &str, payload: serde_json::Value) -> Vec<u8> {
    let state = session.snapshot();
    serde_json::to_vec(&json!({
        "schema_version":1,"request_id":id,"zone_id":"main",
        "owner_id":state.owner_id,"session_id":state.session_id,
        "expected_revision":state.revision,"payload":payload
    }))
    .unwrap()
}
fn submit(
    session: &mut Session,
    id: &str,
    payload: serde_json::Value,
) -> earph_core::control::Receipt {
    let bytes = wire(session, id, payload);
    session.submit(decode_command(&bytes, &ControlLimits::default()).unwrap())
}
fn sources(session: &mut Session) -> serde_json::Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "query-sources-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    serde_json::to_value(submit(session,&id,json!({"method":"sources.get"})))
        .unwrap()["result"]["value"].clone()
}
fn stage(uri: &str, metadata: &str) -> serde_json::Value {
    json!({"method":"sources.stage_uri","uri":uri,"metadata":metadata})
}

#[test]
fn replacing_and_clearing_staging_preserves_exact_request_replay() {
    let mut session = owner();
    let bytes = wire(
        &session,
        "first",
        stage("file:///untrusted/no-read", "<DIDL-Lite/>"),
    );
    let first = session.submit(decode_command(&bytes, &ControlLimits::default()).unwrap());
    assert_eq!(first.status, ReceiptStatus::Applied);
    assert_eq!(
        session.submit(decode_command(&bytes, &ControlLimits::default()).unwrap()),
        first
    );
    assert_eq!(sources(&mut session)["staged"]["media_id"], "staged-1");
    let next = submit(
        &mut session,
        "replace",
        stage("https://example.invalid/b", ""),
    );
    assert_eq!(next.status, ReceiptStatus::Applied);
    assert_eq!(sources(&mut session)["generation"], 2);
    assert_eq!(
        session.submit(decode_command(&bytes, &ControlLimits::default()).unwrap()),
        first
    );
    assert_eq!(sources(&mut session)["staged"]["media_id"], "staged-2");
    let cleared = submit(&mut session, "clear", stage("", ""));
    assert_eq!(cleared.status, ReceiptStatus::Applied);
    let result = serde_json::to_value(&cleared).unwrap();
    assert_eq!(
        result["result"]["value"],
        json!({"generation":3,"staged":null})
    );
    assert_eq!(sources(&mut session), json!({"generation":3,"staged":null}));
    let bad = submit(&mut session, "orphan-metadata", stage("", "metadata"));
    assert_eq!(bad.status, ReceiptStatus::Rejected);
    assert_eq!(bad.error.unwrap().code, earph_core::ErrorCode::AssetInvalid);
    assert_eq!(sources(&mut session), json!({"generation":3,"staged":null}));
}

#[test]
fn staged_data_never_reaches_checkpoints_or_general_state() {
    let mut session = owner();
    let receipt = submit(
        &mut session,
        "stage-private",
        stage(
            "http://example.invalid/?token=private-uri",
            "private-metadata",
        ),
    );
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    let checkpoint = serde_json::to_vec(&session.checkpoint()).unwrap();
    for bytes in [
        checkpoint.clone(),
        serde_json::to_vec(&session.snapshot()).unwrap(),
        serde_json::to_vec(&session.events_since(0)).unwrap(),
    ] {
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("private-uri"));
        assert!(!text.contains("private-metadata"));
        assert!(!text.contains("staged-1"));
    }
    let decoded = serde_json::from_slice(&checkpoint).unwrap();
    let mut restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("replacement-owner").unwrap(),
        ControlLimits::default(),
        decoded,
    )
    .unwrap();
    assert_eq!(
        sources(&mut restored),
        json!({"generation":0,"staged":null})
    );
    assert!(restored.snapshot().muted);
    assert!(restored.snapshot().source.is_none());
}

#[test]
fn failed_commit_does_not_replace_staging_or_advance_its_generation() {
    let mut session = owner();
    assert_eq!(
        submit(
            &mut session,
            "original",
            stage("http://example.invalid/a", "")
        )
        .status,
        ReceiptStatus::Applied
    );
    let before = session.snapshot();
    let bytes = wire(
        &session,
        "storage-failed",
        stage("http://example.invalid/?secret=never-persist", "metadata"),
    );
    let failed = session.submit_with_commit(
        decode_command(&bytes, &ControlLimits::default()).unwrap(),
        |candidate| {
            let encoded = serde_json::to_string(&candidate.checkpoint()).unwrap();
            assert!(!encoded.contains("never-persist"));
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::OutputFault,
                "test storage refusal",
            ))
        },
    );
    assert_eq!(failed.status, ReceiptStatus::Failed);
    assert_eq!(session.snapshot(), before);
    assert_eq!(sources(&mut session)["staged"]["media_id"], "staged-1");
}

#[test]
fn byte_limits_reject_oversized_utf8_before_receipt_retention() {
    let mut session = owner();
    let uri = "雪".repeat(2730) + "ab";
    let metadata = "雪".repeat(21845) + "a";
    let first = submit(&mut session, "exact-bounds", stage(&uri, &metadata));
    assert_eq!(first.status, ReceiptStatus::Applied);
    let summary = sources(&mut session);
    assert_eq!(summary["staged"]["uri_bytes"], 8192);
    assert_eq!(summary["staged"]["metadata_bytes"], 65536);
    for payload in [
        stage(&(uri.clone() + "x"), ""),
        stage("valid", &(metadata.clone() + "x")),
    ] {
        let bytes = wire(&session, "over-bound", payload);
        assert_eq!(
            decode_command(&bytes, &ControlLimits::default())
                .unwrap_err()
                .code,
            earph_core::ErrorCode::ResourceBudgetExceeded
        );
    }
    assert_eq!(sources(&mut session), summary);
}

#[test]
fn staging_preserves_pending_local_playback_and_output_authority() {
    use earph_core::{control::*, ids::*, safety::*};
    let mut session = owner();
    session.enable_local_playback();
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "silent-fixture".into(),
        device_id: DeviceId::new("dac").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    for (id, payload) in [
        ("output", json!({"method":"output.select","output":output})),
        (
            "consent",
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        ),
        (
            "append",
            json!({"method":"queue.append","path":"unopened-fixture.wav"}),
        ),
    ] {
        assert_eq!(
            submit(&mut session, id, payload).status,
            ReceiptStatus::Applied
        );
    }
    let mut start:serde_json::Value=serde_json::from_slice(&wire(&session,"local-start",json!({
        "method":"sessions.start","source":"local_file","device_id":"dac","takeover":false,"seekable":true
    }))).unwrap();
    start["session_id"] = json!("local-session");
    let start = decode_command(
        &serde_json::to_vec(&start).unwrap(),
        &ControlLimits::default(),
    )
    .unwrap();
    assert_eq!(
        session.submit(start.clone()).status,
        ReceiptStatus::Accepted
    );
    let before = session.snapshot();
    let queue = session.queue_snapshot();
    assert_eq!(
        submit(
            &mut session,
            "metadata-only",
            stage("http://example.invalid/no-fetch", "metadata")
        )
        .status,
        ReceiptStatus::Applied
    );
    let after = session.snapshot();
    let mut expected = before.clone();
    expected.revision += 1;
    expected.event_sequence += 1;
    assert_eq!(after, expected);
    assert_eq!(session.queue_snapshot(), queue);
    assert!(session.wants_playback());
    assert_eq!(session.submit(start).status, ReceiptStatus::Accepted);
    let caps = serde_json::to_value(submit(
        &mut session,
        "capabilities",
        json!({"method":"capabilities.get"}),
    ))
    .unwrap();
    assert_eq!(caps["result"]["value"]["sources"], json!(["local_file"]));
}

#[test]
fn owner_revision_session_and_request_conflicts_preserve_staging() {
    use earph_core::ErrorCode;
    let mut session = owner();
    let bytes = wire(&session, "original", stage("http://example.invalid/a", ""));
    let mut original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        session
            .submit(decode_command(&bytes, &ControlLimits::default()).unwrap())
            .status,
        ReceiptStatus::Applied
    );
    let stable = sources(&mut session);
    for (key, value, code) in [
        ("owner_id", json!("old-owner"), ErrorCode::StaleSession),
        ("owner_id", serde_json::Value::Null, ErrorCode::StaleSession),
        ("zone_id", json!("elsewhere"), ErrorCode::StaleSession),
        ("session_id", json!("old-session"), ErrorCode::StaleSession),
        ("expected_revision", json!(0), ErrorCode::StaleRevision),
    ] {
        let mut request: serde_json::Value = serde_json::from_slice(&wire(
            &session,
            &format!("stale-{key}-{value}"),
            stage("http://example.invalid/b", ""),
        ))
        .unwrap();
        request[key] = value;
        let receipt = session.submit(
            decode_command(
                &serde_json::to_vec(&request).unwrap(),
                &ControlLimits::default(),
            )
            .unwrap(),
        );
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(receipt.error.unwrap().code, code);
        assert_eq!(sources(&mut session), stable);
    }
    original["payload"]["uri"] = json!("http://example.invalid/conflict");
    let receipt = session.submit(
        decode_command(
            &serde_json::to_vec(&original).unwrap(),
            &ControlLimits::default(),
        )
        .unwrap(),
    );
    assert_eq!(receipt.error.unwrap().code, ErrorCode::AssetInvalid);
    assert_eq!(sources(&mut session), stable);
}

#[test]
fn excessive_typed_capacity_does_not_reserve_a_request_or_publish_metadata() {
    use earph_core::control::*;
    let mut session = owner();
    for metadata_capacity in [false, true] {
        let mut command = decode_command(
            &wire(&session, "capacity", stage("x", "")),
            &ControlLimits::default(),
        )
        .unwrap();
        let mut excess = String::with_capacity(if metadata_capacity { 65537 } else { 8193 });
        excess.push('x');
        command.payload = if metadata_capacity {
            Command::SourceStageUri {
                uri: "x".into(),
                metadata: excess,
            }
        } else {
            Command::SourceStageUri {
                uri: excess,
                metadata: String::new(),
            }
        };
        let receipt = session.submit(command);
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(
            receipt.error.unwrap().code,
            earph_core::ErrorCode::ResourceBudgetExceeded
        );
    }
    assert_eq!(sources(&mut session), json!({"generation":0,"staged":null}));
    assert_eq!(
        submit(&mut session, "capacity", stage("x", "")).status,
        ReceiptStatus::Applied
    );
}

#[test]
fn caller_grants_and_malformed_uri_text_cannot_create_source_authority() {
    let mut session = owner();
    let mut forged = stage("http://example.invalid/a", "");
    forged["origin_grant_id"] = json!("caller-claims-trust");
    assert!(decode_command(&wire(&session, "forged", forged), &ControlLimits::default()).is_err());
    for (index, uri) in [
        "   ",
        " leading",
        "trailing ",
        "uri\0control",
        "uri\ncontrol",
    ]
    .into_iter()
    .enumerate()
    {
        let rejected = submit(&mut session, &format!("invalid-{index}"), stage(uri, ""));
        assert_eq!(rejected.status, ReceiptStatus::Rejected);
        assert_eq!(
            rejected.error.unwrap().code,
            earph_core::ErrorCode::AssetInvalid
        );
    }
    assert_eq!(sources(&mut session), json!({"generation":0,"staged":null}));
}

#[test]
fn canonical_staging_summary_is_a_strict_secret_free_consumer_contract() {
    use earph_core::source::SourceStagingSnapshot;
    let mut session = owner();
    assert_eq!(
        submit(
            &mut session,
            "stage-summary",
            stage("untrusted:value", "metadata")
        )
        .status,
        ReceiptStatus::Applied
    );
    let original = sources(&mut session);
    let parsed: SourceStagingSnapshot = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
    for key in ["generation", "staged"] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(key);
        assert!(
            serde_json::from_value::<SourceStagingSnapshot>(missing).is_err(),
            "missing {key}"
        );
    }
    for (key, value) in [
        ("media_id", json!("staged-2")),
        ("admission", json!("admitted")),
        ("uri_bytes", json!(0)),
        ("uri_bytes", json!(8193)),
        ("metadata_bytes", json!(65537)),
        ("uri", json!("private")),
    ] {
        let mut bad = original.clone();
        bad["staged"][key] = value;
        assert!(
            serde_json::from_value::<SourceStagingSnapshot>(bad).is_err(),
            "invalid {key}"
        );
    }
    let mut bad = original;
    bad["generation"] = json!(0);
    assert!(serde_json::from_value::<SourceStagingSnapshot>(bad).is_err());
    assert!(
        serde_json::from_value::<SourceStagingSnapshot>(json!({"generation":0,"staged":null}))
            .is_ok()
    );
}

#[test]
fn oversized_source_request_identity_capacity_does_not_enter_history() {
    use earph_core::{control::*, ids::*};
    let mut session = owner();
    let mut command = decode_command(
        &wire(&session, "large-id", stage("x", "")),
        &ControlLimits::default(),
    )
    .unwrap();
    let mut identity = String::with_capacity(129);
    identity.push_str("large-id");
    command.request_id = RequestId::new(identity).unwrap();
    let rejected = session.submit(command);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(
        submit(&mut session, "large-id", stage("x", "")).status,
        ReceiptStatus::Applied
    );
}

#[test]
fn staging_requires_an_authoritative_owner_even_without_a_playable_source() {
    let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    let receipt = submit(
        &mut session,
        "owner-required",
        stage("untrusted:metadata", ""),
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(sources(&mut session), json!({"generation":0,"staged":null}));
}
