// ABOUTME: Defines observable graph preparation, activation, and recovery contracts.
// ABOUTME: Keeps platform resources and compiled kernels outside session state.
use crate::{
    EngineError, ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
    dsp_nodes::SrcParameters,
    graph::{
        Edge, GraphDocument, GraphProcessingRequest, MAX_GRAPH_NODES, PreservationDecision,
        QuantizationPolicy, Transformation,
    },
    ids::*,
    session::{BackendObservation, ObservedSampleRepresentation, PlaybackObservation},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, sync::Arc};

pub const GRAPH_COMMAND_BYTES: usize = 128 * 1024;
pub const GRAPH_TOKEN_TTL_NS: u64 = 30_000_000_000;
pub const GRAPH_PREPARATION_DEADLINE_NS: u64 = 5_000_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphIntent {
    pub graph: Arc<GraphDocument>,
    pub processing: GraphProcessingRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphBinding {
    pub owner_id: OwnerId,
    pub session_id: SessionId,
    pub epochs: Epochs,
    pub device_epoch: u64,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphTransition {
    MutedRebuffer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalTransformation {
    Kernel {
        transformation: Transformation,
    },
    IntegerToFloat64,
    SoftwareVolume,
    Quantization {
        policy: QuantizationPolicy,
    },
    BackendAdapterConversion {
        client_representation: ObservedSampleRepresentation,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct GraphResidentUsage {
    pub kernel_audio_bytes: u64,
    pub host_input_bytes: u64,
    pub host_output_bytes: u64,
    pub native_period_bytes: u64,
    pub metadata_bytes: u64,
    pub pipeline_bytes: u64,
    pub aggregate_peak_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphTailPolicy {
    AuthoredFiniteSupport,
    BoundedIirTruncation,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FilterTailObservation {
    pub node: crate::ids::NodeId,
    pub channels: crate::audio::ChannelLayout,
    pub horizon_frames: u64,
    pub terminal_output_bounds: Vec<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CompiledStreamStage {
    pub original_nodes: Vec<NodeId>,
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    pub input_peak_bounds: Vec<f64>,
    pub output_peak_bounds: Vec<f64>,
    pub kind: CompiledStreamStageKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "timing_kind", rename_all = "snake_case")]
pub enum GraphTiming {
    SameRate {
        latency_frames: u64,
        tail_frames: u64,
        tail_policy: GraphTailPolicy,
        terminal_output_bounds: Vec<f64>,
        node_tails: Vec<FilterTailObservation>,
    },
    Streaming {
        stages: Arc<[CompiledStreamStage]>,
    },
}
/// Compact progress facts. Streaming observers share the preview's immutable stage array.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "timing_kind", rename_all = "snake_case")]
pub enum PlaybackTiming {
    SameRate {
        latency_frames: u64,
        tail_frames: u64,
    },
    Streaming {
        stages: Arc<[CompiledStreamStage]>,
    },
}
impl GraphTiming {
    pub fn latency_frames(&self) -> Option<u64> {
        match self {
            Self::SameRate { latency_frames, .. } => Some(*latency_frames),
            Self::Streaming { .. } => None,
        }
    }
    pub fn tail_frames(&self) -> Option<u64> {
        match self {
            Self::SameRate { tail_frames, .. } => Some(*tail_frames),
            Self::Streaming { .. } => None,
        }
    }
    pub fn playback_timing(&self) -> PlaybackTiming {
        match self {
            Self::SameRate {
                latency_frames,
                tail_frames,
                ..
            } => PlaybackTiming::SameRate {
                latency_frames: *latency_frames,
                tail_frames: *tail_frames,
            },
            Self::Streaming { stages } => PlaybackTiming::Streaming {
                stages: stages.clone(),
            },
        }
    }
    fn valid(
        &self,
        input: PcmFormat,
        output: PcmFormat,
        bounds: &[f64],
        aggregate_peak_bytes: u64,
    ) -> bool {
        match self {
            Self::SameRate {
                tail_frames,
                terminal_output_bounds,
                node_tails,
                ..
            } => {
                input.sample_rate_hz() == output.sample_rate_hz()
                    && terminal_bounds(terminal_output_bounds, output)
                    && node_tails.len() <= MAX_GRAPH_NODES
                    && node_tails.iter().all(|node| {
                        node.horizon_frames <= *tail_frames
                            && node.terminal_output_bounds.len() == node.channels.count()
                            && node
                                .terminal_output_bounds
                                .iter()
                                .all(|bound| bound.is_finite() && *bound >= 0.0)
                    })
            }
            Self::Streaming { stages } => {
                streaming_facts_valid(stages, input, output, bounds, aggregate_peak_bytes)
            }
        }
    }
}
fn peak_bounds(bounds: &[f64], format: PcmFormat) -> bool {
    bounds.len() == format.channels().count()
        && bounds
            .iter()
            .all(|bound| bound.is_finite() && *bound >= 0.0)
}
fn terminal_bounds(bounds: &[f64], format: PcmFormat) -> bool {
    peak_bounds(bounds, format) && bounds.iter().all(|bound| *bound <= 1e-12)
}
fn streaming_facts_valid(
    stages: &[CompiledStreamStage],
    input: PcmFormat,
    output: PcmFormat,
    bounds: &[f64],
    aggregate_peak_bytes: u64,
) -> bool {
    if stages.is_empty()
        || stages.len() > MAX_GRAPH_NODES
        || stages[0].input_format != input
        || stages[stages.len() - 1].output_format != output
        || stages[stages.len() - 1].output_peak_bounds != bounds
    {
        return false;
    }
    let mut nodes = BTreeSet::new();
    let mut has_src = false;
    for (index, stage) in stages.iter().enumerate() {
        if stage.original_nodes.is_empty()
            || stage.original_nodes.len() > MAX_GRAPH_NODES
            || stage.original_nodes.iter().any(|node| !nodes.insert(node))
            || nodes.len() > MAX_GRAPH_NODES
            || stage.input_format.representation() != PcmRepresentation::Float64
            || stage.output_format.representation() != PcmRepresentation::Float64
            || !peak_bounds(&stage.input_peak_bounds, stage.input_format)
            || !peak_bounds(&stage.output_peak_bounds, stage.output_format)
        {
            return false;
        }
        if index > 0 {
            let previous = &stages[index - 1];
            if previous.output_format != stage.input_format
                || previous
                    .output_peak_bounds
                    .iter()
                    .zip(&stage.input_peak_bounds)
                    .any(|(actual, admitted)| actual > admitted)
            {
                return false;
            }
        }
        match &stage.kind {
            CompiledStreamStageKind::Island {
                kernel_hash,
                bounded_iir_truncation,
                terminal_output_bounds,
                ..
            } => {
                if !valid_graph_hash(kernel_hash)
                    || stage.input_format.sample_rate_hz() != stage.output_format.sample_rate_hz()
                    || *bounded_iir_truncation && index + 1 != stages.len()
                    || !terminal_bounds(terminal_output_bounds, stage.output_format)
                {
                    return false;
                }
            }
            CompiledStreamStageKind::Src {
                parameters,
                guard_time_denominator,
                maximum_adapter_finish_output_frames,
                ..
            } => {
                has_src = true;
                if !src_facts_valid(parameters, stage.input_format, stage.output_format)
                    || parameters.resources.preparation_peak_bytes > aggregate_peak_bytes
                    || *guard_time_denominator != parameters.input_rate
                    || *maximum_adapter_finish_output_frames < parameters.max_output_frames as u64
                {
                    return false;
                }
            }
        }
    }
    has_src
}
fn src_facts_valid(parameters: &SrcParameters, input: PcmFormat, output: PcmFormat) -> bool {
    parameters.input_rate == input.sample_rate_hz()
        && parameters.output_rate == output.sample_rate_hz()
        && parameters.channels == input.channels()
        && parameters.channels == output.channels()
        && (1..=8192).contains(&parameters.chunk_frames)
        && (1..=8192).contains(&parameters.max_output_frames)
        && parameters.filter_length > 0
        && parameters.phases > 0
        && parameters.drain_zero_calls > 0
        && parameters.guard_time_numerator > 0
        && parameters.pre_scaling_cutoff.is_finite()
        && parameters.pre_scaling_cutoff > 0.0
        && parameters.pre_scaling_cutoff <= 1.0
        && parameters
            .explicit_pre_scaling_cutoff
            .is_none_or(|cutoff| cutoff == parameters.pre_scaling_cutoff)
        && parameters.library_max_relative_ratio.is_finite()
        && parameters.library_max_relative_ratio >= 1.0
        && parameters.resources.resident_bytes > 0
        && parameters.resources.preparation_peak_bytes >= parameters.resources.resident_bytes
        && parameters.resources.preparation_peak_bytes <= 256 * 1024 * 1024
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphPreview {
    pub backend: Option<BackendObservation>,
    pub graph_hash: String,
    pub kernel_hash: String,
    pub implementation_version: String,
    /// Captures preparation context; Session epochs describe current playback.
    pub binding: GraphBinding,
    pub source_format: PcmFormat,
    pub native_output_format: PcmFormat,
    pub processing_input_format: PcmFormat,
    pub processing_output_format: PcmFormat,
    pub quantum_frames: usize,
    #[serde(flatten)]
    pub timing: GraphTiming,
    pub preservation: PreservationDecision,
    pub transformations: Vec<SignalTransformation>,
    pub resources: GraphResidentUsage,
    pub channel_peak_bounds: Vec<f64>,
    pub required_headroom_db: f64,
    pub transition: GraphTransition,
}
impl GraphPreview {
    pub fn validate(&self) -> Result<(), EngineError> {
        if !valid_graph_hash(&self.graph_hash)
            || !valid_graph_hash(&self.kernel_hash)
            || !(1..=8192).contains(&self.quantum_frames)
            || self.channel_peak_bounds.len() != self.native_output_format.channels().count()
            || self.processing_input_format.sample_rate_hz() != self.source_format.sample_rate_hz()
            || self.processing_input_format.channels() != self.source_format.channels()
            || self.processing_output_format.sample_rate_hz()
                != self.native_output_format.sample_rate_hz()
            || self.processing_output_format.channels() != self.native_output_format.channels()
            || !self.timing.valid(
                self.processing_input_format,
                self.processing_output_format,
                &self.channel_peak_bounds,
                self.resources.aggregate_peak_bytes,
            )
            || matches!(self.timing, GraphTiming::Streaming { .. })
                && self.preservation != PreservationDecision::ProcessFloat64
            || self
                .channel_peak_bounds
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0)
            || !self.required_headroom_db.is_finite()
            || self.required_headroom_db < 0.0
            || self.resources.aggregate_peak_bytes > 256 * 1024 * 1024
        {
            return Err(EngineError::new(
                ErrorCode::GraphInvalid,
                "invalid prepared graph metadata",
            ));
        }
        Ok(())
    }
    pub fn validate_headroom(&self, gain_db: f64, cap_db: f64) -> Result<(), EngineError> {
        self.validate()?;
        let bound = self
            .channel_peak_bounds
            .iter()
            .copied()
            .fold(1.0_f64, f64::max);
        if !gain_db.is_finite()
            || !cap_db.is_finite()
            || bound * 10.0_f64.powf(gain_db / 20.0) > 10.0_f64.powf(cap_db / 20.0)
        {
            return Err(EngineError::new(
                ErrorCode::SafetyPolicyBlocked,
                "graph requires more software headroom",
            ));
        }
        Ok(())
    }
}
pub fn valid_graph_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphPrepared {
    pub token: PreparedGraphId,
    pub expires_monotonic_ns: u64,
    pub preview: Arc<GraphPreview>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOperationStage {
    Validating,
    Preparing,
    Activating,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GraphInspection {
    pub service: GraphServiceObservation,
    pub recovery_required: bool,
    pub graph_epoch: u64,
    pub active: Option<Arc<GraphPreview>>,
    pub configured: Option<Arc<GraphIntent>>,
    pub pending_request_id: Option<RequestId>,
    pub pending_stage: Option<GraphOperationStage>,
    pub prepared: Option<GraphPrepared>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct GraphServiceObservation {
    pub resident_bytes: u64,
    pub pending_retirements: usize,
    pub preparing_or_candidate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GraphDiagnostic {
    pub code: ErrorCode,
    pub message: &'static str,
    pub node: Option<NodeId>,
    pub edge: Option<Edge>,
}
impl From<EngineError> for GraphDiagnostic {
    fn from(error: EngineError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            node: None,
            edge: None,
        }
    }
}
impl GraphDiagnostic {
    pub fn error(&self) -> EngineError {
        EngineError::new(self.code, self.message)
    }
}

#[derive(Debug, Clone)]
pub struct GraphJobRequest {
    pub request_id: RequestId,
    pub binding: GraphBinding,
    pub intent: Arc<GraphIntent>,
    pub validate_only: bool,
    pub deadline_monotonic_ns: u64,
}

#[derive(Debug, Clone)]
pub struct GraphActivationRequest {
    pub request_id: RequestId,
    pub previous_binding: GraphBinding,
    pub epochs: Epochs,
    pub prepared: GraphPrepared,
}

#[derive(Debug, Clone)]
pub struct GraphActivationAck {
    pub request_id: RequestId,
    pub epochs: Epochs,
    pub graph_hash: String,
    pub activation_frame: u64,
    pub observation: PlaybackObservation,
    pub source_total_frames: u64,
}
