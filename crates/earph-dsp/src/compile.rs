// ABOUTME: Validates graph intent before resolving bounded execution plans.
// ABOUTME: Derives deterministic schedules and explicit merge compensation.
use crate::model::ResolvedKernel;
use crate::*;
use crate::{convolution::ConvolutionDesign, peq::PeqDesign};
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const IMPLEMENTATION_VERSION: &str = "earph-dsp/0.1.0/compiler-3-peq-fir";

fn invalid(message: &'static str) -> GraphError {
    GraphError::new(ErrorCode::GraphInvalid, message)
}
fn budget() -> GraphError {
    GraphError::new(
        ErrorCode::ResourceBudgetExceeded,
        "graph resource budget exceeded",
    )
}
fn format_error() -> GraphError {
    GraphError::new(
        ErrorCode::FormatUnsupported,
        "explicit matching PCM formats required",
    )
}

/// Resolve graph intent without allocating audio buffers or changing session state.
pub fn compile(
    document: &GraphDocument,
    context: &CompileContext,
) -> Result<CompiledPlan, GraphError> {
    compile_with_assets(document, context, &[])
}

/// Compile against immutable coefficients resolved before this preparation boundary.
pub fn compile_with_assets(
    document: &GraphDocument,
    context: &CompileContext,
    assets: &[FirCoefficients],
) -> Result<CompiledPlan, GraphError> {
    compile_inner(
        document,
        context,
        assets,
        &vec![1.0; context.input_format.channels().count()],
        false,
    )
}

/// Admit explicit channel bounds for a composed stream island and enforce them during processing.
pub fn compile_with_input_bounds(
    document: &GraphDocument,
    context: &CompileContext,
    assets: &[FirCoefficients],
    input_bounds: &[f64],
) -> Result<CompiledPlan, GraphError> {
    compile_inner(document, context, assets, input_bounds, true)
}
fn compile_inner(
    document: &GraphDocument,
    context: &CompileContext,
    assets: &[FirCoefficients],
    input_bounds: &[f64],
    enforce_input_bounds: bool,
) -> Result<CompiledPlan, GraphError> {
    if input_bounds.len() != context.input_format.channels().count()
        || input_bounds.iter().any(|v| !v.is_finite() || *v < 0.0)
    {
        return Err(invalid(
            "input peak bounds must match finite channel geometry",
        ));
    }
    if document.schema_version != 1 {
        return Err(GraphError::new(
            ErrorCode::CapabilityUnsupported,
            "unsupported graph schema",
        ));
    }
    if document.nodes.len() > context.budget.max_nodes.min(MAX_GRAPH_NODES)
        || document.edges.len() > context.budget.max_edges.min(MAX_GRAPH_EDGES)
        || context.quantum_frames > context.budget.max_quantum_frames.min(8192)
    {
        return Err(budget());
    }
    if context.quantum_frames == 0 {
        return Err(invalid("processing quantum must be positive"));
    }
    let mut nodes = BTreeMap::new();
    for node in &document.nodes {
        if nodes.insert(node.id.clone(), node).is_some() {
            return Err(invalid("duplicate node identifier").at_node(&node.id));
        }
        validate_node(node, context)?;
    }
    let output = nodes
        .get(&document.output)
        .ok_or_else(|| invalid("selected output is missing").at_node(&document.output))?;
    if !matches!(output.kind, NodeKind::Output) {
        return Err(invalid("selected node is not an output").at_node(&output.id));
    }
    let sources: Vec<_> = nodes
        .values()
        .filter(|n| matches!(n.kind, NodeKind::Input))
        .copied()
        .collect();
    if sources.len() != 1
        || nodes
            .values()
            .filter(|n| matches!(n.kind, NodeKind::Output))
            .count()
            != 1
    {
        return Err(invalid("graph requires one input and one output"));
    }
    let input = sources[0];
    if !matches_external(input.format, context.input_format) {
        return Err(format_error().at_node(&input.id));
    }
    if !matches_external(output.format, context.output_format) {
        return Err(format_error().at_node(&output.id));
    }

    let mut incoming: BTreeMap<NodeId, Vec<&Edge>> =
        nodes.keys().map(|id| (id.clone(), Vec::new())).collect();
    let mut outgoing = incoming.clone();
    for edge in &document.edges {
        let from = nodes
            .get(&edge.from)
            .ok_or_else(|| invalid("edge source is missing").at_edge(edge))?;
        let to = nodes
            .get(&edge.to)
            .ok_or_else(|| invalid("edge destination is missing").at_edge(edge))?;
        if edge.from_port != 0
            || (!matches!(to.kind, NodeKind::Sum) && edge.to_port != 0)
            || matches!(to.kind, NodeKind::Input)
            || matches!(from.kind, NodeKind::Output)
        {
            return Err(invalid("edge references an unsupported port").at_edge(edge));
        }
        let target_channels = match to.kind {
            NodeKind::Matrix { input_channels, .. } => input_channels,
            _ => to.format.channels,
        };
        if from.format.domain != to.format.domain
            || from.format.sample_rate_hz != to.format.sample_rate_hz
            || from.format.precision != to.format.precision
            || from.format.channels != target_channels
        {
            return Err(format_error().at_edge(edge));
        }
        incoming
            .get_mut(&edge.to)
            .expect("validated target")
            .push(edge);
        outgoing
            .get_mut(&edge.from)
            .expect("validated source")
            .push(edge);
    }
    for (id, edges) in &mut incoming {
        edges.sort_by_key(|edge| edge.to_port);
        let node = nodes[id];
        if matches!(node.kind, NodeKind::Input) {
            continue;
        }
        if edges.is_empty() {
            return Err(invalid("required input is disconnected").at_node(id));
        }
        if !matches!(node.kind, NodeKind::Sum) && edges.len() != 1 {
            return Err(invalid("node requires exactly one input").at_node(id));
        }
        for (port, edge) in edges.iter().enumerate() {
            if edge.to_port != port {
                return Err(invalid("input ports must be unique and contiguous").at_edge(edge));
            }
        }
    }
    let order = topological_order(&nodes, &incoming, &outgoing)?;
    // Every node must contribute to the selected output. No hidden processing is admitted.
    let mut connected = BTreeSet::from([document.output.clone()]);
    for id in order.iter().rev() {
        if connected.contains(id) {
            connected.extend(incoming[id].iter().map(|edge| edge.from.clone()));
        }
    }
    if let Some(id) = nodes.keys().find(|id| !connected.contains(*id)) {
        return Err(invalid("node does not reach selected output").at_node(id));
    }

    let mut transformations = Vec::new();
    for id in &order {
        let transformation = match nodes[id].kind {
            NodeKind::Gain { .. } => Some(Transformation::Gain { node: id.clone() }),
            NodeKind::Peq { .. } => Some(Transformation::Peq { node: id.clone() }),
            NodeKind::Convolution { .. } => Some(Transformation::Convolution { node: id.clone() }),
            NodeKind::Delay { delay_frames } if delay_frames > 0 => {
                Some(Transformation::Delay { node: id.clone() })
            }
            NodeKind::Matrix { .. } => Some(Transformation::Matrix { node: id.clone() }),
            NodeKind::Sum => Some(Transformation::Sum { node: id.clone() }),
            NodeKind::Input
            | NodeKind::Output
            | NodeKind::Bypass
            | NodeKind::Meter
            | NodeKind::Delay { .. } => None,
        };
        if let Some(transformation) = transformation {
            if context.policy == ProcessingPolicy::StrictPreservation {
                return Err(GraphError::new(
                    ErrorCode::FidelityPolicyConflict,
                    "graph changes samples under strict preservation",
                )
                .at_node(id));
            }
            transformations.push(transformation);
        }
    }
    let integer = context.input_format.representation() != PcmRepresentation::Float64;
    if integer && !transformations.is_empty() {
        return Err(format_error());
    }
    let preservation = if integer {
        PreservationDecision::RetainInteger
    } else if transformations.is_empty() {
        PreservationDecision::UnchangedFloat64
    } else {
        PreservationDecision::ProcessFloat64
    };

    let mut schedule: Vec<CompiledNode> = Vec::with_capacity(order.len());
    let mut indexes: BTreeMap<NodeId, usize> = BTreeMap::new();
    let mut resources = ResourceUsage {
        buffer_bytes: 0,
        node_buffer_bytes: 0,
        delay_state_bytes: 0,
    };
    let bounded_iir = document
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Peq { .. }));
    let mut kernel_state_bytes = 0_u64;
    let mut asset_bytes = 0_u64;
    let mut preparation_scratch_bytes = 0_u64;
    let mut charged_assets = BTreeSet::new();
    for id in order {
        let node = nodes[&id];
        let mut inputs = Vec::with_capacity(incoming[&id].len());
        let arrival = incoming[&id]
            .iter()
            .map(|e| schedule[indexes[&e.from]].latency_frames)
            .max()
            .unwrap_or(0);
        let mut tail = 0u64;
        for edge in &incoming[&id] {
            let source_index = indexes[&edge.from];
            let source = &schedule[source_index];
            let compensation_frames = arrival - source.latency_frames;
            if compensation_frames > 0 {
                transformations.push(Transformation::Compensation {
                    edge: (*edge).clone(),
                    delay_frames: compensation_frames,
                });
                let bytes = audio_bytes(compensation_frames, source.format.channels)
                    .map_err(|e| e.at_edge(edge))?;
                resources.delay_state_bytes = resources
                    .delay_state_bytes
                    .checked_add(bytes)
                    .ok_or_else(|| budget().at_edge(edge))?;
            }
            tail = tail.max(
                source
                    .tail_frames
                    .checked_add(compensation_frames)
                    .ok_or_else(|| budget().at_edge(edge))?,
            );
            inputs.push(CompiledInput {
                source_index,
                target_port: edge.to_port,
                compensation_frames,
            });
        }
        let kernel = match &node.kind {
            NodeKind::Peq { filter } => {
                let design =
                    PeqDesign::prepare(filter, node.format.sample_rate_hz, node.format.channels)
                        .map_err(|_| {
                            invalid("equalizer design is not numerically qualified").at_node(&id)
                        })?;
                // Carry design identity without inventing a nonzero input history.
                // Forward bounds select the real composed horizon before the plan is published.
                let tail = design.tail(0.0, 1.0, 1.0).map_err(|_| {
                    invalid("equalizer tail exceeds the processing bound").at_node(&id)
                })?;
                kernel_state_bytes = kernel_state_bytes
                    .checked_add(design.resources().resident_bytes as u64)
                    .ok_or_else(budget)?;
                preparation_scratch_bytes = preparation_scratch_bytes
                    .max(design.resources().preparation_scratch_bytes as u64);
                ResolvedKernel::Peq { design, tail }
            }
            NodeKind::Convolution { asset: reference } => {
                reference.validate().map_err(|_| {
                    GraphError::new(ErrorCode::AssetInvalid, "invalid correction reference")
                        .at_node(&id)
                })?;
                if reference.sample_rate_hz != node.format.sample_rate_hz
                    || reference.channels != node.format.channels
                {
                    return Err(format_error().at_node(&id));
                }
                let asset = assets
                    .iter()
                    .find(|a| a.reference() == reference)
                    .ok_or_else(|| {
                        GraphError::new(ErrorCode::AssetInvalid, "correction asset is unresolved")
                            .at_node(&id)
                    })?
                    .clone();
                if assets.iter().any(|a| {
                    a.reference().sha256 == reference.sha256
                        && (a.reference() != reference
                            || a.coefficient_sha256() != asset.coefficient_sha256())
                }) {
                    return Err(GraphError::new(
                        ErrorCode::AssetInvalid,
                        "correction identity has conflicting content",
                    )
                    .at_node(&id));
                }
                let design =
                    ConvolutionDesign::analyze(asset.coefficients(), context.quantum_frames)
                        .map_err(|_| {
                            invalid("correction design exceeds numeric or storage limits")
                                .at_node(&id)
                        })?;
                let r = design.resources();
                kernel_state_bytes = kernel_state_bytes
                    .checked_add(r.resident_bytes)
                    .ok_or_else(budget)?;
                preparation_scratch_bytes =
                    preparation_scratch_bytes.max(r.preparation_peak_bytes - r.resident_bytes);
                if charged_assets.insert(reference.sha256.clone()) {
                    asset_bytes = asset_bytes
                        .checked_add(asset.resident_bytes())
                        .ok_or_else(budget)?;
                }
                ResolvedKernel::Convolution { design, asset }
            }
            NodeKind::Input
            | NodeKind::Output
            | NodeKind::Bypass
            | NodeKind::Gain { .. }
            | NodeKind::Delay { .. }
            | NodeKind::Matrix { .. }
            | NodeKind::Sum
            | NodeKind::Meter => ResolvedKernel::Structural,
        };
        let own_delay = match node.kind {
            NodeKind::Delay { delay_frames } => delay_frames,
            _ => 0,
        };
        let filter_latency = match &kernel {
            ResolvedKernel::Convolution { design, .. } => design.latency_frames() as u64,
            _ => 0,
        };
        let latency_frames = arrival
            .checked_add(own_delay)
            .and_then(|v| v.checked_add(filter_latency))
            .ok_or_else(|| budget().at_node(&id))?;
        let tail_frames = tail
            .checked_add(own_delay)
            .and_then(|v| {
                v.checked_add(match &kernel {
                    ResolvedKernel::Convolution { design, .. } => design.tail_frames() as u64,
                    _ => 0,
                })
            })
            .ok_or_else(|| budget().at_node(&id))?;
        if latency_frames > context.budget.max_latency_frames {
            return Err(budget().at_node(&id));
        }
        if !integer {
            let output_bytes = audio_bytes(context.quantum_frames as u64, node.format.channels)
                .map_err(|e| e.at_node(&id))?;
            let delay_bytes =
                audio_bytes(own_delay, node.format.channels).map_err(|e| e.at_node(&id))?;
            resources.node_buffer_bytes = resources
                .node_buffer_bytes
                .checked_add(
                    output_bytes
                        .checked_mul(if matches!(kernel, ResolvedKernel::Structural) {
                            1
                        } else {
                            2
                        })
                        .ok_or_else(budget)?,
                )
                .ok_or_else(|| budget().at_node(&id))?;
            resources.delay_state_bytes = resources
                .delay_state_bytes
                .checked_add(delay_bytes)
                .ok_or_else(|| budget().at_node(&id))?;
        }
        resources.buffer_bytes = resources
            .node_buffer_bytes
            .checked_add(resources.delay_state_bytes)
            .and_then(|v| v.checked_add(kernel_state_bytes))
            .and_then(|v| v.checked_add(asset_bytes))
            .ok_or_else(|| budget().at_node(&id))?;
        if resources.buffer_bytes > context.budget.max_buffer_bytes {
            return Err(budget().at_node(&id));
        }
        indexes.insert(id.clone(), schedule.len());
        schedule.push(CompiledNode {
            id,
            kind: node.kind.clone(),
            format: node.format,
            inputs,
            latency_frames,
            tail_frames,
            input_peak_bounds: Vec::new(),
            output_peak_bounds: Vec::new(),
            terminal_output_bounds: Vec::new(),
            kernel,
        });
    }
    resolve_bounds(&mut schedule, bounded_iir, input_bounds)?;
    for node in &schedule {
        if matches!(node.kind, NodeKind::Peq { .. }) {
            transformations.push(Transformation::BoundedIirTruncation {
                node: node.id.clone(),
                frames: node.tail_frames,
            });
        }
    }
    let input_index = indexes[&input.id];
    let output_index = indexes[&output.id];
    let latency_frames = schedule[output_index].latency_frames;
    let tail_frames = schedule[output_index].tail_frames;
    let mut canonical = document.clone();
    canonical.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    canonical.edges.sort_by(|a, b| {
        (&a.from, &a.to, a.from_port, a.to_port).cmp(&(&b.from, &b.to, b.from_port, b.to_port))
    });
    let bytes = serde_json::to_vec(&(
        IMPLEMENTATION_VERSION,
        canonical,
        context.input_format,
        context.output_format,
        context.quantum_frames,
        context.policy,
        input_bounds,
        enforce_input_bounds||kernel_state_bytes>0,
        schedule.iter().map(|n|match &n.kernel{
            ResolvedKernel::Peq{design,tail}=>serde_json::json!({"algorithm":"peq-tdf2-modal-1","coefficients":design.coefficients().map(f64::to_bits),"tail":tail.frames(),"discarded":n.terminal_output_bounds}),
            ResolvedKernel::Convolution{asset,..}=>serde_json::json!({"algorithm":"partitioned-radix2-512-1","coefficients":asset.coefficient_sha256(),"tail":n.tail_frames,"bounded_iir":bounded_iir}),
            ResolvedKernel::Structural=>serde_json::Value::Null,
        }).collect::<Vec<_>>(),
    ))
    .map_err(|_| invalid("graph cannot be hashed"))?;
    let content_hash = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(CompiledPlan {
        content_hash,
        implementation_version: IMPLEMENTATION_VERSION,
        schedule,
        input_index,
        output_index,
        context: *context,
        latency_frames,
        tail_frames,
        resources,
        transformations,
        preservation,
        kernel_state_bytes,
        asset_bytes,
        preparation_scratch_bytes,
        bounded_iir,
        enforce_input_bounds: enforce_input_bounds || kernel_state_bytes > 0,
    })
}

fn matches_external(port: PortFormat, external: PcmFormat) -> bool {
    port.sample_rate_hz == external.sample_rate_hz()
        && port.channels == external.channels()
        && port.precision == external.representation()
}

fn upper_product(a: f64, b: f64) -> Result<f64, GraphError> {
    let value = if a == 0.0 || b == 0.0 {
        0.0
    } else if a == 1.0 {
        b
    } else if b == 1.0 {
        a
    } else {
        (a * b).next_up()
    };
    finite_bound(value)
}
fn upper_sum(a: f64, b: f64) -> Result<f64, GraphError> {
    finite_bound(if a == 0.0 {
        b
    } else if b == 0.0 {
        a
    } else {
        (a + b).next_up()
    })
}
fn finite_bound(v: f64) -> Result<f64, GraphError> {
    if v.is_finite() && v >= 0.0 {
        Ok(v)
    } else {
        Err(invalid("graph absolute peak bound overflowed"))
    }
}

fn structural_bounds(
    node: &CompiledNode,
    previous: &[Vec<f64>],
    input_values: &[f64],
) -> Result<(Vec<f64>, Vec<f64>), GraphError> {
    let channels = node.format.channels.count();
    let input = if matches!(node.kind, NodeKind::Input) {
        input_values.to_vec()
    } else if matches!(node.kind, NodeKind::Sum) {
        let mut v = vec![0.0; channels];
        for edge in &node.inputs {
            for (a, b) in v.iter_mut().zip(&previous[edge.source_index]) {
                *a = upper_sum(*a, *b)?;
            }
        }
        v
    } else {
        previous[node.inputs[0].source_index].clone()
    };
    let output = match &node.kind {
        NodeKind::Gain { gain_linear } => input
            .iter()
            .map(|v| upper_product(*v, gain_linear.get().abs()))
            .collect::<Result<_, _>>()?,
        NodeKind::Matrix {
            input_channels,
            coefficients,
        } => {
            let mut output = vec![0.0; channels];
            for (row, target) in output.iter_mut().enumerate() {
                for (ch, value) in input.iter().enumerate() {
                    *target = upper_sum(
                        *target,
                        upper_product(
                            *value,
                            coefficients[row * input_channels.count() + ch].get().abs(),
                        )?,
                    )?;
                }
            }
            output
        }
        NodeKind::Input
        | NodeKind::Output
        | NodeKind::Bypass
        | NodeKind::Delay { .. }
        | NodeKind::Sum
        | NodeKind::Meter
        | NodeKind::Peq { .. }
        | NodeKind::Convolution { .. } => input.clone(),
    };
    Ok((input, output))
}

fn resolve_bounds(
    schedule: &mut [CompiledNode],
    bounded_iir: bool,
    input_bounds: &[f64],
) -> Result<(), GraphError> {
    let mut peaks = Vec::with_capacity(schedule.len());
    for node in schedule.iter_mut() {
        let (input, mut output) =
            structural_bounds(node, &peaks, input_bounds).map_err(|e| e.at_node(&node.id))?;
        match &node.kernel {
            ResolvedKernel::Peq { design, .. } => {
                for (out, value) in output.iter_mut().zip(&input) {
                    *out = design.peak_bound(*value).map_err(|_| {
                        invalid("equalizer peak cannot be bounded").at_node(&node.id)
                    })?;
                }
            }
            ResolvedKernel::Convolution { design, .. } => {
                for (ch, (out, value)) in output.iter_mut().zip(&input).enumerate() {
                    *out = design.peak_bound(ch, *value).map_err(|_| {
                        invalid("convolution peak cannot be bounded").at_node(&node.id)
                    })?;
                }
            }
            ResolvedKernel::Structural => {}
        }
        node.input_peak_bounds = input;
        node.output_peak_bounds = output.clone();
        peaks.push(output);
    }
    // Weights only guide conservative horizon selection. The final actual envelope is checked below.
    let mut weights = vec![0.0; schedule.len()];
    *weights.last_mut().expect("nonempty schedule") = 1.0;
    for (index, node) in schedule.iter().enumerate().rev().filter(|_| bounded_iir) {
        let factor = match &node.kernel {
            ResolvedKernel::Peq { design, .. } => design
                .peak_bound(1.0)
                .map_err(|_| invalid("equalizer allocation weight overflowed"))?
                .max(1.0),
            ResolvedKernel::Convolution { design, .. } => {
                let mut f = 1.0_f64;
                for ch in 0..node.format.channels.count() {
                    f = f.max(
                        design
                            .peak_bound(ch, 1.0)
                            .map_err(|_| invalid("convolution allocation weight overflowed"))?,
                    );
                }
                f
            }
            ResolvedKernel::Structural => match &node.kind {
                NodeKind::Gain { gain_linear } => gain_linear.get().abs(),
                NodeKind::Matrix {
                    input_channels,
                    coefficients,
                } => {
                    let mut f = 0.0_f64;
                    for row in coefficients.chunks(input_channels.count()) {
                        let mut s = 0.0;
                        for c in row {
                            s = upper_sum(s, c.get().abs())?;
                        }
                        f = f.max(s);
                    }
                    f
                }
                _ => 1.0,
            },
        };
        for input in &node.inputs {
            weights[input.source_index] = upper_sum(
                weights[input.source_index],
                upper_product(weights[index], factor)?,
            )?;
        }
    }
    let sections = schedule
        .iter()
        .filter(|n| matches!(n.kernel, ResolvedKernel::Peq { .. }))
        .count()
        .max(1) as f64;
    let mut allocation = 1e-12 / (sections * 4.0);
    for _ in 0..32 {
        let mut envelopes = Vec::with_capacity(schedule.len());
        let mut horizons = Vec::<u64>::with_capacity(schedule.len());
        for (index, node) in schedule.iter_mut().enumerate() {
            let (input, mut output) =
                structural_bounds(node, &envelopes, &vec![0.0; input_bounds.len()])
                    .map_err(|e| e.at_node(&node.id))?;
            let input_horizon = node.inputs.iter().try_fold(0_u64, |h, e| {
                Ok::<_, GraphError>(
                    h.max(
                        horizons[e.source_index]
                            .checked_add(e.compensation_frames)
                            .ok_or_else(budget)?,
                    ),
                )
            })?;
            let mut horizon = input_horizon;
            match &mut node.kernel {
                ResolvedKernel::Peq { design, tail } => {
                    let peak = node
                        .input_peak_bounds
                        .iter()
                        .copied()
                        .fold(0.0_f64, f64::max);
                    let local = allocation / weights[index].max(1.0);
                    *tail = design.tail(peak, 1.0, local).map_err(|_| {
                        GraphError::new(
                            ErrorCode::ResourceBudgetExceeded,
                            "equalizer tail exceeds the declared bound",
                        )
                        .at_node(&node.id)
                    })?;
                    horizon = horizon
                        .checked_add(tail.frames() as u64)
                        .ok_or_else(budget)?;
                    for (out, value) in output.iter_mut().zip(&input) {
                        *out = design
                            .settled_output_bound(peak, *value, tail.frames())
                            .map_err(|_| {
                                invalid("equalizer future output cannot be bounded")
                                    .at_node(&node.id)
                            })?;
                    }
                }
                ResolvedKernel::Convolution { design, .. } => {
                    horizon = horizon
                        .checked_add(if bounded_iir {
                            design.history_clear_frames()
                        } else {
                            design.tail_frames()
                        } as u64)
                        .ok_or_else(budget)?;
                    for (ch, (out, value)) in output.iter_mut().zip(&input).enumerate() {
                        *out = if bounded_iir {
                            design.peak_bound(ch, *value).map_err(|_| {
                                invalid("convolution future output cannot be bounded")
                                    .at_node(&node.id)
                            })?
                        } else {
                            0.0
                        };
                    }
                }
                ResolvedKernel::Structural => {
                    if let NodeKind::Delay { delay_frames } = node.kind {
                        horizon = horizon.checked_add(delay_frames).ok_or_else(budget)?;
                    }
                }
            }
            if bounded_iir && horizon > 1048576 {
                return Err(GraphError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "composed filter tail exceeds the declared bound",
                )
                .at_node(&node.id));
            }
            node.tail_frames = horizon;
            node.terminal_output_bounds = output.clone();
            envelopes.push(output);
            horizons.push(horizon);
        }
        if envelopes
            .last()
            .expect("output exists")
            .iter()
            .all(|v| *v <= 1e-12)
        {
            return Ok(());
        }
        allocation *= 0.5;
    }
    Err(invalid(
        "composed future output cannot meet the terminal error budget",
    ))
}

fn validate_node(node: &Node, context: &CompileContext) -> Result<(), GraphError> {
    if node.format.domain != SampleDomain::Pcm {
        return Err(GraphError::new(
            ErrorCode::CapabilityUnsupported,
            "DSD graph processing is unsupported",
        )
        .at_node(&node.id));
    }
    if node.format.sample_rate_hz == 0 {
        return Err(format_error().at_node(&node.id));
    }
    if node.format.max_quantum_frames < context.quantum_frames
        || node.format.max_quantum_frames > 8192
    {
        return Err(budget().at_node(&node.id));
    }
    match &node.kind {
        NodeKind::Matrix {
            input_channels,
            coefficients,
        } if coefficients.len() != input_channels.count() * node.format.channels.count() => {
            return Err(invalid("matrix requires output by input coefficients").at_node(&node.id));
        }
        _ => {}
    }
    Ok(())
}

fn audio_bytes(frames: u64, channels: ChannelLayout) -> Result<u64, GraphError> {
    let bytes = frames
        .checked_mul(channels.count() as u64)
        .and_then(|n| n.checked_mul(8))
        .ok_or_else(budget)?;
    // A Vec cannot hold more than isize::MAX bytes, even with a larger admitted budget.
    if bytes > isize::MAX as u64 {
        return Err(budget());
    }
    Ok(bytes)
}

fn topological_order(
    nodes: &BTreeMap<NodeId, &Node>,
    incoming: &BTreeMap<NodeId, Vec<&Edge>>,
    outgoing: &BTreeMap<NodeId, Vec<&Edge>>,
) -> Result<Vec<NodeId>, GraphError> {
    let mut pending: BTreeMap<_, _> = incoming
        .iter()
        .map(|(id, edges)| (id.clone(), edges.len()))
        .collect();
    let mut ready: BTreeSet<_> = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| id.clone())
        .collect();
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(id) = ready.pop_first() {
        for edge in &outgoing[&id] {
            let count = pending.get_mut(&edge.to).expect("validated target");
            *count -= 1;
            if *count == 0 {
                ready.insert(edge.to.clone());
            }
        }
        order.push(id);
    }
    if order.len() != nodes.len() {
        let mut id = pending
            .iter()
            .find(|(_, count)| **count > 0)
            .expect("cycle has pending node")
            .0;
        let mut traced = BTreeSet::new();
        // Pending nodes can lie downstream. Follow pending predecessors until a node repeats.
        // Each pending node has a pending predecessor, so this takes at most nodes.len() hops.
        while traced.insert(id) {
            id = &incoming[id]
                .iter()
                .find(|edge| pending[&edge.from] > 0)
                .expect("pending node has a pending predecessor")
                .from;
        }
        return Err(invalid("graph contains a cycle").at_node(id));
    }
    Ok(order)
}
