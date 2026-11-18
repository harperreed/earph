// ABOUTME: Defines observable graph preparation, activation, and recovery contracts.
// ABOUTME: Keeps platform resources and compiled kernels outside session state.
use crate::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    graph::{
        Edge, GraphDocument, GraphProcessingRequest, PreservationDecision, QuantizationPolicy,
        Transformation,
    },
    ids::*,
    session::{BackendObservation, ObservedSampleRepresentation, PlaybackObservation},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    pub latency_frames: u64,
    pub tail_frames: u64,
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
