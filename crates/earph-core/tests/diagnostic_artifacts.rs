// ABOUTME: Exercises diagnostic artifact admission, pinning and scoped cancellation through Session.
// ABOUTME: Uses actual preview bytes and preserves filesystem uncertainty until real worker completion.
use earph_core::{control::*, ids::*, session::Session};
use serde_json::{Value, json};
fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("zone").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    )
}
fn command(s: &Session, id: &str, payload: Value) -> CommandEnvelope {
    let state = s.snapshot();
    decode_command(&serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"zone","owner_id":"owner","session_id":state.session_id,"expected_revision":state.revision,"payload":payload})).unwrap(),&ControlLimits::default()).expect("artifact command must be supported")
}
fn submit(s: &mut Session, id: &str, payload: Value, now: u64) -> Receipt {
    let c = command(s, id, payload);
    s.submit_with_graph_effects(c, now, |_| unreachable!(), |_| Ok(()))
}
fn preview(s: &mut Session) -> Value {
    let r = submit(s, "preview", json!({"method":"diagnostics.preview"}), 0);
    assert_eq!(r.status, ReceiptStatus::Applied);
    serde_json::to_value(r).unwrap()["result"]["value"]["report"].clone()
}
#[test]
fn export_pins_exact_preview_until_worker_retirement() {
    let mut s = owner();
    let p = preview(&mut s);
    let accepted = submit(
        &mut s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        1,
    );
    assert_eq!(
        accepted.status,
        ReceiptStatus::Accepted,
        "{:?}",
        accepted.error
    );
    assert_eq!(
        serde_json::to_value(&accepted).unwrap()["result"]["value"]["effect"],
        "inspect_required"
    );
    let blocked = submit(
        &mut s,
        "other-preview",
        json!({"method":"diagnostics.preview"}),
        31_000_000_000,
    );
    assert_eq!(
        blocked.error.unwrap().code,
        earph_core::ErrorCode::SourceBusy
    );
    let cancel = submit(
        &mut s,
        "cancel",
        json!({"method":"diagnostics.cancel","request_id":"export"}),
        31_000_000_001,
    );
    assert_eq!(cancel.status, ReceiptStatus::Applied);
    assert_eq!(
        serde_json::to_value(cancel).unwrap()["result"]["value"]["state"],
        "requested"
    );
    let blocked = submit(
        &mut s,
        "still-pinned",
        json!({"method":"diagnostics.preview"}),
        31_000_000_002,
    );
    assert_eq!(
        blocked.error.unwrap().code,
        earph_core::ErrorCode::SourceBusy
    );
    let replay = submit(
        &mut s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        31_000_000_003,
    );
    assert_eq!(replay, accepted);
}
#[test]
fn export_refuses_expiry_hash_mismatch_and_stale_revision_without_regathering() {
    let mut s = owner();
    let p = preview(&mut s);
    let mut c = command(
        &s,
        "stale",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
    );
    c.expected_revision = Some(99);
    assert_eq!(
        s.submit(c).error.unwrap().code,
        earph_core::ErrorCode::StaleRevision
    );
    let mismatch = submit(
        &mut s,
        "hash",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":"a".repeat(64)}),
        1,
    );
    assert_eq!(
        mismatch.error.unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    let expired = submit(
        &mut s,
        "expired",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        30_000_000_000,
    );
    assert_eq!(
        expired.error.unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
}
#[test]
fn inspection_is_an_explicit_bounded_job_and_cancel_is_owner_scoped() {
    let mut s = owner();
    let p = preview(&mut s);
    let bad = submit(
        &mut s,
        "traversal",
        json!({"method":"diagnostics.inspect","directory":"/owned/../escape","expected_sha256":p["sha256"]}),
        1,
    );
    assert_eq!(bad.status, ReceiptStatus::Rejected);
    let c = command(
        &s,
        "inspect",
        json!({"method":"diagnostics.inspect","directory":"/owned/evidence","expected_sha256":p["sha256"]}),
    );
    let accepted = s.submit(c.clone());
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    let mut cancel = command(
        &s,
        "foreign-cancel",
        json!({"method":"diagnostics.cancel","request_id":"inspect"}),
    );
    cancel.owner_id = Some(OwnerId::new("foreign").unwrap());
    assert_eq!(
        s.submit(cancel).error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(s.submit(c), accepted);
}

#[test]
fn refused_worker_dispatch_records_unchanged_effects_without_pinning() {
    let mut s = owner();
    let p = preview(&mut s);
    let c = command(
        &s,
        "dispatch-refused",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
    );
    let r = s.submit_with_graph_effects(
        c.clone(),
        1,
        |_| unreachable!(),
        |_| {
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::SourceBusy,
                "artifact worker unavailable",
            ))
        },
    );
    assert_eq!(r.status, ReceiptStatus::Failed);
    let value = serde_json::to_value(&r).unwrap();
    assert_eq!(value["result"]["value"]["effect"], "unchanged");
    assert_eq!(value["result"]["value"]["completed_monotonic_ns"], 1);
    assert!(s.diagnostic_work().is_none());
    assert_eq!(s.submit(c), r);
    assert_eq!(
        submit(
            &mut s,
            "fresh-preview",
            json!({"method":"diagnostics.preview"}),
            2
        )
        .status,
        ReceiptStatus::Applied
    );
}

#[test]
fn saturated_read_history_keeps_capacity_for_the_first_scoped_cancellation() {
    for prior_refusals in 0..2 {
        let mut s = owner();
        let p = preview(&mut s);
        let r = submit(
            &mut s,
            "export",
            json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
            1,
        );
        assert_eq!(r.status, ReceiptStatus::Accepted);
        for n in 0..prior_refusals {
            let r = submit(
                &mut s,
                &format!("refused-{n}"),
                json!({"method":"events.get","cursor":{"owner_id":"different-owner","after_sequence":0}}),
                2,
            );
            assert_eq!(r.status, ReceiptStatus::Rejected);
        }
        let mut exhausted = false;
        for n in 0..200 {
            let r = submit(
                &mut s,
                &format!("metric-{n}"),
                json!({"method":"metrics.get"}),
                2,
            );
            if r.status == ReceiptStatus::Rejected {
                exhausted = true;
                break;
            }
        }
        assert!(exhausted);
        let r = submit(
            &mut s,
            "cancel",
            json!({"method":"diagnostics.cancel","request_id":"export"}),
            3,
        );
        assert_eq!(
            r.status,
            ReceiptStatus::Applied,
            "the admitted job must reserve cancellation metadata: {:?}",
            r.error
        );
        assert!(s.diagnostic_cancel_requested(&RequestId::new("export").unwrap()));
        let mute = command(&s, "mute", json!({"method":"mute.set","muted":true}));
        assert_eq!(s.submit(mute).status, ReceiptStatus::Applied);
    }
}
#[test]
fn malformed_worker_completion_retires_with_uncertain_effects() {
    use earph_core::diagnostics::*;
    let mut s = owner();
    let p = preview(&mut s);
    let c = command(
        &s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
    );
    assert_eq!(s.submit(c.clone()).status, ReceiptStatus::Accepted);
    let job = s.diagnostic_work().unwrap();
    // Negative domain input: no filesystem success is fabricated or claimed.
    s.complete_diagnostic_work(
        &job.request_id,
        &job.binding,
        2,
        DiagnosticWorkCompletion {
            receipt: job.pending_receipt(),
            status: ReceiptStatus::Applied,
            error: None,
        },
    )
    .unwrap();
    assert!(s.diagnostic_work().is_none());
    let replay = s.submit(c);
    assert_eq!(replay.status, ReceiptStatus::Failed);
    let Some(QueryResult::DiagnosticArtifact(result)) = replay.result else {
        panic!("typed result")
    };
    assert_eq!(result.effect, DiagnosticFilesystemEffect::InspectRequired);
}
#[test]
fn accepted_artifact_fields_are_required_nullable_and_unknown_fields_fail() {
    use earph_core::diagnostics::DiagnosticArtifactReceipt;
    let mut s = owner();
    let p = preview(&mut s);
    let accepted = submit(
        &mut s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        1,
    );
    let original = serde_json::to_value(accepted).unwrap()["result"]["value"].clone();
    DiagnosticArtifactReceipt::decode(
        &serde_json::to_vec(&original).unwrap(),
        ReceiptStatus::Accepted,
    )
    .unwrap();
    for key in [
        "stage_directory",
        "inspection",
        "manifest_sha256",
        "diagnostics_bytes",
        "manifest_bytes",
        "bundle_cut",
        "completed_monotonic_ns",
        "reason",
    ] {
        let mut bad = original.clone();
        bad.as_object_mut().unwrap().remove(key);
        assert!(
            DiagnosticArtifactReceipt::decode(
                &serde_json::to_vec(&bad).unwrap(),
                ReceiptStatus::Accepted
            )
            .is_err(),
            "{key}"
        );
    }
    let mut bad = original.clone();
    bad["private_payload"] = json!("must reject");
    assert!(
        DiagnosticArtifactReceipt::decode(
            &serde_json::to_vec(&bad).unwrap(),
            ReceiptStatus::Accepted
        )
        .is_err()
    );
    bad = original;
    bad["effect"] = json!("installed_verified");
    assert!(
        DiagnosticArtifactReceipt::decode(
            &serde_json::to_vec(&bad).unwrap(),
            ReceiptStatus::Applied
        )
        .is_err()
    );
}

fn profile_payload() -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    json!({"method":"profiles.create","id":"generated-profile","draft":{"name":"Generated profile","source_policy":"local_file","fidelity":"compatible_conversion","output":{"backend":"alsa","route":"system","device_id":"generated-device","manual_setup":null},"volume":{"mode":"software","software_cap_millidecibels":-10000,"hardware_cap_millidecibels":null},"latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]},"graph":{"graph":{"schema_version":1,"output":"out","nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}}})
}
#[test]
fn profile_and_diagnostic_work_share_one_admission_slot() {
    for profile_first in [true, false] {
        let mut s = owner();
        let p = preview(&mut s);
        let diagnostic = json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]});
        let (first, second) = if profile_first {
            (profile_payload(), diagnostic)
        } else {
            (diagnostic, profile_payload())
        };
        assert_eq!(
            submit(&mut s, "first", first, 1).status,
            ReceiptStatus::Accepted
        );
        let blocked = submit(&mut s, "second", second, 2);
        assert_eq!(
            blocked.error.unwrap().code,
            earph_core::ErrorCode::SourceBusy
        );
    }
}
#[test]
fn shutdown_stops_new_artifact_admission() {
    let mut s = owner();
    let stopped = submit(&mut s, "shutdown", json!({"method":"owner.shutdown"}), 1);
    assert_eq!(stopped.status, ReceiptStatus::Applied);
    let profile = submit(&mut s, "late-profile", profile_payload(), 2);
    assert_eq!(profile.status, ReceiptStatus::Rejected);
    let inspection = submit(
        &mut s,
        "late-inspect",
        json!({"method":"diagnostics.inspect","directory":"/owned/diagnostics","expected_sha256":"a".repeat(64)}),
        2,
    );
    assert_eq!(inspection.status, ReceiptStatus::Rejected);
}
#[test]
fn explicit_foreign_session_cannot_target_an_artifact_operation() {
    let mut s = owner();
    let p = preview(&mut s);
    submit(
        &mut s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        1,
    );
    let mut cancel = command(
        &s,
        "foreign-session",
        json!({"method":"diagnostics.cancel","request_id":"export"}),
    );
    cancel.session_id = Some(SessionId::new("foreign-session").unwrap());
    assert_eq!(
        s.submit(cancel).error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert!(!s.diagnostic_cancel_requested(&RequestId::new("export").unwrap()));
}

#[test]
fn local_artifact_decoder_requires_real_owner_and_deadline_scope() {
    use earph_core::diagnostics::DiagnosticArtifactReceipt;
    let mut s = owner();
    let p = preview(&mut s);
    let accepted = submit(
        &mut s,
        "export",
        json!({"method":"diagnostics.export","directory":"/owned/diagnostics","expected_sha256":p["sha256"]}),
        1,
    );
    let original = serde_json::to_value(accepted).unwrap()["result"]["value"].clone();
    let mut bad = original.clone();
    bad["binding"]["owner_id"] = Value::Null;
    assert!(
        DiagnosticArtifactReceipt::decode(
            &serde_json::to_vec(&bad).unwrap(),
            ReceiptStatus::Accepted
        )
        .is_err()
    );
    bad = original;
    bad["deadline_monotonic_ns"] = json!(0);
    assert!(
        DiagnosticArtifactReceipt::decode(
            &serde_json::to_vec(&bad).unwrap(),
            ReceiptStatus::Accepted
        )
        .is_err()
    );
}
