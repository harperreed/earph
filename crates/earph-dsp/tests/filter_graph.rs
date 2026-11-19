// ABOUTME: Verifies prepared filters through the public graph compiler and renderer.
// ABOUTME: Uses direct signal references and observable tail and resource contracts.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    dsp_nodes::{FirAssetReference, FirPurpose, FirSha256, PeqFilter},
    ids::Epochs,
};
use earph_dsp::*;

fn context() -> CompileContext {
    let format = PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
    CompileContext {
        input_format: format,
        output_format: format,
        quantum_frames: 64,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    }
}
fn document(kinds: Vec<NodeKind>) -> GraphDocument {
    let mut nodes = vec![Node {
        id: NodeId::new("input").unwrap(),
        kind: NodeKind::Input,
        format: PortFormat {
            domain: SampleDomain::Pcm,
            sample_rate_hz: 48000,
            channels: ChannelLayout::Mono,
            precision: PcmRepresentation::Float64,
            max_quantum_frames: 64,
        },
    }];
    for (i, kind) in kinds.into_iter().enumerate() {
        nodes.push(Node {
            id: NodeId::new(format!("effect{i}")).unwrap(),
            kind,
            format: nodes[0].format,
        });
    }
    nodes.push(Node {
        id: NodeId::new("output").unwrap(),
        kind: NodeKind::Output,
        format: nodes[0].format,
    });
    let edges = nodes
        .windows(2)
        .map(|pair| Edge {
            from: pair[0].id.clone(),
            to: pair[1].id.clone(),
            from_port: 0,
            to_port: 0,
        })
        .collect();
    GraphDocument {
        schema_version: 1,
        output: nodes.last().unwrap().id.clone(),
        nodes,
        edges,
    }
}
fn asset(taps: Vec<f64>) -> FirCoefficients {
    FirCoefficients::new(
        FirAssetReference {
            sha256: FirSha256::new("a".repeat(64)).unwrap(),
            sample_rate_hz: 48000,
            channels: ChannelLayout::Mono,
            taps_per_channel: taps.len(),
            purpose: FirPurpose::Correction,
        },
        vec![taps],
    )
    .unwrap()
}
fn peq() -> NodeKind {
    NodeKind::Peq {
        filter: PeqFilter::Peaking {
            frequency_hz: FiniteGraphScalar::new(1000.0).unwrap(),
            q: FiniteGraphScalar::new(0.707).unwrap(),
            gain_db: FiniteGraphScalar::new(-6.0).unwrap(),
        },
    }
}
fn render(plan: CompiledPlan, input: &[f64], chunk: usize) -> Vec<f64> {
    let mut graph = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let mut result = Vec::new();
    for part in input.chunks(chunk) {
        let mut out = vec![99.0; part.len()];
        graph.process(part, &mut out).unwrap();
        result.extend(out);
    }
    loop {
        let mut out = vec![99.0; chunk];
        let n = graph.finish(&mut out).unwrap();
        assert!(out[n..].iter().all(|s| *s == 99.0));
        result.extend(&out[..n]);
        if n == 0 {
            break;
        }
    }
    assert_eq!(graph.lifecycle(), GraphLifecycle::Finished);
    result
}

#[test]
fn fir_graph_retains_authored_zeros_and_exact_partition_support() {
    let asset = asset(vec![0.5, -0.25, 0.0]);
    let graph = document(vec![NodeKind::Convolution {
        asset: asset.reference().clone(),
    }]);
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        earph_core::ErrorCode::AssetInvalid
    );
    let plan = compile_with_assets(&graph, &context(), &[asset]).unwrap();
    assert_eq!(plan.latency_frames(), 256);
    assert_eq!(plan.tail_frames(), 258);
    assert!(plan.output_peak_bounds()[0] >= 0.75);
    let input = [1.0, -0.5, 0.25, 0.0, 0.0];
    let out = render(plan.clone(), &input, 3);
    assert_eq!(out.len(), 263);
    let mut expected = vec![0.0; 263];
    for (i, x) in input.iter().enumerate() {
        expected[256 + i] += x * 0.5;
        expected[257 + i] -= x * 0.25;
    }
    assert!(out.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-12));
    assert_eq!(render(plan, &[], 7), Vec::<f64>::new());
}

#[test]
fn peq_graph_matches_the_recurrence_and_publishes_a_proved_tail() {
    let plan = compile(&document(vec![peq(), peq()]), &context()).unwrap();
    assert!(plan.tail_frames() > 0 && plan.tail_frames() <= 1048576);
    assert!(plan.terminal_output_bounds().iter().all(|b| *b <= 1e-12));
    let input: Vec<_> = (0..73)
        .map(|i| {
            if i == 72 {
                0.8
            } else {
                (i as f64 * 0.3).sin() * 0.1
            }
        })
        .collect();
    let first = render(plan.clone(), &input, 1);
    let second = render(plan.clone(), &input, 31);
    assert_eq!(first, second);
    let filter = match peq() {
        NodeKind::Peq { filter } => filter,
        _ => unreachable!(),
    };
    let design = earph_dsp::peq::PeqDesign::prepare(&filter, 48000, ChannelLayout::Mono).unwrap();
    let [b0, b1, b2, a1, a2] = design.coefficients();
    let mut expected = input.clone();
    expected.resize(first.len() + 2000, 0.0);
    for _ in 0..2 {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for x in &mut expected {
            let old = *x;
            let y = b0 * old + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = old;
            y2 = y1;
            y1 = y;
            *x = y;
        }
    }
    assert!(
        first
            .iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 1e-13)
    );
    assert!(expected[first.len()..].iter().all(|s| s.abs() <= 1e-12));
}

#[test]
fn strict_filters_and_mismatched_assets_reject_before_preparation() {
    let mut strict = context();
    strict.policy = ProcessingPolicy::StrictPreservation;
    assert_eq!(
        compile(&document(vec![peq()]), &strict).unwrap_err().code,
        earph_core::ErrorCode::FidelityPolicyConflict
    );
    let asset = asset(vec![1.0]);
    let mut graph = document(vec![NodeKind::Convolution {
        asset: asset.reference().clone(),
    }]);
    assert_eq!(
        compile_with_assets(&graph, &strict, std::slice::from_ref(&asset))
            .unwrap_err()
            .code,
        earph_core::ErrorCode::FidelityPolicyConflict
    );
    if let NodeKind::Convolution { asset } = &mut graph.nodes[1].kind {
        asset.sample_rate_hz = 44100;
    }
    assert_eq!(
        compile_with_assets(&graph, &context(), &[asset])
            .unwrap_err()
            .code,
        earph_core::ErrorCode::FormatUnsupported
    );
}

#[test]
fn bounded_islands_enforce_declared_channel_peaks_before_exposing_output() {
    let graph = document(vec![NodeKind::Gain {
        gain_linear: FiniteGraphScalar::new(2.0).unwrap(),
    }]);
    let plan = compile_with_input_bounds(&graph, &context(), &[], &[0.25]).unwrap();
    assert_eq!(plan.input_peak_bounds(), [0.25]);
    assert!((0.5..=0.500000000000001).contains(&plan.output_peak_bounds()[0]));
    let mut prepared = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let mut output = [99.0];
    prepared.process(&[0.25], &mut output).unwrap();
    assert_eq!(output, [0.5]);
    output = [99.0];
    assert!(prepared.process(&[0.3], &mut output).is_err());
    assert_eq!(output, [99.0]);
    assert_eq!(prepared.lifecycle(), GraphLifecycle::Failed);
    for invalid in [vec![], vec![-1.0], vec![f64::INFINITY], vec![0.1, 0.2]] {
        assert!(compile_with_input_bounds(&graph, &context(), &[], &invalid).is_err());
    }
}

fn direct_peq(input: &[f64]) -> Vec<f64> {
    let filter = match peq() {
        NodeKind::Peq { filter } => filter,
        _ => unreachable!(),
    };
    let [b0, b1, b2, a1, a2] =
        earph_dsp::peq::PeqDesign::prepare(&filter, 48000, ChannelLayout::Mono)
            .unwrap()
            .coefficients();
    let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
    input
        .iter()
        .map(|x| {
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = *x;
            y2 = y1;
            y1 = y;
            y
        })
        .collect()
}

#[test]
fn composed_filters_and_compensated_fanout_meet_their_actual_future_envelope() {
    let asset = asset(vec![0.5, -0.25, 0.0]);
    let fir = NodeKind::Convolution {
        asset: asset.reference().clone(),
    };
    let input: Vec<_> = (0..79)
        .map(|i| {
            if i == 78 {
                0.8
            } else {
                (i as f64 * 0.51).cos() * 0.4
            }
        })
        .collect();
    for kinds in [
        vec![fir.clone(), peq()],
        vec![peq(), fir.clone()],
        vec![fir.clone(), fir.clone()],
    ] {
        let plan = compile_with_assets(&document(kinds), &context(), std::slice::from_ref(&asset))
            .unwrap();
        assert!(plan.terminal_output_bounds()[0] <= 1e-12);
        let out = render(plan.clone(), &input, 7);
        assert_eq!(out, render(plan.clone(), &input, 63));
        assert!(out.iter().all(|v| v.abs() <= plan.output_peak_bounds()[0]));
        assert_eq!(out.len(), input.len() + plan.tail_frames() as usize);
    }
    let mut graph = document(vec![fir, peq()]);
    let output = graph.output.clone();
    graph.edges.clear();
    let sum = Node {
        id: NodeId::new("sum").unwrap(),
        kind: NodeKind::Sum,
        format: graph.nodes[0].format,
    };
    graph.nodes.push(sum.clone());
    for from in ["effect0", "effect1"] {
        graph.edges.push(Edge {
            from: NodeId::new("input").unwrap(),
            to: NodeId::new(from).unwrap(),
            from_port: 0,
            to_port: 0,
        });
        graph.edges.push(Edge {
            from: NodeId::new(from).unwrap(),
            to: sum.id.clone(),
            from_port: 0,
            to_port: usize::from(from == "effect1"),
        });
    }
    graph.edges.push(Edge {
        from: sum.id,
        to: output,
        from_port: 0,
        to_port: 0,
    });
    let plan = compile_with_assets(&graph, &context(), &[asset]).unwrap();
    assert_eq!(plan.latency_frames(), 256);
    let out = render(plan.clone(), &input, 13);
    let mut padded = input.clone();
    padded.resize(out.len() + 2000, 0.0);
    let eq = direct_peq(&padded);
    let mut expected = vec![0.0; out.len() + 2000];
    for i in 0..expected.len() - 256 {
        expected[i + 256] += eq[i];
        if i < input.len() {
            expected[i + 256] += input[i] * 0.5;
            expected[i + 257] -= input[i] * 0.25;
        }
    }
    assert!(
        out.iter()
            .zip(&expected)
            .all(|(a, b)| (a - b).abs() < 2e-13)
    );
    assert!(expected[out.len()..].iter().all(|v| v.abs() <= 1e-12));
    assert!(plan.terminal_output_bounds()[0] <= 1e-12);
    assert!(plan.output_peak_bounds()[0] >= 1.0);
}

#[test]
fn shared_coefficients_are_charged_once_and_boxed_kernels_are_charged_per_node() {
    let asset = asset(vec![0.5; 257]);
    let kind = NodeKind::Convolution {
        asset: asset.reference().clone(),
    };
    let one = compile_with_assets(
        &document(vec![kind.clone()]),
        &context(),
        std::slice::from_ref(&asset),
    )
    .unwrap();
    let two = compile_with_assets(
        &document(vec![kind.clone(), kind]),
        &context(),
        std::slice::from_ref(&asset),
    )
    .unwrap();
    assert_eq!(one.asset_bytes(), two.asset_bytes());
    assert_eq!(two.kernel_state_bytes(), one.kernel_state_bytes() * 2);
    assert!(one.kernel_state_bytes() > 33000);
    let mut tiny = context();
    tiny.budget.max_buffer_bytes = one.resources().buffer_bytes;
    assert_eq!(
        compile_with_assets(
            &document(vec![
                NodeKind::Convolution {
                    asset: asset.reference().clone()
                };
                2
            ]),
            &tiny,
            std::slice::from_ref(&asset)
        )
        .unwrap_err()
        .code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    let mut roomy = context();
    roomy.budget.max_buffer_bytes -= 1;
    assert_eq!(
        compile_with_assets(
            &document(vec![NodeKind::Convolution {
                asset: asset.reference().clone()
            }]),
            &roomy,
            std::slice::from_ref(&asset)
        )
        .unwrap()
        .content_hash(),
        one.content_hash()
    );
    let graph = PreparedGraph::prepare(two.clone(), Epochs::default()).unwrap();
    assert_eq!(
        graph.resident_audio_bytes().unwrap() as u64,
        two.resources().buffer_bytes
    );
}

#[test]
fn overflowing_normalized_peak_cascades_reject_during_compilation() {
    let graph = document(vec![
        NodeKind::Gain {
            gain_linear: FiniteGraphScalar::new(f64::MAX).unwrap(),
        },
        NodeKind::Gain {
            gain_linear: FiniteGraphScalar::new(2.0).unwrap(),
        },
    ]);
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, earph_core::ErrorCode::GraphInvalid);
    assert_eq!(error.node.unwrap().as_str(), "effect1");
    let silence = compile_with_input_bounds(&graph, &context(), &[], &[0.0]).unwrap();
    assert_eq!(silence.output_peak_bounds(), [0.0]);
    assert_eq!(render(silence, &[0.0], 1), [0.0]);
}

#[test]
fn equalizer_admission_uses_actual_input_bounds_at_slow_decay_rates() {
    let f = |v| FiniteGraphScalar::new(v).unwrap();
    let kind = NodeKind::Peq {
        filter: PeqFilter::Peaking {
            frequency_hz: f(10.0),
            q: f(20.0),
            gain_db: f(24.0),
        },
    };
    for rate in [192000, 384000, 768000] {
        let mut context = context();
        let format = PcmFormat::new(rate, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
        context.input_format = format;
        context.output_format = format;
        let mut graph = document(vec![kind.clone()]);
        for node in &mut graph.nodes {
            node.format.sample_rate_hz = rate;
        }
        let silence = compile_with_input_bounds(&graph, &context, &[], &[0.0]).unwrap();
        assert_eq!(silence.tail_frames(), 0);
        assert!(silence.terminal_output_bounds()[0] <= 1e-12);
        assert_eq!(render(silence, &[0.0; 3], 1), [0.0; 3]);
        let mut graph = document(vec![
            NodeKind::Gain {
                gain_linear: f(0.0),
            },
            kind.clone(),
        ]);
        for node in &mut graph.nodes {
            node.format.sample_rate_hz = rate;
        }
        let muted = compile(&graph, &context).unwrap();
        assert_eq!(muted.tail_frames(), 0);
        assert_eq!(render(muted, &[1.0, -1.0, 0.5], 1), [0.0; 3]);
    }
}
