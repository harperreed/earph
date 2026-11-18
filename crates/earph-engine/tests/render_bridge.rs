// ABOUTME: Exercises integer conversion, graph execution, shared gain, and native quantization.
// ABOUTME: Checks literal PCM captures, exact tail lifetimes, and prepared allocation discipline.
#[path = "../src/render.rs"]
mod render;

use earph_core::{
    ErrorCode,
    audio::{
        BlockHeader, ChannelLayout, MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation,
        PcmSamples, TimeDomain,
    },
    ids::Epochs,
};
use earph_dsp::{CompileContext, GraphBudget, GraphDocument, ProcessingPolicy, compile};
use render::PreparedRender;

fn format(channels: ChannelLayout, representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(48000, channels, representation).unwrap()
}
fn plan(
    effect: serde_json::Value,
    input: ChannelLayout,
    output: ChannelLayout,
    quantum: usize,
) -> earph_dsp::CompiledPlan {
    chain_plan(&[effect], input, output, quantum)
}
fn chain_plan(
    effects: &[serde_json::Value],
    input: ChannelLayout,
    output: ChannelLayout,
    quantum: usize,
) -> earph_dsp::CompiledPlan {
    chain_plan_with_bounds(effects, input, output, quantum, None)
}
fn chain_plan_with_bounds(
    effects: &[serde_json::Value],
    input: ChannelLayout,
    output: ChannelLayout,
    quantum: usize,
    bounds: Option<&[f64]>,
) -> earph_dsp::CompiledPlan {
    let port = |channels| serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":channels,"precision":"float64","max_quantum_frames":quantum});
    let mut nodes =
        vec![serde_json::json!({"id":"in","kind":{"kind":"input"},"format":port(input)})];
    let mut edges = Vec::new();
    let mut previous = String::from("in");
    for (index, effect) in effects.iter().enumerate() {
        let name = format!("effect{index}");
        nodes.push(serde_json::json!({"id":name,"kind":effect,"format":port(output)}));
        edges.push(serde_json::json!({"from":previous,"to":name,"from_port":0,"to_port":0}));
        previous = name;
    }
    nodes.push(serde_json::json!({"id":"out","kind":{"kind":"output"},"format":port(output)}));
    edges.push(serde_json::json!({"from":previous,"to":"out","from_port":0,"to_port":0}));
    let document: GraphDocument = serde_json::from_value(
        serde_json::json!({"schema_version":1,"nodes":nodes,"edges":edges,"output":"out"}),
    )
    .unwrap();
    let context = CompileContext {
        input_format: format(input, PcmRepresentation::Float64),
        output_format: format(output, PcmRepresentation::Float64),
        quantum_frames: quantum,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    };
    match bounds {
        Some(bounds) => earph_dsp::compile_with_input_bounds(&document, &context, &[], bounds),
        None => compile(&document, &context),
    }
    .unwrap()
}
fn prepare(
    effect: serde_json::Value,
    representation: PcmRepresentation,
    quantum: usize,
) -> PreparedRender {
    let native = format(ChannelLayout::Stereo, representation);
    PreparedRender::prepare(
        plan(
            effect,
            ChannelLayout::Stereo,
            ChannelLayout::Stereo,
            quantum,
        ),
        native,
        native,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap()
}
fn block(representation: PcmRepresentation, samples: &[i64]) -> PcmBlock {
    let pcm = match representation {
        PcmRepresentation::Signed16 => {
            PcmSamples::Signed16(samples.iter().map(|n| *n as i16).collect())
        }
        PcmRepresentation::Signed24 => {
            PcmSamples::Signed24(samples.iter().map(|n| *n as i32).collect())
        }
        PcmRepresentation::Signed32 => {
            PcmSamples::Signed32(samples.iter().map(|n| *n as i32).collect())
        }
        PcmRepresentation::Float64 => unreachable!(),
    };
    PcmBlock::new(
        BlockHeader {
            epochs: Epochs::default(),
            sequence: 0,
            frame_position: 0,
            frame_count: (samples.len() / 2) as u64,
            format: format(ChannelLayout::Stereo, representation),
            time_domain: TimeDomain::LocalFrames,
        },
        pcm,
    )
    .unwrap()
}
fn capture(render: &PreparedRender) -> Vec<i64> {
    let count = render
        .period()
        .header()
        .format
        .scalar_count(render.period().header().frame_count)
        .unwrap();
    (0..count)
        .map(|index| render.period().integer_sample(index).unwrap())
        .collect()
}

#[test]
fn graph_and_safety_gain_precede_the_only_quantization() {
    let mut render = prepare(
        serde_json::json!({"kind":"gain","gain_linear":3.0}),
        PcmRepresentation::Signed16,
        4,
    );
    render.set_gain_target(Some(-6.020599913279624), 0).unwrap();
    let packet = block(PcmRepresentation::Signed16, &[1, -1, 20000, -20000]);
    let report = render.process(&packet, 0, 2).unwrap();
    assert_eq!(capture(&render), [2, -2, 30000, -30000]);
    assert_eq!(report.valid_frames, 2);
    assert_eq!(report.source_frames, 2);
    assert_eq!(report.saturation_count, 0);
    assert_eq!(report.input_peak, 0.6103515625);
    assert_eq!(report.graph_peak, 1.8310546875);
    assert_eq!(report.prequantization_peak, 0.91552734375);
    assert_eq!(report.output_peak, 0.91552734375);
}

#[test]
fn quantization_rounds_signed_ties_and_saturates_each_native_format() {
    for (representation, minimum, maximum) in [
        (PcmRepresentation::Signed16, -32768, 32767),
        (PcmRepresentation::Signed24, -8388608, 8388607),
        (PcmRepresentation::Signed32, -2147483648, 2147483647),
    ] {
        let mut render = prepare(
            serde_json::json!({"kind":"gain","gain_linear":0.5}),
            representation,
            4,
        );
        render.set_gain_target(Some(0.0), 0).unwrap();
        render
            .process(&block(representation, &[1, -1, 3, -3]), 0, 2)
            .unwrap();
        assert_eq!(capture(&render), [1, -1, 2, -2]);
        let mut render = prepare(
            serde_json::json!({"kind":"gain","gain_linear":2.0}),
            representation,
            4,
        );
        render.set_gain_target(Some(0.0), 0).unwrap();
        let report = render
            .process(&block(representation, &[minimum, maximum]), 0, 1)
            .unwrap();
        assert_eq!(capture(&render), [minimum, maximum]);
        assert_eq!(report.saturation_count, 2);
        assert_eq!(report.output_peak, 1.0);
    }
}

#[test]
fn compatible_short_spans_preserve_delay_and_drain_the_exact_tail_once() {
    let mut render = prepare(
        serde_json::json!({"kind":"delay","delay_frames":3}),
        PcmRepresentation::Signed16,
        2,
    );
    render.set_gain_target(Some(0.0), 0).unwrap();
    let packet = block(PcmRepresentation::Signed16, &[1, 10, 2, 20, 3, 30]);
    let report = render.process(&packet, 0, 2).unwrap();
    assert_eq!((report.latency_frames, report.tail_frames), (3, 3));
    assert_eq!(capture(&render), [0, 0, 0, 0]);
    render.process(&packet, 2, 1).unwrap();
    assert_eq!(capture(&render), [0, 0]);
    let report = render.finish().unwrap();
    assert_eq!(capture(&render), [1, 10, 2, 20]);
    assert_eq!(report.source_frames, 0);
    assert!(!report.tail_finished);
    let report = render.finish().unwrap();
    assert_eq!(capture(&render), [3, 30]);
    assert_eq!(report.valid_frames, 1);
    assert!(report.tail_finished);
    assert_eq!(render.finish().unwrap().valid_frames, 0);
    assert_eq!(capture(&render), [3, 30]);
    assert!(render.process(&packet, 0, 1).is_err());
    let epochs = Epochs {
        session: 0,
        stream: 1,
    };
    render.reset(epochs).unwrap();
    assert_eq!(render.graph().epochs(), epochs);
    let mut fresh_packet = packet.clone();
    fresh_packet.set_span(epochs, 0, 0, 3).unwrap();
    render.process(&fresh_packet, 0, 1).unwrap();
    assert_eq!(capture(&render), [0, 0]);
}

#[test]
fn stale_epoch_blocks_preserve_period_gain_and_delay_without_allocating() {
    let mut render = prepare(
        serde_json::json!({"kind":"delay","delay_frames":1}),
        PcmRepresentation::Signed16,
        2,
    );
    let epochs = Epochs {
        session: 3,
        stream: 7,
    };
    render.reset(epochs).unwrap();
    render.set_gain_target(Some(0.0), 4).unwrap();
    let mut current = block(PcmRepresentation::Signed16, &[1000, -1000, 2000, -2000]);
    current.set_span(epochs, 0, 0, 2).unwrap();
    render.process(&current, 0, 1).unwrap();
    assert_eq!(capture(&render), [0, 0]);
    let pending_header = render.period().header();
    for stale_epochs in [
        Epochs {
            session: 3,
            stream: 6,
        },
        Epochs {
            session: 2,
            stream: 7,
        },
        Epochs {
            session: 4,
            stream: 7,
        },
    ] {
        let mut stale = block(PcmRepresentation::Signed16, &[9000, -9000]);
        stale.set_span(stale_epochs, 0, 0, 1).unwrap();
        let (result, counts) = tracked(|| render.process(&stale, 0, 1));
        assert_eq!(counts, AllocationCounts::default());
        assert_eq!(result.unwrap_err().code, ErrorCode::StaleSession);
        assert_eq!(render.period().header(), pending_header);
        assert_eq!(capture(&render), [0, 0]);
    }
    render.process(&current, 1, 1).unwrap();
    assert_eq!(capture(&render), [500, -500]);
    assert_eq!(render.period().header().sequence, 1);
    assert_eq!(render.period().header().frame_position, 1);
    assert_eq!(render.finish().unwrap().valid_frames, 1);
    assert_eq!(capture(&render), [1500, -1500]);
}

#[test]
fn invalid_geometry_and_gain_changes_preserve_the_pending_period_and_envelope() {
    let mut render = prepare(
        serde_json::json!({"kind":"gain","gain_linear":1.0}),
        PcmRepresentation::Signed16,
        2,
    );
    let packet = block(PcmRepresentation::Signed16, &[100, 100, 100, 100]);
    render.set_gain_target(Some(0.0), 4).unwrap();
    render.process(&packet, 0, 1).unwrap();
    assert_eq!(capture(&render), [25, 25]);
    for (offset, frames) in [(0, 0), (0, 3), (2, 1), (usize::MAX, 1), (1, usize::MAX)] {
        assert!(render.process(&packet, offset, frames).is_err());
        assert_eq!(capture(&render), [25, 25]);
    }
    assert!(render.set_gain_target(Some(f64::NAN), 0).is_err());
    assert!(render.set_gain_target(Some(1.0), 0).is_err());
    assert!(render.set_gain_target(Some(0.0), 48001).is_err());
    render.process(&packet, 1, 1).unwrap();
    assert_eq!(capture(&render), [50, 50]);
}

#[test]
fn input_bound_failure_retains_diagnostics_and_pending_output() {
    let native = format(ChannelLayout::Stereo, PcmRepresentation::Signed16);
    let graph = chain_plan_with_bounds(
        &[
            serde_json::json!({"kind":"gain","gain_linear":f64::MAX}),
            serde_json::json!({"kind":"gain","gain_linear":f64::MAX}),
        ],
        ChannelLayout::Stereo,
        ChannelLayout::Stereo,
        2,
        Some(&[0.0, 0.0]),
    );
    let mut render = PreparedRender::prepare(
        graph,
        native,
        native,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    render.set_gain_target(Some(0.0), 0).unwrap();
    let packet = block(PcmRepresentation::Signed16, &[0, 0, 1, -1]);
    render.process(&packet, 0, 1).unwrap();
    let before = capture(&render);
    let error = render.process(&packet, 1, 1).unwrap_err();
    assert_eq!(error.code, ErrorCode::OutputFault);
    let index = error.failure_node_index.unwrap();
    assert_eq!(render.graph().plan().schedule()[index].id().as_str(), "in");
    assert_eq!(render.finish().unwrap_err(), error);
    assert_eq!(render.process(&packet, 0, 1).unwrap_err(), error);
    assert_eq!(capture(&render), before);
}

#[test]
fn matrix_channels_and_integer_width_conversion_reach_real_native_bytes() {
    let input = format(ChannelLayout::Stereo, PcmRepresentation::Signed16);
    let native = format(ChannelLayout::Mono, PcmRepresentation::Signed24);
    let graph = plan(
        serde_json::json!({"kind":"matrix","input_channels":"stereo","coefficients":[0.5,-0.25]}),
        ChannelLayout::Stereo,
        ChannelLayout::Mono,
        2,
    );
    let mut render = PreparedRender::prepare(
        graph,
        input,
        native,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    render.set_gain_target(Some(0.0), 0).unwrap();
    let report = render
        .process(
            &block(PcmRepresentation::Signed16, &[16384, -16384, -8192, 8192]),
            0,
            2,
        )
        .unwrap();
    assert_eq!(capture(&render), [3145728, -1572864]);
    assert_eq!(report.input_peak, 0.5);
    assert_eq!(report.output_peak, 0.375);
    let mut bytes = [0_u8; 6];
    assert_eq!(render.period().pack_le(&mut bytes).unwrap(), 6);
    assert_eq!(bytes, [0x00, 0x00, 0x30, 0x00, 0x00, 0xe8]);
}

#[test]
fn admission_counts_both_conversion_buffers_and_actual_kernel_capacity() {
    let native = format(ChannelLayout::Stereo, PcmRepresentation::Signed24);
    let graph = plan(
        serde_json::json!({"kind":"delay","delay_frames":3}),
        ChannelLayout::Stereo,
        ChannelLayout::Stereo,
        2,
    );
    let required = render::RenderResources::required(&graph, native).unwrap();
    assert_eq!(required.kernel_audio_bytes, 144);
    assert_eq!(required.host_input_bytes, 32);
    assert_eq!(required.host_output_bytes, 32);
    assert_eq!(required.native_period_bytes, 16);
    assert_eq!(required.audio_payload_bytes, 224);
    let error = PreparedRender::prepare(
        graph.clone(),
        native,
        native,
        Epochs::default(),
        MemoryBudget::new(223).unwrap(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::ResourceBudgetExceeded);
    let render = PreparedRender::prepare(
        graph,
        native,
        native,
        Epochs::default(),
        MemoryBudget::new(224).unwrap(),
    )
    .unwrap();
    assert_eq!(render.resources(), required);
    assert_eq!(render.graph().resident_audio_bytes(), Some(144));
}

#[test]
fn format_rejection_and_cancel_leave_pending_output_intact() {
    let mut render = prepare(
        serde_json::json!({"kind":"gain","gain_linear":1.0}),
        PcmRepresentation::Signed16,
        2,
    );
    render.set_gain_target(Some(0.0), 0).unwrap();
    let input = block(PcmRepresentation::Signed16, &[111, -222]);
    render.process(&input, 0, 1).unwrap();
    let wrong = block(PcmRepresentation::Signed32, &[111, -222]);
    assert_eq!(
        render.process(&wrong, 0, 1).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    assert_eq!(capture(&render), [111, -222]);
    let float = PcmBlock::new(
        BlockHeader {
            format: format(ChannelLayout::Stereo, PcmRepresentation::Float64),
            ..input.header()
        },
        PcmSamples::Float64(vec![1.0, 2.0]),
    )
    .unwrap();
    assert_eq!(
        render.process(&float, 0, 1).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    render.cancel();
    assert_eq!(
        render.process(&input, 0, 1).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        render.finish().unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert!(
        render
            .reset(Epochs {
                session: 0,
                stream: 1
            })
            .is_err()
    );
    assert_eq!(capture(&render), [111, -222]);
}
#[path = "support/allocations.rs"]
mod allocations;
use allocations::{AllocationCounts, tracked};

#[test]
fn prepared_numeric_spans_and_native_packing_allocate_and_free_nothing() {
    let (calibration, counts) = tracked(|| std::hint::black_box(vec![1_u8; 128]));
    assert!(counts.allocations > 0);
    assert!(tracked(|| drop(calibration)).1.frees > 0);
    let mut render = prepare(
        serde_json::json!({"kind":"delay","delay_frames":3}),
        PcmRepresentation::Signed16,
        2,
    );
    let packet = block(PcmRepresentation::Signed16, &[1, -1, 3, -3, 5, -5]);
    let mut current_epoch_packet = block(PcmRepresentation::Signed16, &[1, -1]);
    current_epoch_packet
        .set_span(
            Epochs {
                session: 0,
                stream: 1,
            },
            0,
            0,
            1,
        )
        .unwrap();
    let mut bytes = [0_u8; 8];
    let (_, counts) = tracked(|| {
        render.set_gain_target(Some(0.0), 4).unwrap();
        assert_eq!(render.process(&packet, 0, 2).unwrap().valid_frames, 2);
        assert_eq!(render.period().pack_le(&mut bytes).unwrap(), 8);
        assert_eq!(render.process(&packet, 2, 1).unwrap().valid_frames, 1);
        assert!(render.set_gain_target(Some(f64::INFINITY), 0).is_err());
        assert!(render.process(&packet, usize::MAX, 1).is_err());
        assert_eq!(render.finish().unwrap().valid_frames, 2);
        render.period().pack_le(&mut bytes).unwrap();
        assert_eq!(bytes, [1, 0, 255, 255, 3, 0, 253, 255]);
        assert_eq!(render.finish().unwrap().valid_frames, 1);
        assert_eq!(render.finish().unwrap().valid_frames, 0);
        render
            .reset(Epochs {
                session: 0,
                stream: 1,
            })
            .unwrap();
        assert!(render.reset(Epochs::default()).is_err());
        render.cancel();
        assert_eq!(
            render
                .process(&current_epoch_packet, 0, 1)
                .unwrap_err()
                .code,
            ErrorCode::OperationCancelled
        );
        assert!(render.finish().is_err());
    });
    assert_eq!(counts, AllocationCounts::default());
}

#[test]
fn terminal_bound_failure_and_repeated_diagnostics_allocate_and_free_nothing() {
    let native = format(ChannelLayout::Stereo, PcmRepresentation::Signed16);
    let graph = chain_plan_with_bounds(
        &[
            serde_json::json!({"kind":"gain","gain_linear":f64::MAX}),
            serde_json::json!({"kind":"gain","gain_linear":2.0}),
        ],
        ChannelLayout::Stereo,
        ChannelLayout::Stereo,
        2,
        Some(&[0.0, 0.0]),
    );
    let mut render = PreparedRender::prepare(
        graph,
        native,
        native,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    let packet = block(PcmRepresentation::Signed16, &[-32768, 32767]);
    let (_, counts) = tracked(|| {
        let error = render.process(&packet, 0, 1).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputFault);
        assert_eq!(render.finish().unwrap_err(), error);
        assert_eq!(render.process(&packet, 0, 1).unwrap_err(), error);
    });
    assert_eq!(counts, AllocationCounts::default());
}

#[test]
fn finite_extreme_headroom_saturates_at_quantization_without_hidden_graph_clipping() {
    let mut render = prepare(
        serde_json::json!({"kind":"gain","gain_linear":f64::MAX}),
        PcmRepresentation::Signed32,
        2,
    );
    render.set_gain_target(Some(0.0), 0).unwrap();
    let report = render
        .process(
            &block(PcmRepresentation::Signed32, &[-2147483648, 2147483647]),
            0,
            1,
        )
        .unwrap();
    assert_eq!(capture(&render), [-2147483648, 2147483647]);
    assert_eq!(report.saturation_count, 2);
    assert_eq!(report.graph_peak, f64::MAX);
    assert_eq!(report.prequantization_peak, f64::MAX);
}

#[test]
fn real_wav_decode_to_processed_native_period_preserves_the_owned_source() {
    use earph_media::LocalFile;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/render-bridge")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("owned-literal.wav");
    let payload = [1_i16, -1, 100, -100, 32767, -32768];
    let mut wave = Vec::new();
    wave.extend(b"RIFF");
    wave.extend(48_u32.to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(16_u32.to_le_bytes());
    wave.extend(1_u16.to_le_bytes());
    wave.extend(2_u16.to_le_bytes());
    wave.extend(48000_u32.to_le_bytes());
    wave.extend(192000_u32.to_le_bytes());
    wave.extend(4_u16.to_le_bytes());
    wave.extend(16_u16.to_le_bytes());
    wave.extend(b"data");
    wave.extend(12_u32.to_le_bytes());
    for sample in payload {
        wave.extend(sample.to_le_bytes());
    }
    fs::write(&source, &wave).unwrap();
    let mut file = LocalFile::open(&source, Epochs::default()).unwrap();
    let packet = file.next_block().unwrap().unwrap();
    assert_eq!(packet.header().frame_count, 3);
    let mut render = prepare(
        serde_json::json!({"kind":"gain","gain_linear":0.5}),
        PcmRepresentation::Signed16,
        2,
    );
    render.set_gain_target(Some(0.0), 0).unwrap();
    let mut bytes = [0_u8; 12];
    let (_, counts) = tracked(|| {
        render.process(&packet, 0, 2).unwrap();
        render.period().pack_le(&mut bytes[..8]).unwrap();
        assert_eq!(bytes[..8], [1, 0, 255, 255, 50, 0, 206, 255]);
        render.process(&packet, 2, 1).unwrap();
        assert_eq!(render.period().pack_le(&mut bytes[8..]).unwrap(), 4);
        assert_eq!(bytes[8..], [0, 64, 0, 192]);
        assert_eq!(render.finish().unwrap().valid_frames, 0);
    });
    assert_eq!(counts, AllocationCounts::default());
    assert!(file.next_block().unwrap().is_none());
    assert_eq!(fs::read(&source).unwrap(), wave);
    fs::write(directory.join("native-capture.pcm"), bytes).unwrap();
    assert_eq!(
        fs::read(directory.join("native-capture.pcm")).unwrap(),
        [1, 0, 255, 255, 50, 0, 206, 255, 0, 64, 0, 192]
    );
}
