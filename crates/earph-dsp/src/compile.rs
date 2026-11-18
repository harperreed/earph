// ABOUTME: Validates graph intent before resolving bounded execution plans.
// ABOUTME: Derives deterministic schedules and explicit merge compensation.
use crate::*;
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const IMPLEMENTATION_VERSION: &str = "earph-dsp/0.1.0/compiler-1";

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
    if document.schema_version != 1 {
        return Err(GraphError::new(
            ErrorCode::CapabilityUnsupported,
            "unsupported graph schema",
        ));
    }
    if document.nodes.len() > context.budget.max_nodes.min(128)
        || document.edges.len() > context.budget.max_edges.min(256)
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
            NodeKind::Delay { delay_frames } if delay_frames > 0 => {
                Some(Transformation::Delay { node: id.clone() })
            }
            NodeKind::Matrix { .. } => Some(Transformation::Matrix { node: id.clone() }),
            NodeKind::Sum => Some(Transformation::Sum { node: id.clone() }),
            _ => None,
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
        let own_delay = match node.kind {
            NodeKind::Delay { delay_frames } => delay_frames,
            _ => 0,
        };
        let latency_frames = arrival
            .checked_add(own_delay)
            .ok_or_else(|| budget().at_node(&id))?;
        let tail_frames = tail
            .checked_add(own_delay)
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
                .checked_add(output_bytes)
                .ok_or_else(|| budget().at_node(&id))?;
            resources.delay_state_bytes = resources
                .delay_state_bytes
                .checked_add(delay_bytes)
                .ok_or_else(|| budget().at_node(&id))?;
        }
        resources.buffer_bytes = resources
            .node_buffer_bytes
            .checked_add(resources.delay_state_bytes)
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
        });
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
    let bytes = serde_json::to_vec(&(IMPLEMENTATION_VERSION, canonical, context))
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
    })
}

fn matches_external(port: PortFormat, external: PcmFormat) -> bool {
    port.sample_rate_hz == external.sample_rate_hz()
        && port.channels == external.channels()
        && port.precision == external.representation()
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
        NodeKind::Gain { gain_linear } if !gain_linear.is_finite() => {
            return Err(invalid("gain must be finite").at_node(&node.id));
        }
        NodeKind::Matrix {
            input_channels,
            coefficients,
        } if coefficients.len() != input_channels.count() * node.format.channels.count()
            || coefficients.iter().any(|n| !n.is_finite()) =>
        {
            return Err(
                invalid("matrix requires finite output by input coefficients").at_node(&node.id),
            );
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
