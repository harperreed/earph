// ABOUTME: Exercises volatile network authority through the real Session transaction boundary.
// ABOUTME: Checks exact replay, policy rollback, expiry, retention, and local wire preservation.
use earph_core::{control::*, ids::*, session::*};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    )
}
fn command(s: &Session, id: &str, payload: Value) -> CommandEnvelope {
    let state = s.snapshot();
    decode_command(
        &serde_json::to_vec(&json!({
            "schema_version":1,"request_id":id,"zone_id":"main","owner_id":"owner",
            "session_id":state.session_id,"expected_revision":state.revision,"payload":payload
        }))
        .unwrap(),
        &ControlLimits::default(),
    )
    .unwrap()
}
fn grants(expected_generation: u64, lifetime_ms: u64) -> Value {
    json!({"method":"sources.grants.replace","expected_generation":expected_generation,
        "grants":{"origins":[{"origin":"http://private.example:80","pins":["192.168.1.2"],"roots_der":[]}]},"lifetime_ms":lifetime_ms})
}
#[test]
fn network_commands_decode_strictly_and_default_owner_rejects_authority() {
    let mut s = owner();
    let c = command(&s, "grant", grants(0, 100));
    let receipt = s.submit(c);
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::CapabilityUnsupported
    );
    assert!(
        !serde_json::to_string(&s.snapshot())
            .unwrap()
            .contains("network_playback")
    );
    assert!(
        !serde_json::to_string(&s.checkpoint())
            .unwrap()
            .contains("private.example")
    );
}
#[test]
fn network_get_is_a_redacted_additive_query() {
    let mut s = owner();
    let c = command(&s, "network", json!({"method":"sources.network.get"}));
    let r = s.submit(c);
    assert_eq!(r.status, ReceiptStatus::Applied);
    let value = serde_json::to_value(r).unwrap();
    assert_eq!(value["result"]["kind"], "network");
    assert_eq!(value["result"]["value"]["grant_generation"], 0);
    assert_eq!(value["result"]["value"]["enabled"], false);
}

fn supported() -> Session {
    let mut s = owner();
    s.enable_local_playback();
    s.enable_network_playback();
    let output = json!({"method":"output.select","output":{
        "backend":"alsa","route":"system","endpoint":"silent-fixture","device_id":"dac","profile_id":"profile",
        "period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}});
    for (id, payload) in [
        ("output", output),
        (
            "consent",
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        ),
    ] {
        let c = command(&s, id, payload);
        assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    }
    s
}
fn network(s: &mut Session, id: &str, payload: Value, now: u64) -> Receipt {
    let c = command(s, id, payload);
    s.submit_with_network_effects(c, now, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
}
fn staged(s: &mut Session) {
    let c = command(
        s,
        "stage",
        json!({"method":"sources.stage_uri","uri":"http://private.example/file?secret=raw","metadata":"private-metadata"}),
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
}
fn start_command(s: &Session, id: &str, session_id: &str, takeover: bool) -> CommandEnvelope {
    let mut c = command(
        s,
        id,
        json!({"method":"sources.start_http_flac","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac","takeover":takeover}),
    );
    c.session_id = Some(SessionId::new(session_id).unwrap());
    c
}
fn started() -> Session {
    let mut s = supported();
    staged(&mut s);
    assert_eq!(
        network(&mut s, "grants", grants(0, 100), 10).status,
        ReceiptStatus::Applied
    );
    let c = start_command(&s, "start", "network-session", false);
    assert_eq!(
        s.submit_with_network_effects(
            c,
            20,
            |_| unreachable!(),
            |candidate, _| {
                let active = candidate.network_source().unwrap();
                assert_eq!(
                    active.descriptor.uri(),
                    "http://private.example/file?secret=raw"
                );
                assert!(candidate.network_authority_valid(&active.binding, 20));
                Ok(())
            },
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Accepted
    );
    s
}
#[test]
fn exact_descriptor_access_and_start_fences_preserve_authority() {
    use earph_core::ErrorCode;
    let mut s = supported();
    staged(&mut s);
    assert!(
        s.staged_uri_descriptor(&StagedMediaId::new("staged-1").unwrap(), 1)
            .is_ok()
    );
    assert!(
        s.staged_uri_descriptor(&StagedMediaId::new("staged-2").unwrap(), 1)
            .is_err()
    );
    assert!(
        s.staged_uri_descriptor(&StagedMediaId::new("staged-1").unwrap(), 2)
            .is_err()
    );
    assert_eq!(
        network(&mut s, "grants", grants(0, 100), 10).status,
        ReceiptStatus::Applied
    );
    let base = start_command(&s, "start", "fresh", false);
    for (name, mut c, code) in [
        ("owner", base.clone(), ErrorCode::StaleSession),
        ("revision", base.clone(), ErrorCode::StaleRevision),
        ("staging", base.clone(), ErrorCode::StaleSession),
        ("grants", base.clone(), ErrorCode::AuthDenied),
        ("session", base.clone(), ErrorCode::StaleSession),
    ] {
        c.request_id = RequestId::new(format!("bad-{name}")).unwrap();
        match name {
            "owner" => c.owner_id = Some(OwnerId::new("stale").unwrap()),
            "revision" => c.expected_revision = Some(0),
            "session" => c.session_id = None,
            "staging" => {
                if let Command::NetworkStartHttpFlac {
                    staging_generation, ..
                } = &mut c.payload
                {
                    *staging_generation = 2
                }
            }
            "grants" => {
                if let Command::NetworkStartHttpFlac {
                    grant_generation, ..
                } = &mut c.payload
                {
                    *grant_generation = 2
                }
            }
            _ => unreachable!(),
        }
        let before = s.snapshot();
        let r = s.submit_with_network_effects(
            c,
            20,
            |_| unreachable!(),
            |_, _| panic!("bad fences reached host policy"),
            |_| panic!("bad fences reached commit"),
        );
        assert_eq!(r.error.unwrap().code, code, "{name}");
        assert_eq!(s.snapshot(), before);
    }
    let r = s.submit_with_network_effects(base, 20, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()));
    assert_eq!(r.status, ReceiptStatus::Accepted);
    assert_eq!(
        s.network_snapshot().lifecycle,
        Some(earph_core::network_source::NetworkLifecycle::Preparing)
    );
    let state = s.snapshot();
    assert_eq!(state.source, Some(SourceKind::HttpFlac));
    assert!(state.playback.is_none());
    assert!(state.active_entry_id.is_none());
    assert!(!state.seekable);
}
#[test]
fn staged_policy_and_checkpoint_refusals_roll_back_exactly_and_replay_never_renews() {
    use earph_core::{EngineError, ErrorCode};
    let mut s = supported();
    staged(&mut s);
    for (id, policy_fail) in [("policy-refused", true), ("commit-refused", false)] {
        let c = command(&s, id, grants(0, 100));
        let before = s.snapshot();
        let r = s.submit_with_network_effects(
            c.clone(),
            11,
            |_| unreachable!(),
            |candidate, _| {
                assert_eq!(candidate.network_grants().unwrap().0, 1);
                if policy_fail {
                    Err(EngineError::new(ErrorCode::AuthDenied, "policy refused"))
                } else {
                    Ok(())
                }
            },
            |_| {
                if policy_fail {
                    panic!("refused policy reached commit")
                } else {
                    Err(EngineError::new(ErrorCode::InternalFault, "commit refused"))
                }
            },
        );
        assert_eq!(
            r.status,
            if policy_fail {
                ReceiptStatus::Rejected
            } else {
                ReceiptStatus::Failed
            }
        );
        assert_eq!(s.snapshot(), before);
        assert!(s.network_grants().is_none());
        assert_eq!(
            s.submit_with_network_effects(
                c,
                11,
                |_| unreachable!(),
                |_, _| panic!("replay validated"),
                |_| panic!("replay committed")
            ),
            r
        );
    }
    let c = command(&s, "grant-success", grants(0, 100));
    let r =
        s.submit_with_network_effects(c.clone(), 20, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()));
    assert_eq!(s.network_grants().unwrap().1, 100_000_020);
    assert_eq!(
        s.submit_with_network_effects(
            c.clone(),
            21,
            |_| unreachable!(),
            |_, _| panic!("replay validated"),
            |_| panic!("replay committed")
        ),
        r
    );
    s.expire_network_authority(100_000_020).unwrap();
    assert!(s.network_grants().is_none());
    assert_eq!(
        s.submit_with_network_effects(
            c,
            100_000_021,
            |_| unreachable!(),
            |_, _| panic!("expired replay validated"),
            |_| panic!("expired replay committed")
        ),
        r
    );
    assert!(s.network_grants().is_none());
}
#[test]
fn grant_lifetime_bounds_and_current_idle_clock_are_checked_without_state_events() {
    let mut s = supported();
    for (i, lifetime) in [0, 3_600_001, u64::MAX].into_iter().enumerate() {
        assert_eq!(
            network(&mut s, &format!("invalid-{i}"), grants(0, lifetime), 100).status,
            ReceiptStatus::Rejected
        );
    }
    assert_eq!(
        network(&mut s, "minimum", grants(0, 1), 100).status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().monotonic_ns, 0);
    s.expire_network_authority(1_000_100).unwrap();
    assert!(s.network_grants().is_none());
    assert!(s.expire_network_authority(1_000_099).is_err());
    assert_eq!(
        network(&mut s, "overflow", grants(1, 1), u64::MAX).status,
        ReceiptStatus::Rejected
    );
}
#[test]
fn starting_requires_explicit_consent_flexible_fidelity_and_fresh_session() {
    let mut s = started();
    let c = start_command(&s, "same-session", "network-session", true);
    assert_eq!(
        s.submit_with_network_effects(
            c,
            21,
            |_| unreachable!(),
            |_, _| panic!("same session policy"),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Rejected
    );
    let c = start_command(&s, "implicit-takeover", "other-session", false);
    assert_eq!(
        s.submit_with_network_effects(
            c,
            22,
            |_| unreachable!(),
            |_, _| panic!("implicit takeover policy"),
            |_| Ok(())
        )
        .error
        .unwrap()
        .code,
        earph_core::ErrorCode::SourceBusy
    );
    let c = command(&s, "unmute", json!({"method":"mute.set","muted":false}));
    assert_eq!(
        s.submit_graph(c, 23).error.unwrap().code,
        earph_core::ErrorCode::SafetyPolicyBlocked
    );
    for (id, payload) in [
        (
            "seek",
            json!({"method":"sessions.seek","position_frames":0}),
        ),
        (
            "profile",
            json!({"method":"profiles.prepare","id":"p","expected_profile_revision":1,"profile_sha256":"00".repeat(32)}),
        ),
    ] {
        let c = command(&s, id, payload);
        assert_eq!(s.submit_graph(c, 24).status, ReceiptStatus::Rejected);
    }
}
#[test]
fn checkpoint_and_debug_never_restore_private_network_authority() {
    let s = started();
    let active = s.network_source().unwrap();
    assert!(!format!("{active:?}").contains("secret=raw"));
    assert!(!format!("{active:?}").contains("private.example"));
    let encoded = serde_json::to_string(&s.checkpoint()).unwrap();
    assert!(!encoded.contains("private.example"));
    assert!(!encoded.contains("private-metadata"));
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("replacement").unwrap(),
        ControlLimits::default(),
        serde_json::from_str(&encoded).unwrap(),
    )
    .unwrap();
    assert!(restored.network_grants().is_none());
    assert!(restored.network_source().is_none());
    assert!(restored.closing_network_source().is_none());
    assert!(!restored.snapshot().output_consented);
    assert!(
        !serde_json::to_string(&restored.snapshot())
            .unwrap()
            .contains("network_playback")
    );
}

use earph_core::{audio::*, network_source::*};
fn observation(s: &Session) -> NetworkPlaybackObservation {
    let active = s.network_source().unwrap();
    let state = s.snapshot();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    NetworkPlaybackObservation {
        binding: active.binding.clone(),
        source_epochs: active.source_epochs,
        output_epochs: state.epochs,
        graph_epoch: state.graph_epoch,
        device_epoch: state.device_epoch,
        lifecycle: NetworkLifecycle::Ready,
        format,
        output_format: format,
        total_frames: None,
        source_position_frames: 0,
        audible_source_position_frames: None,
        submitted_frames: 0,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "unknown",
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: None,
        output_activity: None,
        processing: None,
        replacement_boundary: None,
    }
}
fn event(s: &Session, kind: NetworkEventKind, now: u64) -> NetworkEvent {
    let a = s.network_source().unwrap();
    let st = s.snapshot();
    NetworkEvent {
        binding: a.binding.clone(),
        source_epochs: a.source_epochs,
        output_epochs: st.epochs,
        graph_epoch: st.graph_epoch,
        device_epoch: st.device_epoch,
        monotonic_ns: now,
        kind,
    }
}
fn retirement(s: &Session) -> NetworkRetirementAck {
    let c = s.closing_network_source().unwrap();
    NetworkRetirementAck {
        binding: c.context.binding.clone(),
        source_epochs: c.context.source_epochs,
        output_epochs: c.output_epochs,
        graph_epoch: c.graph_epoch,
        device_epoch: c.device_epoch,
        observation: None,
    }
}
#[test]
fn startup_pause_keeps_readiness_preparing_until_measured_observation() {
    let mut s = started();
    let source = s.network_source().unwrap();
    let pause = command(&s, "startup-pause", json!({"method":"sessions.pause"}));
    assert_eq!(s.submit_graph(pause, 21).status, ReceiptStatus::Applied);
    assert_eq!(s.snapshot().transport, TransportState::Paused);
    assert!(s.snapshot().network_playback.is_none());
    assert_eq!(
        s.network_snapshot().lifecycle,
        Some(NetworkLifecycle::Preparing)
    );

    let ready = observation(&s);
    let prepared = event(&s, NetworkEventKind::Prepared { observation: ready }, 22);
    s.advance_network(prepared).unwrap();
    assert_eq!(s.snapshot().transport, TransportState::Paused);
    let mut paused = observation(&s);
    paused.lifecycle = NetworkLifecycle::Paused;
    let paused_event = event(
        &s,
        NetworkEventKind::Paused {
            observation: paused,
        },
        23,
    );
    s.advance_network(paused_event).unwrap();
    assert_eq!(
        s.network_snapshot().lifecycle,
        Some(NetworkLifecycle::Paused)
    );
    let measured = s.snapshot().network_playback.unwrap();
    assert_eq!(measured.binding, source.binding);
    assert_eq!(measured.source_epochs, source.source_epochs);
    assert_eq!(measured.submitted_frames, 0);
    assert!(s.snapshot().muted);
}

#[test]
fn measured_readiness_preserves_unknown_duration_and_never_updates_the_queue() {
    let mut s = started();
    let q = s.queue_snapshot();
    let p = observation(&s);
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: p.clone(),
        },
        30,
    );
    s.advance_network(e).unwrap();
    assert_eq!(s.queue_snapshot(), q);
    assert_eq!(s.snapshot().network_playback, Some(p));
    let state = serde_json::to_value(s.snapshot()).unwrap();
    assert!(state["network_playback"]["total_frames"].is_null());
    assert!(state["playback"].is_null());
    assert!(state["active_entry_id"].is_null());
    assert!(state["audible_position_frames"].is_null());
    assert!(!state.to_string().contains("private.example"));
    let stale = NetworkEvent {
        binding: NetworkSourceBinding {
            source_generation: 2,
            ..s.network_source().unwrap().binding.clone()
        },
        ..event(
            &s,
            NetworkEventKind::Progress {
                observation: observation(&s),
            },
            31,
        )
    };
    let before = s.snapshot();
    assert!(s.advance_network(stale).is_err());
    assert_eq!(s.snapshot(), before);
}
#[test]
fn revocation_settles_only_after_exact_retirement_and_blocks_replacement() {
    let mut s = started();
    let active = s.network_source().unwrap();
    let c = command(
        &s,
        "revoke",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
    );
    let r =
        s.submit_with_network_effects(c.clone(), 30, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()));
    assert_eq!(r.status, ReceiptStatus::Accepted);
    assert!(!s.network_authority_valid(&active.binding, 30));
    assert_eq!(s.network_snapshot().grant_generation, 2);
    assert!(s.network_source().is_none());
    let exact = retirement(&s);
    let mut stale = exact.clone();
    stale.binding.source_generation += 1;
    assert!(s.complete_network_retirement(stale, 31).is_err());
    assert!(s.closing_network_source().is_some());
    let start = start_command(&s, "replacement", "second-session", true);
    assert_eq!(
        s.submit_with_network_effects(start, 31, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .error
            .unwrap()
            .code,
        earph_core::ErrorCode::SourceBusy
    );
    s.complete_network_retirement(exact.clone(), 32).unwrap();
    assert!(s.closing_network_source().is_none());
    assert_eq!(
        s.submit_with_network_effects(
            c,
            33,
            |_| unreachable!(),
            |_, _| panic!("settled replay"),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
    assert!(s.complete_network_retirement(exact, 34).is_err());
    assert!(s.snapshot().muted);
    assert!(s.snapshot().source.is_none());
}
#[test]
fn paused_expiry_uses_current_clock_and_keeps_cleanup_authority() {
    let mut s = started();
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(e).unwrap();
    let c = command(&s, "pause", json!({"method":"sessions.pause"}));
    assert_eq!(s.submit_graph(c, 31).status, ReceiptStatus::Accepted);
    let mut p = observation(&s);
    p.lifecycle = NetworkLifecycle::Paused;
    let e = event(&s, NetworkEventKind::Paused { observation: p }, 32);
    s.advance_network(e).unwrap();
    let active = s.network_source().unwrap();
    s.expire_network_authority(100_000_010).unwrap();
    assert_eq!(s.snapshot().transport, TransportState::Failed);
    assert!(!s.network_authority_valid(&active.binding, 100_000_010));
    let ack = retirement(&s);
    assert_eq!(ack.binding, active.binding);
    s.complete_network_retirement(ack, 100_000_011).unwrap();
    assert_eq!(s.snapshot().error, Some(earph_core::ErrorCode::AuthDenied));
    assert_eq!(s.snapshot().transport, TransportState::Failed);
}
#[test]
fn first_source_failure_survives_stop_and_retirement() {
    let mut s = started();
    let e = event(
        &s,
        NetworkEventKind::Failed {
            code: earph_core::ErrorCode::AssetInvalid,
        },
        30,
    );
    s.advance_network(e).unwrap();
    assert_eq!(
        s.snapshot().error,
        Some(earph_core::ErrorCode::AssetInvalid)
    );
    let c = command(&s, "stop-failed", json!({"method":"sessions.stop"}));
    let r = s.submit_graph(c, 31);
    assert!(matches!(
        r.status,
        ReceiptStatus::Accepted | ReceiptStatus::Applied
    ));
    s.complete_network_retirement(retirement(&s), 32).unwrap();
    assert_eq!(
        s.snapshot().error,
        Some(earph_core::ErrorCode::AssetInvalid)
    );
}

use earph_core::{graph::*, graph_control::*};
use std::sync::Arc;
fn prepare_graph(s: &mut Session) -> GraphPrepared {
    prepare_graph_at(s, "prepare", 40)
}
fn prepare_graph_at(s: &mut Session, id: &str, now: u64) -> GraphPrepared {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let input = NodeId::new("input").unwrap();
    let output = NodeId::new("output").unwrap();
    let port = PortFormat {
        domain: earph_core::safety::SampleDomain::Pcm,
        sample_rate_hz: 48000,
        channels: ChannelLayout::Stereo,
        precision: PcmRepresentation::Float64,
        max_quantum_frames: 64,
    };
    let graph = GraphDocument {
        schema_version: 1,
        nodes: vec![
            Node {
                id: input.clone(),
                kind: NodeKind::Input,
                format: port,
            },
            Node {
                id: output.clone(),
                kind: NodeKind::Output,
                format: port,
            },
        ],
        edges: vec![Edge {
            from: input,
            to: output.clone(),
            from_port: 0,
            to_port: 0,
        }],
        output,
    };
    let c = command(
        s,
        id,
        json!({"method":"graph.prepare","graph":graph,"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}),
    );
    assert_eq!(s.submit_graph(c, now).status, ReceiptStatus::Accepted);
    let job = s.graph_job().unwrap();
    let source = s.snapshot().network_playback.unwrap().format;
    let preview = Arc::new(GraphPreview {
        diagnostic_nodes: None,
        profile_application: None,
        backend: None,
        graph_hash: "a".repeat(64),
        kernel_hash: "b".repeat(64),
        implementation_version: "network-contract".into(),
        binding: job.binding.clone(),
        source_format: source,
        native_output_format: source,
        processing_input_format: format,
        processing_output_format: format,
        quantum_frames: 64,
        timing: GraphTiming::Streaming {
            stages: Arc::from(vec![CompiledStreamStage {
                original_nodes: vec![NodeId::new("island").unwrap()],
                input_format: format,
                output_format: format,
                input_peak_bounds: vec![1.0; 2],
                output_peak_bounds: vec![1.0; 2],
                kind: CompiledStreamStageKind::Island {
                    kernel_hash: "c".repeat(64),
                    local_latency_frames: 0,
                    emitted_tail_frames: 0,
                    bounded_iir_truncation: false,
                    terminal_output_bounds: vec![0.0; 2],
                },
            }]),
        },
        preservation: PreservationDecision::ProcessFloat64,
        transformations: vec![],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0; 2],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    });
    s.complete_graph_preparation(&job.request_id, &job.binding, now + 1, Ok(preview))
        .unwrap();
    s.graph_inspection().prepared.unwrap()
}
#[test]
fn explicit_stream_graph_activation_uses_network_ack_and_preserves_local_queue() {
    let mut s = started();
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(e).unwrap();
    let before = s.queue_snapshot();
    let prepared = prepare_graph(&mut s);
    let c = command(
        &s,
        "activate",
        json!({"method":"graph.commit","token":prepared.token,"graph_hash":prepared.preview.graph_hash}),
    );
    assert_eq!(
        s.submit_graph(c.clone(), 42).status,
        ReceiptStatus::Accepted
    );
    let mut p = observation(&s);
    p.graph_epoch += 1;
    let ack = NetworkGraphActivationAck {
        request_id: c.request_id.clone(),
        epochs: s.snapshot().epochs,
        graph_hash: prepared.preview.graph_hash.clone(),
        activation_frame: 0,
        observation: p.clone(),
    };
    let mut stale = ack.clone();
    stale.observation.binding.source_generation += 1;
    assert!(
        s.complete_network_graph_activation(stale, 43, |_| panic!("stale ack committed"))
            .is_err()
    );
    s.complete_network_graph_activation(ack, 44, |candidate| {
        assert_eq!(candidate.queue_snapshot(), before);
        assert_eq!(candidate.snapshot().network_playback, Some(p));
        Ok(())
    })
    .unwrap();
    assert_eq!(s.queue_snapshot(), before);
    assert_eq!(s.submit_graph(c, 45).status, ReceiptStatus::Applied);
    let c = command(
        &s,
        "explicit-unmute",
        json!({"method":"mute.set","muted":false}),
    );
    assert_eq!(s.submit_graph(c, 46).status, ReceiptStatus::Applied);
}

#[test]
fn rejected_roots_history_is_byte_bounded_and_shared_documents_are_counted_once() {
    use earph_core::{EngineError, ErrorCode};
    let make = || {
        Arc::new(NetworkGrantDocument {
            origins: vec![NetworkOriginGrant {
                origin: "http://private.example:80".into(),
                pins: vec!["192.168.1.2".parse().unwrap()],
                roots_der: vec![vec![7; 200 * 1024]],
            }],
        })
    };
    let mut s = supported();
    let shared = make();
    for i in 0..50 {
        let mut c = command(&s, &format!("shared-{i}"), grants(0, 100));
        c.payload = Command::NetworkGrantsReplace {
            expected_generation: 0,
            grants: shared.clone(),
            lifetime_ms: 100,
        };
        let r = s.submit_with_network_effects(
            c,
            10,
            |_| unreachable!(),
            |_, _| Err(EngineError::new(ErrorCode::AuthDenied, "refused")),
            |_| panic!("refused grant committed"),
        );
        assert_eq!(r.error.unwrap().code, ErrorCode::AuthDenied);
    }
    assert!(
        s.network_retained_bytes().unwrap() < 400 * 1024,
        "shared root allocation was multiplied per receipt"
    );
    let mut bounded = false;
    for i in 0..100 {
        let mut c = command(&s, &format!("distinct-{i}"), grants(0, 100));
        c.payload = Command::NetworkGrantsReplace {
            expected_generation: 0,
            grants: make(),
            lifetime_ms: 100,
        };
        let r = s.submit_with_network_effects(
            c,
            10,
            |_| unreachable!(),
            |_, _| Err(EngineError::new(ErrorCode::AuthDenied, "refused")),
            |_| panic!("refused grant committed"),
        );
        assert!(s.network_retained_bytes().unwrap() <= MAX_NETWORK_AUTHORITY_BYTES);
        if r.error.unwrap().code == ErrorCode::ResourceBudgetExceeded {
            bounded = true;
            break;
        }
    }
    assert!(
        bounded,
        "distinct rejected roots evaded authority retention budget"
    );
}
#[test]
fn raw_source_debug_and_general_diagnostics_are_redacted() {
    let mut s = started();
    let c = command(
        &s,
        "debug-stage",
        json!({"method":"sources.stage_uri","uri":"http://private.example/?secret=never-debug","metadata":"private-metadata"}),
    );
    let debug = format!("{c:?}");
    assert!(!debug.contains("secret=never-debug"));
    assert!(!debug.contains("private-metadata"));
    let mut p = observation(&s);
    p.input_peak = Some(0.25);
    p.output_peak = Some(0.125);
    p.backend_delay_frames = Some(3);
    let e = event(&s, NetworkEventKind::Prepared { observation: p }, 30);
    s.advance_network(e).unwrap();
    let c = command(&s, "metrics", json!({"method":"metrics.get"}));
    let r = s.submit_graph(c, 31);
    let encoded = serde_json::to_value(r).unwrap();
    assert_eq!(
        encoded["result"]["value"]["report"]["input_peak"]["value"],
        0.25
    );
    assert!(!encoded.to_string().contains("private.example"));
    assert!(!encoded.to_string().contains("private-metadata"));
}
#[test]
fn network_dto_consumers_reject_zero_generations_and_missing_nullable_fields() {
    let s = started();
    let binding = s.network_source().unwrap().binding.clone();
    let bytes = serde_json::to_vec(&binding).unwrap();
    assert_eq!(
        serde_json::from_slice::<NetworkSourceBinding>(&bytes).unwrap(),
        binding
    );
    for field in ["source_generation", "grant_generation"] {
        let mut bad = serde_json::to_value(&binding).unwrap();
        bad[field] = json!(0);
        assert!(
            serde_json::from_value::<NetworkSourceBinding>(bad).is_err(),
            "zero {field}"
        );
    }
    let value = serde_json::to_value(s.network_snapshot()).unwrap();
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<NetworkSnapshot>(value.clone()).unwrap())
            .unwrap(),
        value
    );
    for field in ["expires_monotonic_ns", "active", "closing", "lifecycle"] {
        let mut bad = value.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<NetworkSnapshot>(bad).is_err(),
            "missing {field}"
        );
    }
}
#[test]
fn local_generic_events_cannot_publish_network_pcm_without_network_identity() {
    let mut s = started();
    let before = s.snapshot();
    let event = EngineEvent {
        epochs: before.epochs,
        monotonic_ns: 30,
        kind: EngineEventKind::Decoded {
            position_frames: 1024,
        },
    };
    assert!(s.advance(event).is_err());
    assert_eq!(s.snapshot(), before);
}

#[test]
fn pending_takeover_can_be_revoked_while_the_outgoing_context_retires() {
    let mut s = started();
    let outgoing = s.network_source().unwrap();
    let c = start_command(&s, "takeover", "second-session", true);
    assert_eq!(
        s.submit_with_network_effects(c, 30, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    assert_eq!(
        s.closing_network_source().unwrap().context.binding,
        outgoing.binding
    );
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        31,
    );
    assert!(s.advance_network(e).is_err());
    let r = network(
        &mut s,
        "revoke-pending",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
        32,
    );
    assert_eq!(r.status, ReceiptStatus::Accepted);
    assert!(s.network_source().is_none());
    assert_eq!(
        s.closing_network_source().unwrap().context.binding,
        outgoing.binding
    );
    s.complete_network_retirement(retirement(&s), 33).unwrap();
    assert!(s.snapshot().source.is_none());
}
#[test]
fn network_start_rejects_missing_consent_and_strict_fidelity_before_host_policy() {
    let mut s = supported();
    staged(&mut s);
    network(&mut s, "grant", grants(0, 100), 10);
    for (i, strict) in [false, true].into_iter().enumerate() {
        let now = 20 + i as u64 * 10;
        let mut output = s.snapshot().output.unwrap();
        if strict {
            output.fidelity = earph_core::safety::FidelityPolicy::StrictPreservation;
        }
        let c = command(
            &s,
            &format!("select-{strict}"),
            json!({"method":"output.select","output":output}),
        );
        assert_eq!(s.submit_graph(c, now).status, ReceiptStatus::Applied);
        if strict {
            let c = command(
                &s,
                "strict-consent",
                json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
            );
            s.submit_graph(c, now + 1);
        }
        let c = start_command(&s, &format!("start-{strict}"), "fresh", false);
        let r = s.submit_with_network_effects(
            c,
            now + 2,
            |_| unreachable!(),
            |_, _| panic!("unsupported authority reached host"),
            |_| panic!("unsupported authority committed"),
        );
        assert_eq!(
            r.error.unwrap().code,
            if strict {
                earph_core::ErrorCode::FidelityPolicyConflict
            } else {
                earph_core::ErrorCode::SafetyPolicyBlocked
            }
        );
    }
}
#[test]
fn grants_without_a_new_retirement_apply_while_an_earlier_revoke_waits() {
    let mut s = started();
    assert_eq!(
        network(
            &mut s,
            "first-revoke",
            json!({"method":"sources.grants.revoke","expected_generation":1}),
            30
        )
        .status,
        ReceiptStatus::Accepted
    );
    assert_eq!(
        network(
            &mut s,
            "second-revoke",
            json!({"method":"sources.grants.revoke","expected_generation":2}),
            31
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.network_snapshot().grant_generation, 3);
    assert!(s.closing_network_source().is_some());
}

#[test]
fn takeover_closing_context_preserves_outgoing_output_epoch_for_final_observation() {
    let mut s = started();
    let outgoing = observation(&s);
    let c = start_command(&s, "takeover-epochs", "second-session", true);
    assert_eq!(
        s.submit_with_network_effects(c, 30, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    let closing = s.closing_network_source().unwrap();
    assert_eq!(closing.output_epochs, outgoing.output_epochs);
    let mut ack = retirement(&s);
    ack.observation = Some(outgoing);
    s.complete_network_retirement(ack, 31).unwrap();
    assert!(s.network_source().is_some());
}
#[test]
fn exact_closing_failure_is_retained_without_readmitting_source_pcm() {
    let mut s = started();
    let mut e = event(
        &s,
        NetworkEventKind::Failed {
            code: earph_core::ErrorCode::AssetInvalid,
        },
        31,
    );
    network(
        &mut s,
        "revoke-late-failure",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
        30,
    );
    let closing = s.closing_network_source().unwrap();
    e.output_epochs = closing.output_epochs;
    s.advance_network(e).unwrap();
    assert_eq!(
        s.closing_network_source().unwrap().reason,
        Some(earph_core::ErrorCode::AssetInvalid)
    );
    let mut stale = retirement(&s);
    stale.binding.grant_generation += 1;
    assert!(s.complete_network_retirement(stale, 32).is_err());
    s.complete_network_retirement(retirement(&s), 33).unwrap();
    assert_eq!(
        s.snapshot().error,
        Some(earph_core::ErrorCode::AssetInvalid)
    );
}
#[test]
fn prepared_source_cannot_claim_native_submission_before_explicit_graph_activation() {
    let mut s = started();
    let mut p = observation(&s);
    p.submitted_frames = 1;
    let e = event(&s, NetworkEventKind::Prepared { observation: p }, 30);
    let before = s.snapshot();
    assert!(s.advance_network(e).is_err());
    assert_eq!(s.snapshot(), before);
}

#[test]
fn owner_shutdown_stays_accepted_until_exact_network_retirement() {
    let mut s = started();
    let c = command(&s, "shutdown-network", json!({"method":"owner.shutdown"}));
    assert_eq!(
        s.submit_graph(c.clone(), 30).status,
        ReceiptStatus::Accepted
    );
    assert!(s.shutdown_requested());
    assert!(s.closing_network_source().is_some());
    s.complete_network_retirement(retirement(&s), 31).unwrap();
    assert_eq!(s.submit_graph(c, 32).status, ReceiptStatus::Applied);
}

#[test]
fn settled_grant_retirement_keeps_its_typed_network_result() {
    let mut s = started();
    let c = command(
        &s,
        "typed-revoke",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
    );
    assert_eq!(
        s.submit_with_network_effects(c.clone(), 30, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    s.complete_network_retirement(retirement(&s), 31).unwrap();
    let r = s.submit_graph(c, 32);
    assert_eq!(r.status, ReceiptStatus::Applied);
    assert!(matches!(r.result, Some(QueryResult::Network(_))));
}

#[test]
fn partial_pause_failure_retains_the_actual_prior_native_epoch() {
    let mut s = started();
    let mut failure = event(
        &s,
        NetworkEventKind::Failed {
            code: earph_core::ErrorCode::OutputFault,
        },
        32,
    );
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(e).unwrap();
    let c = command(&s, "partial-pause", json!({"method":"sessions.pause"}));
    assert_eq!(s.submit_graph(c, 31).status, ReceiptStatus::Accepted);
    assert_ne!(failure.output_epochs, s.snapshot().epochs);
    let mut stale = failure.clone();
    stale.binding.source_generation += 1;
    assert!(s.advance_network(stale).is_err());
    let mut future = failure.clone();
    future.output_epochs = s.snapshot().epochs.next_stream().unwrap();
    assert!(s.advance_network(future).is_err());
    failure.monotonic_ns = 33;
    s.advance_network(failure.clone()).unwrap();
    assert_eq!(
        s.closing_network_source().unwrap().output_epochs,
        failure.output_epochs
    );
    assert_eq!(s.snapshot().error, Some(earph_core::ErrorCode::OutputFault));
}
#[test]
fn graph_failure_uses_measured_previous_suspended_or_installed_fences() {
    for stage in 0..3 {
        let mut s = started();
        let e = event(
            &s,
            NetworkEventKind::Prepared {
                observation: observation(&s),
            },
            30,
        );
        s.advance_network(e).unwrap();
        let p = prepare_graph(&mut s);
        let c = command(
            &s,
            "commit-failure",
            json!({"method":"graph.commit","token":p.token,"graph_hash":p.preview.graph_hash}),
        );
        assert_eq!(
            s.submit_graph(c.clone(), 42).status,
            ReceiptStatus::Accepted
        );
        let activation = s.graph_activation().unwrap();
        let source = s.network_source().unwrap();
        let actual = NetworkExecutionFence {
            binding: source.binding.clone(),
            source_epochs: source.source_epochs,
            output_epochs: if stage == 0 {
                activation.previous_binding.epochs
            } else {
                activation.epochs
            },
            graph_epoch: s.snapshot().graph_epoch + u64::from(stage == 2),
            device_epoch: s.snapshot().device_epoch,
        };
        let error = earph_core::EngineError::new(
            earph_core::ErrorCode::OutputFault,
            "measured installation failure",
        );
        let mut stale = actual.clone();
        stale.binding.source_generation += 1;
        assert!(
            s.fail_network_graph_activation(&c.request_id, error, stale)
                .is_err()
        );
        assert!(
            s.fail_network_graph_activation(
                &RequestId::new("stale-request").unwrap(),
                error,
                actual.clone()
            )
            .is_err()
        );
        s.fail_network_graph_activation(&c.request_id, error, actual.clone())
            .unwrap();
        let closing = s.closing_network_source().unwrap();
        assert_eq!(closing.output_epochs, actual.output_epochs);
        assert_eq!(closing.graph_epoch, actual.graph_epoch);
        assert!(s.graph_activation().is_none());
        assert!(s.network_source().is_none());
        assert!(s.closing_network_source().is_some());
        s.complete_network_retirement(retirement(&s), 43).unwrap();
        assert!(s.closing_network_source().is_none());
    }
}
#[test]
fn late_graph_failure_updates_only_the_matching_retiring_activation_fences() {
    let mut s = started();
    let e = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(e).unwrap();
    let p = prepare_graph(&mut s);
    let c = command(
        &s,
        "commit-before-revoke",
        json!({"method":"graph.commit","token":p.token,"graph_hash":p.preview.graph_hash}),
    );
    s.submit_graph(c.clone(), 42);
    let activation = s.graph_activation().unwrap();
    let source = s.network_source().unwrap();
    network(
        &mut s,
        "revoke-activation",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
        43,
    );
    let actual = NetworkExecutionFence {
        binding: source.binding.clone(),
        source_epochs: source.source_epochs,
        output_epochs: activation.previous_binding.epochs,
        graph_epoch: s.snapshot().graph_epoch,
        device_epoch: s.snapshot().device_epoch,
    };
    s.fail_network_graph_activation(
        &c.request_id,
        earph_core::EngineError::new(
            earph_core::ErrorCode::OperationCancelled,
            "activation cancelled",
        ),
        actual.clone(),
    )
    .unwrap();
    assert_eq!(
        s.closing_network_source().unwrap().output_epochs,
        actual.output_epochs
    );
    assert!(s.network_source().is_none());
    s.complete_network_retirement(retirement(&s), 44).unwrap();
    assert!(s.closing_network_source().is_none());
}

#[test]
fn in_flight_pause_or_mute_cleanup_uses_the_retained_closing_epoch_range() {
    for mute in [false, true] {
        for revoke in [false, true] {
            for suspended in [false, true] {
                let mut s = started();
                let e = event(
                    &s,
                    NetworkEventKind::Prepared {
                        observation: observation(&s),
                    },
                    30,
                );
                s.advance_network(e).unwrap();
                let p = prepare_graph(&mut s);
                let commit = command(
                    &s,
                    "range-graph",
                    json!({"method":"graph.commit","token":p.token,"graph_hash":p.preview.graph_hash}),
                );
                s.submit_graph(commit.clone(), 42);
                let mut ready = observation(&s);
                ready.graph_epoch += 1;
                s.complete_network_graph_activation(
                    NetworkGraphActivationAck {
                        request_id: commit.request_id,
                        epochs: s.snapshot().epochs,
                        graph_hash: p.preview.graph_hash.clone(),
                        activation_frame: 0,
                        observation: ready,
                    },
                    43,
                    |_| Ok(()),
                )
                .unwrap();
                let unmute = command(
                    &s,
                    "range-unmute",
                    json!({"method":"mute.set","muted":false}),
                );
                s.submit_graph(unmute, 44);
                let prior = s.snapshot().epochs;
                let payload = if mute {
                    json!({"method":"mute.set","muted":true})
                } else {
                    json!({"method":"sessions.pause"})
                };
                let control = command(&s, "in-flight", payload);
                assert!(matches!(
                    s.submit_graph(control, 45).status,
                    ReceiptStatus::Applied | ReceiptStatus::Accepted
                ));
                let frontier = s.snapshot().epochs;
                assert_ne!(prior, frontier);
                if revoke {
                    network(
                        &mut s,
                        "range-revoke",
                        json!({"method":"sources.grants.revoke","expected_generation":1}),
                        46,
                    );
                } else {
                    let c = command(&s, "range-stop", json!({"method":"sessions.stop"}));
                    s.submit_graph(c, 46);
                }
                let mut actual = retirement(&s);
                actual.output_epochs = if suspended { frontier } else { prior };
                let mut future = actual.clone();
                future.output_epochs = frontier.next_stream().unwrap();
                assert!(s.complete_network_retirement(future, 47).is_err());
                let mut stale = actual.clone();
                stale.binding.source_generation += 1;
                assert!(s.complete_network_retirement(stale, 47).is_err());
                s.complete_network_retirement(actual.clone(), 48).unwrap();
                if revoke {
                    network(&mut s, "restore-grants", grants(2, 100), 49);
                }
                let mut next = start_command(
                    &s,
                    "replacement-after-cleanup",
                    "replacement-session",
                    false,
                );
                if let Command::NetworkStartHttpFlac {
                    grant_generation, ..
                } = &mut next.payload
                {
                    *grant_generation = s.network_snapshot().grant_generation;
                }
                assert_eq!(
                    s.submit_with_network_effects(
                        next,
                        50,
                        |_| unreachable!(),
                        |_, _| Ok(()),
                        |_| Ok(())
                    )
                    .status,
                    ReceiptStatus::Accepted
                );
                let stop = command(&s, "stop-replacement", json!({"method":"sessions.stop"}));
                s.submit_graph(stop, 51);
                assert!(s.complete_network_retirement(actual, 52).is_err());
                assert!(s.closing_network_source().is_some());
            }
        }
    }
}

#[test]
fn diagnostic_boundary_consumer_is_strict_and_keeps_network_transition_domains() {
    use earph_core::diagnostics::SignalPathRead;
    let mut s = started();
    let ready = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(ready).unwrap();
    let pause = command(
        &s,
        "diagnostic-boundary-pause",
        json!({"method":"sessions.pause"}),
    );
    assert_eq!(s.submit_graph(pause, 31).status, ReceiptStatus::Accepted);
    let c = command(
        &s,
        "boundary-diagnostic-consumer",
        json!({"method":"signal_path.get"}),
    );
    let mut read = serde_json::to_value(s.submit_graph(c, 31)).unwrap()["result"]["value"].clone();
    let epochs = s.network_source().unwrap().source_epochs;
    let output = epochs.next_stream().unwrap();
    let format = PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    read["report"]["observed"]["network_replacement_boundary"] = json!({"source_epochs":epochs,"source_position_frames":32,"previous_output_epochs":epochs,"output_epochs":output,"previous_output_format":format,"discarded_output_frames":12});
    SignalPathRead::decode(&serde_json::to_vec(&read).unwrap())
        .expect("allowlisted numeric network boundary must decode");
    for (field, value) in [
        ("source_epochs", Value::Null),
        ("previous_output_format", Value::Null),
        ("discarded_output_frames", json!(0)),
        ("output_epochs", json!(epochs)),
        ("output_epochs", json!(output.next_stream().unwrap())),
        ("binding", json!({"private":"must-not-share"})),
    ] {
        let mut bad = read.clone();
        bad["report"]["observed"]["network_replacement_boundary"][field] = value;
        assert!(
            SignalPathRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err(),
            "invalid {field}"
        );
    }
    read["report"]["observed"]["network_replacement_boundary"] = Value::Null;
    assert!(
        SignalPathRead::decode(&serde_json::to_vec(&read).unwrap()).is_err(),
        "present optional field must not be null"
    );
}

fn pending_network_replacement() -> (Session, NetworkGraphActivationAck) {
    let mut s = started();
    let ready = event(
        &s,
        NetworkEventKind::Prepared {
            observation: observation(&s),
        },
        30,
    );
    s.advance_network(ready).unwrap();
    let first = prepare_graph(&mut s);
    let commit = command(
        &s,
        "first-boundary-graph",
        json!({"method":"graph.commit","token":first.token,"graph_hash":first.preview.graph_hash}),
    );
    assert_eq!(
        s.submit_graph(commit.clone(), 42).status,
        ReceiptStatus::Accepted
    );
    let mut ready = observation(&s);
    ready.graph_epoch += 1;
    s.complete_network_graph_activation(
        NetworkGraphActivationAck {
            request_id: commit.request_id,
            epochs: s.snapshot().epochs,
            graph_hash: first.preview.graph_hash.clone(),
            activation_frame: 0,
            observation: ready,
        },
        43,
        |_| Ok(()),
    )
    .unwrap();
    let c = command(
        &s,
        "boundary-unmute",
        json!({"method":"mute.set","muted":false}),
    );
    assert_eq!(s.submit_graph(c, 44).status, ReceiptStatus::Applied);
    let started = event(
        &s,
        NetworkEventKind::Started {
            activation_frame: 0,
        },
        45,
    );
    s.advance_network(started).unwrap();
    let mut progress = observation(&s);
    progress.lifecycle = NetworkLifecycle::Playing;
    progress.source_position_frames = 64;
    progress.submitted_frames = 20;
    let progress = event(
        &s,
        NetworkEventKind::Progress {
            observation: progress,
        },
        46,
    );
    s.advance_network(progress).unwrap();
    let prepared = prepare_graph_at(&mut s, "replacement-prepare", 50);
    let commit = command(
        &s,
        "replacement-commit",
        json!({"method":"graph.commit","token":prepared.token,"graph_hash":prepared.preview.graph_hash}),
    );
    assert_eq!(
        s.submit_graph(commit.clone(), 52).status,
        ReceiptStatus::Accepted
    );
    let previous = s.snapshot().network_playback.unwrap();
    let mut next = observation(&s);
    next.graph_epoch += 1;
    next.source_position_frames = 64;
    next.replacement_boundary = Some(Arc::new(NetworkReplacementBoundary {
        binding: next.binding.clone(),
        source_epochs: next.source_epochs,
        source_position_frames: 32,
        previous_output_epochs: previous.output_epochs,
        output_epochs: next.output_epochs,
        previous_output_format: previous.output_format,
        discarded_output_frames: 12,
    }));
    let ack = NetworkGraphActivationAck {
        request_id: commit.request_id,
        epochs: s.snapshot().epochs,
        graph_hash: prepared.preview.graph_hash.clone(),
        activation_frame: 20,
        observation: next,
    };
    (s, ack)
}
#[test]
fn network_boundary_adoption_rejects_foreign_sources_and_invalid_transition_facts() {
    let (base, ack) = pending_network_replacement();
    for invalid in [
        "binding",
        "source-epochs",
        "frontier",
        "zero-discard",
        "future-output",
        "historical-invention",
        "previous-format",
    ] {
        let mut s = base.clone();
        let before = s.snapshot();
        let mut bad = ack.clone();
        let b = Arc::make_mut(bad.observation.replacement_boundary.as_mut().unwrap());
        match invalid {
            "binding" => b.binding.source_generation += 1,
            "source-epochs" => b.source_epochs = b.source_epochs.next_stream().unwrap(),
            "frontier" => b.source_position_frames = 65,
            "zero-discard" => b.discarded_output_frames = 0,
            "future-output" => b.output_epochs = b.output_epochs.next_stream().unwrap(),
            "historical-invention" => {
                b.previous_output_epochs = b.source_epochs;
                b.output_epochs = b.source_epochs.next_stream().unwrap();
            }
            "previous-format" => {
                b.previous_output_format =
                    PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Signed24)
                        .unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            s.complete_network_graph_activation(bad, 53, |_| Ok(()))
                .is_err(),
            "accepted {invalid}"
        );
        assert_eq!(s.snapshot(), before, "published {invalid}");
    }
}
#[test]
fn network_boundary_producer_and_allowlisted_diagnostics_preserve_measured_domains() {
    use earph_core::diagnostics::SignalPathRead;
    let (mut s, ack) = pending_network_replacement();
    let boundary = ack.observation.replacement_boundary.clone().unwrap();
    let before = serde_json::to_value(s.snapshot()).unwrap();
    assert!(
        before["network_playback"]
            .get("replacement_boundary")
            .is_none()
    );
    s.complete_network_graph_activation(ack, 53, |_| Ok(()))
        .unwrap();
    let value = serde_json::to_value(s.snapshot()).unwrap();
    let parsed: NetworkReplacementBoundary =
        serde_json::from_value(value["network_playback"]["replacement_boundary"].clone()).unwrap();
    parsed.validate().unwrap();
    assert_eq!(&parsed, boundary.as_ref());
    for field in ["binding", "source_epochs", "previous_output_format"] {
        let mut bad = serde_json::to_value(&parsed).unwrap();
        bad[field] = Value::Null;
        assert!(
            serde_json::from_value::<NetworkReplacementBoundary>(bad).is_err(),
            "null {field}"
        );
    }
    let c = command(
        &s,
        "boundary-produced-diagnostic",
        json!({"method":"signal_path.get"}),
    );
    let diagnostic =
        serde_json::to_value(s.submit_graph(c, 54)).unwrap()["result"]["value"].clone();
    let read = SignalPathRead::decode(&serde_json::to_vec(&diagnostic).unwrap()).unwrap();
    let projected = read
        .report
        .observed
        .unwrap()
        .network_replacement_boundary
        .unwrap();
    assert_eq!(projected.source_position_frames, 32);
    assert_eq!(projected.discarded_output_frames, 12);
    assert_eq!(
        projected.previous_output_format,
        boundary.previous_output_format
    );
    let shared = serde_json::to_string(&projected).unwrap();
    assert!(!shared.contains("binding"));
    assert!(!shared.contains("private.example"));
    assert!(!shared.contains("staged-1"));
    let mut overflow = parsed;
    overflow.discarded_output_frames = u64::MAX;
    assert!(overflow.validate().is_err());
}
#[test]
fn retained_network_boundary_survives_pause_progress_and_zero_discard_graph_changes() {
    let (mut s, ack) = pending_network_replacement();
    let original = ack.observation.replacement_boundary.clone().unwrap();
    s.complete_network_graph_activation(ack, 53, |_| Ok(()))
        .unwrap();
    let c = command(&s, "boundary-pause", json!({"method":"sessions.pause"}));
    assert_eq!(s.submit_graph(c, 54).status, ReceiptStatus::Accepted);
    let mut p = s.snapshot().network_playback.unwrap();
    p.output_epochs = s.snapshot().epochs;
    p.lifecycle = NetworkLifecycle::Paused;
    let paused = event(
        &s,
        NetworkEventKind::Paused {
            observation: p.clone(),
        },
        55,
    );
    s.advance_network(paused).unwrap();
    p.source_position_frames += 1;
    let progress = event(
        &s,
        NetworkEventKind::Progress {
            observation: p.clone(),
        },
        56,
    );
    s.advance_network(progress).unwrap();
    assert_eq!(
        s.snapshot().network_playback.unwrap().replacement_boundary,
        Some(original.clone())
    );
    assert!(original.output_epochs.stream < s.snapshot().epochs.stream);
    let mut invented = p.clone();
    Arc::make_mut(invented.replacement_boundary.as_mut().unwrap()).source_position_frames += 1;
    let invented = event(
        &s,
        NetworkEventKind::Progress {
            observation: invented,
        },
        57,
    );
    assert!(
        s.advance_network(invented).is_err(),
        "progress invented a replacement boundary"
    );
    let prepared = prepare_graph_at(&mut s, "zero-discard-prepare", 60);
    let c = command(
        &s,
        "zero-discard-commit",
        json!({"method":"graph.commit","token":prepared.token,"graph_hash":prepared.preview.graph_hash}),
    );
    s.submit_graph(c.clone(), 62);
    p.output_epochs = s.snapshot().epochs;
    p.graph_epoch += 1;
    p.submitted_frames = 0;
    s.complete_network_graph_activation(
        NetworkGraphActivationAck {
            request_id: c.request_id,
            epochs: s.snapshot().epochs,
            graph_hash: prepared.preview.graph_hash.clone(),
            activation_frame: 0,
            observation: p,
        },
        63,
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(
        s.snapshot().network_playback.unwrap().replacement_boundary,
        Some(original)
    );
}
