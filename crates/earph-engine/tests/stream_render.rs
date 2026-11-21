// ABOUTME: Checks actual source acceptance, native staging, and pre-volume held replay.
// ABOUTME: Keeps SRC state independent from native writes and applies one final quantization.
use earph_core::{
    ErrorCode,
    audio::{
        BlockHeader, ChannelLayout, PcmBlock, PcmFormat, PcmRepresentation, PcmSamples, TimeDomain,
    },
    ids::Epochs,
};
use earph_dsp::{
    CompileContext, GraphBudget, GraphDocument, ProcessingPolicy, compile_with_input_bounds,
    stream::{StreamBudget, StreamPlan, StreamStagePlan},
};
use earph_engine::stream_render::{PreparedStreamRender, SourceSpan, StreamRenderConfig};
use std::sync::{Arc, atomic::AtomicBool};
#[path = "support/allocations.rs"]
mod allocations;

fn epoch(stream: u64) -> Epochs {
    Epochs { session: 1, stream }
}
fn format(representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(48000, ChannelLayout::Mono, representation).unwrap()
}
fn plan() -> StreamPlan {
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"mono","precision":"float64","max_quantum_frames":4});
    let graph:GraphDocument=serde_json::from_value(serde_json::json!({"schema_version":1,"nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"})).unwrap();
    let compiled = compile_with_input_bounds(
        &graph,
        &CompileContext {
            input_format: format(PcmRepresentation::Float64),
            output_format: format(PcmRepresentation::Float64),
            quantum_frames: 4,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        },
        &[],
        &[1.0],
    )
    .unwrap();
    StreamPlan::new(
        vec![StreamStagePlan::SameRate(Box::new(compiled))],
        &[1.0],
        StreamBudget::default(),
    )
    .unwrap()
}
fn config() -> StreamRenderConfig {
    StreamRenderConfig {
        input_format: format(PcmRepresentation::Signed16),
        native_format: format(PcmRepresentation::Signed16),
        period_frames: 2,
        queue_frames: 4,
        epochs: epoch(0),
        budget_bytes: 64 << 20,
    }
}
fn block() -> PcmBlock {
    PcmBlock::new(
        BlockHeader {
            format: format(PcmRepresentation::Signed16),
            epochs: epoch(0),
            sequence: 0,
            frame_position: 0,
            frame_count: 4,
            time_domain: TimeDomain::LocalFrames,
        },
        PcmSamples::Signed16(vec![1000, 2000, 3000, 4000]),
    )
    .unwrap()
}

#[test]
fn leaving_exact_zero_applies_the_requested_ramp_to_retained_pre_gain_output() {
    for ramp in [0, 2] {
        let mut render =
            PreparedStreamRender::prepare(plan(), config(), Arc::new(AtomicBool::new(false)))
                .unwrap();
        render.set_gain_target(None, 0).unwrap();
        let input = block();
        let mut offset = 0;
        for _ in 0..20 {
            let step = render
                .pump(Some(SourceSpan {
                    block: &input,
                    offset,
                    frames: 4 - offset,
                }))
                .unwrap();
            offset += step.consumed_frames;
            if offset == 4 {
                break;
            }
        }
        assert_eq!(offset, 4);
        for _ in 0..20 {
            if render.output_positions().generated == 4 {
                break;
            }
            render.pump(None).unwrap();
        }
        assert_eq!(
            render.output_positions().generated,
            4,
            "source frontier {offset}; {:?}",
            render.stream().stats()
        );
        assert_eq!(render.stage().unwrap().unwrap().frames, 2);
        assert_eq!(render.period().integer_sample(0), Some(0));
        assert_eq!(render.period().integer_sample(1), Some(0));
        render.accepted(2).unwrap();
        render.observe(2, 2).unwrap();
        let stats = render.stream().stats();
        let generated = render.output_positions().generated;
        render.set_gain_target(Some(0.0), ramp).unwrap();
        assert_eq!(render.stage().unwrap().unwrap().frames, 2);
        assert_eq!(render.period().header().frame_position, 2);
        assert_eq!(
            render.period().integer_sample(0),
            Some(if ramp == 0 { 3000 } else { 1500 })
        );
        assert_eq!(render.period().integer_sample(1), Some(4000));
        assert_eq!(render.stream().stats(), stats);
        assert_eq!(render.output_positions().generated, generated);
    }
}

#[test]
fn partial_native_writes_and_holds_preserve_source_frontier_and_reapply_current_volume() {
    let mut render =
        PreparedStreamRender::prepare(plan(), config(), Arc::new(AtomicBool::new(false))).unwrap();
    render.set_gain_target(Some(0.0), 0).unwrap();
    let input = block();
    let mut offset = 0;
    for _ in 0..20 {
        let step = render
            .pump(Some(SourceSpan {
                block: &input,
                offset,
                frames: 4 - offset,
            }))
            .unwrap();
        offset += step.consumed_frames;
        if offset == 4 {
            break;
        }
    }
    assert_eq!(offset, 4);
    let report = render.stage().unwrap().unwrap();
    assert_eq!(report.frames, 2);
    assert_eq!(render.period().integer_sample(0), Some(1000));
    render.accepted(1).unwrap();
    assert_eq!(render.period_offset(), 1);
    let source_frontier = render.stream().stats().source_frames;
    let generated = render.output_positions().generated;
    render.hold(1, 0, epoch(1)).unwrap();
    assert_eq!(render.output_positions().generated, generated);
    assert_eq!(render.output_positions().next_submission, 0);
    render
        .set_gain_target(Some(20.0 * 0.5_f64.log10()), 0)
        .unwrap();
    let report = render.stage().unwrap().unwrap();
    assert_eq!(report.frames, 2);
    assert_eq!(render.period().header().epochs, epoch(1));
    assert_eq!(render.period().header().frame_position, 0);
    assert_eq!(render.period().integer_sample(0), Some(500));
    assert_eq!(render.period().integer_sample(1), Some(1000));
    assert_eq!(render.stream().stats().source_frames, source_frontier);
    render.accepted(2).unwrap();
    render.observe(2, 2).unwrap();
    let mut output = Vec::new();
    let mut submitted = 2;
    for _ in 0..50 {
        let step = render.finish().unwrap();
        if let Some(report) = render.stage().unwrap() {
            for index in 0..report.frames {
                output.push(render.period().integer_sample(index).unwrap());
            }
            render.accepted(report.frames).unwrap();
            submitted += report.frames as u64;
            render.observe(submitted, submitted).unwrap();
        }
        if step.terminal && render.finished() {
            break;
        }
    }
    assert_eq!(output, [1500, 2000]);
    assert!(render.finished());
    assert_eq!(render.stream().stats().source_frames, 4);
    assert!(render.resources().resident_bytes <= render.resources().preparation_peak_bytes);
}

#[test]
fn whole_span_retirement_recovers_replay_backpressure_and_holds_exact_unretired_samples() {
    let mut render =
        PreparedStreamRender::prepare(plan(), config(), Arc::new(AtomicBool::new(false))).unwrap();
    render.set_gain_target(Some(0.0), 0).unwrap();
    let mut input = block();
    let capacity = render.resources().replay_frames;
    let mut source = 0;
    let mut submitted = 0;
    for _ in 0..100 {
        input.set_span(epoch(0), source / 4, source, 4).unwrap();
        let step = render
            .pump(Some(SourceSpan {
                block: &input,
                offset: 0,
                frames: 4,
            }))
            .unwrap();
        source += step.consumed_frames as u64;
        while let Some(report) = render.stage().unwrap() {
            render.accepted(report.frames).unwrap();
            submitted += report.frames as u64;
        }
        // A queue clock inside an unfinished span still retires no payload.
        render.observe(submitted, 0).unwrap();
        if step.need_output && step.consumed_frames == 0 {
            break;
        }
    }
    let full = render.output_positions();
    assert_eq!(full.generated - full.retained_start, capacity as u64);
    assert_eq!(full.retained_start, 0);
    assert_eq!(full.next_submission, submitted);
    let blocked = render.pump(None).unwrap();
    assert!(blocked.need_output);
    assert_eq!(blocked.produced_frames, 0);
    let before = render.stream().stats();
    // One complete native span releases exactly one period of the existing replay budget.
    render.observe(submitted, 2).unwrap();
    assert_eq!(render.output_positions().retained_start, 2);
    let step = render
        .pump(Some(SourceSpan {
            block: &input,
            offset: 0,
            frames: 4,
        }))
        .unwrap();
    assert!(step.consumed_frames > 0 || step.produced_frames > 0);
    assert!(render.stream().stats().source_frames >= before.source_frames);
    let generated = render.output_positions().generated;
    let source_frontier = render.stream().stats().source_frames;
    render.hold(submitted, 2, epoch(1)).unwrap();
    assert_eq!(render.output_positions().backend_origin, 2);
    assert_eq!(render.output_positions().generated, generated);
    assert_eq!(render.stream().stats().source_frames, source_frontier);
    render.set_gain_target(Some(0.0), 0).unwrap();
    assert_eq!(render.stage().unwrap().unwrap().frames, 2);
    assert_eq!(render.period().integer_sample(0), Some(3000));
    assert_eq!(render.period().integer_sample(1), Some(4000));
    assert_eq!(render.period().header().epochs, epoch(1));
    assert_eq!(render.period().header().frame_position, 0);
}

#[test]
fn acknowledged_native_prefix_remains_committed_after_source_cancellation() {
    use std::sync::atomic::Ordering;
    let source = Arc::new(AtomicBool::new(false));
    let mut render = PreparedStreamRender::prepare(plan(), config(), source.clone()).unwrap();
    let input = block();
    render
        .pump(Some(SourceSpan {
            block: &input,
            offset: 0,
            frames: 4,
        }))
        .unwrap();
    assert_eq!(render.stage().unwrap().unwrap().frames, 2);
    let before = render.output_positions();
    source.store(true, Ordering::Release);
    render.accepted(1).unwrap();
    assert_eq!(
        render.output_positions().next_submission,
        before.next_submission + 1
    );
    assert_eq!(render.period_offset(), 1);
    let committed = render.output_positions();
    assert_eq!(
        render.accepted(2).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(render.output_positions(), committed);
    assert_eq!(
        render.stage().unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        render.pump(None).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        render.finish().unwrap_err().code,
        ErrorCode::OperationCancelled
    );
}

#[test]
fn reset_rejects_old_source_packets_and_invalid_backend_history_fails_closed() {
    let mut render =
        PreparedStreamRender::prepare(plan(), config(), Arc::new(AtomicBool::new(false))).unwrap();
    let input = block();
    render.reset(epoch(2)).unwrap();
    assert_eq!(
        render
            .pump(Some(SourceSpan {
                block: &input,
                offset: 0,
                frames: 4
            }))
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    assert_eq!(render.stream().stats().source_frames, 0);
    assert_eq!(
        render.observe(1, 0).unwrap_err().code,
        ErrorCode::OutputFault
    );
    assert_eq!(render.pump(None).unwrap_err().code, ErrorCode::OutputFault);
    render.reset(epoch(3)).unwrap();
    assert!(render.pump(None).is_ok());
    render.cancel();
    assert_eq!(
        render.stage().unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        render.reset(epoch(4)).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
}

#[test]
fn invalid_held_restart_history_blocks_further_output_until_a_true_reset() {
    let mut render =
        PreparedStreamRender::prepare(plan(), config(), Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(
        render.hold(1, 0, epoch(1)).unwrap_err().code,
        ErrorCode::OutputFault
    );
    assert_eq!(render.pump(None).unwrap_err().code, ErrorCode::OutputFault);
    render.reset(epoch(2)).unwrap();
    assert!(render.pump(None).is_ok());
}

#[test]
fn resource_preflight_checks_complete_bridge_storage_and_geometry_without_allocating() {
    let plan = plan();
    let (bridge, counts) = allocations::tracked(|| config().bridge_resources(4).unwrap());
    assert_eq!(counts, allocations::AllocationCounts::default());
    let (required, counts) = allocations::tracked(|| {
        PreparedStreamRender::resource_requirements(&plan, config()).unwrap()
    });
    assert_eq!(counts, allocations::AllocationCounts::default());
    assert_eq!(required.replay_frames, 6);
    assert_eq!(bridge.bridge_bytes, required.bridge_bytes);
    assert_eq!(bridge.replay_frames, required.replay_frames);
    for quantum in [0, 8193, usize::MAX] {
        assert_eq!(
            config().bridge_resources(quantum).unwrap_err().code,
            ErrorCode::FormatUnsupported
        );
    }
    assert!(required.bridge_bytes >= 6 * 8 + 4 * 8 + 2 * 8 + 2 * 2 * 2);
    let mut limited = config();
    limited.budget_bytes = required.preparation_peak_bytes - 1;
    assert_eq!(
        PreparedStreamRender::resource_requirements(&plan, limited)
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut overflow = config();
    overflow.queue_frames = usize::MAX;
    assert_eq!(
        PreparedStreamRender::resource_requirements(&plan, overflow)
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut wrong = config();
    wrong.native_format =
        PcmFormat::new(44100, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap();
    assert_eq!(
        PreparedStreamRender::resource_requirements(&plan, wrong)
            .unwrap_err()
            .code,
        ErrorCode::FormatUnsupported
    );
}

#[test]
fn real_src_roundtrip_tracks_actual_counts_and_quantizes_partial_native_periods_without_heap_work()
{
    use earph_core::dsp_nodes::SrcPreset;
    use earph_dsp::resample::SrcDesign;
    let a = SrcDesign::analyze(
        48000,
        96000,
        ChannelLayout::Mono,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let b = SrcDesign::analyze(
        96000,
        48000,
        ChannelLayout::Mono,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let mut samples = vec![0_i16; 257];
    samples[0] = 4096;
    samples[256] = -8192;
    let normalized: Vec<_> = samples.iter().map(|v| f64::from(*v) / 32768.0).collect();
    let convert = |design: SrcDesign, input: &[f64]| {
        let mut src = design.instantiate(16 << 20).unwrap();
        let mut result = Vec::new();
        let mut output = [0.0; 23];
        let mut offset = 0;
        for _ in 0..100000 {
            let step = src.process(&input[offset..], &mut output).unwrap();
            offset += step.consumed_frames;
            result.extend_from_slice(&output[..step.produced_frames]);
            if offset == input.len() {
                break;
            }
        }
        assert_eq!(offset, input.len());
        src.begin_finish().unwrap();
        let mut terminal = false;
        for _ in 0..100000 {
            let step = src.finish(&mut output).unwrap();
            result.extend_from_slice(&output[..step.produced_frames]);
            if step.terminal {
                terminal = true;
                break;
            }
        }
        assert!(terminal);
        result
    };
    let intermediate = convert(a, &normalized);
    let expected: Vec<i64> = convert(b, &intermediate)
        .iter()
        .map(|v| (v * 0.5 * 8388608.0).round().clamp(-8388608.0, 8388607.0) as i64)
        .collect();
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(a), StreamStagePlan::Src(b)],
        &[1.0],
        StreamBudget::default(),
    )
    .unwrap();
    let config = StreamRenderConfig {
        native_format: format(PcmRepresentation::Signed24),
        period_frames: 7,
        queue_frames: 28,
        ..config()
    };
    let mut render =
        PreparedStreamRender::prepare(plan, config, Arc::new(AtomicBool::new(false))).unwrap();
    render
        .set_gain_target(Some(20.0 * 0.5_f64.log10()), 0)
        .unwrap();
    let source = PcmBlock::new(
        BlockHeader {
            format: format(PcmRepresentation::Signed16),
            epochs: epoch(0),
            sequence: 0,
            frame_position: 0,
            frame_count: 257,
            time_domain: TimeDomain::OfflineFrames,
        },
        PcmSamples::Signed16(samples),
    )
    .unwrap();
    let mut actual = Vec::with_capacity(expected.len());
    let (mut offset, mut submitted, mut terminal) = (0, 0_u64, false);
    let (_, counts) = allocations::tracked(|| {
        for _ in 0..100000 {
            let step = if offset < 257 {
                render
                    .pump(Some(SourceSpan {
                        block: &source,
                        offset,
                        frames: (257 - offset).min(13),
                    }))
                    .unwrap()
            } else {
                render.finish().unwrap()
            };
            offset += step.consumed_frames;
            if let Some(report) = render.stage().unwrap() {
                let count = (report.frames - render.period_offset()).min(3);
                for index in render.period_offset()..render.period_offset() + count {
                    actual.push(render.period().integer_sample(index).unwrap());
                }
                render.accepted(count).unwrap();
                submitted += count as u64;
                render.observe(submitted, submitted).unwrap();
            }
            if step.terminal && render.finished() {
                terminal = true;
                break;
            }
        }
    });
    assert_eq!(counts, allocations::AllocationCounts::default());
    assert!(terminal);
    assert_eq!(offset, 257);
    assert_eq!(actual, expected);
    assert_ne!(submitted, 257);
    assert_eq!(render.stream().stats().source_frames, 257);
    assert_eq!(render.stream().stats().output_frames, submitted);
    assert_eq!(render.stream().src_stats(0).unwrap().real_input_frames, 257);
    assert_eq!(
        render.stream().src_stats(1).unwrap().real_input_frames,
        intermediate.len() as u64
    );
}
