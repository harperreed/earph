// ABOUTME: Exercises owner device queries through shared command and receipt contracts.
// ABOUTME: Keeps discovery independent from output authority and transport state.
use earph_core::{EngineError, ErrorCode, devices::*};
use earph_core::{control::*, ids::*, session::Session};
use serde_json::json;

fn session() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("device-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn query(method: &str) -> CommandEnvelope {
    let mut payload = json!({"method":method,"backend":"alsa"});
    if method == "devices.inspect" {
        payload["endpoint"] = json!("null");
    }
    let bytes = serde_json::to_vec(&json!({"schema_version":1,"request_id":"discovery","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":payload})).unwrap();
    decode_command(&bytes, &ControlLimits::default()).expect("canonical device query must decode")
}
#[test]
fn embedded_domain_without_helper_support_reports_discovery_unavailable() {
    for method in ["devices.list", "devices.inspect"] {
        let mut session = session();
        let before = session.snapshot();
        let receipt = session.submit(query(method));
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(
            receipt.error.unwrap().code,
            earph_core::ErrorCode::CapabilityUnsupported
        );
        assert_eq!(session.snapshot(), before);
    }
}

#[test]
fn discovery_replay_pins_pending_receipts_and_never_changes_playback_state() {
    let mut session = session();
    session.enable_device_discovery(OutputBackend::Alsa);
    let before = session.snapshot();
    let command = query("devices.list");
    let accepted = session.submit(command.clone());
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    let job = session.device_query_job().unwrap();
    assert_eq!(session.submit(command.clone()), accepted);
    let mut collision = command.clone();
    collision.payload = query("devices.inspect").payload;
    assert_eq!(
        session.submit(collision).error.unwrap().code,
        ErrorCode::AssetInvalid
    );
    let mut busy = command.clone();
    busy.request_id = RequestId::new("busy").unwrap();
    assert_eq!(
        session.submit(busy).error.unwrap().code,
        ErrorCode::SourceBusy
    );
    for index in 0..1030 {
        let mut state = command.clone();
        state.request_id = RequestId::new(format!("state-{index}")).unwrap();
        state.payload = Command::State;
        assert_eq!(session.submit(state).status, ReceiptStatus::Applied);
    }
    assert_eq!(session.submit(command.clone()), accepted);
    let mut stale = job.clone();
    stale.owner_id = OwnerId::new("another-owner").unwrap();
    assert_eq!(
        session
            .fail_device_query(&stale, 1, DEVICE_INVALID)
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    session
        .complete_device_query(
            &job,
            42,
            DeviceObservation::List {
                devices: vec![],
                complete: true,
                failures: vec![],
            },
        )
        .unwrap();
    let applied = session.submit(command.clone());
    assert_eq!(applied.status, ReceiptStatus::Applied);
    assert_eq!(session.submit(command), applied);
    let wire = serde_json::to_value(applied).unwrap();
    assert_eq!(wire["result"]["kind"], "devices");
    assert_eq!(wire["result"]["value"]["read_boundary"], "alsa_pcm_hints");
    assert_eq!(wire["result"]["value"]["completed_monotonic_ns"], 42);
    assert_eq!(session.snapshot(), before);
    assert!(session.device_query_job().is_none());
}

#[test]
fn result_admission_rejects_unknown_cleanup_and_rotates_bounded_results() {
    let mut session = session();
    session.enable_device_discovery(OutputBackend::Alsa);
    let first = query("devices.list");
    for index in 0..20 {
        let mut command = first.clone();
        command.request_id = RequestId::new(format!("result-{index}")).unwrap();
        assert_eq!(session.submit(command).status, ReceiptStatus::Accepted);
        let job = session.device_query_job().unwrap();
        assert_eq!(
            session
                .complete_device_query(
                    &job,
                    1,
                    DeviceObservation::Failed {
                        error: DEVICE_INVALID.into(),
                        cleanup_confirmed: false
                    }
                )
                .unwrap_err()
                .code,
            ErrorCode::AssetInvalid
        );
        let devices = (0..40)
            .map(|n| {
                DeviceSummary::advertised(
                    OutputBackend::Alsa,
                    format!("pcm-{n:03}"),
                    None,
                    Some("x".repeat(4000)),
                    None,
                )
            })
            .collect();
        session
            .complete_device_query(
                &job,
                2,
                DeviceObservation::List {
                    devices,
                    complete: true,
                    failures: vec![],
                },
            )
            .unwrap();
    }
    let mut oldest = first.clone();
    oldest.request_id = RequestId::new("result-0").unwrap();
    assert_eq!(
        session.submit(oldest).status,
        ReceiptStatus::Accepted,
        "old terminal data exceeded the result budget instead of being evicted"
    );
    let job = session.device_query_job().unwrap();
    session
        .fail_device_query(
            &job,
            3,
            EngineError::new(ErrorCode::OperationCancelled, "discovery stopped"),
        )
        .unwrap();
    let mut latest = first;
    latest.request_id = RequestId::new("result-19").unwrap();
    assert_eq!(session.submit(latest).status, ReceiptStatus::Applied);
}

#[test]
fn malformed_or_oversize_results_cannot_settle_a_query() {
    let mut session = session();
    session.enable_device_discovery(OutputBackend::Alsa);
    let command = query("devices.list");
    session.submit(command.clone());
    let job = session.device_query_job().unwrap();
    let devices = (0..257)
        .map(|index| {
            DeviceSummary::advertised(
                OutputBackend::Alsa,
                format!("pcm-{index:03}"),
                None,
                None,
                None,
            )
        })
        .collect();
    assert_eq!(
        session
            .complete_device_query(
                &job,
                1,
                DeviceObservation::List {
                    devices,
                    complete: true,
                    failures: vec![]
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let huge = DeviceSummary::advertised(
        OutputBackend::Alsa,
        "null".into(),
        None,
        Some("x".repeat(4097)),
        None,
    );
    assert_eq!(
        session
            .complete_device_query(
                &job,
                1,
                DeviceObservation::List {
                    devices: vec![huge],
                    complete: true,
                    failures: vec![]
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let duplicate = DeviceSummary::advertised(OutputBackend::Alsa, "null".into(), None, None, None);
    assert_eq!(
        session
            .complete_device_query(
                &job,
                1,
                DeviceObservation::List {
                    devices: vec![duplicate.clone(), duplicate],
                    complete: true,
                    failures: vec![]
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        session.submit(command.clone()).status,
        ReceiptStatus::Accepted
    );
    session.fail_device_query(&job, 2, DEVICE_BUDGET).unwrap();
    let failed = session.submit(command.clone());
    assert_eq!(failed.status, ReceiptStatus::Failed);
    assert_eq!(session.submit(command), failed);
}
