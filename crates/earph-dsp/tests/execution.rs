// ABOUTME: Executes compiled graphs against hand-computed interleaved PCM captures.
// ABOUTME: Checks prepared state, finite samples, exact tails, and real-time allocation discipline.
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    ids::Epochs,
};
use earph_dsp::*;

fn scalar(value: f64) -> FiniteGraphScalar {
    FiniteGraphScalar::new(value).unwrap()
}
fn coefficients<const N: usize>(values: [f64; N]) -> Vec<FiniteGraphScalar> {
    values.into_iter().map(scalar).collect()
}

fn id(value: &str) -> NodeId {
    NodeId::new(value).unwrap()
}
fn port(channels: ChannelLayout) -> PortFormat {
    PortFormat {
        domain: SampleDomain::Pcm,
        sample_rate_hz: 48000,
        channels,
        precision: PcmRepresentation::Float64,
        max_quantum_frames: 8,
    }
}
fn node(name: &str, kind: NodeKind, channels: ChannelLayout) -> Node {
    Node {
        id: id(name),
        kind,
        format: port(channels),
    }
}
fn edge(from: &str, to: &str, to_port: usize) -> Edge {
    Edge {
        from: id(from),
        to: id(to),
        from_port: 0,
        to_port,
    }
}
fn context(input: ChannelLayout, output: ChannelLayout) -> CompileContext {
    CompileContext {
        input_format: PcmFormat::new(48000, input, PcmRepresentation::Float64).unwrap(),
        output_format: PcmFormat::new(48000, output, PcmRepresentation::Float64).unwrap(),
        quantum_frames: 8,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    }
}
fn chain(kind: NodeKind) -> GraphDocument {
    GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input, ChannelLayout::Stereo),
            node("effect", kind, ChannelLayout::Stereo),
            node("meter", NodeKind::Meter, ChannelLayout::Stereo),
            node("out", NodeKind::Output, ChannelLayout::Stereo),
        ],
        edges: vec![
            edge("in", "effect", 0),
            edge("effect", "meter", 0),
            edge("meter", "out", 0),
        ],
        output: id("out"),
    }
}
fn prepare(graph: &GraphDocument) -> PreparedGraph {
    PreparedGraph::prepare(
        compile(
            graph,
            &context(ChannelLayout::Stereo, ChannelLayout::Stereo),
        )
        .unwrap(),
        Epochs::default(),
    )
    .unwrap()
}

#[test]
fn gain_preserves_channel_order_and_headroom_with_read_only_metering() {
    let mut graph = prepare(&chain(NodeKind::Gain {
        gain_linear: scalar(-2.0),
    }));
    assert_eq!(graph.lifecycle(), GraphLifecycle::Prepared);
    let mut output = [99.0; 6];
    assert_eq!(
        graph
            .process(&[0.25, -0.5, 1.5, -2.0, 0.0, 3.0], &mut output)
            .unwrap(),
        3
    );
    assert_eq!(output, [-0.5, 1.0, -3.0, 4.0, -0.0, -6.0]);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Running);
    assert_eq!(
        graph.meter(&id("meter")),
        Some(MeterObservation {
            peak_abs: 6.0,
            sample_count: 6
        })
    );
    assert_eq!(graph.meter(&id("effect")), None);
    assert_eq!(graph.finish(&mut output).unwrap(), 0);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Finished);
    assert!(graph.process(&[0.0; 6], &mut output).is_err());
}

#[test]
fn delay_spans_short_blocks_and_drains_each_tail_frame_once() {
    let mut graph = prepare(&chain(NodeKind::Delay { delay_frames: 3 }));
    let mut output = [99.0; 4];
    assert_eq!(
        graph.process(&[1.0, 10.0, 2.0, 20.0], &mut output).unwrap(),
        2
    );
    assert_eq!(output, [0.0; 4]);
    assert_eq!(graph.process(&[3.0, 30.0], &mut output[..2]).unwrap(), 1);
    assert_eq!(output[..2], [0.0; 2]);
    assert_eq!(graph.finish(&mut output).unwrap(), 2);
    assert_eq!(output, [1.0, 10.0, 2.0, 20.0]);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Draining);
    assert!(graph.process(&[0.0; 2], &mut output[..2]).is_err());
    output.fill(99.0);
    assert_eq!(graph.finish(&mut output).unwrap(), 1);
    assert_eq!(output, [3.0, 30.0, 99.0, 99.0]);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Finished);
    assert_eq!(graph.finish(&mut output).unwrap(), 0);
    assert_eq!(graph.meter(&id("meter")).unwrap().sample_count, 12);
}

#[test]
fn matrices_use_output_rows_and_input_columns_for_each_typed_layout() {
    for (input_channels, output_channels, coefficients, input, expected) in [
        (
            ChannelLayout::Stereo,
            ChannelLayout::Stereo,
            vec![1.0, 2.0, -3.0, 4.0],
            vec![1.0, 10.0, 2.0, 20.0],
            vec![21.0, 37.0, 42.0, 74.0],
        ),
        (
            ChannelLayout::Stereo,
            ChannelLayout::Mono,
            vec![0.5, -1.0],
            vec![2.0, 10.0, 4.0, 20.0],
            vec![-9.0, -18.0],
        ),
        (
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            vec![2.0, -3.0],
            vec![1.0, 2.0],
            vec![2.0, -3.0, 4.0, -6.0],
        ),
        (
            ChannelLayout::Mono,
            ChannelLayout::Mono,
            vec![0.5],
            vec![2.0, 4.0],
            vec![1.0, 2.0],
        ),
    ] {
        let document = GraphDocument {
            schema_version: 1,
            nodes: vec![
                node("in", NodeKind::Input, input_channels),
                node(
                    "matrix",
                    NodeKind::Matrix {
                        input_channels,
                        coefficients: coefficients.into_iter().map(scalar).collect(),
                    },
                    output_channels,
                ),
                node("out", NodeKind::Output, output_channels),
            ],
            edges: vec![edge("in", "matrix", 0), edge("matrix", "out", 0)],
            output: id("out"),
        };
        let mut graph = PreparedGraph::prepare(
            compile(&document, &context(input_channels, output_channels)).unwrap(),
            Epochs::default(),
        )
        .unwrap();
        let mut output = vec![99.0; expected.len()];
        assert_eq!(graph.process(&input, &mut output).unwrap(), 2);
        assert_eq!(output, expected);
    }
}

fn branches() -> GraphDocument {
    GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input, ChannelLayout::Stereo),
            node("dry", NodeKind::Bypass, ChannelLayout::Stereo),
            node(
                "slow",
                NodeKind::Delay { delay_frames: 3 },
                ChannelLayout::Stereo,
            ),
            node("sum", NodeKind::Sum, ChannelLayout::Stereo),
            node(
                "post",
                NodeKind::Delay { delay_frames: 2 },
                ChannelLayout::Stereo,
            ),
            node("out", NodeKind::Output, ChannelLayout::Stereo),
        ],
        edges: vec![
            edge("in", "dry", 0),
            edge("in", "slow", 0),
            edge("dry", "sum", 0),
            edge("slow", "sum", 1),
            edge("sum", "post", 0),
            edge("post", "out", 0),
        ],
        output: id("out"),
    }
}

#[test]
fn unequal_branches_align_before_summing_and_compound_tail_is_exact() {
    let mut graph = prepare(&branches());
    assert_eq!(graph.plan().latency_frames(), 5);
    assert_eq!(graph.plan().tail_frames(), 5);
    let mut output = [99.0; 4];
    graph.process(&[1.0, 10.0, 2.0, 20.0], &mut output).unwrap();
    let mut capture = output.to_vec();
    loop {
        let frames = graph.finish(&mut output).unwrap();
        capture.extend_from_slice(&output[..frames * 2]);
        if frames == 0 {
            break;
        }
    }
    assert_eq!(
        capture,
        [
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 20.0, 4.0, 40.0
        ]
    );
}

#[test]
fn sum_accumulates_in_destination_port_order() {
    let document = GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input, ChannelLayout::Mono),
            node(
                "a",
                NodeKind::Gain {
                    gain_linear: scalar(1.0),
                },
                ChannelLayout::Mono,
            ),
            node(
                "b",
                NodeKind::Gain {
                    gain_linear: scalar(-1e16),
                },
                ChannelLayout::Mono,
            ),
            node(
                "c",
                NodeKind::Gain {
                    gain_linear: scalar(1e16),
                },
                ChannelLayout::Mono,
            ),
            node("sum", NodeKind::Sum, ChannelLayout::Mono),
            node("out", NodeKind::Output, ChannelLayout::Mono),
        ],
        edges: vec![
            edge("in", "a", 0),
            edge("in", "b", 0),
            edge("in", "c", 0),
            edge("c", "sum", 0),
            edge("b", "sum", 1),
            edge("a", "sum", 2),
            edge("sum", "out", 0),
        ],
        output: id("out"),
    };
    let mut graph = PreparedGraph::prepare(
        compile(
            &document,
            &context(ChannelLayout::Mono, ChannelLayout::Mono),
        )
        .unwrap(),
        Epochs::default(),
    )
    .unwrap();
    let mut output = [0.0];
    graph.process(&[1.0], &mut output).unwrap();
    assert_eq!(output, [1.0]);
}

fn render(chunks: &[usize]) -> Vec<f64> {
    let input = [
        1.0, 10.0, 2.0, 20.0, 3.0, 30.0, 4.0, 40.0, 5.0, 50.0, 6.0, 60.0, 7.0, 70.0, 8.0, 80.0,
        9.0, 90.0, 10.0, 100.0, 11.0, 110.0, 12.0, 120.0, 13.0, 130.0, 14.0, 140.0, 15.0, 150.0,
        16.0, 160.0,
    ];
    let mut graph = prepare(&branches());
    let mut capture = Vec::new();
    let mut output = [0.0; 16];
    let mut position = 0;
    for &frames in chunks {
        let samples = frames * 2;
        graph
            .process(&input[position..position + samples], &mut output[..samples])
            .unwrap();
        capture.extend_from_slice(&output[..samples]);
        position += samples;
    }
    assert_eq!(position, input.len());
    loop {
        let frames = graph.finish(&mut output).unwrap();
        capture.extend_from_slice(&output[..frames * 2]);
        if frames == 0 {
            break;
        }
    }
    capture
}

#[test]
fn compatible_split_joins_match_unsplit_execution_without_reset_or_extra_tail() {
    let reference = render(&[8, 8]);
    let split = render(&[3, 1, 4, 2, 6]);
    assert_eq!(split, reference);
    assert_eq!(split.len(), 42);
    assert_eq!(
        &split[..14],
        &[
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 20.0, 4.0, 40.0
        ]
    );
    assert_eq!(&split[38..], &[30.0, 300.0, 32.0, 320.0]);
}

#[test]
fn malformed_spans_do_not_advance_delay_or_lifecycle() {
    let mut graph = prepare(&chain(NodeKind::Delay { delay_frames: 1 }));
    let mut output = [99.0; 4];
    assert!(graph.process(&[1.0], &mut output[..2]).is_err());
    assert_eq!(graph.lifecycle(), GraphLifecycle::Prepared);
    graph.process(&[1.0, 10.0], &mut output[..2]).unwrap();
    assert_eq!(output[..2], [0.0; 2]);
    for (input, output_len) in [(&[7.0][..], 2), (&[7.0, 70.0][..], 4), (&[0.0; 18][..], 2)] {
        assert_eq!(
            graph
                .process(input, &mut output[..output_len])
                .unwrap_err()
                .code,
            ErrorCode::GraphInvalid
        );
        assert_eq!(graph.lifecycle(), GraphLifecycle::Running);
    }
    assert!(graph.finish(&mut []).is_err());
    assert!(graph.finish(&mut output[..3]).is_err());
    assert_eq!(graph.lifecycle(), GraphLifecycle::Running);
    graph.process(&[2.0, 20.0], &mut output[..2]).unwrap();
    assert_eq!(output[..2], [1.0, 10.0]);
    assert_eq!(graph.finish(&mut output).unwrap(), 1);
    assert_eq!(output[..2], [2.0, 20.0]);
}

#[test]
fn nonfinite_inputs_and_overflow_fail_terminally_without_publishing_output() {
    for sample in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let mut graph = prepare(&chain(NodeKind::Bypass));
        let mut output = [99.0; 2];
        assert!(graph.process(&[sample, 0.0], &mut output).is_err());
        assert_eq!(output, [99.0; 2]);
        assert_eq!(graph.lifecycle(), GraphLifecycle::Failed);
        assert!(graph.process(&[0.0; 2], &mut output).is_err());
        assert!(graph.finish(&mut output).is_err());
    }
    for kind in [
        NodeKind::Gain {
            gain_linear: scalar(2.0),
        },
        NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: coefficients([1.0; 4]),
        },
    ] {
        let mut graph = prepare(&chain(kind));
        let mut output = [99.0; 2];
        assert!(graph.process(&[f64::MAX; 2], &mut output).is_err());
        assert_eq!(output, [99.0; 2]);
        assert_eq!(graph.lifecycle(), GraphLifecycle::Failed);
    }
}

#[test]
fn reset_requires_newer_epochs_and_clears_delay_and_meter_state_in_place() {
    let mut graph = prepare(&chain(NodeKind::Delay { delay_frames: 1 }));
    let mut output = [0.0; 2];
    graph.process(&[1.0, 10.0], &mut output).unwrap();
    assert_eq!(
        graph.reset(Epochs::default()).unwrap_err().code,
        ErrorCode::StaleSession
    );
    graph
        .reset(Epochs {
            session: 0,
            stream: 1,
        })
        .unwrap();
    assert_eq!(graph.lifecycle(), GraphLifecycle::Prepared);
    assert_eq!(
        graph.epochs(),
        Epochs {
            session: 0,
            stream: 1
        }
    );
    assert_eq!(graph.meter(&id("meter")), Some(MeterObservation::default()));
    graph.process(&[2.0, 20.0], &mut output).unwrap();
    assert_eq!(output, [0.0; 2]);
    graph.finish(&mut output).unwrap();
    assert_eq!(output, [2.0, 20.0]);
    assert!(
        graph
            .reset(Epochs {
                session: 0,
                stream: 0
            })
            .is_err()
    );
    graph
        .reset(Epochs {
            session: 1,
            stream: 0,
        })
        .unwrap();
    assert!(
        graph
            .reset(Epochs {
                session: 0,
                stream: 9
            })
            .is_err()
    );
    graph.process(&[f64::NAN, 0.0], &mut output).unwrap_err();
    graph
        .reset(Epochs {
            session: 1,
            stream: 1,
        })
        .unwrap();
    graph.process(&[3.0, 30.0], &mut output).unwrap();
    assert_eq!(output, [0.0; 2]);
}

#[test]
fn cancellation_is_terminal_and_empty_streams_invent_no_tail() {
    let mut graph = prepare(&chain(NodeKind::Delay { delay_frames: 100 }));
    assert_eq!(graph.process(&[], &mut []).unwrap(), 0);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Prepared);
    assert_eq!(graph.finish(&mut []).unwrap(), 0);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Finished);
    graph
        .reset(Epochs {
            session: 0,
            stream: 1,
        })
        .unwrap();
    graph.cancel();
    graph.cancel();
    assert_eq!(graph.lifecycle(), GraphLifecycle::Cancelled);
    assert_eq!(
        graph.process(&[], &mut []).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        graph.finish(&mut []).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        graph
            .reset(Epochs {
                session: 1,
                stream: 0
            })
            .unwrap_err()
            .code,
        ErrorCode::OperationCancelled
    );
}

#[test]
fn integer_preservation_plans_cannot_enter_f64_execution() {
    let mut graph = chain(NodeKind::Bypass);
    for node in &mut graph.nodes {
        node.format.precision = PcmRepresentation::Signed24;
    }
    let mut ctx = context(ChannelLayout::Stereo, ChannelLayout::Stereo);
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    ctx.input_format = format;
    ctx.output_format = format;
    let error = PreparedGraph::prepare(compile(&graph, &ctx).unwrap(), Epochs::default())
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
}

#[test]
fn failure_node_index_identifies_the_actual_kernel_and_survives_rejected_reset() {
    let mut graph = prepare(&chain(NodeKind::Gain {
        gain_linear: scalar(2.0),
    }));
    let mut output = [99.0; 2];
    graph.process(&[f64::MAX, 0.0], &mut output).unwrap_err();
    let failed = graph.failure_node_index().unwrap();
    assert_eq!(graph.plan().schedule()[failed].id(), &id("effect"));
    graph.reset(Epochs::default()).unwrap_err();
    assert_eq!(graph.failure_node_index(), Some(failed));
    graph
        .reset(Epochs {
            session: 0,
            stream: 1,
        })
        .unwrap();
    assert_eq!(graph.failure_node_index(), None);
    graph
        .process(&[0.0, f64::INFINITY], &mut output)
        .unwrap_err();
    let failed = graph.failure_node_index().unwrap();
    assert_eq!(graph.plan().schedule()[failed].id(), &id("in"));
    graph.cancel();
    assert_eq!(graph.failure_node_index(), Some(failed));
}

#[test]
fn sum_overflow_while_draining_is_terminal_and_does_not_publish_a_partial_tail() {
    let document = GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input, ChannelLayout::Stereo),
            node(
                "delay",
                NodeKind::Delay { delay_frames: 1 },
                ChannelLayout::Stereo,
            ),
            node("sum", NodeKind::Sum, ChannelLayout::Stereo),
            node("out", NodeKind::Output, ChannelLayout::Stereo),
        ],
        edges: vec![
            edge("in", "delay", 0),
            edge("delay", "sum", 0),
            edge("delay", "sum", 1),
            edge("sum", "out", 0),
        ],
        output: id("out"),
    };
    let mut graph = prepare(&document);
    let mut output = [99.0; 2];
    graph.process(&[f64::MAX; 2], &mut output).unwrap();
    assert_eq!(output, [0.0; 2]);
    output.fill(99.0);
    graph.finish(&mut output).unwrap_err();
    assert_eq!(output, [99.0; 2]);
    assert_eq!(graph.lifecycle(), GraphLifecycle::Failed);
    let failed = graph.failure_node_index().unwrap();
    assert_eq!(graph.plan().schedule()[failed].id(), &id("sum"));
    graph.finish(&mut output).unwrap_err();
}

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AllocationCounts {
    allocations: usize,
    frees: usize,
}
thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<AllocationCounts> = const { Cell::new(AllocationCounts { allocations: 0, frees: 0 }) };
}
struct TrackingAllocator;
// SAFETY: Each operation delegates to System with the caller's original pointer and layout.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(false);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count_allocation(true);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(false);
        count_allocation(true);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
fn count_allocation(free: bool) {
    if TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            if free {
                counts.frees += 1;
            } else {
                counts.allocations += 1;
            }
            cell.set(counts);
        });
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;
struct TrackingGuard;
impl Drop for TrackingGuard {
    fn drop(&mut self) {
        TRACKING.with(|active| active.set(false));
    }
}
fn tracked<T>(operation: impl FnOnce() -> T) -> (T, AllocationCounts) {
    COUNTS.with(|counts| counts.set(AllocationCounts::default()));
    TRACKING.with(|active| active.set(true));
    let guard = TrackingGuard;
    let result = operation();
    drop(guard);
    (result, COUNTS.with(Cell::get))
}

#[test]
fn prepared_filters_process_finish_reset_cancel_and_fail_without_allocating_or_freeing() {
    use earph_core::dsp_nodes::{FirAssetReference, FirPurpose, FirSha256, PeqFilter};
    let asset = FirCoefficients::new(
        FirAssetReference {
            sha256: FirSha256::new("c".repeat(64)).unwrap(),
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            taps_per_channel: 3,
            purpose: FirPurpose::Correction,
        },
        vec![vec![0.5, -0.25, 0.0], vec![0.25, 0.0, 0.0]],
    )
    .unwrap();
    let kinds = [
        NodeKind::Peq {
            filter: PeqFilter::Peaking {
                frequency_hz: scalar(1000.0),
                q: scalar(0.707),
                gain_db: scalar(-6.0),
            },
        },
        NodeKind::Convolution {
            asset: asset.reference().clone(),
        },
    ];
    for kind in kinds {
        let plan = compile_with_assets(
            &chain(kind),
            &context(ChannelLayout::Stereo, ChannelLayout::Stereo),
            std::slice::from_ref(&asset),
        )
        .unwrap();
        let mut graph = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
        let mut output = [0.0; 16];
        let ((ran, finished, reset, failed, reset_after_failure, cancelled), counts) =
            tracked(|| {
                let ran = graph.process(&[0.2; 16], &mut output).is_ok();
                let mut finished = false;
                for _ in 0..131074 {
                    match graph.finish(&mut output) {
                        Ok(0) => {
                            finished = true;
                            break;
                        }
                        Ok(_) => {}
                        Err(_) => break,
                    }
                }
                let reset = graph
                    .reset(Epochs {
                        session: 0,
                        stream: 1,
                    })
                    .is_ok();
                output.fill(99.0);
                let failed =
                    graph.process(&[f64::NAN; 16], &mut output).is_err() && output == [99.0; 16];
                let reset_after_failure = graph
                    .reset(Epochs {
                        session: 0,
                        stream: 2,
                    })
                    .is_ok();
                graph.cancel();
                let cancelled = graph.process(&[0.0; 16], &mut output).is_err()
                    && graph
                        .reset(Epochs {
                            session: 0,
                            stream: 3,
                        })
                        .is_err();
                (ran, finished, reset, failed, reset_after_failure, cancelled)
            });
        assert_eq!(counts, AllocationCounts::default());
        assert!(ran && finished && reset && failed && reset_after_failure && cancelled);
    }
}

#[test]
fn prepared_processing_drain_reset_cancel_and_failures_allocate_and_free_nothing() {
    let (calibration, counts) = tracked(|| std::hint::black_box(vec![7_u8; 128]));
    assert!(counts.allocations > 0);
    let (_, counts) = tracked(|| drop(calibration));
    assert!(counts.frees > 0);

    let mut document = branches();
    document
        .nodes
        .iter_mut()
        .find(|node| node.id == id("dry"))
        .unwrap()
        .kind = NodeKind::Gain {
        gain_linear: scalar(2.0),
    };
    document
        .nodes
        .iter_mut()
        .find(|node| node.id == id("post"))
        .unwrap()
        .kind = NodeKind::Matrix {
        input_channels: ChannelLayout::Stereo,
        coefficients: coefficients([0.0, 1.0, 1.0, 0.0]),
    };
    document
        .nodes
        .push(node("meter", NodeKind::Meter, ChannelLayout::Stereo));
    document.edges.retain(|edge| edge.to != id("out"));
    document
        .edges
        .extend([edge("post", "meter", 0), edge("meter", "out", 0)]);
    let mut graph = prepare(&document);
    let mut overflow = prepare(&chain(NodeKind::Gain {
        gain_linear: scalar(f64::MAX),
    }));
    let meter = id("meter");
    let mut output = [0.0; 16];
    let (results, counts) = tracked(|| {
        let first = graph.process(&[1.0; 16], &mut output);
        let short = graph.process(&[2.0, 3.0], &mut output[..2]);
        let tail = graph.finish(&mut output);
        let end = graph.finish(&mut output);
        let observation = graph.meter(&meter);
        let reset = graph.reset(Epochs {
            session: 0,
            stream: 1,
        });
        let geometry = graph.process(&[1.0], &mut output[..2]);
        let bad_input = graph.process(&[f64::NAN, 0.0], &mut output[..2]);
        let reset_after_failure = graph.reset(Epochs {
            session: 1,
            stream: 0,
        });
        let overflow_result = overflow.process(&[2.0, 0.0], &mut output[..2]);
        graph.cancel();
        let cancelled = graph.finish(&mut output);
        (
            first,
            short,
            tail,
            end,
            observation,
            reset,
            geometry,
            bad_input,
            reset_after_failure,
            overflow_result,
            cancelled,
        )
    });
    assert_eq!(counts, AllocationCounts::default());
    assert_eq!(results.0.unwrap(), 8);
    assert_eq!(results.1.unwrap(), 1);
    assert_eq!(results.2.unwrap(), 3);
    assert_eq!(results.3.unwrap(), 0);
    assert_eq!(
        results.4,
        Some(MeterObservation {
            peak_abs: 9.0,
            sample_count: 24
        })
    );
    results.5.unwrap();
    assert!(results.6.is_err());
    assert!(results.7.is_err());
    results.8.unwrap();
    assert!(results.9.is_err());
    assert!(results.10.is_err());
}
