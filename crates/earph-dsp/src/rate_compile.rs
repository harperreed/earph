// ABOUTME: Resolves serial SRC cuts into bounded same-rate islands and stream stages.
// ABOUTME: Preserves original node identities, actual peak certificates, and honest per-domain timing.
use crate::{
    compile::{IslandBuilder, ValidatedGraph, resolve_island, validate_graph},
    resample::{SrcDesign, SrcParameters},
    stream::{
        StreamBudget, StreamError, StreamErrorKind, StreamPlan, StreamResources, StreamStagePlan,
        StreamStageRequirement,
    },
    *,
};
use earph_core::{
    ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const VERSION: &str = "earph-dsp/0.1.0/serial-rate-compiler-1";
// Covers bounded graph maps, deferred builders, hash workspace, and retained stage metadata.
const COMPILER_METADATA_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CompiledStreamStageKind {
    Island {
        kernel_hash: String,
        local_latency_frames: u64,
        emitted_tail_frames: u64,
        bounded_iir_truncation: bool,
        terminal_output_bounds: Vec<f64>,
    },
    Src {
        parameters: SrcParameters,
        coefficient_fingerprint: [u8; 32],
        parameter_fingerprint: [u8; 32],
        guard_time_denominator: u32,
        maximum_adapter_finish_output_frames: u64,
        /// Actual library delay is observed only after instantiation; it is not exact group delay.
        truncated_library_output_delay_frames: Option<u64>,
    },
}
#[derive(Debug, Clone, Serialize)]
pub struct CompiledStreamStage {
    pub original_nodes: Vec<NodeId>,
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    pub input_peak_bounds: Vec<f64>,
    pub output_peak_bounds: Vec<f64>,
    pub kind: CompiledStreamStageKind,
}
pub struct CompiledStreamPlan {
    plan: StreamPlan,
    hash: String,
    stages: Vec<CompiledStreamStage>,
    transformations: Vec<Transformation>,
    context: CompileContext,
    input_bounds: Vec<f64>,
    output_bounds: Vec<f64>,
}
impl CompiledStreamPlan {
    pub fn content_hash(&self) -> &str {
        &self.hash
    }
    pub fn implementation_version(&self) -> &str {
        VERSION
    }
    pub fn stages(&self) -> &[CompiledStreamStage] {
        &self.stages
    }
    pub fn transformations(&self) -> &[Transformation] {
        &self.transformations
    }
    pub fn context(&self) -> CompileContext {
        self.context
    }
    pub fn resources(&self) -> StreamResources {
        self.plan.resources()
    }
    pub fn input_format(&self) -> PcmFormat {
        self.context.input_format
    }
    pub fn output_format(&self) -> PcmFormat {
        self.context.output_format
    }
    pub fn input_peak_bounds(&self) -> &[f64] {
        &self.input_bounds
    }
    pub fn output_peak_bounds(&self) -> &[f64] {
        &self.output_bounds
    }
    pub fn into_stream_plan(self) -> StreamPlan {
        self.plan
    }
}
fn invalid(message: &'static str) -> GraphError {
    GraphError::new(ErrorCode::GraphInvalid, message)
}
fn unsupported(message: &'static str) -> GraphError {
    GraphError::new(ErrorCode::CapabilityUnsupported, message)
}
fn resource() -> GraphError {
    GraphError::new(
        ErrorCode::ResourceBudgetExceeded,
        "stream resource budget exceeded",
    )
}
fn pcm(port: PortFormat) -> Result<PcmFormat, GraphError> {
    PcmFormat::new(port.sample_rate_hz, port.channels, port.precision)
        .map_err(|_| GraphError::new(ErrorCode::FormatUnsupported, "invalid rate-domain format"))
}
fn src_parameters(node: &Node) -> Result<SrcParameters, GraphError> {
    let NodeKind::Src {
        input_rate_hz,
        preset,
        chunk_frames,
    } = node.kind
    else {
        return Err(invalid("rate boundary is not SRC").at_node(&node.id));
    };
    SrcDesign::preflight(
        input_rate_hz,
        node.format.sample_rate_hz,
        node.format.channels,
        chunk_frames,
        preset,
    )
    .map_err(|_| {
        GraphError::new(
            ErrorCode::FormatUnsupported,
            "SRC rate or block geometry is not qualified",
        )
        .at_node(&node.id)
    })
}

/// Compile explicit serial rate domains. Existing same-rate compilation never inserts SRC.
pub fn compile_stream(
    document: &GraphDocument,
    context: &CompileContext,
    assets: &[FirCoefficients],
    mut budget: StreamBudget,
) -> Result<CompiledStreamPlan, GraphError> {
    if context.input_format.representation() != PcmRepresentation::Float64
        || context.output_format.representation() != PcmRepresentation::Float64
    {
        return Err(GraphError::new(
            ErrorCode::FormatUnsupported,
            "stream compilation requires explicit f64 PCM",
        ));
    }
    if document
        .retained_capacity_bytes()
        .map_err(|e| GraphError::new(e.code, e.message))?
        > MAX_GRAPH_INTENT_BYTES
    {
        return Err(resource());
    }
    let topology = validate_graph(document, context, true)?;
    let cuts: Vec<_> = topology
        .order
        .iter()
        .filter(|id| matches!(topology.nodes[*id].kind, NodeKind::Src { .. }))
        .cloned()
        .collect();
    if !cuts.is_empty() && context.budget.max_latency_frames != u64::MAX {
        return Err(
            unsupported("aggregate SRC latency cannot satisfy a finite latency ceiling")
                .at_node(&cuts[0]),
        );
    }
    for cut in &cuts {
        if output_reachable_without(&topology, cut) {
            return Err(
                unsupported("every SRC must dominate the complete input-to-output path")
                    .at_node(cut),
            );
        }
    }
    let domains = label_domains(&topology, &cuts)?;
    for id in &topology.order {
        if matches!(topology.nodes[id].kind, NodeKind::Peq { .. }) && domains[id] != cuts.len() {
            return Err(unsupported("PEQ requires the final same-rate island").at_node(id));
        }
    }
    let mut used: BTreeSet<_> = topology.nodes.keys().cloned().collect();
    let mut synthetic_counter = 0;
    let mut pending = Vec::with_capacity(document.nodes.len());
    let partition = IslandPartition {
        document,
        context,
        assets,
        topology: &topology,
        cuts: &cuts,
        domains: &domains,
    };
    for domain in 0..=cuts.len() {
        let original_nodes: Vec<_> = topology
            .order
            .iter()
            .filter(|id| {
                domains[*id] == domain && !matches!(topology.nodes[*id].kind, NodeKind::Src { .. })
            })
            .cloned()
            .collect();
        if !original_nodes.is_empty() {
            let draft =
                partition.make_island(domain, original_nodes, &mut used, &mut synthetic_counter)?;
            pending.push(PendingStage::Island(draft));
        }
        if domain < cuts.len() {
            let node = topology.nodes[&cuts[domain]];
            pending.push(PendingStage::Src {
                node: node.id.clone(),
                parameters: src_parameters(node)?,
            });
        }
    }
    budget.max_resident_bytes = budget
        .max_resident_bytes
        .min(context.budget.max_buffer_bytes);
    let requirements: Vec<_> = pending
        .iter()
        .map(|s| match s {
            PendingStage::Island(d) => d.builder.requirement(),
            PendingStage::Src { parameters, .. } => StreamStageRequirement::Src(*parameters),
        })
        .collect();
    let preflight = StreamPlan::resource_preflight(&requirements, COMPILER_METADATA_BYTES, budget)
        .map_err(|e| stream_error(e, &pending))?;
    let mut stages = Vec::with_capacity(pending.len());
    let mut metadata = Vec::with_capacity(pending.len());
    let input_bounds = vec![1.0; context.input_format.channels().count()];
    let mut bounds = input_bounds.clone();
    let mut transformations = Vec::new();
    let mut semantic_stages = Vec::new();
    for pending_stage in pending {
        match pending_stage {
            PendingStage::Island(draft) => {
                let plan = draft
                    .builder
                    .finish(&bounds, true)
                    .map_err(|e| remap_error(e, &draft.aliases))?;
                let output = plan.output_peak_bounds().to_vec();
                transformations.extend(
                    plan.transformations()
                        .iter()
                        .map(|t| remap_transformation(t, &draft.aliases)),
                );
                semantic_stages.push(serde_json::json!({"kind":"island","kernel_hash":plan.content_hash(),"nodes":draft.original_nodes}));
                metadata.push(CompiledStreamStage {
                    original_nodes: draft.original_nodes,
                    input_format: plan.context().input_format,
                    output_format: plan.context().output_format,
                    input_peak_bounds: bounds,
                    output_peak_bounds: output.clone(),
                    kind: CompiledStreamStageKind::Island {
                        kernel_hash: plan.content_hash().into(),
                        local_latency_frames: plan.latency_frames(),
                        emitted_tail_frames: plan.tail_frames(),
                        bounded_iir_truncation: plan.bounded_iir_truncation(),
                        terminal_output_bounds: plan.terminal_output_bounds().to_vec(),
                    },
                });
                bounds = output;
                stages.push(StreamStagePlan::SameRate(Box::new(plan)));
            }
            PendingStage::Src {
                node,
                parameters: p,
            } => {
                let design = SrcDesign::analyze(
                    p.input_rate,
                    p.output_rate,
                    p.channels,
                    p.chunk_frames,
                    p.preset,
                    p.resources.preparation_peak_bytes,
                )
                .map_err(|_| invalid("SRC coefficients cannot be resolved").at_node(&node))?;
                let output = bounds
                    .iter()
                    .map(|b| {
                        design.peak_bound(*b).map_err(|_| {
                            invalid("SRC input exceeds its numerical bound").at_node(&node)
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let coefficient_fingerprint = design.coefficient_fingerprint();
                let parameter_fingerprint = design.parameter_fingerprint();
                semantic_stages.push(serde_json::json!({"kind":"src","node":node,"algorithm":crate::resample::ALGORITHM_VERSION,
                    "upstream":crate::resample::UPSTREAM_COMMIT,"coefficients":coefficient_fingerprint,"parameters":parameter_fingerprint,
                    "preset":p.preset,"input_rate":p.input_rate,"output_rate":p.output_rate,"channels":p.channels,"chunk":p.chunk_frames,
                    "length":p.filter_length,"phases":p.phases,"cutoff_bits":p.pre_scaling_cutoff.to_bits(),
                    "explicit_cutoff_bits":p.explicit_pre_scaling_cutoff.map(f32::to_bits),"guard":p.guard_time_numerator,
                    "zero_calls":p.drain_zero_calls,"max_relative_ratio_bits":p.library_max_relative_ratio.to_bits(),
                    "window":"blackman_harris2","interpolation":"cubic","tail_policy":"retain-all-returned-output"}));
                let maximum_adapter_finish_output_frames = (p.drain_zero_calls as u64 + 2)
                    .checked_mul(p.max_output_frames as u64)
                    .ok_or_else(resource)?;
                transformations.push(Transformation::Src { node: node.clone() });
                metadata.push(CompiledStreamStage {
                    original_nodes: vec![node],
                    input_format: PcmFormat::new(
                        p.input_rate,
                        p.channels,
                        PcmRepresentation::Float64,
                    )
                    .map_err(|_| invalid("SRC input format is invalid"))?,
                    output_format: PcmFormat::new(
                        p.output_rate,
                        p.channels,
                        PcmRepresentation::Float64,
                    )
                    .map_err(|_| invalid("SRC output format is invalid"))?,
                    input_peak_bounds: bounds,
                    output_peak_bounds: output.clone(),
                    kind: CompiledStreamStageKind::Src {
                        parameters: p,
                        coefficient_fingerprint,
                        parameter_fingerprint,
                        guard_time_denominator: p.input_rate,
                        maximum_adapter_finish_output_frames,
                        truncated_library_output_delay_frames: None,
                    },
                });
                bounds = output;
                stages.push(StreamStagePlan::Src(design));
            }
        }
    }
    let origins: Vec<_> = metadata
        .iter()
        .map(|s| s.original_nodes.first().cloned())
        .collect();
    let plan =
        StreamPlan::new_with_metadata(stages, &input_bounds, budget, COMPILER_METADATA_BYTES)
            .map_err(|e| {
                let mut diagnostic = stream_diagnostic(e);
                if let Some(stage) = e.stage {
                    diagnostic.node = origins.get(stage).cloned().flatten();
                }
                diagnostic
            })?;
    if plan.resources() != preflight {
        return Err(resource());
    }
    let mut canonical = document.clone();
    normalize_port_quantums(&mut canonical.nodes, context.quantum_frames);
    canonical.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    canonical.edges.sort_by(|a, b| {
        (&a.from, &a.to, a.from_port, a.to_port).cmp(&(&b.from, &b.to, b.from_port, b.to_port))
    });
    let bytes = serde_json::to_vec(&(
        VERSION,
        "serial-complete-cuts-final-iir-1",
        canonical,
        context.input_format,
        context.output_format,
        context.quantum_frames,
        context.policy,
        &input_bounds,
        &bounds,
        semantic_stages,
    ))
    .map_err(|_| invalid("stream plan cannot be hashed"))?;
    let hash = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(CompiledStreamPlan {
        plan,
        hash,
        stages: metadata,
        transformations,
        context: *context,
        input_bounds,
        output_bounds: bounds,
    })
}

fn output_reachable_without(topology: &ValidatedGraph<'_>, removed: &NodeId) -> bool {
    let mut stack = vec![&topology.input.id];
    let mut seen = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if id == removed || !seen.insert(id) {
            continue;
        }
        if id == &topology.output.id {
            return true;
        }
        for edge in &topology.outgoing[id] {
            stack.push(&edge.to);
        }
    }
    false
}
fn label_domains(
    topology: &ValidatedGraph<'_>,
    cuts: &[NodeId],
) -> Result<BTreeMap<NodeId, usize>, GraphError> {
    let ranks: BTreeMap<_, _> = cuts.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let mut domains = BTreeMap::new();
    for id in &topology.order {
        let node = topology.nodes[id];
        let before = topology.incoming[id]
            .first()
            .map(|e| domains[&e.from])
            .unwrap_or(0);
        if topology.incoming[id]
            .iter()
            .any(|e| domains[&e.from] != before)
        {
            return Err(unsupported("multirate reconvergence is unsupported").at_node(id));
        }
        let after = if matches!(node.kind, NodeKind::Src { .. }) {
            if ranks[id] != before {
                return Err(unsupported("SRC cuts are not serial").at_node(id));
            }
            before + 1
        } else {
            before
        };
        domains.insert(id.clone(), after);
    }
    Ok(domains)
}
struct IslandDraft {
    builder: IslandBuilder,
    original_nodes: Vec<NodeId>,
    aliases: BTreeMap<NodeId, NodeId>,
}
enum PendingStage {
    Island(IslandDraft),
    Src {
        node: NodeId,
        parameters: SrcParameters,
    },
}
fn synthetic(used: &mut BTreeSet<NodeId>, counter: &mut usize) -> Result<NodeId, GraphError> {
    for _ in 0..MAX_GRAPH_NODES * 3 + 1 {
        let id = NodeId::new(format!("__earph_rate_{counter}")).map_err(|_| resource())?;
        *counter += 1;
        if used.insert(id.clone()) {
            return Ok(id);
        }
    }
    Err(resource())
}
fn synthetic_port(format: PcmFormat, quantum: usize) -> PortFormat {
    PortFormat {
        domain: SampleDomain::Pcm,
        sample_rate_hz: format.sample_rate_hz(),
        channels: format.channels(),
        precision: format.representation(),
        max_quantum_frames: quantum,
    }
}
fn normalize_port_quantums(nodes: &mut [Node], quantum: usize) {
    // Original ceilings were admitted before partitioning. Islands execute only this quantum.
    // Keep those admission ceilings out of both island and stream semantic identities.
    for node in nodes {
        node.format.max_quantum_frames = quantum;
    }
}
struct IslandPartition<'a> {
    document: &'a GraphDocument,
    context: &'a CompileContext,
    assets: &'a [FirCoefficients],
    topology: &'a ValidatedGraph<'a>,
    cuts: &'a [NodeId],
    domains: &'a BTreeMap<NodeId, usize>,
}
impl IslandPartition<'_> {
    fn make_island(
        &self,
        domain: usize,
        original_nodes: Vec<NodeId>,
        used: &mut BTreeSet<NodeId>,
        counter: &mut usize,
    ) -> Result<IslandDraft, GraphError> {
        let document = self.document;
        let context = self.context;
        let assets = self.assets;
        let topology = self.topology;
        let cuts = self.cuts;
        let domains = self.domains;
        let input_format = if domain == 0 {
            context.input_format
        } else {
            pcm(topology.nodes[&cuts[domain - 1]].format)?
        };
        let output_format = if domain == cuts.len() {
            context.output_format
        } else {
            let p = src_parameters(topology.nodes[&cuts[domain]])?;
            PcmFormat::new(p.input_rate, p.channels, PcmRepresentation::Float64)
                .map_err(|_| invalid("island output format is invalid"))?
        };
        let mut nodes: Vec<_> = original_nodes
            .iter()
            .map(|id| topology.nodes[id].clone())
            .collect();
        normalize_port_quantums(&mut nodes, context.quantum_frames);
        let mut aliases = BTreeMap::new();
        let input = if domain == 0 {
            topology.input.id.clone()
        } else {
            let id = synthetic(used, counter)?;
            aliases.insert(id.clone(), cuts[domain - 1].clone());
            nodes.push(Node {
                id: id.clone(),
                kind: NodeKind::Input,
                format: synthetic_port(input_format, context.quantum_frames),
            });
            id
        };
        let output = if domain == cuts.len() {
            topology.output.id.clone()
        } else {
            let id = synthetic(used, counter)?;
            aliases.insert(id.clone(), cuts[domain].clone());
            nodes.push(Node {
                id: id.clone(),
                kind: NodeKind::Output,
                format: synthetic_port(output_format, context.quantum_frames),
            });
            id
        };
        let mut edges = Vec::new();
        for edge in &document.edges {
            let from = topology.nodes[&edge.from];
            let to = topology.nodes[&edge.to];
            let from_inside =
                domains[&edge.from] == domain && !matches!(from.kind, NodeKind::Src { .. });
            let to_inside = domains[&edge.to] == domain && !matches!(to.kind, NodeKind::Src { .. });
            if from_inside && to_inside {
                edges.push(edge.clone());
            } else if to_inside && domain > 0 && edge.from == cuts[domain - 1] {
                let mut e = edge.clone();
                e.from = input.clone();
                edges.push(e);
            } else if from_inside && domain < cuts.len() && edge.to == cuts[domain] {
                let mut e = edge.clone();
                e.to = output.clone();
                edges.push(e);
            }
        }
        let island = GraphDocument {
            schema_version: 1,
            nodes,
            edges,
            output,
        };
        let local = CompileContext {
            input_format,
            output_format,
            ..*context
        };
        let builder =
            resolve_island(&island, &local, assets).map_err(|e| remap_error(e, &aliases))?;
        Ok(IslandDraft {
            builder,
            original_nodes,
            aliases,
        })
    }
}
fn alias(id: &NodeId, aliases: &BTreeMap<NodeId, NodeId>) -> NodeId {
    aliases.get(id).unwrap_or(id).clone()
}
fn remap_edge(edge: &Edge, aliases: &BTreeMap<NodeId, NodeId>) -> Edge {
    Edge {
        from: alias(&edge.from, aliases),
        to: alias(&edge.to, aliases),
        from_port: edge.from_port,
        to_port: edge.to_port,
    }
}
fn remap_error(mut error: GraphError, aliases: &BTreeMap<NodeId, NodeId>) -> GraphError {
    error.node = error.node.as_ref().map(|id| alias(id, aliases));
    error.edge = error.edge.as_ref().map(|e| remap_edge(e, aliases));
    error
}
fn remap_transformation(
    transformation: &Transformation,
    aliases: &BTreeMap<NodeId, NodeId>,
) -> Transformation {
    let mut result = transformation.clone();
    match &mut result {
        Transformation::Gain { node }
        | Transformation::Peq { node }
        | Transformation::Convolution { node }
        | Transformation::Src { node }
        | Transformation::BoundedIirTruncation { node, .. }
        | Transformation::Delay { node }
        | Transformation::Matrix { node }
        | Transformation::Sum { node } => *node = alias(node, aliases),
        Transformation::Compensation { edge, .. } => *edge = remap_edge(edge, aliases),
    }
    result
}
fn stream_diagnostic(error: StreamError) -> GraphError {
    match error.kind {
        StreamErrorKind::Budget | StreamErrorKind::Counter => resource(),
        StreamErrorKind::UnsupportedTail => unsupported("IIR must occupy the final stream island"),
        StreamErrorKind::Adjacency => GraphError::new(
            ErrorCode::FormatUnsupported,
            "stream stage formats do not match",
        ),
        _ => invalid("stream stage certificate is invalid"),
    }
}
fn stream_error(error: StreamError, stages: &[PendingStage]) -> GraphError {
    let mut diagnostic = stream_diagnostic(error);
    if let Some(index) = error.stage {
        diagnostic.node = stages.get(index).and_then(|s| match s {
            PendingStage::Island(d) => d.original_nodes.first().cloned(),
            PendingStage::Src { node, .. } => Some(node.clone()),
        });
    }
    diagnostic
}
