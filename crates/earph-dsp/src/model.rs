// ABOUTME: Defines immutable compiled schedules from shared graph intent.
// ABOUTME: Carries format, latency, resource, and preservation decisions explicitly.
use crate::{
    convolution::ConvolutionDesign,
    peq::{PeqDesign, PeqTail},
};
use earph_core::dsp_nodes::FirAssetReference;
use earph_core::{ErrorCode, audio::PcmFormat};
pub use earph_core::{graph::*, ids::NodeId, safety::SampleDomain};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Verified immutable coefficients supplied by a bounded asset resolver.
#[derive(Debug, Clone, PartialEq)]
pub struct FirCoefficients {
    reference: FirAssetReference,
    taps: Arc<[Vec<f64>]>,
    fingerprint: String,
    resident_bytes: u64,
}
impl FirCoefficients {
    pub fn new(reference: FirAssetReference, taps: Vec<Vec<f64>>) -> Result<Self, GraphError> {
        reference.validate().map_err(|_| {
            GraphError::new(ErrorCode::AssetInvalid, "invalid correction reference")
        })?;
        if taps.len() != reference.channels.count()
            || taps
                .iter()
                .any(|v| v.len() != reference.taps_per_channel || v.iter().any(|x| !x.is_finite()))
        {
            return Err(GraphError::new(
                ErrorCode::AssetInvalid,
                "correction coefficients differ from declared geometry",
            ));
        }
        let mut hash = Sha256::new();
        let mut resident_bytes =
            (taps.len() * size_of::<Vec<f64>>() + 2 * size_of::<usize>()) as u64;
        for channel in &taps {
            resident_bytes = resident_bytes
                .checked_add((channel.capacity() * 8) as u64)
                .ok_or_else(|| {
                    GraphError::new(
                        ErrorCode::ResourceBudgetExceeded,
                        "correction storage overflow",
                    )
                })?;
            for value in channel {
                hash.update(value.to_bits().to_le_bytes());
            }
        }
        Ok(Self {
            reference,
            taps: taps.into(),
            fingerprint: hash.finalize().iter().map(|v| format!("{v:02x}")).collect(),
            resident_bytes,
        })
    }
    pub fn reference(&self) -> &FirAssetReference {
        &self.reference
    }
    pub fn coefficients(&self) -> &[Vec<f64>] {
        &self.taps
    }
    pub fn coefficient_sha256(&self) -> &str {
        &self.fingerprint
    }
    pub fn resident_bytes(&self) -> u64 {
        self.resident_bytes
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ResolvedKernel {
    Structural,
    Peq {
        design: PeqDesign,
        tail: PeqTail,
    },
    Convolution {
        design: ConvolutionDesign,
        asset: FirCoefficients,
    },
}
impl PartialEq for ResolvedKernel {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Structural, Self::Structural) => true,
            (
                Self::Peq {
                    design: a,
                    tail: at,
                },
                Self::Peq {
                    design: b,
                    tail: bt,
                },
            ) => {
                a.coefficients().map(f64::to_bits) == b.coefficients().map(f64::to_bits) && at == bt
            }
            (Self::Convolution { asset: a, .. }, Self::Convolution { asset: b, .. }) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingPolicy {
    StrictPreservation,
    AllowProcessing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct GraphBudget {
    pub max_nodes: usize,
    pub max_edges: usize,
    pub max_quantum_frames: usize,
    pub max_buffer_bytes: u64,
    pub max_latency_frames: u64,
}
impl Default for GraphBudget {
    fn default() -> Self {
        Self {
            max_nodes: MAX_GRAPH_NODES,
            max_edges: MAX_GRAPH_EDGES,
            max_quantum_frames: 8192,
            max_buffer_bytes: 256 * 1024 * 1024,
            max_latency_frames: u64::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CompileContext {
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    pub quantum_frames: usize,
    pub policy: ProcessingPolicy,
    pub budget: GraphBudget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphError {
    pub code: ErrorCode,
    pub message: &'static str,
    pub node: Option<NodeId>,
    pub edge: Option<Edge>,
}
impl GraphError {
    pub fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            node: None,
            edge: None,
        }
    }
    pub(crate) fn at_node(mut self, node: &NodeId) -> Self {
        self.node = Some(node.clone());
        self
    }
    pub(crate) fn at_edge(mut self, edge: &Edge) -> Self {
        self.edge = Some(edge.clone());
        self
    }
}
impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for GraphError {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CompiledInput {
    pub source_index: usize,
    pub target_port: usize,
    pub compensation_frames: u64,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompiledNode {
    pub(crate) id: NodeId,
    pub(crate) kind: NodeKind,
    pub(crate) format: PortFormat,
    pub(crate) inputs: Vec<CompiledInput>,
    pub(crate) latency_frames: u64,
    pub(crate) tail_frames: u64,
    pub(crate) input_peak_bounds: Vec<f64>,
    pub(crate) output_peak_bounds: Vec<f64>,
    pub(crate) terminal_output_bounds: Vec<f64>,
    #[serde(skip)]
    pub(crate) kernel: ResolvedKernel,
}
impl CompiledNode {
    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn kind(&self) -> &NodeKind {
        &self.kind
    }
    pub fn format(&self) -> PortFormat {
        self.format
    }
    pub fn inputs(&self) -> &[CompiledInput] {
        &self.inputs
    }
    pub fn latency_frames(&self) -> u64 {
        self.latency_frames
    }
    pub fn tail_frames(&self) -> u64 {
        self.tail_frames
    }
    pub fn input_peak_bounds(&self) -> &[f64] {
        &self.input_peak_bounds
    }
    pub fn output_peak_bounds(&self) -> &[f64] {
        &self.output_peak_bounds
    }
    pub fn terminal_output_bounds(&self) -> &[f64] {
        &self.terminal_output_bounds
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompiledPlan {
    pub(crate) content_hash: String,
    pub(crate) implementation_version: &'static str,
    pub(crate) schedule: Vec<CompiledNode>,
    pub(crate) input_index: usize,
    pub(crate) output_index: usize,
    pub(crate) context: CompileContext,
    pub(crate) latency_frames: u64,
    pub(crate) tail_frames: u64,
    pub(crate) resources: ResourceUsage,
    pub(crate) transformations: Vec<Transformation>,
    pub(crate) preservation: PreservationDecision,
    pub(crate) kernel_state_bytes: u64,
    pub(crate) asset_bytes: u64,
    pub(crate) preparation_scratch_bytes: u64,
    pub(crate) bounded_iir: bool,
    pub(crate) enforce_input_bounds: bool,
}
impl CompiledPlan {
    /// Projects this compiler's resolved schedule without authored names or inferred kernel versions.
    pub fn diagnostic_nodes(&self) -> Vec<earph_core::diagnostics::DiagnosticNode> {
        use earph_core::diagnostics::{
            DiagnosticNode, DiagnosticNodeKind as Kind, UnavailableReason,
        };
        self.schedule
            .iter()
            .enumerate()
            .map(|(index, node)| DiagnosticNode {
                alias: (index + 1) as u16,
                kind: match node.kind() {
                    NodeKind::Input => Kind::Input,
                    NodeKind::Output => Kind::Output,
                    NodeKind::Bypass => Kind::Bypass,
                    NodeKind::Gain { .. } => Kind::Gain,
                    NodeKind::Peq { .. } => Kind::Peq,
                    NodeKind::Convolution { .. } => Kind::Convolution,
                    NodeKind::Src { .. } => Kind::Src,
                    NodeKind::Delay { .. } => Kind::Delay,
                    NodeKind::Matrix { .. } => Kind::Matrix,
                    NodeKind::Sum => Kind::Sum,
                    NodeKind::Meter => Kind::Meter,
                },
                format: PcmFormat::new(
                    node.format.sample_rate_hz,
                    node.format.channels,
                    node.format.precision,
                )
                .expect("compiled PCM node format"),
                path_latency_frames: node.latency_frames,
                gain_linear: match node.kind() {
                    NodeKind::Gain { gain_linear } => Some(gain_linear.get()),
                    _ => None,
                },
                bypass: matches!(node.kind(), NodeKind::Bypass),
                implementation_version: None,
                implementation_unavailable_reason: UnavailableReason::NotObserved,
            })
            .collect()
    }
    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }
    pub fn implementation_version(&self) -> &str {
        self.implementation_version
    }
    pub fn schedule(&self) -> &[CompiledNode] {
        &self.schedule
    }
    pub fn input_index(&self) -> usize {
        self.input_index
    }
    pub fn output_index(&self) -> usize {
        self.output_index
    }
    pub fn context(&self) -> CompileContext {
        self.context
    }
    pub fn latency_frames(&self) -> u64 {
        self.latency_frames
    }
    pub fn tail_frames(&self) -> u64 {
        self.tail_frames
    }
    pub fn resources(&self) -> ResourceUsage {
        self.resources
    }
    pub fn transformations(&self) -> &[Transformation] {
        &self.transformations
    }
    pub fn preservation(&self) -> PreservationDecision {
        self.preservation
    }
    pub fn output_peak_bounds(&self) -> &[f64] {
        self.schedule[self.output_index].output_peak_bounds()
    }
    pub fn terminal_output_bounds(&self) -> &[f64] {
        self.schedule[self.output_index].terminal_output_bounds()
    }
    pub fn kernel_state_bytes(&self) -> u64 {
        self.kernel_state_bytes
    }
    pub fn asset_bytes(&self) -> u64 {
        self.asset_bytes
    }
    pub fn preparation_scratch_bytes(&self) -> u64 {
        self.preparation_scratch_bytes
    }
    pub fn bounded_iir_truncation(&self) -> bool {
        self.bounded_iir
    }
    pub fn input_peak_bounds(&self) -> &[f64] {
        self.schedule[self.input_index].output_peak_bounds()
    }
    pub fn enforces_input_peak_bounds(&self) -> bool {
        self.enforce_input_bounds
    }
}
