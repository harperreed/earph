// ABOUTME: Parses actual Core network observations and preserves absent duration facts.
// ABOUTME: Rejects malformed network records without imposing local queue authority.
use earph_core::{audio::*, control::*, ids::*, network_source::*, session::Session};
use earph_desktop::{Reply, decode_reply};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("network-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn state_reply(total_frames: Option<u64>) -> Vec<u8> {
    let mut owner = owner();
    let mut state = owner.snapshot();
    state.source = Some(SourceKind::HttpFlac);
    state.session_id = Some(SessionId::new("http-session").unwrap());
    state.epochs = Epochs {
        session: 3,
        stream: 5,
    };
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    state.observation = (NetworkPlaybackObservation {
        binding: NetworkSourceBinding {
            owner_id: state.owner_id.clone().unwrap(),
            session_id: state.session_id.clone().unwrap(),
            media_id: StagedMediaId::new("staged-1").unwrap(),
            source_generation: 1,
            grant_generation: 1,
        },
        source_epochs: Epochs {
            session: 3,
            stream: 2,
        },
        output_epochs: state.epochs,
        graph_epoch: state.graph_epoch,
        device_epoch: state.device_epoch,
        lifecycle: NetworkLifecycle::Ready,
        format,
        output_format: PcmFormat::new(96000, ChannelLayout::Stereo, PcmRepresentation::Signed16)
            .unwrap(),
        total_frames,
        source_position_frames: 23,
        audible_source_position_frames: Some(7),
        submitted_frames: 47,
        consumed_frames: Some(31),
        backend_delay_frames: None,
        consumption_basis: "backend_consumed",
        input_peak: None,
        output_peak: Some(0.5),
        modified: true,
        backend: None,
        output_activity: None,
        processing: None,
        replacement_boundary: None,
    })
    .into();
    let receipt = owner.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("state").unwrap(),
        zone_id: state.zone_id.clone(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::State,
    });
    let mut reply = json!({"receipt": receipt});
    reply["receipt"]["epochs"] = json!(state.epochs);
    reply["receipt"]["result"]["value"] = serde_json::to_value(state).unwrap();
    serde_json::to_vec(&reply).unwrap()
}
#[test]
fn core_network_state_keeps_nullable_totals_and_distinct_source_output_epochs() {
    for total in [None, Some(100)] {
        let bytes = state_reply(total);
        let Reply::Receipt(receipt) =
            decode_reply(bytes.clone()).expect("Core network state must project")
        else {
            panic!("receipt")
        };
        assert_eq!(decode_reply(bytes.clone()).unwrap().bytes(), bytes);
        let state = receipt.state().unwrap();
        assert!(state.playback.is_none());
        assert_eq!(state.network_playback.as_ref().unwrap().total_frames, total);
        assert_ne!(
            state.network_playback.as_ref().unwrap().source_epochs,
            state.network_playback.as_ref().unwrap().output_epochs
        );
        let observed = serde_json::from_slice::<Value>(&bytes).unwrap();
        assert_eq!(
            observed["receipt"]["result"]["value"]["network_playback"]["total_frames"],
            json!(total)
        );
    }
}
#[test]
fn network_projection_requires_nullable_fields_and_rejects_unknown_fields() {
    let reply: Value = serde_json::from_slice(&state_reply(None)).unwrap();
    for field in [
        "total_frames",
        "consumed_frames",
        "backend",
        "processing",
        "output_activity",
        "audible_source_position_frames",
    ] {
        let mut bad = reply.clone();
        bad["receipt"]["result"]["value"]["network_playback"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("local_entry_id", json!("fake-entry")),
        ("total_frames", json!("unknown")),
        ("lifecycle", json!("magic")),
    ] {
        let mut bad = reply.clone();
        bad["receipt"]["result"]["value"]["network_playback"][field] = value;
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "bad {field}"
        );
    }
}
#[test]
fn real_core_network_query_is_a_typed_result_with_required_nulls() {
    let mut owner = owner();
    let receipt = owner.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("network").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::NetworkGet {},
    });
    let bytes = serde_json::to_vec(&json!({"receipt":receipt})).unwrap();
    decode_reply(bytes.clone()).expect("Core network query must project");
    for field in ["active", "closing", "lifecycle", "expires_monotonic_ns"] {
        let mut bad: Value = serde_json::from_slice(&bytes).unwrap();
        bad["receipt"]["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "missing {field}"
        );
    }
}

#[path = "network_fixture/mod.rs"]
mod network_fixture;
#[test]
fn real_core_admission_and_source_event_project_without_local_queue_facts() {
    let mut session = network_fixture::ready(None);
    let c = network_fixture::command(&session, "observe", json!({"method":"state.get"}));
    let receipt = session.submit(c);
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    let Reply::Receipt(receipt) =
        decode_reply(serde_json::to_vec(&json!({"receipt":receipt})).unwrap()).unwrap()
    else {
        panic!("receipt")
    };
    let state = receipt.state().unwrap();
    assert!(state.playback.is_none());
    assert!(state.active_entry_id.is_none());
    assert!(!state.seekable);
    let observed = state.network_playback.as_ref().unwrap();
    assert_eq!(observed.total_frames, None);
    assert_eq!(observed.format.sample_rate_hz(), 48000);
    assert_eq!(observed.output_format.sample_rate_hz(), 96000);
    assert_eq!(session.queue_snapshot().entries.len(), 1);
}
#[test]
fn network_request_start_captures_fresh_session_and_revoke_uses_safety_lane() {
    use earph_desktop::{Lane, Request};
    let session = network_fixture::ready(None);
    let state: earph_desktop::StateView =
        serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap();
    let fresh = SessionId::new("fresh-http-session").unwrap();
    let request = Request::start(
        RequestId::new("next-http").unwrap(),
        &state,
        fresh.clone(),
        Command::NetworkStartHttpFlac {
            media_id: StagedMediaId::new("staged-1").unwrap(),
            staging_generation: 1,
            grant_generation: 1,
            device_id: DeviceId::new("dac").unwrap(),
            takeover: true,
        },
    )
    .expect("network start must capture caller's new identity");
    assert_eq!(request.envelope().session_id, Some(fresh));
    assert_eq!(request.envelope().expected_revision, Some(state.revision));
    assert_eq!(request.lane(), Lane::Ordinary);
    assert_ne!(request.envelope().session_id, state.session_id);
    let revoke = Request::mutation(
        RequestId::new("revoke").unwrap(),
        &state,
        Command::NetworkGrantsRevoke {
            expected_generation: 1,
        },
    )
    .unwrap();
    assert_eq!(revoke.lane(), Lane::Safety);
}

#[test]
fn final_and_closing_core_network_facts_survive_live_grant_revocation() {
    let mut session = network_fixture::ready(None);
    let c = network_fixture::command(
        &session,
        "revoke",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
    );
    assert_eq!(
        session
            .submit_with_network_effects(c, 40, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    assert!(session.network_grants().is_none());
    for finish in [false, true] {
        if finish {
            let closing = session.closing_network_source().unwrap();
            session
                .complete_network_retirement(
                    NetworkRetirementAck {
                        binding: closing.context.binding.clone(),
                        source_epochs: closing.context.source_epochs,
                        output_epochs: closing.output_epochs,
                        graph_epoch: closing.graph_epoch,
                        device_epoch: closing.device_epoch,
                        observation: None,
                    },
                    50,
                )
                .unwrap();
        }
        let c = network_fixture::command(
            &session,
            if finish {
                "final-state"
            } else {
                "closing-state"
            },
            json!({"method":"state.get"}),
        );
        let receipt = session.submit(c);
        let Reply::Receipt(receipt) =
            decode_reply(serde_json::to_vec(&json!({"receipt":receipt})).unwrap()).unwrap()
        else {
            panic!("receipt")
        };
        assert!(receipt.state().unwrap().network_playback.is_some());
        let c = network_fixture::command(
            &session,
            if finish {
                "final-network"
            } else {
                "closing-network"
            },
            json!({"method":"sources.network.get"}),
        );
        let receipt = session.submit(c);
        decode_reply(serde_json::to_vec(&json!({"receipt":receipt})).unwrap()).unwrap();
    }
}

#[test]
fn network_observation_rejects_false_accounting_and_local_replacement_boundary() {
    let original: Value = serde_json::from_slice(&state_reply(None)).unwrap();
    for (field, value) in [
        ("consumed_frames", json!(48)),
        ("audible_source_position_frames", json!(24)),
        ("total_frames", json!(22)),
        ("input_peak", json!(-0.1)),
        ("consumption_basis", json!("server_presentation_clock")),
    ] {
        let mut bad = original.clone();
        bad["receipt"]["result"]["value"]["network_playback"][field] = value;
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "false {field}"
        );
    }
    let mut bad = original;
    bad["receipt"]["result"]["value"]["network_playback"]["processing"] =
        serde_json::to_value(earph_core::session::PlaybackProcessingObservation {
            resident_bytes: 0,
            audio_payload_bytes: None,
            failure_node: None,
            graph_hash: "a".repeat(64),
            timing: earph_core::graph_control::PlaybackTiming::Streaming {
                stages: std::sync::Arc::from([]),
            },
            streaming: None,
            source_boundary: Some(std::sync::Arc::new(
                earph_core::session::StreamingReplacementBoundary {
                    source_entry_id: QueueEntryId::new("local-entry").unwrap(),
                    source_position_frames: 0,
                    source_epochs: Epochs {
                        session: 3,
                        stream: 2,
                    },
                    discarded_output_frames: 0,
                },
            )),
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
        })
        .unwrap();
    assert!(decode_reply(serde_json::to_vec(&bad).unwrap()).is_err());
}
#[test]
fn local_shape_omits_network_observation_and_rejects_explicit_null() {
    let mut session = owner();
    let c = network_fixture::command(&session, "local-state", json!({"method":"state.get"}));
    let receipt = session.submit(c);
    let mut reply = json!({"receipt":receipt});
    assert!(
        reply["receipt"]["result"]["value"]
            .get("network_playback")
            .is_none()
    );
    decode_reply(serde_json::to_vec(&reply).unwrap()).unwrap();
    reply["receipt"]["result"]["value"]["network_playback"] = Value::Null;
    assert!(decode_reply(serde_json::to_vec(&reply).unwrap()).is_err());
}

fn replacement_reply() -> Vec<u8> {
    let mut session = network_fixture::replacement();
    let request =
        network_fixture::command(&session, "replacement-state", json!({"method":"state.get"}));
    serde_json::to_vec(&json!({"receipt": session.submit(request)})).unwrap()
}
#[test]
fn core_replacement_boundary_projects_after_a_later_pause_with_its_previous_output_domain() {
    let bytes = replacement_reply();
    let Reply::Receipt(receipt) =
        decode_reply(bytes.clone()).expect("Core historical network replacement must project")
    else {
        panic!("receipt")
    };
    assert_eq!(decode_reply(bytes.clone()).unwrap().bytes(), bytes);
    assert!(receipt.state().unwrap().playback.is_none());
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let observation = &value["receipt"]["result"]["value"]["network_playback"];
    let boundary = &observation["replacement_boundary"];
    assert_eq!(boundary["source_position_frames"], 23);
    assert_eq!(observation["source_position_frames"], 29);
    assert_eq!(boundary["discarded_output_frames"], 19);
    assert_ne!(boundary["output_epochs"], observation["output_epochs"]);
    assert_ne!(
        boundary["previous_output_format"],
        observation["output_format"]
    );
}
#[test]
fn network_replacement_boundary_rejects_wrong_source_and_impossible_bounds() {
    let original: Value = serde_json::from_slice(&replacement_reply()).unwrap();
    let observed = &original["receipt"]["result"]["value"]["network_playback"];
    let boundary = &observed["replacement_boundary"];
    let mut foreign_source = boundary["source_epochs"].clone();
    foreign_source["stream"] = json!(foreign_source["stream"].as_u64().unwrap() + 1);
    let mut future_output = observed["output_epochs"].clone();
    future_output["stream"] = json!(future_output["stream"].as_u64().unwrap() + 1);
    for (key, value) in [
        ("source_position_frames", json!(30)),
        ("discarded_output_frames", json!(0)),
        ("discarded_output_frames", json!(u64::MAX)),
        ("queue_entry_id", json!("invented-local")),
        ("source_epochs", foreign_source),
        ("output_epochs", future_output),
        ("previous_output_epochs", boundary["output_epochs"].clone()),
    ] {
        let mut bad = original.clone();
        bad["receipt"]["result"]["value"]["network_playback"]["replacement_boundary"][key] = value;
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "invalid replacement {key}"
        );
    }
    let mut wrong = original.clone();
    wrong["receipt"]["result"]["value"]["network_playback"]["replacement_boundary"]["binding"]["source_generation"] =
        json!(2);
    assert!(decode_reply(serde_json::to_vec(&wrong).unwrap()).is_err());
    for key in [
        "binding",
        "source_epochs",
        "source_position_frames",
        "previous_output_epochs",
        "output_epochs",
        "previous_output_format",
        "discarded_output_frames",
    ] {
        let mut bad = original.clone();
        bad["receipt"]["result"]["value"]["network_playback"]["replacement_boundary"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "missing replacement {key}"
        );
    }
    let mut null = original;
    null["receipt"]["result"]["value"]["network_playback"]["replacement_boundary"] = Value::Null;
    assert!(decode_reply(serde_json::to_vec(&null).unwrap()).is_err());
}
#[test]
fn real_diagnostic_producer_carries_allowlisted_replacement_facts_without_authored_binding() {
    let mut session = network_fixture::replacement();
    let request = network_fixture::command(
        &session,
        "replacement-diagnostic",
        json!({"method":"signal_path.get"}),
    );
    let bytes = serde_json::to_vec(&json!({"receipt":session.submit(request)})).unwrap();
    decode_reply(bytes.clone()).expect("actual Core diagnostic network boundary must project");
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    let boundary =
        &value["receipt"]["result"]["value"]["report"]["observed"]["network_replacement_boundary"];
    assert_eq!(boundary["discarded_output_frames"], 19);
    assert_eq!(boundary["source_position_frames"], 23);
    assert!(boundary.get("binding").is_none());
    assert!(!boundary.to_string().contains("http-session"));
}

#[test]
fn captured_software_owner_replacement_state_passes_the_strict_desktop_consumer() {
    let state: Value = serde_json::from_slice(include_bytes!(
        "network_fixture/owner-replacement-state.json"
    ))
    .unwrap();
    let mut reply: Value = serde_json::from_slice(&state_reply(None)).unwrap();
    reply["receipt"]["revision"] = state["revision"].clone();
    reply["receipt"]["epochs"] = state["epochs"].clone();
    reply["receipt"]["zone_id"] = state["zone_id"].clone();
    reply["receipt"]["result"]["value"] = state;
    let Reply::Receipt(receipt) = decode_reply(serde_json::to_vec(&reply).unwrap())
        .expect("actual software-owner body must project")
    else {
        panic!("receipt")
    };
    let observation = receipt.state().unwrap().network_playback.as_ref().unwrap();
    let boundary = observation.replacement_boundary.as_ref().unwrap();
    assert_eq!(boundary.discarded_output_frames, 60);
    assert_eq!(boundary.source_position_frames, 1024);
    assert_eq!(boundary.source_epochs, observation.source_epochs);
    assert_eq!(boundary.previous_output_format.sample_rate_hz(), 48000);
    assert!(
        observation
            .processing
            .as_ref()
            .unwrap()
            .source_boundary
            .is_none()
    );
}
