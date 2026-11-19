// ABOUTME: Projects real Core artifact admission and cancellation without claiming filesystem effects.
// ABOUTME: Rejects forged status, capture bindings and missing nullable fields at the shared client boundary.
use earph_core::{control::*, diagnostics::*, ids::*, session::Session};
use earph_desktop::decode_reply;
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("diagnostic-zone").unwrap(),
        OwnerId::new("diagnostic-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn command(owner: &Session, id: &str, payload: Command) -> CommandEnvelope {
    let state = owner.snapshot();
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(id).unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: None,
        expected_revision: Some(state.revision),
        payload,
    }
}
fn preview(owner: &mut Session) -> earph_core::profiles::ProfileObjectHash {
    let request = command(owner, "preview", Command::DiagnosticsPreview {});
    let Some(QueryResult::DiagnosticPreview(preview)) = owner.submit(request).result else {
        panic!("actual Core preview")
    };
    preview.report.sha256
}
fn bytes(receipt: &Receipt) -> Vec<u8> {
    serde_json::to_vec(&json!({"receipt": receipt})).unwrap()
}
fn pending(inspect: bool) -> (Session, Receipt) {
    let mut owner = owner();
    let hash = preview(&mut owner);
    let payload = if inspect {
        Command::DiagnosticsInspect {
            directory: "/owned/evidence".into(),
            expected_sha256: hash,
        }
    } else {
        Command::DiagnosticsExport {
            directory: "/owned/evidence".into(),
            expected_sha256: hash,
        }
    };
    let request = command(&owner, "artifact", payload);
    let receipt = owner.submit(request);
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    (owner, receipt)
}

#[test]
fn actual_pending_artifacts_and_cancellation_preserve_exact_core_reply_bytes() {
    for inspect in [false, true] {
        let (mut owner, receipt) = pending(inspect);
        let original = bytes(&receipt);
        assert_eq!(
            decode_reply(original.clone())
                .expect("actual accepted artifact")
                .bytes(),
            original
        );
        let request = command(
            &owner,
            "cancel",
            Command::DiagnosticsCancel {
                request_id: RequestId::new("artifact").unwrap(),
            },
        );
        let receipt = owner.submit(request);
        assert_eq!(receipt.status, ReceiptStatus::Applied);
        assert!(
            owner.diagnostic_work().is_some(),
            "cancellation acknowledgment must not retire work"
        );
        let original = bytes(&receipt);
        assert_eq!(
            decode_reply(original.clone())
                .expect("actual cancellation acknowledgment")
                .bytes(),
            original
        );
    }
}

#[test]
fn actual_dispatch_refusal_retains_a_typed_unchanged_effect() {
    let mut owner = owner();
    let hash = preview(&mut owner);
    let request = command(
        &owner,
        "refused",
        Command::DiagnosticsExport {
            directory: "/owned/evidence".into(),
            expected_sha256: hash,
        },
    );
    let receipt = owner.submit_with_graph_effects(
        request,
        1,
        |_| unreachable!(),
        |_| {
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::SourceBusy,
                "worker unavailable",
            ))
        },
    );
    assert_eq!(receipt.status, ReceiptStatus::Failed);
    let Some(QueryResult::DiagnosticArtifact(result)) = &receipt.result else {
        panic!("typed refusal")
    };
    assert_eq!(result.effect, DiagnosticFilesystemEffect::Unchanged);
    let original = bytes(&receipt);
    assert_eq!(
        decode_reply(original.clone())
            .expect("actual pre-dispatch refusal")
            .bytes(),
        original
    );
}

#[test]
fn artifact_projection_rejects_forged_status_binding_effects_and_missing_fields() {
    let (_, receipt) = pending(false);
    let original: Value = serde_json::from_slice(&bytes(&receipt)).unwrap();
    for (field, value) in [
        ("status", json!("applied")),
        ("request_id", json!("another-request")),
        ("zone_id", json!("another-zone")),
        ("revision", json!(99)),
        ("epochs", json!({"session":99,"stream":99})),
    ] {
        let mut altered = original.clone();
        altered["receipt"][field] = value;
        assert!(
            decode_reply(serde_json::to_vec(&altered).unwrap()).is_err(),
            "forged {field}"
        );
    }
    for field in [
        "stage_directory",
        "inspection",
        "completed_monotonic_ns",
        "reason",
        "manifest_sha256",
    ] {
        let mut altered = original.clone();
        altered["receipt"]["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            decode_reply(serde_json::to_vec(&altered).unwrap()).is_err(),
            "missing {field}"
        );
    }
    let mut altered = original;
    altered["receipt"]["result"]["value"]["effect"] = json!("installed_verified");
    assert!(decode_reply(serde_json::to_vec(&altered).unwrap()).is_err());
}

#[test]
fn artifact_requests_scope_owner_and_capture_revision_only_for_export() {
    use earph_desktop::{Reply, Request, RequestError};
    let mut owner = owner();
    let initial = command(&owner, "state", Command::State);
    let Reply::Receipt(initial) = decode_reply(bytes(&owner.submit(initial))).unwrap() else {
        panic!("state")
    };
    let state = initial.state().unwrap();
    let hash = preview(&mut owner);
    for payload in [
        Command::DiagnosticsExport {
            directory: "/owned/evidence".into(),
            expected_sha256: hash,
        },
        Command::DiagnosticsInspect {
            directory: "/owned/evidence".into(),
            expected_sha256: hash,
        },
        Command::DiagnosticsCancel {
            request_id: RequestId::new("artifact").unwrap(),
        },
    ] {
        let export = matches!(payload, Command::DiagnosticsExport { .. });
        let request = Request::diagnostic_artifact(
            RequestId::new("request").unwrap(),
            state,
            payload.clone(),
        )
        .unwrap();
        let envelope = decode_command(request.bytes(), &ControlLimits::default()).unwrap();
        assert_eq!(envelope.owner_id, state.owner_id);
        assert_eq!(envelope.zone_id, state.zone_id);
        assert_eq!(envelope.session_id, None);
        assert_eq!(envelope.expected_revision, export.then_some(state.revision));
        assert_eq!(envelope.payload, payload);
    }
    assert_eq!(
        Request::diagnostic_artifact(
            RequestId::new("wrong").unwrap(),
            state,
            Command::Capabilities
        )
        .unwrap_err(),
        RequestError::NotDiagnosticArtifact
    );
    let mut missing = state.clone();
    missing.owner_id = None;
    assert_eq!(
        Request::diagnostic_artifact(
            RequestId::new("missing").unwrap(),
            &missing,
            Command::DiagnosticsCancel {
                request_id: RequestId::new("artifact").unwrap()
            }
        )
        .unwrap_err(),
        RequestError::MissingOwner
    );
}
