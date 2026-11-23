// ABOUTME: Builds actual Core network sessions through commands and measured source events.
// ABOUTME: Supplies desktop contract fixtures without transport or hardware claims.
use earph_core::{audio::*, control::*, ids::*, network_source::*, session::Session};
use serde_json::json;

pub fn command(session: &Session, id: &str, payload: serde_json::Value) -> CommandEnvelope {
    let state = session.snapshot();
    decode_command(&serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":state.zone_id,"owner_id":state.owner_id,"session_id":state.session_id,"expected_revision":state.revision,"payload":payload})).unwrap(),&ControlLimits::default()).unwrap()
}
pub fn ready(total_frames: Option<u64>) -> Session {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("network-owner").unwrap(),
        ControlLimits::default(),
    );
    session.enable_local_playback();
    session.enable_network_playback();
    for (id, payload) in [
        (
            "queue",
            json!({"method":"queue.append","path":"retained-local-intent.flac"}),
        ),
        (
            "output",
            json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"controlled-software-output","device_id":"dac","profile_id":"profile","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}}),
        ),
        (
            "consent",
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        ),
        (
            "stage",
            json!({"method":"sources.stage_uri","uri":"http://example.invalid/track?private=token","metadata":"private"}),
        ),
    ] {
        let c = command(&session, id, payload);
        assert_eq!(session.submit(c).status, ReceiptStatus::Applied);
    }
    let c = command(
        &session,
        "grant",
        json!({"method":"sources.grants.replace","expected_generation":0,"grants":{"origins":[{"origin":"http://example.invalid:80","pins":["192.168.1.2"],"roots_der":[]}]},"lifetime_ms":60_000}),
    );
    assert_eq!(
        session
            .submit_with_network_effects(c, 10, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Applied
    );
    let mut c = command(
        &session,
        "start",
        json!({"method":"sources.start_http_flac","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac","takeover":false}),
    );
    c.session_id = Some(SessionId::new("http-session").unwrap());
    assert_eq!(
        session
            .submit_with_network_effects(c, 20, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    let active = session.network_source().unwrap();
    let state = session.snapshot();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    let observation = NetworkPlaybackObservation {
        binding: active.binding.clone(),
        source_epochs: active.source_epochs,
        output_epochs: state.epochs,
        graph_epoch: state.graph_epoch,
        device_epoch: state.device_epoch,
        lifecycle: NetworkLifecycle::Ready,
        format,
        output_format: PcmFormat::new(96000, ChannelLayout::Stereo, PcmRepresentation::Signed16)
            .unwrap(),
        total_frames,
        source_position_frames: 23,
        audible_source_position_frames: None,
        submitted_frames: 0,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "backend_consumed",
        input_peak: None,
        output_peak: Some(0.5),
        modified: true,
        backend: None,
        output_activity: None,
        processing: None,
        replacement_boundary: None,
    };
    session
        .advance_network(NetworkEvent {
            binding: active.binding.clone(),
            source_epochs: active.source_epochs,
            output_epochs: state.epochs,
            graph_epoch: state.graph_epoch,
            device_epoch: state.device_epoch,
            monotonic_ns: 30,
            kind: NetworkEventKind::Prepared { observation },
        })
        .unwrap();
    session
}

#[allow(dead_code)]
pub fn preview(session: &Session) -> earph_core::graph_control::GraphPreview {
    use earph_core::{graph::*, graph_control::*};
    let state = session.snapshot();
    let format = state.observation.network().unwrap().format;
    GraphPreview {
        diagnostic_nodes: None,
        profile_application: None,
        backend: None,
        graph_hash: "a".repeat(64),
        kernel_hash: "b".repeat(64),
        implementation_version: "controlled-core-serializer".into(),
        binding: GraphBinding {
            owner_id: state.owner_id.unwrap(),
            session_id: state.session_id.unwrap(),
            epochs: state.epochs,
            device_epoch: state.device_epoch,
            expected_revision: state.revision,
        },
        source_format: format,
        native_output_format: state.observation.network().unwrap().output_format,
        processing_input_format: format,
        processing_output_format: format,
        quantum_frames: 64,
        timing: GraphTiming::Streaming {
            stages: std::sync::Arc::from([]),
        },
        preservation: PreservationDecision::ProcessFloat64,
        transformations: vec![],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0; 2],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    }
}

// Feed controlled graph acknowledgements through Core; this proves no native discard.
#[allow(dead_code)]
pub fn replacement() -> Session {
    let mut session = ready(Some(100));
    activate(
        &mut session,
        "first-graph",
        40,
        PcmRepresentation::Signed16,
        false,
    );
    activate(
        &mut session,
        "replacement-graph",
        50,
        PcmRepresentation::Signed24,
        true,
    );
    let request = command(
        &session,
        "replacement-pause",
        json!({"method":"sessions.pause"}),
    );
    assert_eq!(
        session.submit_graph(request, 60).status,
        ReceiptStatus::Accepted
    );
    let state = session.snapshot();
    let active = session.network_source().unwrap();
    let mut observation = state.observation.network().cloned().unwrap();
    observation.output_epochs = state.epochs;
    observation.lifecycle = NetworkLifecycle::Paused;
    observation.source_position_frames += 6;
    session
        .advance_network(NetworkEvent {
            binding: active.binding.clone(),
            source_epochs: active.source_epochs,
            output_epochs: state.epochs,
            graph_epoch: state.graph_epoch,
            device_epoch: state.device_epoch,
            monotonic_ns: 61,
            kind: NetworkEventKind::Paused { observation },
        })
        .unwrap();
    session
}
fn activate(
    session: &mut Session,
    id: &str,
    now: u64,
    representation: PcmRepresentation,
    replacement: bool,
) {
    use earph_core::graph_control::{CompiledStreamStage, CompiledStreamStageKind, GraphTiming};
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let request = command(
        session,
        &format!("{id}-prepare"),
        json!({"method":"graph.prepare","graph":{"schema_version":1,"nodes":[{"id":"input","kind":{"kind":"input"},"format":port},{"id":"output","kind":{"kind":"output"},"format":port}],"edges":[{"from":"input","to":"output","from_port":0,"to_port":0}],"output":"output"},"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}),
    );
    assert_eq!(
        session.submit_graph(request, now).status,
        ReceiptStatus::Accepted
    );
    let job = session.graph_job().unwrap();
    let mut compiled = preview(session);
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let output = PcmFormat::new(48000, ChannelLayout::Stereo, representation).unwrap();
    compiled.binding = job.binding.clone();
    compiled.native_output_format = output;
    compiled.processing_input_format = format;
    compiled.processing_output_format = format;
    compiled.timing = GraphTiming::Streaming {
        stages: std::sync::Arc::from([CompiledStreamStage {
            original_nodes: vec![
                NodeId::new("input").unwrap(),
                NodeId::new("output").unwrap(),
            ],
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
    };
    session
        .complete_graph_preparation(
            &job.request_id,
            &job.binding,
            now + 1,
            Ok(std::sync::Arc::new(compiled)),
        )
        .unwrap();
    let prepared = session
        .graph_inspection()
        .prepared
        .expect("controlled prepared graph");
    let request = command(
        session,
        &format!("{id}-commit"),
        json!({"method":"graph.commit","token":prepared.token,"graph_hash":prepared.preview.graph_hash}),
    );
    let previous = session.snapshot().observation.network().cloned().unwrap();
    assert_eq!(
        session.submit_graph(request.clone(), now + 2).status,
        ReceiptStatus::Accepted
    );
    let state = session.snapshot();
    let mut observation = previous.clone();
    observation.output_epochs = state.epochs;
    observation.graph_epoch = state.graph_epoch + 1;
    observation.output_format = output;
    observation.lifecycle = NetworkLifecycle::Ready;
    if replacement {
        observation.replacement_boundary = Some(std::sync::Arc::new(NetworkReplacementBoundary {
            binding: observation.binding.clone(),
            source_epochs: observation.source_epochs,
            source_position_frames: observation.source_position_frames,
            previous_output_epochs: previous.output_epochs,
            output_epochs: state.epochs,
            previous_output_format: previous.output_format,
            discarded_output_frames: 19,
        }));
    }
    session
        .complete_network_graph_activation(
            NetworkGraphActivationAck {
                request_id: request.request_id,
                epochs: state.epochs,
                graph_hash: prepared.preview.graph_hash.clone(),
                activation_frame: 0,
                observation,
            },
            now + 3,
            |_| Ok(()),
        )
        .unwrap();
}
