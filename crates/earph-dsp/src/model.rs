// ABOUTME: Defines graph documents and immutable resolved schedules.
// ABOUTME: Carries format, latency, resource, and preservation decisions explicitly.
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct NodeId(String);
impl NodeId {
    pub fn new(value: impl Into<String>) -> Result<Self, GraphError> {
        let value = value.into();
        if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
            return Err(GraphError::new(
                ErrorCode::GraphInvalid,
                "invalid node identifier",
            ));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for NodeId {
    type Error = GraphError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<NodeId> for String {
    fn from(value: NodeId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleDomain {
    Pcm,
    Dsd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortFormat {
    pub domain: SampleDomain,
    pub sample_rate_hz: u32,
    pub channels: ChannelLayout,
    pub precision: PcmRepresentation,
    pub max_quantum_frames: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NodeKind {
    Input,
    Output,
    Bypass,
    Gain {
        gain_linear: f64,
    },
    Delay {
        delay_frames: u64,
    },
    Matrix {
        input_channels: ChannelLayout,
        coefficients: Vec<f64>,
    },
    Sum,
    Meter,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub id: NodeId,
    pub kind: NodeKind,
    pub format: PortFormat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub from_port: usize,
    pub to_port: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDocument {
    pub schema_version: u32,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub output: NodeId,
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
            max_nodes: 128,
            max_edges: 256,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservationDecision {
    RetainInteger,
    UnchangedFloat64,
    ProcessFloat64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transformation {
    Gain { node: NodeId },
    Delay { node: NodeId },
    Matrix { node: NodeId },
    Sum { node: NodeId },
    Compensation { edge: Edge, delay_frames: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResourceUsage {
    pub buffer_bytes: u64,
    pub node_buffer_bytes: u64,
    pub delay_state_bytes: u64,
}

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
