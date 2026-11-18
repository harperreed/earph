// ABOUTME: Exercises bounded real SRC and same-rate graph stream composition.
// ABOUTME: Checks frame authority, backpressure, finite finish, and retained lifecycle storage.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    dsp_nodes::SrcPreset,
    ids::Epochs,
};
use earph_dsp::resample::SrcDesign;
pub use earph_dsp::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use stream::{PreparedStream, StreamBudget, StreamErrorKind, StreamPlan, StreamStagePlan};

fn design(input: u32, output: u32, chunk: usize) -> SrcDesign {
    SrcDesign::analyze(
        input,
        output,
        ChannelLayout::Stereo,
        chunk,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap()
}

fn island(rate: u32, kind: serde_json::Value, input_bound: f64) -> CompiledPlan {
    let format = PcmFormat::new(rate, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":rate,
        "channels":"stereo","precision":"float64","max_quantum_frames":64});
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"nodes":[
            {"id":"input","kind":{"kind":"input"},"format":port},
            {"id":"effect","kind":kind,"format":port},
            {"id":"output","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"input","to":"effect","from_port":0,"to_port":0},
            {"from":"effect","to":"output","from_port":0,"to_port":0}],"output":"output"
    }))
    .unwrap();
    compile_with_input_bounds(
        &document,
        &CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        },
        &[],
        &[input_bound; 2],
    )
    .unwrap()
}

#[test]
fn finite_island_tail_reaches_later_src_without_becoming_source_input() {
    let a = design(48000, 96000, 64);
    let b = design(96000, 48000, 128);
    let input: Vec<_> = (0..130).map(|i| (i as f64 * 0.1).sin() * 0.2).collect();
    let converted = convert(a, &input);
    let mut delayed = vec![0.0; 6];
    delayed.extend(converted.iter().copied());
    let expected = convert(b, &delayed);
    let delay = island(
        96000,
        serde_json::json!({"kind":"delay","delay_frames":3}),
        a.peak_bound(1.0).unwrap(),
    );
    let plan = StreamPlan::new(
        vec![
            StreamStagePlan::Src(a),
            StreamStagePlan::SameRate(Box::new(delay)),
            StreamStagePlan::Src(b),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(render(&mut graph, &input, 7, 3), expected);
    assert_eq!(graph.stats().source_frames, 65);
    assert_eq!(
        graph.stage_stats(1).unwrap().output_frames,
        (delayed.len() / 2) as u64
    );
    assert_eq!(
        graph.src_stats(2).unwrap().real_input_frames,
        (delayed.len() / 2) as u64
    );
}

#[test]
fn island_rejects_a_normalized_peak_certificate_after_src_amplification() {
    let src = design(48000, 96000, 64);
    let plan = island(96000, serde_json::json!({"kind":"bypass"}), 1.0);
    let error = StreamPlan::new(
        vec![
            StreamStagePlan::Src(src),
            StreamStagePlan::SameRate(Box::new(plan)),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, StreamErrorKind::PeakContract);
    assert_eq!(error.stage, Some(1));
}
fn convert(design: SrcDesign, input: &[f64]) -> Vec<f64> {
    let channels = design.channels().count();
    let mut kernel = design.instantiate(16 << 20).unwrap();
    let mut output = [0.0; 74];
    let mut result = Vec::new();
    let mut offset = 0;
    for _ in 0..100000 {
        let step = kernel.process(&input[offset..], &mut output).unwrap();
        offset += step.consumed_frames * channels;
        result.extend_from_slice(&output[..step.produced_frames * channels]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    kernel.begin_finish().unwrap();
    for _ in 0..100000 {
        let step = kernel.finish(&mut output).unwrap();
        result.extend_from_slice(&output[..step.produced_frames * channels]);
        if step.terminal {
            return result;
        }
    }
    panic!("standalone SRC did not finish");
}
fn render(stream: &mut PreparedStream, input: &[f64], packet: usize, capacity: usize) -> Vec<f64> {
    let input_channels = stream.input_format().channels().count();
    let output_channels = stream.output_format().channels().count();
    let mut output = vec![123.0; capacity * output_channels];
    let mut result = Vec::new();
    let mut offset = 0;
    for _ in 0..100000 {
        let end = (offset + packet * input_channels).min(input.len());
        let step = stream.pump(&input[offset..end], &mut output).unwrap();
        offset += step.consumed_frames * input_channels;
        result.extend_from_slice(&output[..step.produced_frames * output_channels]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    stream.begin_finish().unwrap();
    let bound = stream.remaining_output_bound().unwrap();
    let before = result.len() / output_channels;
    for _ in 0..100000 {
        let step = stream.finish(&mut output).unwrap();
        result.extend_from_slice(&output[..step.produced_frames * output_channels]);
        if step.terminal {
            assert!((result.len() / output_channels - before) as u64 <= bound);
            return result;
        }
    }
    panic!("stream did not finish");
}

#[test]
fn serial_src_matches_standalone_stages_with_one_frame_backpressure() {
    let a = design(48000, 44100, 64);
    let b = design(44100, 48000, 128);
    let input: Vec<_> = (0..514).map(|i| (i as f64 * 0.3).sin() * 0.25).collect();
    let expected = convert(b, &convert(a, &input));
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(a), StreamStagePlan::Src(b)],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(render(&mut graph, &input, 13, 1), expected);
    assert_eq!(graph.stats().source_frames, 257);
    assert_eq!(graph.stats().output_frames, (expected.len() / 2) as u64);
    assert_eq!(graph.src_stats(0).unwrap().guard_frames, 1);
    assert_eq!(graph.src_stats(1).unwrap().guard_frames, 1);
    assert_eq!(
        graph.src_stats(1).unwrap().real_input_frames,
        graph.src_stats(0).unwrap().emitted_frames
    );
}

#[test]
fn empty_stream_has_no_padding_and_repeated_finish_is_terminal() {
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(design(48000, 96000, 1))],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert!(render(&mut graph, &[], 1, 1).is_empty());
    assert_eq!(graph.src_stats(0).unwrap().guard_frames, 0);
    let mut out = [9.0; 2];
    let step = graph.finish(&mut out).unwrap();
    assert!(step.terminal);
    assert_eq!(step.produced_frames, 0);
    assert_eq!(out, [9.0; 2]);
}

#[test]
fn zero_output_priming_reports_work_and_never_eof() {
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(design(48000, 44100, 1))],
        &[1.0; 2],
        StreamBudget {
            max_sweeps: 1,
            ..StreamBudget::default()
        },
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    let step = graph.pump(&[1.0, 0.0], &mut [0.0; 2]).unwrap();
    assert_eq!(step.consumed_frames, 1);
    assert_eq!(step.produced_frames, 0);
    assert!(step.work_units > 0);
    assert!(!step.terminal);
    assert_eq!(graph.src_stats(0).unwrap().library_calls, 1);
}

#[test]
fn full_output_preserves_bounded_backpressure_and_numeric_failure_preserves_caller_output() {
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(design(48000, 96000, 2))],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    let mut accepted = 0;
    for _ in 0..10000 {
        let step = graph.pump(&[0.25; 4], &mut []).unwrap();
        accepted += step.consumed_frames;
        if step.consumed_frames == 0 && step.work_units == 0 {
            break;
        }
    }
    assert!(accepted > 0 && accepted < 10000);
    let mut output = [5.0; 6];
    graph
        .reset(Epochs {
            session: 0,
            stream: 1,
        })
        .unwrap();
    let error = graph.pump(&[f64::NAN, 0.0], &mut output).unwrap_err();
    assert_eq!(error.kind, StreamErrorKind::NonFinite);
    assert_eq!(output, [5.0; 6]);
    assert!(graph.pump(&[0.0; 2], &mut output).is_err());
    assert_eq!(output, [5.0; 6]);
}

#[test]
fn stage_admission_rejects_rate_mismatch_and_full_construction_budget() {
    let a = design(48000, 44100, 64);
    let b = design(48000, 96000, 64);
    assert_eq!(
        StreamPlan::new(
            vec![StreamStagePlan::Src(a), StreamStagePlan::Src(b)],
            &[1.0; 2],
            StreamBudget::default()
        )
        .err()
        .unwrap()
        .kind,
        StreamErrorKind::Adjacency
    );
    assert_eq!(
        StreamPlan::new(
            vec![StreamStagePlan::Src(a)],
            &[1.0; 2],
            StreamBudget {
                max_preparation_bytes: 1,
                ..StreamBudget::default()
            }
        )
        .err()
        .unwrap()
        .kind,
        StreamErrorKind::Budget
    );
}

#[test]
fn reset_replays_the_same_stream_and_cancel_cannot_be_reset() {
    let source = design(48000, 44100, 2);
    let signal = [0.25, -0.25, 0.5, -0.5, 0.1, -0.1];
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(source)],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    let expected = render(&mut graph, &signal, 1, 1);
    assert_eq!(
        graph.reset(Epochs::default()).unwrap_err().kind,
        StreamErrorKind::StaleEpoch
    );
    graph
        .reset(Epochs {
            session: 0,
            stream: 1,
        })
        .unwrap();
    assert_eq!(render(&mut graph, &signal, 2, 31), expected);
    graph.cancel();
    assert_eq!(
        graph
            .reset(Epochs {
                session: 0,
                stream: 2
            })
            .unwrap_err()
            .kind,
        StreamErrorKind::Lifecycle
    );
    let mut output = [7.0; 2];
    assert!(graph.finish(&mut output).is_err());
    assert_eq!(output, [7.0; 2]);
}

#[test]
fn malformed_spans_preserve_state_and_valid_prefixes_preserve_output_suffixes() {
    let source = design(48000, 96000, 1);
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(source)],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    let mut bad_output = [9.0; 3];
    assert_eq!(
        graph.pump(&[0.25; 2], &mut bad_output).unwrap_err().kind,
        StreamErrorKind::Geometry
    );
    assert_eq!(graph.stats().source_frames, 0);
    assert_eq!(bad_output, [9.0; 3]);
    let mut output = [19.0; 128];
    assert!(graph.finish(&mut output).is_err());
    let step = graph.pump(&[0.25, 0.0], &mut output).unwrap();
    assert!(
        output[step.produced_frames * 2..]
            .iter()
            .all(|x| *x == 19.0)
    );
    assert!(!step.terminal);
}

#[test]
fn final_peq_uses_the_amplified_src_input_certificate() {
    let src = design(48000, 44100, 64);
    let peq_kind = serde_json::json!({"kind":"peq","filter":{"kind":"peaking","frequency_hz":1000.0,"q":0.707,"gain_db":-6.0}});
    let peq = island(44100, peq_kind, src.peak_bound(1.0).unwrap());
    let input: Vec<_> = (0..146).map(|i| (i as f64 * 0.19).sin() * 0.25).collect();
    let converted = convert(src, &input);
    let mut reference = PreparedGraph::prepare(peq.clone(), Epochs::default()).unwrap();
    let mut expected = Vec::new();
    for part in converted.chunks(22) {
        let mut output = vec![0.0; part.len()];
        reference.process(part, &mut output).unwrap();
        expected.extend(output);
    }
    loop {
        let mut output = [0.0; 22];
        let n = reference.finish(&mut output).unwrap();
        expected.extend(&output[..n * 2]);
        if n == 0 {
            break;
        }
    }
    let plan = StreamPlan::new(
        vec![
            StreamStagePlan::Src(src),
            StreamStagePlan::SameRate(Box::new(peq.clone())),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(render(&mut graph, &input, 7, 1), expected);
    assert_eq!(
        StreamPlan::new(
            vec![
                StreamStagePlan::SameRate(Box::new(peq)),
                StreamStagePlan::Src(design(44100, 48000, 64))
            ],
            &[1.0; 2],
            StreamBudget::default()
        )
        .err()
        .unwrap()
        .kind,
        StreamErrorKind::UnsupportedTail
    );
}

#[test]
fn bounded_iir_cannot_feed_a_separate_finite_island() {
    let peq = island(
        44100,
        serde_json::json!({"kind":"peq","filter":{"kind":"peaking",
        "frequency_hz":1000.0,"q":0.707,"gain_db":-6.0}}),
        1.0,
    );
    let delay = island(
        44100,
        serde_json::json!({"kind":"delay","delay_frames":1}),
        peq.output_peak_bounds()[0],
    );
    let error = StreamPlan::new(
        vec![
            StreamStagePlan::SameRate(Box::new(peq)),
            StreamStagePlan::SameRate(Box::new(delay)),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.kind, StreamErrorKind::UnsupportedTail);
}

#[test]
fn finished_stream_still_rejects_partial_output_frames() {
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(design(48000, 44100, 64))],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert!(render(&mut graph, &[], 1, 1).is_empty());
    let mut output = [7.0; 3];
    assert_eq!(
        graph.finish(&mut output).unwrap_err().kind,
        StreamErrorKind::Geometry
    );
    assert_eq!(output, [7.0; 3]);
}

#[test]
fn mono_src_and_stereo_matrix_count_frames_in_their_own_channel_layouts() {
    let src = SrcDesign::analyze(
        48000,
        44100,
        ChannelLayout::Mono,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let input = vec![1.0, 0.0, -0.5, 0.25];
    let converted = convert(src, &input);
    let expected: Vec<_> = converted.iter().flat_map(|x| [*x, -0.5 * x]).collect();
    let input_format =
        PcmFormat::new(44100, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
    let output_format =
        PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let mono = serde_json::json!({"domain":"pcm","sample_rate_hz":44100,"channels":"mono","precision":"float64","max_quantum_frames":64});
    let stereo = serde_json::json!({"domain":"pcm","sample_rate_hz":44100,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let document=serde_json::from_value(serde_json::json!({"schema_version":1,"nodes":[
        {"id":"in","kind":{"kind":"input"},"format":mono},
        {"id":"matrix","kind":{"kind":"matrix","input_channels":"mono","coefficients":[1,-0.5]},"format":stereo},
        {"id":"out","kind":{"kind":"output"},"format":stereo}],
        "edges":[{"from":"in","to":"matrix","from_port":0,"to_port":0},{"from":"matrix","to":"out","from_port":0,"to_port":0}],"output":"out"})).unwrap();
    let matrix = compile_with_input_bounds(
        &document,
        &CompileContext {
            input_format,
            output_format,
            quantum_frames: 64,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        },
        &[],
        &[src.peak_bound(1.0).unwrap()],
    )
    .unwrap();
    let plan = StreamPlan::new(
        vec![
            StreamStagePlan::Src(src),
            StreamStagePlan::SameRate(Box::new(matrix)),
        ],
        &[1.0],
        StreamBudget::default(),
    )
    .unwrap();
    let mut stream =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(render(&mut stream, &input, 1, 3), expected);
    assert_eq!(stream.stats().source_frames, 4);
    assert_eq!(stream.stats().output_frames, converted.len() as u64);
}

#[test]
fn intermediate_fir_keeps_authored_support_before_the_next_src() {
    use earph_core::dsp_nodes::{FirAssetReference, FirPurpose, FirSha256};
    let src1 = design(48000, 96000, 64);
    let src2 = design(96000, 48000, 128);
    let asset = FirCoefficients::new(
        FirAssetReference {
            sha256: FirSha256::new("a".repeat(64)).unwrap(),
            sample_rate_hz: 96000,
            channels: ChannelLayout::Stereo,
            taps_per_channel: 3,
            purpose: FirPurpose::Correction,
        },
        vec![vec![0.25, -0.125, 0.0], vec![0.5, 0.0, 0.0]],
    )
    .unwrap();
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":96000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let document=serde_json::from_value(serde_json::json!({"schema_version":1,"nodes":[
        {"id":"in","kind":{"kind":"input"},"format":port},
        {"id":"fir","kind":{"kind":"convolution","asset":asset.reference()},"format":port},
        {"id":"out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"in","to":"fir","from_port":0,"to_port":0},{"from":"fir","to":"out","from_port":0,"to_port":0}],"output":"out"})).unwrap();
    let format = PcmFormat::new(96000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let fir = compile_with_input_bounds(
        &document,
        &CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        },
        &[asset],
        &[src1.peak_bound(1.0).unwrap(); 2],
    )
    .unwrap();
    let input: Vec<_> = (0..146).map(|i| (i as f64 * 0.31).sin() * 0.125).collect();
    let first = convert(src1, &input);
    let mut convolution = vec![0.0; first.len() + 258 * 2];
    for (frame, pair) in first.as_chunks::<2>().0.iter().enumerate() {
        convolution[(256 + frame) * 2] += pair[0] * 0.25;
        convolution[(257 + frame) * 2] -= pair[0] * 0.125;
        convolution[(256 + frame) * 2 + 1] += pair[1] * 0.5;
    }
    let expected = convert(src2, &convolution);
    let plan = StreamPlan::new(
        vec![
            StreamStagePlan::Src(src1),
            StreamStagePlan::SameRate(Box::new(fir)),
            StreamStagePlan::Src(src2),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut stream =
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap();
    let output = render(&mut stream, &input, 17, 1);
    assert_eq!(output.len(), expected.len());
    assert!(
        output
            .iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 1e-12)
    );
    assert_eq!(
        stream.src_stats(2).unwrap().real_input_frames,
        (convolution.len() / 2) as u64
    );
    assert_eq!(stream.stats().source_frames, 73);
}

use std::alloc::{GlobalAlloc, Layout, System};
#[test]
fn shared_worker_cancellation_prevents_source_acceptance_and_output_publication() {
    let source = design(48000, 44100, 64);
    let cancel = Arc::new(AtomicBool::new(false));
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(source)],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut graph = PreparedStream::prepare(plan, Epochs::default(), cancel.clone()).unwrap();
    cancel.store(true, Ordering::Release);
    let mut output = [9.0; 2];
    assert_eq!(
        graph.pump(&[0.5, -0.5], &mut output).unwrap_err().kind,
        StreamErrorKind::Cancelled
    );
    assert_eq!(graph.stats().source_frames, 0);
    assert_eq!(graph.src_stats(0).unwrap().library_calls, 0);
    assert_eq!(output, [9.0; 2]);
    assert_eq!(
        graph
            .reset(Epochs {
                session: 0,
                stream: 1
            })
            .unwrap_err()
            .kind,
        StreamErrorKind::Lifecycle
    );
}
use std::cell::Cell;
#[derive(Clone, Copy, Default, Debug)]
struct AllocationCounts {
    allocs: usize,
    frees: usize,
    live: usize,
    peak: usize,
}
thread_local! { static METER: Cell<Option<AllocationCounts>> = const {Cell::new(None)}; }
struct AllocationMeter;
#[global_allocator]
static ALLOCATOR: AllocationMeter = AllocationMeter;
unsafe impl GlobalAlloc for AllocationMeter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let _ = METER.try_with(|m| {
                if let Some(mut n) = m.get() {
                    n.allocs += 1;
                    n.live += layout.size();
                    n.peak = n.peak.max(n.live);
                    m.set(Some(n));
                }
            });
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            let _ = METER.try_with(|m| {
                if let Some(mut n) = m.get() {
                    n.allocs += 1;
                    n.live += layout.size();
                    n.peak = n.peak.max(n.live);
                    m.set(Some(n));
                }
            });
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = METER.try_with(|m| {
            if let Some(mut n) = m.get() {
                n.frees += 1;
                n.live = n.live.saturating_sub(layout.size());
                m.set(Some(n));
            }
        });
        unsafe { System.dealloc(ptr, layout) };
    }
}
fn measured<T>(f: impl FnOnce() -> T) -> (T, AllocationCounts) {
    METER.with(|m| m.set(Some(AllocationCounts::default())));
    let result = f();
    let counts = METER.with(|m| m.replace(None).unwrap());
    (result, counts)
}
#[test]
fn processing_finish_reset_cancel_and_faults_retain_all_storage() {
    let source = design(48000, 44100, 64);
    let bounded = island(
        44100,
        serde_json::json!({"kind":"delay","delay_frames":3}),
        source.peak_bound(1.0).unwrap(),
    );
    let plan = StreamPlan::new(
        vec![
            StreamStagePlan::Src(source),
            StreamStagePlan::SameRate(Box::new(bounded)),
        ],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let resources = plan.resources();
    let (mut graph, preparation) = measured(|| {
        PreparedStream::prepare(plan, Epochs::default(), Arc::new(AtomicBool::new(false))).unwrap()
    });
    assert!(
        preparation.peak as u64 <= resources.preparation_peak_bytes,
        "{preparation:?} {resources:?}"
    );
    let signal = [0.125; 258];
    let mut output = [0.0; 62];
    let (_, counts) = measured(|| {
        let mut offset = 0;
        for _ in 0..10000 {
            let step = graph.pump(&signal[offset..], &mut output).unwrap();
            offset += step.consumed_frames * 2;
            if offset == signal.len() {
                break;
            }
        }
        assert_eq!(offset, signal.len());
        graph.begin_finish().unwrap();
        let mut terminal = false;
        for _ in 0..10000 {
            if graph.finish(&mut output).unwrap().terminal {
                terminal = true;
                break;
            }
        }
        assert!(terminal);
        graph
            .reset(Epochs {
                session: 0,
                stream: 1,
            })
            .unwrap();
        graph.pump(&[f64::NAN, 0.0], &mut output).unwrap_err();
        graph
            .reset(Epochs {
                session: 0,
                stream: 2,
            })
            .unwrap();
        graph.pump(&signal, &mut output).unwrap();
        graph.cancel();
    });
    assert_eq!(counts.allocs, 0, "{counts:?}");
    assert_eq!(counts.frees, 0, "{counts:?}");
}
