// ABOUTME: Checks explicit rate-domain timing through real Core serialization and validation.
// ABOUTME: Retains same-rate wire values and rejects fabricated or inconsistent streaming facts.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    dsp_nodes::{SrcParameters, SrcPreset, SrcResources},
    graph::PreservationDecision,
    graph_control::*,
    ids::*,
    session::PlaybackProcessingObservation,
};
use std::sync::Arc;

fn format(rate: u32, representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(rate, ChannelLayout::Stereo, representation).unwrap()
}
fn preview() -> GraphPreview {
    GraphPreview {
        diagnostic_nodes: None,
        profile_application: None,
        backend: None,
        graph_hash: "a".repeat(64),
        kernel_hash: "b".repeat(64),
        implementation_version: "timing-contract".into(),
        binding: GraphBinding {
            owner_id: OwnerId::new("owner").unwrap(),
            session_id: SessionId::new("session").unwrap(),
            epochs: Epochs::default(),
            device_epoch: 0,
            expected_revision: 1,
        },
        source_format: format(48000, PcmRepresentation::Signed16),
        native_output_format: format(48000, PcmRepresentation::Signed16),
        processing_input_format: format(48000, PcmRepresentation::Float64),
        processing_output_format: format(48000, PcmRepresentation::Float64),
        quantum_frames: 64,
        timing: GraphTiming::SameRate {
            latency_frames: 123,
            tail_frames: 456,
            tail_policy: GraphTailPolicy::BoundedIirTruncation,
            terminal_output_bounds: vec![1e-13; 2],
            node_tails: vec![FilterTailObservation {
                node: NodeId::new("filter").unwrap(),
                channels: ChannelLayout::Stereo,
                horizon_frames: 400,
                terminal_output_bounds: vec![1e-13; 2],
            }],
        },
        preservation: PreservationDecision::ProcessFloat64,
        transformations: vec![],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0; 2],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    }
}
fn processing(timing: PlaybackTiming) -> PlaybackProcessingObservation {
    PlaybackProcessingObservation {
        resident_bytes: 1024,
        audio_payload_bytes: Some(64),
        failure_node: None,
        graph_hash: "a".repeat(64),
        timing,
        streaming: None,
        source_boundary: None,
        saturation_count: 0,
        graph_peak: None,
        prequantization_peak: None,
    }
}
fn streaming() -> Arc<[CompiledStreamStage]> {
    Arc::from(vec![CompiledStreamStage {
        original_nodes: vec![NodeId::new("src").unwrap()],
        input_format: format(48000, PcmRepresentation::Float64),
        output_format: format(44100, PcmRepresentation::Float64),
        input_peak_bounds: vec![1.0; 2],
        output_peak_bounds: vec![1.0; 2],
        kind: CompiledStreamStageKind::Src {
            parameters: SrcParameters {
                input_rate: 48000,
                output_rate: 44100,
                channels: ChannelLayout::Stereo,
                preset: SrcPreset::Responsive,
                chunk_frames: 64,
                filter_length: 256,
                phases: 256,
                explicit_pre_scaling_cutoff: None,
                pre_scaling_cutoff: 0.9,
                drain_zero_calls: 9,
                max_output_frames: 80,
                guard_time_numerator: 1,
                library_max_relative_ratio: 1.001,
                resources: SrcResources {
                    resident_bytes: 1024,
                    preparation_peak_bytes: 2048,
                },
            },
            coefficient_fingerprint: [1; 32],
            parameter_fingerprint: [2; 32],
            guard_time_denominator: 48000,
            maximum_adapter_finish_output_frames: 880,
            truncated_library_output_delay_frames: None,
        },
    }])
}
fn stream_preview(stages: Arc<[CompiledStreamStage]>) -> GraphPreview {
    let mut p = preview();
    p.native_output_format = format(44100, PcmRepresentation::Signed16);
    p.processing_output_format = format(44100, PcmRepresentation::Float64);
    p.timing = GraphTiming::Streaming { stages };
    p.resources.aggregate_peak_bytes = 4096;
    p
}

#[test]
fn streaming_facts_cannot_understate_src_peak_resources_or_claim_unchanged_samples() {
    let mut p = stream_preview(streaming());
    p.resources.aggregate_peak_bytes = 1024;
    assert!(p.validate().is_err());
    for preservation in [
        PreservationDecision::RetainInteger,
        PreservationDecision::UnchangedFloat64,
    ] {
        let mut p = stream_preview(streaming());
        p.preservation = preservation;
        assert!(p.validate().is_err());
    }
}

#[test]
fn same_rate_preview_and_playback_keep_existing_numeric_wire_meaning() {
    let p = preview();
    p.validate().unwrap();
    let wire = serde_json::to_value(&p).unwrap();
    assert_eq!(wire["timing_kind"], "same_rate");
    assert_eq!(wire["latency_frames"], 123);
    assert_eq!(wire["tail_frames"], 456);
    assert_eq!(wire["tail_policy"], "bounded_iir_truncation");
    assert_eq!(
        wire["terminal_output_bounds"],
        serde_json::json!([1e-13, 1e-13])
    );
    assert_eq!(wire["node_tails"][0]["horizon_frames"], 400);
    assert!(wire.get("stages").is_none());
    let playback = serde_json::to_value(processing(p.timing.playback_timing())).unwrap();
    assert_eq!(playback["timing_kind"], "same_rate");
    assert_eq!(playback["latency_frames"], 123);
    assert_eq!(playback["tail_frames"], 456);
    assert_eq!(playback["audio_payload_bytes"], 64);
    assert!(playback.get("streaming").is_none());
    assert!(playback.get("source_boundary").is_none());
    assert!(playback.get("node_tails").is_none());
}
#[test]
fn streaming_runtime_keeps_source_and_output_counters_distinct_and_shares_the_boundary() {
    use earph_core::session::{StreamingReplacementBoundary, StreamingRuntimeObservation};
    let boundary = Arc::new(StreamingReplacementBoundary {
        source_entry_id: QueueEntryId::new("entry").unwrap(),
        source_position_frames: 31,
        source_epochs: Epochs {
            session: 1,
            stream: 2,
        },
        discarded_output_frames: 17,
    });
    let mut observation = processing(PlaybackTiming::Streaming {
        stages: streaming(),
    });
    observation.audio_payload_bytes = None;
    observation.source_boundary = Some(boundary.clone());
    observation.streaming = Some(StreamingRuntimeObservation {
        source_epochs: Epochs {
            session: 1,
            stream: 3,
        },
        output_epochs: Epochs {
            session: 1,
            stream: 4,
        },
        source_accepted_frames: 64,
        generated_output_frames: 37,
        submission_frontier_frames: 29,
        backend_origin_frames: 11,
        retained_start_frames: 13,
    });
    let cloned = observation.clone();
    assert!(Arc::ptr_eq(
        cloned.source_boundary.as_ref().unwrap(),
        &boundary
    ));
    let wire = serde_json::to_value(cloned).unwrap();
    assert!(wire["audio_payload_bytes"].is_null());
    assert_eq!(wire["streaming"]["source_accepted_frames"], 64);
    assert_eq!(wire["streaming"]["generated_output_frames"], 37);
    assert_eq!(wire["streaming"]["submission_frontier_frames"], 29);
    assert_eq!(wire["streaming"]["backend_origin_frames"], 11);
    assert_eq!(wire["source_boundary"]["source_position_frames"], 31);
    assert_eq!(wire["source_boundary"]["discarded_output_frames"], 17);
    assert!(wire.get("latency_frames").is_none());
}
#[test]
fn streaming_shares_stage_facts_without_fabricating_aggregate_timing() {
    let stages = streaming();
    let p = stream_preview(stages.clone());
    p.validate().unwrap();
    let timing = p.timing.playback_timing();
    let PlaybackTiming::Streaming { stages: observed } = &timing else {
        panic!("streaming facts lost")
    };
    assert!(Arc::ptr_eq(&stages, observed));
    for value in [
        serde_json::to_value(&p).unwrap(),
        serde_json::to_value(processing(timing)).unwrap(),
    ] {
        assert_eq!(value["timing_kind"], "streaming");
        assert_eq!(value["stages"][0]["input_format"]["sample_rate_hz"], 48000);
        for key in [
            "latency_frames",
            "tail_frames",
            "tail_policy",
            "terminal_output_bounds",
            "node_tails",
        ] {
            assert!(value.get(key).is_none(), "fabricated {key}");
        }
    }
}
#[test]
fn malformed_streaming_facts_reject_empty_capacity_geometry_and_nonfinite_bounds() {
    assert!(stream_preview(Arc::from([])).validate().is_err());
    let stage = streaming()[0].clone();
    assert!(
        stream_preview(Arc::from(vec![stage.clone(); 129]))
            .validate()
            .is_err()
    );
    for mutation in 0..8 {
        let mut stage = stage.clone();
        match mutation {
            0 => stage.input_peak_bounds[0] = f64::NAN,
            1 => {
                stage.output_peak_bounds.pop();
            }
            2 => stage.input_format = format(96000, PcmRepresentation::Float64),
            3 => stage.original_nodes.clear(),
            4 => {
                if let CompiledStreamStageKind::Src { parameters, .. } = &mut stage.kind {
                    parameters.pre_scaling_cutoff = f32::INFINITY;
                }
            }
            5 => {
                if let CompiledStreamStageKind::Src {
                    guard_time_denominator,
                    ..
                } = &mut stage.kind
                {
                    *guard_time_denominator = 0;
                }
            }
            6 => {
                if let CompiledStreamStageKind::Src { parameters, .. } = &mut stage.kind {
                    parameters.resources.preparation_peak_bytes = 0;
                }
            }
            _ => stage.output_format = format(44100, PcmRepresentation::Signed16),
        }
        assert!(
            stream_preview(Arc::from(vec![stage])).validate().is_err(),
            "mutation{mutation}"
        );
    }
}

#[test]
fn streaming_chain_rejects_stale_endpoints_bounds_terminal_facts_and_intermediate_iir() {
    let src = streaming()[0].clone();
    let island = CompiledStreamStage {
        original_nodes: vec![NodeId::new("out").unwrap()],
        input_format: src.output_format,
        output_format: src.output_format,
        input_peak_bounds: vec![1.0; 2],
        output_peak_bounds: vec![1.0; 2],
        kind: CompiledStreamStageKind::Island {
            kernel_hash: "c".repeat(64),
            local_latency_frames: 256,
            emitted_tail_frames: 258,
            bounded_iir_truncation: false,
            terminal_output_bounds: vec![0.0; 2],
        },
    };
    stream_preview(Arc::from(vec![src.clone(), island.clone()]))
        .validate()
        .unwrap();
    for mutation in 0..6 {
        let mut last = island.clone();
        let mut p = stream_preview(Arc::from(vec![src.clone(), last.clone()]));
        match mutation {
            0 => last.input_format = src.input_format,
            1 => last.input_peak_bounds[0] = 0.5,
            2 => {
                if let CompiledStreamStageKind::Island {
                    terminal_output_bounds,
                    ..
                } = &mut last.kind
                {
                    terminal_output_bounds[0] = f64::INFINITY;
                }
            }
            3 => last.original_nodes = src.original_nodes.clone(),
            4 => p.channel_peak_bounds[0] = 0.5,
            _ => p.processing_output_format = format(48000, PcmRepresentation::Float64),
        }
        p.timing = GraphTiming::Streaming {
            stages: Arc::from(vec![src.clone(), last]),
        };
        assert!(p.validate().is_err(), "mutation{mutation}");
    }
    let mut iir = island.clone();
    iir.input_format = src.input_format;
    iir.output_format = src.input_format;
    if let CompiledStreamStageKind::Island {
        bounded_iir_truncation,
        ..
    } = &mut iir.kind
    {
        *bounded_iir_truncation = true;
    }
    assert!(
        stream_preview(Arc::from(vec![iir, src]))
            .validate()
            .is_err()
    );
    let mut without_src = stream_preview(Arc::from(vec![island]));
    without_src.source_format = format(44100, PcmRepresentation::Signed16);
    without_src.processing_input_format = format(44100, PcmRepresentation::Float64);
    assert!(without_src.validate().is_err());
    without_src.timing = preview().timing;
    without_src.validate().unwrap();
}
