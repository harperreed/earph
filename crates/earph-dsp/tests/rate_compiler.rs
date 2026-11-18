// ABOUTME: Compiles explicit rate-domain graphs into the real bounded stream renderer.
// ABOUTME: Checks serial cuts, actual peak contracts, resource admission, and stable identities.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    dsp_nodes::SrcPreset,
    ids::Epochs,
};
use earph_dsp::resample::PreparedSrc;
use earph_dsp::stream::{PreparedStream, StreamBudget};
use earph_dsp::*;
use std::sync::{Arc, atomic::AtomicBool};

fn context(input: u32, output: u32) -> CompileContext {
    CompileContext {
        input_format: PcmFormat::new(input, ChannelLayout::Stereo, PcmRepresentation::Float64)
            .unwrap(),
        output_format: PcmFormat::new(output, ChannelLayout::Stereo, PcmRepresentation::Float64)
            .unwrap(),
        quantum_frames: 64,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    }
}
fn node(id: &str, kind: NodeKind, rate: u32) -> Node {
    Node {
        id: NodeId::new(id).unwrap(),
        kind,
        format: PortFormat {
            domain: SampleDomain::Pcm,
            sample_rate_hz: rate,
            channels: ChannelLayout::Stereo,
            precision: PcmRepresentation::Float64,
            max_quantum_frames: 512,
        },
    }
}
fn src(input: u32) -> NodeKind {
    NodeKind::Src {
        input_rate_hz: input,
        preset: SrcPreset::Responsive,
        chunk_frames: 64,
    }
}
fn document(nodes: Vec<Node>, edges: &[(&str, &str, usize)]) -> GraphDocument {
    GraphDocument {
        schema_version: 1,
        output: NodeId::new("out").unwrap(),
        nodes,
        edges: edges
            .iter()
            .map(|(from, to, to_port)| Edge {
                from: NodeId::new(*from).unwrap(),
                to: NodeId::new(*to).unwrap(),
                from_port: 0,
                to_port: *to_port,
            })
            .collect(),
    }
}
fn serial() -> GraphDocument {
    document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("down", src(48000), 44100),
            node("up", src(44100), 48000),
            node("out", NodeKind::Output, 48000),
        ],
        &[("in", "down", 0), ("down", "up", 0), ("up", "out", 0)],
    )
}
fn convert(input_rate: u32, output_rate: u32, input: &[f64]) -> Vec<f64> {
    let mut src = PreparedSrc::prepare(
        input_rate,
        output_rate,
        ChannelLayout::Stereo,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let mut output = [0.0; 74];
    let mut result = Vec::new();
    let mut offset = 0;
    for _ in 0..100000 {
        let step = src.process(&input[offset..], &mut output).unwrap();
        offset += step.consumed_frames * 2;
        result.extend_from_slice(&output[..step.produced_frames * 2]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    src.begin_finish().unwrap();
    for _ in 0..100000 {
        let step = src.finish(&mut output).unwrap();
        result.extend_from_slice(&output[..step.produced_frames * 2]);
        if step.terminal {
            return result;
        }
    }
    panic!("SRC reference did not finish")
}
fn render(plan: CompiledStreamPlan, input: &[f64]) -> (Vec<f64>, PreparedStream) {
    let mut graph = PreparedStream::prepare(
        plan.into_stream_plan(),
        Epochs::default(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let mut output = [0.0; 6];
    let mut result = Vec::new();
    let mut offset = 0;
    for _ in 0..100000 {
        let end = (offset + 14).min(input.len());
        let step = graph.pump(&input[offset..end], &mut output).unwrap();
        offset += step.consumed_frames * 2;
        result.extend_from_slice(&output[..step.produced_frames * 2]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    graph.begin_finish().unwrap();
    for _ in 0..100000 {
        let step = graph.finish(&mut output).unwrap();
        result.extend_from_slice(&output[..step.produced_frames * 2]);
        if step.terminal {
            return (result, graph);
        }
    }
    panic!("compiled stream did not finish")
}

#[test]
fn serial_rate_graph_compiles_and_runs_actual_src_kernels() {
    let graph = serial();
    let before = serde_json::to_vec(&graph).unwrap();
    let plan =
        compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default()).unwrap();
    assert_eq!(serde_json::to_vec(&graph).unwrap(), before);
    assert_eq!(plan.input_format(), context(48000, 48000).input_format);
    assert_eq!(plan.output_format(), context(48000, 48000).output_format);
    assert_eq!(plan.content_hash().len(), 64);
    let all_ids: Vec<_> = plan
        .stages()
        .iter()
        .flat_map(|s| s.original_nodes.iter().cloned())
        .collect();
    assert_eq!(all_ids.len(), graph.nodes.len());
    for n in &graph.nodes {
        assert!(all_ids.contains(&n.id));
    }
    let input: Vec<_> = (0..146).map(|i| (i as f64 * 0.23).sin() * 0.125).collect();
    let expected = convert(44100, 48000, &convert(48000, 44100, &input));
    let (output, runtime) = render(plan, &input);
    assert_eq!(output, expected);
    assert_eq!(runtime.stats().source_frames, 73);
}

#[test]
fn unity_src_rejects_strict_preservation_and_old_same_rate_compiler() {
    let graph = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("convert", src(48000), 48000),
            node("out", NodeKind::Output, 48000),
        ],
        &[("in", "convert", 0), ("convert", "out", 0)],
    );
    assert_eq!(
        compile(&graph, &context(48000, 48000)).unwrap_err().code,
        earph_core::ErrorCode::CapabilityUnsupported
    );
    let mut strict = context(48000, 48000);
    strict.policy = ProcessingPolicy::StrictPreservation;
    let error = compile_stream(&graph, &strict, &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::FidelityPolicyConflict);
    assert_eq!(error.node.unwrap().as_str(), "convert");
}

#[test]
fn rate_boundary_cannot_be_bypassed_even_when_the_nominal_rate_is_unchanged() {
    let graph = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("convert", src(48000), 48000),
            node("sum", NodeKind::Sum, 48000),
            node("out", NodeKind::Output, 48000),
        ],
        &[
            ("in", "convert", 0),
            ("convert", "sum", 0),
            ("in", "sum", 1),
            ("sum", "out", 0),
        ],
    );
    let error = compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::CapabilityUnsupported);
    assert_eq!(error.node.unwrap().as_str(), "convert");
}

#[test]
fn src_requires_a_port_budget_for_actual_input_and_output_spans() {
    let mut graph = serial();
    graph.nodes[1].format.max_quantum_frames = 64;
    let error = compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::ResourceBudgetExceeded);
    assert_eq!(error.node.unwrap().as_str(), "down");
    graph.nodes[1].format.max_quantum_frames = 512;
    let mut finite = context(48000, 48000);
    finite.budget.max_latency_frames = 100000;
    let error = compile_stream(&graph, &finite, &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::CapabilityUnsupported);
}

#[test]
fn same_rate_branches_align_on_both_sides_of_src_and_keep_original_edge_diagnostics() {
    let graph = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("delay_before", NodeKind::Delay { delay_frames: 3 }, 48000),
            node("sum_before", NodeKind::Sum, 48000),
            node("convert", src(48000), 44100),
            node("delay_after", NodeKind::Delay { delay_frames: 2 }, 44100),
            node("sum_after", NodeKind::Sum, 44100),
            node("out", NodeKind::Output, 44100),
        ],
        &[
            ("in", "delay_before", 0),
            ("in", "sum_before", 0),
            ("delay_before", "sum_before", 1),
            ("sum_before", "convert", 0),
            ("convert", "delay_after", 0),
            ("convert", "sum_after", 0),
            ("delay_after", "sum_after", 1),
            ("sum_after", "out", 0),
        ],
    );
    let plan =
        compile_stream(&graph, &context(48000, 44100), &[], StreamBudget::default()).unwrap();
    let ids: std::collections::BTreeSet<_> = graph.nodes.iter().map(|n| n.id.clone()).collect();
    let mut compensation = 0;
    for transformation in plan.transformations() {
        if let Transformation::Compensation { edge, .. } = transformation {
            assert!(ids.contains(&edge.from) && ids.contains(&edge.to));
            compensation += 1;
        }
    }
    assert_eq!(compensation, 2);
    let input: Vec<_> = (0..130).map(|i| (i as f64 * 0.17).sin() * 0.125).collect();
    let mut before = vec![0.0; 6];
    before.extend(input.iter().map(|v| v * 2.0));
    let converted = convert(48000, 44100, &before);
    let mut expected = vec![0.0; 4];
    expected.extend(converted.iter().map(|v| v * 2.0));
    let (actual, runtime) = render(plan, &input);
    assert_eq!(actual, expected);
    assert_eq!(runtime.stats().source_frames, 65);
    assert_eq!(runtime.src_stats(1).unwrap().real_input_frames, 68);
}

#[test]
fn invalid_src_geometry_and_cross_rate_edges_name_the_original_nodes() {
    let cases = [
        (
            0,
            SrcPreset::Quality,
            64,
            earph_core::ErrorCode::FormatUnsupported,
        ),
        (
            48000,
            SrcPreset::Responsive,
            3,
            earph_core::ErrorCode::FormatUnsupported,
        ),
        (
            192000,
            SrcPreset::Quality,
            64,
            earph_core::ErrorCode::FormatUnsupported,
        ),
    ];
    for (input, preset, chunk, code) in cases {
        let mut graph = serial();
        graph.nodes[1].kind = NodeKind::Src {
            input_rate_hz: input,
            preset,
            chunk_frames: chunk,
        };
        let error = compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default())
            .err()
            .unwrap();
        assert_eq!(error.code, code);
        assert_eq!(error.node.unwrap().as_str(), "down");
    }
    let mut graph = serial();
    graph.nodes[1].kind = src(44100);
    let error = compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::FormatUnsupported);
    let edge = error.edge.unwrap();
    assert_eq!(edge.from.as_str(), "in");
    assert_eq!(edge.to.as_str(), "down");
}

#[test]
fn parameter_and_coefficient_identity_changes_hash_while_admission_limits_do_not() {
    let graph = serial();
    let reference =
        compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default()).unwrap();
    let hash = reference.content_hash().to_owned();
    let mut reordered = graph.clone();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    for node in &mut reordered.nodes {
        node.format.max_quantum_frames = 1024;
    }
    let mut generous = context(48000, 48000);
    generous.budget.max_buffer_bytes *= 2;
    generous.budget.max_quantum_frames = 4096;
    let second = compile_stream(
        &reordered,
        &generous,
        &[],
        StreamBudget {
            max_sweeps: 1,
            max_resident_bytes: 128 << 20,
            max_preparation_bytes: 128 << 20,
            ..StreamBudget::default()
        },
    )
    .unwrap();
    assert_eq!(hash, second.content_hash());
    let mut changed = graph.clone();
    changed.nodes[1].kind = NodeKind::Src {
        input_rate_hz: 48000,
        preset: SrcPreset::Quality,
        chunk_frames: 64,
    };
    assert_ne!(
        hash,
        compile_stream(
            &changed,
            &context(48000, 48000),
            &[],
            StreamBudget::default()
        )
        .unwrap()
        .content_hash()
    );
    changed.nodes[1].kind = NodeKind::Src {
        input_rate_hz: 48000,
        preset: SrcPreset::Responsive,
        chunk_frames: 128,
    };
    assert_ne!(
        hash,
        compile_stream(
            &changed,
            &context(48000, 48000),
            &[],
            StreamBudget::default()
        )
        .unwrap()
        .content_hash()
    );
}

#[test]
fn synthetic_id_collisions_preserve_real_nodes_and_runtime_output() {
    let graph = serial();
    let baseline =
        compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default()).unwrap();
    let (_, runtime) = render(baseline, &[0.25, -0.25]);
    let original: std::collections::BTreeSet<_> =
        graph.nodes.iter().map(|n| n.id.clone()).collect();
    let synthetic = runtime
        .island(0)
        .unwrap()
        .plan()
        .schedule()
        .iter()
        .find(|n| !original.contains(n.id()))
        .unwrap()
        .id()
        .clone();
    let mut colliding = graph.clone();
    colliding.nodes[0].id = synthetic.clone();
    for e in &mut colliding.edges {
        if e.from.as_str() == "in" {
            e.from = synthetic.clone();
        }
    }
    let plan = compile_stream(
        &colliding,
        &context(48000, 48000),
        &[],
        StreamBudget::default(),
    )
    .unwrap();
    assert!(
        plan.stages()
            .iter()
            .flat_map(|s| &s.original_nodes)
            .any(|id| id == &synthetic)
    );
    let stage_count = plan.stages().len();
    let (output, prepared) = render(plan, &[0.25, -0.25]);
    assert_eq!(
        output,
        convert(44100, 48000, &convert(48000, 44100, &[0.25, -0.25]))
    );
    for i in 0..stage_count {
        if let Some(island) = prepared.island(i) {
            let unique: std::collections::BTreeSet<_> =
                island.plan().schedule().iter().map(|n| n.id()).collect();
            assert_eq!(unique.len(), island.plan().schedule().len());
        }
    }
}

#[test]
fn intermediate_fir_preserves_authored_support_and_asset_identity() {
    use earph_core::dsp_nodes::{FirAssetReference, FirPurpose, FirSha256};
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
    let graph = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("up", src(48000), 96000),
            node(
                "fir",
                NodeKind::Convolution {
                    asset: asset.reference().clone(),
                },
                96000,
            ),
            node("down", src(96000), 48000),
            node("out", NodeKind::Output, 48000),
        ],
        &[
            ("in", "up", 0),
            ("up", "fir", 0),
            ("fir", "down", 0),
            ("down", "out", 0),
        ],
    );
    let plan = compile_stream(
        &graph,
        &context(48000, 48000),
        &[asset],
        StreamBudget::default(),
    )
    .unwrap();
    let input: Vec<_> = (0..146).map(|i| (i as f64 * 0.31).sin() * 0.125).collect();
    let first = convert(48000, 96000, &input);
    let mut convolved = vec![0.0; first.len() + 516];
    for (frame, pair) in first.as_chunks::<2>().0.iter().enumerate() {
        convolved[(256 + frame) * 2] += pair[0] * 0.25;
        convolved[(257 + frame) * 2] -= pair[0] * 0.125;
        convolved[(256 + frame) * 2 + 1] += pair[1] * 0.5;
    }
    let expected = convert(96000, 48000, &convolved);
    let (actual, runtime) = render(plan, &input);
    assert_eq!(actual.len(), expected.len());
    assert!(
        actual
            .iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 1e-12)
    );
    assert_eq!(
        runtime.src_stats(3).unwrap().real_input_frames,
        (convolved.len() / 2) as u64
    );
}

#[test]
fn final_peq_uses_actual_src_peak_bounds_and_earlier_peq_is_rejected() {
    use earph_core::dsp_nodes::PeqFilter;
    let peq = NodeKind::Peq {
        filter: PeqFilter::Peaking {
            frequency_hz: FiniteGraphScalar::new(1000.0).unwrap(),
            q: FiniteGraphScalar::new(0.707).unwrap(),
            gain_db: FiniteGraphScalar::new(-6.0).unwrap(),
        },
    };
    let graph = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("convert", src(48000), 44100),
            node("peq", peq.clone(), 44100),
            node("out", NodeKind::Output, 44100),
        ],
        &[
            ("in", "convert", 0),
            ("convert", "peq", 0),
            ("peq", "out", 0),
        ],
    );
    let plan =
        compile_stream(&graph, &context(48000, 44100), &[], StreamBudget::default()).unwrap();
    let last = plan.stages().last().unwrap();
    assert!(last.input_peak_bounds.iter().all(|x| *x > 1.0));
    let CompiledStreamStageKind::Island {
        terminal_output_bounds,
        ..
    } = &last.kind
    else {
        panic!("PEQ must be an island")
    };
    assert!(terminal_output_bounds.iter().all(|b| *b <= 1e-12));
    let input = [0.5, -0.5, 0.25, -0.25];
    let (output, _) = render(plan, &input);
    assert!(output.iter().any(|x| x.abs() > 0.1));
    let early = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("peq", peq, 48000),
            node("convert", src(48000), 44100),
            node("out", NodeKind::Output, 44100),
        ],
        &[
            ("in", "peq", 0),
            ("peq", "convert", 0),
            ("convert", "out", 0),
        ],
    );
    let error = compile_stream(&early, &context(48000, 44100), &[], StreamBudget::default())
        .err()
        .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::CapabilityUnsupported);
    assert_eq!(error.node.unwrap().as_str(), "peq");
}

#[test]
fn src_timing_reports_guard_padding_and_unknown_library_delay_without_fake_exact_latency() {
    let plan = compile_stream(
        &serial(),
        &context(48000, 48000),
        &[],
        StreamBudget::default(),
    )
    .unwrap();
    let mut count = 0;
    for stage in plan.stages() {
        if let CompiledStreamStageKind::Src {
            parameters,
            guard_time_denominator,
            maximum_adapter_finish_output_frames,
            truncated_library_output_delay_frames,
            ..
        } = &stage.kind
        {
            assert_eq!(parameters.guard_time_numerator, 1);
            assert_eq!(*guard_time_denominator, parameters.input_rate);
            assert!(maximum_adapter_finish_output_frames > &0);
            assert_eq!(*truncated_library_output_delay_frames, None);
            count += 1;
        }
    }
    assert_eq!(count, 2);
    let (_, runtime) = render(plan, &[0.25, -0.25]);
    assert!(runtime.src_metadata(1).is_some());
}

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
#[derive(Clone, Copy, Default, Debug)]
struct Allocations {
    live: usize,
    peak: usize,
}
thread_local! {static METER:Cell<Option<Allocations>>=const{Cell::new(None)};}
struct AllocationMeter;
#[global_allocator]
static ALLOCATOR: AllocationMeter = AllocationMeter;
unsafe impl GlobalAlloc for AllocationMeter {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let _ = METER.try_with(|m| {
                if let Some(mut n) = m.get() {
                    n.live += l.size();
                    n.peak = n.peak.max(n.live);
                    m.set(Some(n));
                }
            });
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            let _ = METER.try_with(|m| {
                if let Some(mut n) = m.get() {
                    n.live += l.size();
                    n.peak = n.peak.max(n.live);
                    m.set(Some(n));
                }
            });
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        let _ = METER.try_with(|m| {
            if let Some(mut n) = m.get() {
                n.live = n.live.saturating_sub(l.size());
                m.set(Some(n));
            }
        });
        unsafe { System.dealloc(p, l) };
    }
}
fn measured<T>(f: impl FnOnce() -> T) -> (T, Allocations) {
    METER.with(|m| m.set(Some(Allocations::default())));
    let result = f();
    let a = METER.with(|m| m.replace(None).unwrap());
    (result, a)
}

#[test]
fn complete_peak_admission_precedes_src_coefficient_allocation() {
    let mut graph = serial();
    for n in &mut graph.nodes {
        if let NodeKind::Src { preset, .. } = &mut n.kind {
            *preset = SrcPreset::Quality;
        }
    }
    let valid =
        compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default()).unwrap();
    let required = valid.resources().preparation_peak_bytes;
    let (rejected, allocations) = measured(|| {
        compile_stream(
            &graph,
            &context(48000, 48000),
            &[],
            StreamBudget {
                max_preparation_bytes: required - 1,
                ..StreamBudget::default()
            },
        )
    });
    assert_eq!(
        rejected.err().unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert!(
        allocations.peak < 1 << 20,
        "coefficient construction occurred before aggregate admission: {allocations:?}"
    );
    drop(valid);
    let ((prepared, resources), allocations) = measured(|| {
        let plan =
            compile_stream(&graph, &context(48000, 48000), &[], StreamBudget::default()).unwrap();
        let resources = plan.resources();
        let prepared = PreparedStream::prepare(
            plan.into_stream_plan(),
            Epochs::default(),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        (prepared, resources)
    });
    assert!(
        allocations.peak as u64 <= resources.preparation_peak_bytes,
        "{allocations:?} exceeded {resources:?}"
    );
    assert!(allocations.live as u64 <= resources.resident_bytes);
    drop(prepared);
}

#[test]
fn rate_compiler_rejects_parallel_converters_cycles_and_implicit_channel_changes() {
    let parallel = document(
        vec![
            node("in", NodeKind::Input, 48000),
            node("left", src(48000), 44100),
            node("right", src(48000), 44100),
            node("sum", NodeKind::Sum, 44100),
            node("out", NodeKind::Output, 44100),
        ],
        &[
            ("in", "left", 0),
            ("in", "right", 0),
            ("left", "sum", 0),
            ("right", "sum", 1),
            ("sum", "out", 0),
        ],
    );
    let error = compile_stream(
        &parallel,
        &context(48000, 44100),
        &[],
        StreamBudget::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::CapabilityUnsupported);
    assert!(matches!(error.node.unwrap().as_str(), "left" | "right"));

    let mut cyclic = serial();
    cyclic.edges.push(Edge {
        from: NodeId::new("up").unwrap(),
        to: NodeId::new("down").unwrap(),
        from_port: 0,
        to_port: 0,
    });
    assert_eq!(
        compile_stream(
            &cyclic,
            &context(48000, 48000),
            &[],
            StreamBudget::default()
        )
        .err()
        .unwrap()
        .code,
        earph_core::ErrorCode::GraphInvalid
    );

    let mut channels = serial();
    channels.nodes[1].format.channels = ChannelLayout::Mono;
    let error = compile_stream(
        &channels,
        &context(48000, 48000),
        &[],
        StreamBudget::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::FormatUnsupported);
    assert_eq!(error.edge.unwrap().to.as_str(), "down");

    let mut precision = serial();
    precision.nodes[1].format.precision = PcmRepresentation::Signed24;
    let error = compile_stream(
        &precision,
        &context(48000, 48000),
        &[],
        StreamBudget::default(),
    )
    .err()
    .unwrap();
    assert_eq!(error.code, earph_core::ErrorCode::FormatUnsupported);
    assert_eq!(error.node.unwrap().as_str(), "down");
}
