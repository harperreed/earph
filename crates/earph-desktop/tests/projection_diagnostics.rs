// ABOUTME: Decodes real Session diagnostic replies through the shared desktop projection.
// ABOUTME: Rejects missing fields, altered local bindings, and invented instrumentation.
use earph_core::{control::*, ids::*, session::Session};
use earph_desktop::{Reply, decode_reply};
use serde_json::{Value, json};

fn reply(method: &str) -> Vec<u8> {
    let mut owner = Session::new_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("private-owner").unwrap(),
        ControlLimits::default(),
    );
    let bytes = serde_json::to_vec(
        &json!({"schema_version":1,"request_id":"read","zone_id":"private-zone",
        "owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":method}}),
    )
    .unwrap();
    let receipt = owner.submit(decode_command(&bytes, &ControlLimits::default()).unwrap());
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    serde_json::to_vec(&json!({"receipt":receipt})).unwrap()
}

#[test]
fn real_diagnostic_reads_project_without_rewriting_reply_bytes() {
    for method in ["signal_path.get", "metrics.get"] {
        let original = reply(method);
        let decoded = decode_reply(original.clone()).expect("real diagnostic owner reply");
        assert_eq!(decoded.bytes(), original);
        let Reply::Receipt(receipt) = decoded else {
            panic!("diagnostic receipt")
        };
        assert!(receipt.result.is_some());
        let value: Value = serde_json::from_slice(&original).unwrap();
        assert!(
            !value["receipt"]["result"]["value"]["report"]
                .to_string()
                .contains("private")
        );
    }
}

#[test]
fn diagnostic_projection_rejects_forged_binding_missing_nulls_and_false_metrics() {
    for method in ["signal_path.get", "metrics.get"] {
        let original: Value = serde_json::from_slice(&reply(method)).unwrap();
        let mut forged = original.clone();
        forged["receipt"]["result"]["value"]["binding"]["revision"] = json!(99);
        assert!(decode_reply(serde_json::to_vec(&forged).unwrap()).is_err());
        let mut forged = original.clone();
        forged["receipt"]["result"]["value"]["binding"]["zone_id"] = json!("other-zone");
        assert!(decode_reply(serde_json::to_vec(&forged).unwrap()).is_err());
        let mut forged = original;
        forged["receipt"]["status"] = json!("failed");
        assert!(decode_reply(serde_json::to_vec(&forged).unwrap()).is_err());
    }
    let mut missing: Value = serde_json::from_slice(&reply("signal_path.get")).unwrap();
    missing["receipt"]["result"]["value"]["report"]
        .as_object_mut()
        .unwrap()
        .remove("resolved");
    assert!(decode_reply(serde_json::to_vec(&missing).unwrap()).is_err());
    let mut invented: Value = serde_json::from_slice(&reply("metrics.get")).unwrap();
    invented["receipt"]["result"]["value"]["report"]["underruns"]["value"] = json!(0);
    invented["receipt"]["result"]["value"]["report"]["underruns"]["unavailable_reason"] =
        Value::Null;
    assert!(decode_reply(serde_json::to_vec(&invented).unwrap()).is_err());
}

#[path = "../../earph-core/tests/support/diagnostic_session.rs"]
mod diagnostic_session;

fn active_contract_reply() -> Vec<u8> {
    use earph_core::{audio::*, graph::PreservationDecision, graph_control::*};
    use std::sync::Arc;
    let mut session = diagnostic_session::preparing("/private/decoder-route", None);
    let job = session.graph_job().unwrap();
    let snapshot = session.snapshot();
    let source = snapshot.observation.pcm().unwrap().format;
    let processing =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    // This is a Core preparation-contract input, not compiler or native qualification.
    // The companion GraphService regression supplies the actual compiled producer proof.
    let preview = Arc::new(GraphPreview {
        diagnostic_nodes: None,
        profile_application: None,
        backend: snapshot.observation.pcm().unwrap().backend.clone(),
        graph_hash: "a".repeat(64),
        kernel_hash: "b".repeat(64),
        implementation_version: "controlled Core contract".into(),
        binding: job.binding,
        source_format: source,
        native_output_format: source,
        processing_input_format: processing,
        processing_output_format: processing,
        quantum_frames: 64,
        timing: GraphTiming::SameRate {
            latency_frames: 0,
            tail_frames: 0,
            tail_policy: GraphTailPolicy::AuthoredFiniteSupport,
            terminal_output_bounds: vec![0.0; 2],
            node_tails: vec![],
        },
        preservation: PreservationDecision::ProcessFloat64,
        transformations: vec![],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0; 2],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    });
    preview.validate().unwrap();
    let receipt = diagnostic_session::activate(&mut session, preview);
    serde_json::to_vec(&json!({"receipt":receipt})).unwrap()
}

#[test]
fn diagnostic_review_desktop_decoder_preserves_active_session_outer_formats() {
    let bytes = active_contract_reply();
    decode_reply(bytes.clone()).unwrap();
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    for (field, parameter, replacement) in [
        ("source_format", "sample_rate_hz", json!(96000)),
        ("source_format", "channels", json!("mono")),
        ("native_output_format", "sample_rate_hz", json!(96000)),
        ("native_output_format", "channels", json!("mono")),
    ] {
        let mut altered = original.clone();
        altered["receipt"]["result"]["value"]["report"]["resolved"][field][parameter] = replacement;
        assert!(
            decode_reply(serde_json::to_vec(&altered).unwrap()).is_err(),
            "decoder accepted {field}.{parameter}"
        );
    }
}

#[test]
fn actual_history_and_preview_use_the_shared_strict_projection() {
    for method in ["events.get", "diagnostics.preview"] {
        let mut owner = Session::new_owner(
            ZoneId::new("private-zone").unwrap(),
            OwnerId::new("private-owner").unwrap(),
            ControlLimits::default(),
        );
        let mut payload = json!({"method":method});
        if method == "events.get" {
            payload["cursor"] = Value::Null;
        }
        let command=decode_command(&serde_json::to_vec(&json!({"schema_version":1,"request_id":"new-read","zone_id":"private-zone","owner_id":"private-owner","session_id":null,"expected_revision":null,"payload":payload})).unwrap(),&ControlLimits::default()).unwrap();
        let r = owner.submit(command);
        assert_eq!(r.status, ReceiptStatus::Applied);
        let original = serde_json::to_vec(&json!({"receipt":r})).unwrap();
        let decoded =
            decode_reply(original.clone()).expect("actual Core diagnostic read must decode");
        assert_eq!(decoded.bytes(), original);
        let mut altered: Value = serde_json::from_slice(&original).unwrap();
        altered["receipt"]["result"]["value"]["binding"]["epochs"]["stream"] = json!(9);
        assert!(decode_reply(serde_json::to_vec(&altered).unwrap()).is_err());
        altered = serde_json::from_slice(&original).unwrap();
        altered["receipt"]["result"]["value"]["report"]["extra"] = json!("secret");
        assert!(decode_reply(serde_json::to_vec(&altered).unwrap()).is_err());
        if method == "diagnostics.preview" {
            altered = serde_json::from_slice(&original).unwrap();
            let report = &mut altered["receipt"]["result"]["value"]["report"];
            report["bytes"] = json!(format!("{} ", report["bytes"].as_str().unwrap()));
            assert!(decode_reply(serde_json::to_vec(&altered).unwrap()).is_err());
        }
    }
}

#[test]
fn bound_request_builder_accepts_only_the_new_diagnostic_queries() {
    use earph_desktop::{Request, ResultView};
    let mut owner = Session::new_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("private-owner").unwrap(),
        ControlLimits::default(),
    );
    let r = owner.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("state").unwrap(),
        zone_id: ZoneId::new("private-zone").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::State,
    });
    let Reply::Receipt(r) =
        decode_reply(serde_json::to_vec(&json!({"receipt":r})).unwrap()).unwrap()
    else {
        panic!("receipt")
    };
    let Some(ResultView::State(state)) = r.result else {
        panic!("state")
    };
    for payload in [
        Command::EventsGet { cursor: None },
        Command::DiagnosticsPreview {},
    ] {
        let request =
            Request::diagnostic_query(RequestId::new("query").unwrap(), &state, payload.clone())
                .expect("bound diagnostic request");
        assert_eq!(request.envelope().owner_id, state.owner_id);
        assert_eq!(request.envelope().payload, payload);
    }
}
