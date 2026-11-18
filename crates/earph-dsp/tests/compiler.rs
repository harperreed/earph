// ABOUTME: Exercises graph compilation through real typed and serialized documents.
// ABOUTME: Checks schedule alignment, preservation, and bounded rejection paths.
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
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
        max_quantum_frames: 64,
    }
}
fn node(name: &str, kind: NodeKind) -> Node {
    Node {
        id: id(name),
        kind,
        format: port(ChannelLayout::Stereo),
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
fn context() -> CompileContext {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    CompileContext {
        input_format: format,
        output_format: format,
        quantum_frames: 64,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    }
}
fn chain(kind: NodeKind) -> GraphDocument {
    GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input),
            node("effect", kind),
            node("out", NodeKind::Output),
        ],
        edges: vec![edge("in", "effect", 0), edge("effect", "out", 0)],
        output: id("out"),
    }
}
fn branches() -> GraphDocument {
    GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input),
            node("dry", NodeKind::Bypass),
            node("slow", NodeKind::Delay { delay_frames: 3 }),
            node("sum", NodeKind::Sum),
            node("out", NodeKind::Output),
        ],
        edges: vec![
            edge("in", "dry", 0),
            edge("in", "slow", 0),
            edge("dry", "sum", 0),
            edge("slow", "sum", 1),
            edge("sum", "out", 0),
        ],
        output: id("out"),
    }
}

#[test]
fn resolves_integer_merge_compensation_and_exact_resources() {
    let plan = compile(&branches(), &context()).unwrap();
    assert_eq!(
        plan.schedule()
            .iter()
            .map(|n| n.id().as_str())
            .collect::<Vec<_>>(),
        vec!["in", "dry", "slow", "sum", "out"]
    );
    assert_eq!(
        plan.schedule()[3].inputs(),
        &[
            CompiledInput {
                source_index: 1,
                target_port: 0,
                compensation_frames: 3
            },
            CompiledInput {
                source_index: 2,
                target_port: 1,
                compensation_frames: 0
            }
        ]
    );
    assert_eq!(plan.latency_frames(), 3);
    assert_eq!(plan.tail_frames(), 3);
    // Five 64-frame stereo outputs, plus two 3-frame stereo delay states.
    assert_eq!(
        plan.resources(),
        ResourceUsage {
            buffer_bytes: 5216,
            node_buffer_bytes: 5120,
            delay_state_bytes: 96
        }
    );
    assert!(
        plan.transformations()
            .contains(&Transformation::Compensation {
                edge: edge("dry", "sum", 0),
                delay_frames: 3
            })
    );
    assert_eq!(plan.preservation(), PreservationDecision::ProcessFloat64);
}

#[test]
fn reorder_preserves_plan_hash_and_semantic_changes_invalidate_it() {
    let graph = branches();
    let first = compile(&graph, &context()).unwrap();
    let mut reordered = graph.clone();
    reordered.nodes.reverse();
    reordered.edges.reverse();
    assert_eq!(compile(&reordered, &context()).unwrap(), first);
    reordered
        .nodes
        .iter_mut()
        .find(|n| n.id == id("slow"))
        .unwrap()
        .kind = NodeKind::Delay { delay_frames: 4 };
    assert_ne!(
        compile(&reordered, &context()).unwrap().content_hash(),
        first.content_hash()
    );
    assert_eq!(first.content_hash().len(), 64);
    assert!(!first.implementation_version().is_empty());
    let mut changed_context = context();
    changed_context.quantum_frames = 32;
    assert_ne!(
        compile(&graph, &changed_context).unwrap().content_hash(),
        first.content_hash()
    );
}

#[test]
fn matrix_declares_input_and_output_channel_layouts() {
    let mut graph = chain(NodeKind::Matrix {
        input_channels: ChannelLayout::Stereo,
        coefficients: coefficients([0.5, 0.5]),
    });
    graph.nodes[1].format.channels = ChannelLayout::Mono;
    graph.nodes[2].format.channels = ChannelLayout::Mono;
    let mut ctx = context();
    ctx.output_format =
        PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
    let plan = compile(&graph, &ctx).unwrap();
    assert_eq!(plan.schedule()[1].format().channels, ChannelLayout::Mono);
    assert_eq!(plan.resources().node_buffer_bytes, 2048);
    assert_eq!(
        plan.transformations(),
        &[Transformation::Matrix { node: id("effect") }]
    );
    graph.nodes[1].kind = NodeKind::Matrix {
        input_channels: ChannelLayout::Mono,
        coefficients: coefficients([1.0]),
    };
    let error = compile(&graph, &ctx).unwrap_err();
    assert_eq!(error.code, ErrorCode::FormatUnsupported);
    assert_eq!(error.edge, Some(edge("in", "effect", 0)));
}

#[test]
fn strict_policy_retains_integer_identity_without_f64_conversion() {
    let mut graph = chain(NodeKind::Bypass);
    for n in &mut graph.nodes {
        n.format.precision = PcmRepresentation::Signed24;
    }
    let mut ctx = context();
    ctx.input_format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    ctx.output_format = ctx.input_format;
    ctx.policy = ProcessingPolicy::StrictPreservation;
    let plan = compile(&graph, &ctx).unwrap();
    assert_eq!(plan.preservation(), PreservationDecision::RetainInteger);
    assert!(plan.transformations().is_empty());
    assert_eq!(plan.resources().buffer_bytes, 0);
    graph.nodes[1].kind = NodeKind::Gain {
        gain_linear: scalar(0.5),
    };
    assert_eq!(
        compile(&graph, &ctx).unwrap_err().code,
        ErrorCode::FidelityPolicyConflict
    );
    ctx.policy = ProcessingPolicy::AllowProcessing;
    assert_eq!(
        compile(&graph, &ctx).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
}

#[test]
fn identity_nodes_and_meter_compile_as_unchanged_float64_in_strict_mode() {
    let mut ctx = context();
    ctx.policy = ProcessingPolicy::StrictPreservation;
    for kind in [
        NodeKind::Bypass,
        NodeKind::Delay { delay_frames: 0 },
        NodeKind::Meter,
    ] {
        let plan = compile(&chain(kind), &ctx).unwrap();
        assert_eq!(plan.preservation(), PreservationDecision::UnchangedFloat64);
        assert!(plan.transformations().is_empty());
    }
    for kind in [
        NodeKind::Gain {
            gain_linear: scalar(1.0),
        },
        NodeKind::Sum,
        NodeKind::Gain {
            gain_linear: scalar(2.0),
        },
        NodeKind::Delay { delay_frames: 1 },
        NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: coefficients([1.0, 0.0, 0.0, 1.0]),
        },
        NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: coefficients([0.0, 1.0, 1.0, 0.0]),
        },
    ] {
        assert_eq!(
            compile(&chain(kind), &ctx).unwrap_err().code,
            ErrorCode::FidelityPolicyConflict
        );
    }
    assert_eq!(
        compile(&branches(), &ctx).unwrap_err().code,
        ErrorCode::FidelityPolicyConflict
    );
}

#[test]
fn rejects_topology_and_port_errors_with_owned_location() {
    let original = chain(NodeKind::Bypass);
    let mut graph = original.clone();
    graph.nodes.push(graph.nodes[1].clone());
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, ErrorCode::GraphInvalid);
    assert_eq!(error.node, Some(id("effect")));
    graph = original.clone();
    graph.edges[0].from = id("missing");
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, ErrorCode::GraphInvalid);
    assert_eq!(error.edge, Some(graph.edges[0].clone()));
    graph = original.clone();
    graph.edges.push(edge("out", "effect", 1));
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
    graph = original.clone();
    graph.edges[0].from_port = 1;
    assert_eq!(
        compile(&graph, &context()).unwrap_err().edge,
        Some(graph.edges[0].clone())
    );
    graph = original.clone();
    graph.edges[0].to_port = 1;
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
    graph = original.clone();
    graph.nodes.push(node("unused", NodeKind::Meter));
    assert_eq!(
        compile(&graph, &context()).unwrap_err().node,
        Some(id("unused"))
    );
    graph = original.clone();
    graph.edges.pop();
    assert_eq!(
        compile(&graph, &context()).unwrap_err().node,
        Some(id("out"))
    );
    graph = original.clone();
    graph.edges.push(graph.edges[0].clone());
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
    graph = original;
    graph.output = id("effect");
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
}

#[test]
fn cycle_diagnosis_identifies_cycle_member_instead_of_downstream_output() {
    let graph = GraphDocument {
        schema_version: 1,
        nodes: vec![
            node("in", NodeKind::Input),
            node("b-loop", NodeKind::Sum),
            node("c-loop", NodeKind::Bypass),
            node("a-output", NodeKind::Output),
        ],
        edges: vec![
            edge("in", "b-loop", 0),
            edge("b-loop", "c-loop", 0),
            edge("c-loop", "b-loop", 1),
            edge("b-loop", "a-output", 0),
        ],
        output: id("a-output"),
    };
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, ErrorCode::GraphInvalid);
    assert!(
        matches!(
            error.node.as_ref().map(NodeId::as_str),
            Some("b-loop" | "c-loop")
        ),
        "cycle diagnosis must identify an actual cycle member: {error:?}"
    );
}

#[test]
fn rejects_a_true_cycle_with_otherwise_valid_port_counts() {
    let mut graph = branches();
    graph.nodes[1].kind = NodeKind::Sum;
    graph.edges.push(edge("sum", "dry", 1));
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
}

#[test]
fn rejects_invalid_matrix_shape_with_node_diagnostics() {
    for kind in [
        NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: coefficients([1.0]),
        },
        NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: coefficients([1.0; 5]),
        },
    ] {
        let error = compile(&chain(kind), &context()).unwrap_err();
        assert_eq!(error.code, ErrorCode::GraphInvalid);
        assert_eq!(error.node, Some(id("effect")));
    }
}

#[test]
fn rejects_domains_rates_precision_and_external_format_mismatch() {
    let original = chain(NodeKind::Bypass);
    let mut graph = original.clone();
    graph.nodes[1].format.domain = SampleDomain::Dsd;
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
    assert_eq!(error.node, Some(id("effect")));
    graph = original.clone();
    graph.nodes[1].format.sample_rate_hz = 96000;
    let error = compile(&graph, &context()).unwrap_err();
    assert_eq!(error.code, ErrorCode::FormatUnsupported);
    assert_eq!(error.edge, Some(edge("in", "effect", 0)));
    graph = original.clone();
    graph.nodes[1].format.precision = PcmRepresentation::Signed16;
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    graph = original.clone();
    graph.nodes[1].format.sample_rate_hz = 0;
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    let mut ctx = context();
    ctx.output_format =
        PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    assert_eq!(
        compile(&original, &ctx).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    graph = branches();
    graph.nodes[2].format.sample_rate_hz = 96000;
    assert_eq!(
        compile(&graph, &context()).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
}

#[test]
fn rejects_node_edge_quantum_memory_and_latency_budgets_before_allocation() {
    let graph = branches();
    for budget in [
        GraphBudget {
            max_nodes: 4,
            ..GraphBudget::default()
        },
        GraphBudget {
            max_edges: 4,
            ..GraphBudget::default()
        },
        GraphBudget {
            max_quantum_frames: 63,
            ..GraphBudget::default()
        },
        GraphBudget {
            max_buffer_bytes: 5215,
            ..GraphBudget::default()
        },
        GraphBudget {
            max_latency_frames: 2,
            ..GraphBudget::default()
        },
    ] {
        let ctx = CompileContext {
            budget,
            ..context()
        };
        assert_eq!(
            compile(&graph, &ctx).unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
    let mut ctx = context();
    ctx.quantum_frames = 0;
    assert_eq!(
        compile(&graph, &ctx).unwrap_err().code,
        ErrorCode::GraphInvalid
    );
    ctx.quantum_frames = 8193;
    ctx.budget.max_quantum_frames = usize::MAX;
    assert_eq!(
        compile(&graph, &ctx).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut too_many = chain(NodeKind::Bypass);
    for i in 0..126 {
        too_many
            .nodes
            .push(node(&format!("unused-{i}"), NodeKind::Bypass));
    }
    ctx = context();
    ctx.budget.max_nodes = usize::MAX;
    assert_eq!(
        compile(&too_many, &ctx).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut large = chain(NodeKind::Bypass);
    large.edges = vec![edge("in", "effect", 0); 257];
    ctx.budget.max_edges = usize::MAX;
    assert_eq!(
        compile(&large, &ctx).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut narrow = graph.clone();
    narrow.nodes[0].format.max_quantum_frames = 63;
    assert_eq!(
        compile(&narrow, &context()).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn rejects_delay_arithmetic_overflow_before_preparation() {
    let error = compile(
        &chain(NodeKind::Delay {
            delay_frames: u64::MAX,
        }),
        &context(),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ResourceBudgetExceeded);
    assert_eq!(error.node, Some(id("effect")));
}

#[test]
fn documents_roundtrip_and_unsupported_features_reject() {
    let graph = branches();
    let document = serde_json::to_vec(&graph).unwrap();
    let parsed: GraphDocument = serde_json::from_slice(&document).unwrap();
    let resolved = compile(&parsed, &context()).unwrap();
    assert_eq!(resolved, compile(&graph, &context()).unwrap());
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let plan_document = serde_json::to_vec_pretty(&resolved).unwrap();
    let preview: serde_json::Value = serde_json::from_slice(&plan_document).unwrap();
    assert_eq!(preview["latency_frames"], 3);
    assert_eq!(preview["tail_frames"], 3);
    assert_eq!(preview["resources"]["buffer_bytes"], 5216);
    assert_eq!(preview["preservation"], "process_float64");
    if let Some(destination) = std::env::var_os("EARPH_DSP_FIXTURE_DIR") {
        let evidence =
            std::path::PathBuf::from(destination).join(format!("{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&evidence).unwrap();
        std::fs::write(evidence.join("branch-merge.graph.json"), document).unwrap();
        std::fs::write(evidence.join("branch-merge.plan.json"), plan_document).unwrap();
    }
    let mut unknown = graph;
    unknown.schema_version = 2;
    assert_eq!(
        compile(&unknown, &context()).unwrap_err().code,
        ErrorCode::CapabilityUnsupported
    );
    let node_json =
        serde_json::to_string(&node("effect", NodeKind::Delay { delay_frames: 1 })).unwrap();
    assert!(
        serde_json::from_str::<Node>(
            &node_json.replace("\"delay_frames\":1", "\"delay_frames\":0.5")
        )
        .is_err()
    );
    assert!(serde_json::from_str::<Node>(&node_json.replace("\"delay\"", "\"peq\"")).is_err());
    assert!(NodeId::new("").is_err());
    assert!(NodeId::new("line\nbreak").is_err());
    assert!(NodeId::new("a".repeat(129)).is_err());
}

#[test]
fn serialized_graph_rejects_oversized_intent_before_compilation() {
    let mut graph = chain(NodeKind::Bypass);
    graph.nodes = vec![graph.nodes[0].clone(); 129];
    assert!(serde_json::from_slice::<GraphDocument>(&serde_json::to_vec(&graph).unwrap()).is_err());
    graph = chain(NodeKind::Bypass);
    graph.edges = vec![graph.edges[0].clone(); 257];
    assert!(serde_json::from_slice::<GraphDocument>(&serde_json::to_vec(&graph).unwrap()).is_err());
    graph = chain(NodeKind::Matrix {
        input_channels: ChannelLayout::Stereo,
        coefficients: coefficients([1.0; 5]),
    });
    assert!(serde_json::from_slice::<GraphDocument>(&serde_json::to_vec(&graph).unwrap()).is_err());
}
