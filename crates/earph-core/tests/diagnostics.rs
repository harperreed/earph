// ABOUTME: Exercises default-redacted diagnostic reads through real Session command receipts.
// ABOUTME: Keeps missing instrumentation explicit and checks immutable query replay.
use earph_core::{control::*, ids::*, session::Session};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("private-zone-token").unwrap(),
        OwnerId::new("/private/owner-secret").unwrap(),
        ControlLimits::default(),
    )
}
fn command(id: &str, method: &str) -> CommandEnvelope {
    decode_command(
        &serde_json::to_vec(&json!({
            "schema_version":1,"request_id":id,"zone_id":"private-zone-token",
            "owner_id":null,"session_id":null,"expected_revision":null,
            "payload":{"method":method}
        }))
        .unwrap(),
        &ControlLimits::default(),
    )
    .unwrap()
}
fn result(session: &mut Session, id: &str, method: &str) -> Value {
    let receipt = session.submit(command(id, method));
    assert_eq!(
        receipt.status,
        ReceiptStatus::Applied,
        "{method}: {:?}",
        receipt.error
    );
    serde_json::to_value(receipt).unwrap()["result"].clone()
}

#[test]
fn idle_signal_path_is_owner_bound_but_shareable_content_has_no_private_ids() {
    let mut session = owner();
    let reply = result(&mut session, "path", "signal_path.get");
    assert_eq!(reply["kind"], "signal_path");
    let value = &reply["value"];
    assert_eq!(value["binding"]["owner_id"], "/private/owner-secret");
    assert_eq!(value["binding"]["revision"], 0);
    let shareable = &value["report"];
    assert_eq!(shareable["schema_version"], 1);
    assert_eq!(shareable["requested"]["software_gain_db"], -40.0);
    assert_eq!(shareable["requested"]["muted"], true);
    assert!(shareable["observed"].is_null());
    assert!(shareable["resolved"].is_null());
    assert_eq!(
        shareable["unknown_segments"],
        json!([
            "upstream_processing",
            "output_route_internals",
            "dac_internals"
        ])
    );
    assert!(!shareable.to_string().contains("private"));
    assert_eq!(session.snapshot().revision, 0);
    assert!(session.snapshot().muted);
}

#[test]
fn unavailable_metrics_are_null_with_reasons_and_query_replay_is_exact() {
    let mut session = owner();
    let first = session.submit(command("metrics", "metrics.get"));
    assert_eq!(first.status, ReceiptStatus::Applied);
    let value = serde_json::to_value(&first).unwrap();
    assert_eq!(value["result"]["kind"], "metrics");
    for field in [
        "underruns",
        "deadline_misses",
        "cpu_load",
        "queue_depth_frames",
    ] {
        let metric = &value["result"]["value"]["report"][field];
        assert!(metric["value"].is_null(), "{field}");
        assert_eq!(metric["unavailable_reason"], "unsupported");
    }
    assert_eq!(session.submit(command("metrics", "metrics.get")), first);
    assert_eq!(session.snapshot().revision, 0);
}

#[test]
fn shared_decoders_reject_missing_nulls_unknown_fields_and_cut_mismatch() {
    use earph_core::diagnostics::{MetricsRead, SignalPathRead};
    let mut session = owner();
    let path = result(&mut session, "validate-path", "signal_path.get")["value"].clone();
    SignalPathRead::decode(&serde_json::to_vec(&path).unwrap()).unwrap();
    for key in ["resolved", "observed"] {
        let mut bad = path.clone();
        bad["report"].as_object_mut().unwrap().remove(key);
        assert!(SignalPathRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = path.clone();
    bad["report"]["cut"]["revision"] = json!(9);
    assert!(SignalPathRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = path;
    bad["report"]["requested"]["endpoint"] = json!("/private/path");
    assert!(SignalPathRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    let metrics = result(&mut session, "validate-metrics", "metrics.get")["value"].clone();
    MetricsRead::decode(&serde_json::to_vec(&metrics).unwrap()).unwrap();
    for key in ["value", "unavailable_reason"] {
        let mut bad = metrics.clone();
        bad["report"]["cpu_load"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(MetricsRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}

#[test]
fn decoder_cannot_upgrade_missing_instrumentation_to_observed_zero() {
    use earph_core::diagnostics::MetricsRead;
    let mut session = owner();
    let metrics = result(&mut session, "false-zero", "metrics.get")["value"].clone();
    for field in [
        "underruns",
        "deadline_misses",
        "cpu_load",
        "queue_depth_frames",
    ] {
        let mut bad = metrics.clone();
        bad["report"][field]["value"] = json!(0);
        bad["report"][field]["unavailable_reason"] = Value::Null;
        assert!(
            MetricsRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "{field}"
        );
    }
}

#[test]
fn advertised_read_bounds_match_actual_method_support() {
    let mut session = owner();
    let capability = result(&mut session, "capabilities", "capabilities.get");
    assert_eq!(capability["value"]["diagnostics"]["schema_version"], 1);
    assert_eq!(capability["value"]["diagnostics"]["max_read_bytes"], 65536);
    assert_eq!(
        capability["value"]["diagnostics"]["methods"],
        json!(["signal_path.get", "metrics.get"])
    );
}

#[test]
fn typed_diagnostic_request_cannot_retain_oversized_identity_capacity() {
    let mut session = owner();
    let mut request = command("sized", "metrics.get");
    let mut id = String::with_capacity(129);
    id.push_str("large");
    request.request_id = RequestId::new(id).unwrap();
    assert_eq!(
        session.submit(request).error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn bound_diagnostic_reads_reject_replacement_owner_without_retargeting() {
    let mut session = owner();
    let mut request = command("bound-path", "signal_path.get");
    request.owner_id = Some(OwnerId::new("previous-owner").unwrap());
    let receipt = session.submit(request);
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(session.snapshot().revision, 0);
}

#[test]
fn diagnostic_decoder_rejects_scope_forgery_and_unbounded_spare_capacity() {
    use earph_core::diagnostics::{MetricScope, MetricsRead, SignalPathRead};
    let mut session = owner();
    let metrics = result(&mut session, "metric-scope", "metrics.get")["value"].clone();
    let mut read = MetricsRead::decode(&serde_json::to_vec(&metrics).unwrap()).unwrap();
    read.report.input_peak.scope = MetricScope::GraphProcessingSinceReset;
    assert!(read.validate().is_err());
    let path = result(&mut session, "path-capacity", "signal_path.get")["value"].clone();
    let mut read = SignalPathRead::decode(&serde_json::to_vec(&path).unwrap()).unwrap();
    read.report.unknown_segments.reserve(1024);
    assert!(read.validate().is_err());
}

#[test]
fn restored_authored_graph_and_private_output_are_requested_only() {
    let mut checkpoint = owner().checkpoint();
    checkpoint.output = Some(serde_json::from_value(json!({
        "backend":"alsa","route":"system","endpoint":"/private/endpoint-secret",
        "device_id":"private-device","profile_id":"private-profile",
        "period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"
    })).unwrap());
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    checkpoint.graph_intent = Some(std::sync::Arc::new(
        serde_json::from_value(json!({
            "graph":{"schema_version":1,"output":"private-out","nodes":[
                {"id":"private-in","kind":{"kind":"input"},"format":port},
                {"id":"private-out","kind":{"kind":"output"},"format":port}],
                "edges":[{"from":"private-in","to":"private-out","from_port":0,"to_port":0}]},
            "processing":{"kind":"integer_bypass"}
        }))
        .unwrap(),
    ));
    let mut restored = Session::restore_owner(
        ZoneId::new("private-zone-token").unwrap(),
        OwnerId::new("/private/replacement").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    let value = result(&mut restored, "restored-path", "signal_path.get");
    let report = &value["value"]["report"];
    assert_eq!(report["requested"]["graph_configured"], true);
    assert_eq!(report["requested"]["output"]["backend"], "alsa");
    assert!(report["resolved"].is_null());
    assert!(report["observed"].is_null());
    assert!(!report.to_string().contains("private"));
    assert!(restored.snapshot().muted);
}

#[test]
fn diagnostic_hardware_caps_reuse_the_full_volume_policy_domain() {
    use earph_core::diagnostics::SignalPathRead;
    for millidecibels in [-200000, -180000, 6000, 24000] {
        let mut session = owner();
        let state = session.snapshot();
        let mut set_caps = command("caps", "signal_path.get");
        set_caps.owner_id = state.owner_id;
        set_caps.expected_revision = Some(state.revision);
        set_caps.payload = Command::VolumeCaps {
            software_millidecibels: 0,
            hardware_millidecibels: Some(millidecibels),
        };
        assert_eq!(session.submit(set_caps).status, ReceiptStatus::Applied);
        let reply = result(&mut session, "valid-cap-read", "signal_path.get");
        let read = SignalPathRead::decode(&serde_json::to_vec(&reply["value"]).unwrap()).unwrap();
        assert_eq!(
            read.report.requested.hardware_cap_db,
            Some(f64::from(millidecibels) / 1000.0)
        );
        assert!(session.snapshot().muted);
        let mut restored = Session::restore_owner(
            ZoneId::new("private-zone-token").unwrap(),
            OwnerId::new("replacement-owner").unwrap(),
            ControlLimits::default(),
            session.checkpoint(),
        )
        .unwrap();
        let restored = result(&mut restored, "restored-cap-read", "signal_path.get");
        assert_eq!(
            restored["value"]["report"]["requested"]["hardware_cap_db"],
            reply["value"]["report"]["requested"]["hardware_cap_db"]
        );
    }
}
