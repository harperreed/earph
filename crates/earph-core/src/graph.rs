// ABOUTME: Defines bounded graph intent shared by control and DSP compilation.
// ABOUTME: Preserves numeric wire values while enforcing finite scalar equality.
use crate::{
    EngineError, ErrorCode,
    audio::{ChannelLayout, PcmRepresentation},
    ids::NodeId,
    safety::SampleDomain,
};
use serde::{Deserialize, Deserializer, Serialize, de};

pub const MAX_GRAPH_NODES: usize = 128;
pub const MAX_GRAPH_EDGES: usize = 256;
pub const MAX_MATRIX_COEFFICIENTS: usize = 4;
pub const MAX_GRAPH_INTENT_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct FiniteGraphScalar(f64);
impl FiniteGraphScalar {
    pub fn new(value: f64) -> Result<Self, EngineError> {
        if !value.is_finite() {
            return Err(EngineError::new(
                ErrorCode::GraphInvalid,
                "graph scalar must be finite",
            ));
        }
        Ok(Self(value))
    }
    pub fn get(self) -> f64 {
        self.0
    }
}
impl PartialEq for FiniteGraphScalar {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_bits() == other.0.to_bits()
    }
}
impl Eq for FiniteGraphScalar {}
impl TryFrom<f64> for FiniteGraphScalar {
    type Error = EngineError;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<FiniteGraphScalar> for f64 {
    fn from(value: FiniteGraphScalar) -> Self {
        value.get()
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "NodeFields")]
pub enum NodeKind {
    Input,
    Output,
    Bypass,
    Gain {
        gain_linear: FiniteGraphScalar,
    },
    Peq {
        filter: crate::dsp_nodes::PeqFilter,
    },
    Convolution {
        asset: crate::dsp_nodes::FirAssetReference,
    },
    Src {
        input_rate_hz: u32,
        preset: crate::dsp_nodes::SrcPreset,
        chunk_frames: usize,
    },
    Delay {
        delay_frames: u64,
    },
    Matrix {
        input_channels: ChannelLayout,
        coefficients: Vec<FiniteGraphScalar>,
    },
    Sum,
    Meter,
}

// Empty struct variants prevent serde from ignoring unsupported node parameters.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NodeFields {
    Input {},
    Output {},
    Bypass {},
    Gain {
        gain_linear: FiniteGraphScalar,
    },
    Peq {
        filter: crate::dsp_nodes::PeqFilter,
    },
    Convolution {
        asset: crate::dsp_nodes::FirAssetReference,
    },
    Src {
        input_rate_hz: u32,
        preset: crate::dsp_nodes::SrcPreset,
        chunk_frames: usize,
    },
    Delay {
        delay_frames: u64,
    },
    Matrix {
        input_channels: ChannelLayout,
        #[serde(deserialize_with = "deserialize_coefficients")]
        coefficients: Vec<FiniteGraphScalar>,
    },
    Sum {},
    Meter {},
}
impl From<NodeFields> for NodeKind {
    fn from(fields: NodeFields) -> Self {
        match fields {
            NodeFields::Input {} => Self::Input,
            NodeFields::Output {} => Self::Output,
            NodeFields::Bypass {} => Self::Bypass,
            NodeFields::Gain { gain_linear } => Self::Gain { gain_linear },
            NodeFields::Peq { filter } => Self::Peq { filter },
            NodeFields::Convolution { asset } => Self::Convolution { asset },
            NodeFields::Src {
                input_rate_hz,
                preset,
                chunk_frames,
            } => Self::Src {
                input_rate_hz,
                preset,
                chunk_frames,
            },
            NodeFields::Delay { delay_frames } => Self::Delay { delay_frames },
            NodeFields::Matrix {
                input_channels,
                coefficients,
            } => Self::Matrix {
                input_channels,
                coefficients,
            },
            NodeFields::Sum {} => Self::Sum,
            NodeFields::Meter {} => Self::Meter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDocument {
    pub schema_version: u32,
    #[serde(deserialize_with = "deserialize_nodes")]
    pub nodes: Vec<Node>,
    #[serde(deserialize_with = "deserialize_edges")]
    pub edges: Vec<Edge>,
    pub output: NodeId,
}
impl GraphDocument {
    /// Check structural limits before retaining caller-constructed graph intent.
    /// Compilation separately checks topology, formats, and node semantics.
    pub fn validate_structure(&self) -> Result<(), EngineError> {
        self.validate_storage()?;
        for node in &self.nodes {
            if let NodeKind::Matrix {
                input_channels,
                coefficients,
            } = &node.kind
                && coefficients.len() != input_channels.count() * node.format.channels.count()
            {
                return Err(EngineError::new(
                    ErrorCode::GraphInvalid,
                    "matrix requires output by input coefficients",
                ));
            }
        }
        Ok(())
    }
    /// Check retained storage separately from semantic graph validation.
    pub(crate) fn validate_storage(&self) -> Result<(), EngineError> {
        if self.nodes.len() > MAX_GRAPH_NODES || self.edges.len() > MAX_GRAPH_EDGES {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "graph intent exceeds structural limits",
            ));
        }
        if self.retained_capacity_bytes()? > MAX_GRAPH_INTENT_BYTES {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "graph intent storage exceeds its bound",
            ));
        }
        Ok(())
    }
    /// Count retained payload capacity, including spare vector and ID storage.
    /// Allocator headers and shared-owner headers are separate control overhead.
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let budget = || {
            EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "graph intent storage overflow",
            )
        };
        let mut bytes = size_of::<Self>()
            .checked_add(
                self.nodes
                    .capacity()
                    .checked_mul(size_of::<Node>())
                    .ok_or_else(budget)?,
            )
            .and_then(|n| n.checked_add(self.edges.capacity().checked_mul(size_of::<Edge>())?))
            .ok_or_else(budget)?;
        let mut add = |count: usize| -> Result<(), EngineError> {
            bytes = bytes.checked_add(count).ok_or_else(budget)?;
            Ok(())
        };
        add(self.output.capacity_bytes())?;
        for node in &self.nodes {
            add(node.id.capacity_bytes())?;
            if let NodeKind::Matrix { coefficients, .. } = &node.kind {
                add(coefficients
                    .capacity()
                    .checked_mul(size_of::<FiniteGraphScalar>())
                    .ok_or_else(budget)?)?;
            }
        }
        for edge in &self.edges {
            add(edge.from.capacity_bytes())?;
            add(edge.to.capacity_bytes())?;
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", from = "ProcessingFields")]
pub enum GraphProcessingRequest {
    IntegerBypass,
    Float64 { quantization: QuantizationPolicy },
}

// Empty struct variants make serde reject fields on integer bypass requests.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProcessingFields {
    IntegerBypass {},
    Float64 { quantization: QuantizationPolicy },
}
impl From<ProcessingFields> for GraphProcessingRequest {
    fn from(fields: ProcessingFields) -> Self {
        match fields {
            ProcessingFields::IntegerBypass {} => Self::IntegerBypass,
            ProcessingFields::Float64 { quantization } => Self::Float64 { quantization },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantizationPolicy {
    RoundTiesAwaySaturateNoDither,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservationDecision {
    RetainInteger,
    UnchangedFloat64,
    ProcessFloat64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Transformation {
    Gain { node: NodeId },
    Peq { node: NodeId },
    Convolution { node: NodeId },
    Src { node: NodeId },
    BoundedIirTruncation { node: NodeId, frames: u64 },
    Delay { node: NodeId },
    Matrix { node: NodeId },
    Sum { node: NodeId },
    Compensation { edge: Edge, delay_frames: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceUsage {
    pub buffer_bytes: u64,
    pub node_buffer_bytes: u64,
    pub delay_state_bytes: u64,
}

fn deserialize_nodes<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Node>, D::Error> {
    deserialize_bounded::<D, Node, MAX_GRAPH_NODES>(deserializer)
}
fn deserialize_edges<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Edge>, D::Error> {
    deserialize_bounded::<D, Edge, MAX_GRAPH_EDGES>(deserializer)
}
fn deserialize_coefficients<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<FiniteGraphScalar>, D::Error> {
    deserialize_bounded::<D, FiniteGraphScalar, MAX_MATRIX_COEFFICIENTS>(deserializer)
}

fn deserialize_bounded<'de, D, T, const LIMIT: usize>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Bounded<T, const LIMIT: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> de::Visitor<'de> for Bounded<T, LIMIT> {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "a sequence with at most {LIMIT} entries")
        }
        fn visit_seq<A: de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Vec<T>, A::Error> {
            let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(LIMIT));
            while values.len() < LIMIT {
                match sequence.next_element()? {
                    Some(value) => values.push(value),
                    None => return Ok(values),
                }
            }
            if sequence.next_element::<de::IgnoredAny>()?.is_some() {
                return Err(de::Error::custom("graph intent exceeds structural limits"));
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Bounded::<T, LIMIT>(std::marker::PhantomData))
}
