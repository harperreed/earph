// ABOUTME: Defines immutable compiled schedules from shared graph intent.
// ABOUTME: Carries format, latency, resource, and preservation decisions explicitly.
use earph_core::{ErrorCode, audio::PcmFormat};
pub use earph_core::{graph::*, ids::NodeId, safety::SampleDomain};
use serde::Serialize;

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
}
impl CompiledPlan {
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
}
