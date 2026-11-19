// ABOUTME: Projects graph metadata, timing, and profile bindings from the shared owner.
// ABOUTME: Keeps configured intent, prepared candidates, and actual observations distinct.
use super::{nullable, playback::*, profiles::ProfileApplicationBindingView};
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat},
    dsp_nodes::SrcPreset,
    graph::{Edge, PreservationDecision, QuantizationPolicy, Transformation},
    graph_control::GraphIntent,
    ids::*,
    profiles::ProfileObjectHash,
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphBindingView {
    pub owner_id: OwnerId,
    pub session_id: SessionId,
    pub epochs: Epochs,
    pub device_epoch: u64,
    pub expected_revision: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphTransitionView {
    MutedRebuffer,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignalTransformationView {
    Kernel {
        transformation: Transformation,
    },
    IntegerToFloat64 {},
    SoftwareVolume {},
    Quantization {
        policy: QuantizationPolicy,
    },
    BackendAdapterConversion {
        client_representation: ObservedSampleRepresentationView,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphResidentUsageView {
    pub backend_audio_bytes: u64,
    pub backend_audio_peak_bytes: u64,
    pub stream_bridge_bytes: u64,
    pub kernel_audio_bytes: u64,
    pub host_input_bytes: u64,
    pub host_output_bytes: u64,
    pub native_period_bytes: u64,
    pub metadata_bytes: u64,
    pub pipeline_bytes: u64,
    pub aggregate_peak_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphTailPolicyView {
    AuthoredFiniteSupport,
    BoundedIirTruncation,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterTailView {
    pub node: NodeId,
    pub channels: ChannelLayout,
    pub horizon_frames: u64,
    pub terminal_output_bounds: Vec<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcResourcesView {
    pub resident_bytes: u64,
    pub preparation_peak_bytes: u64,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SrcParametersView {
    pub input_rate: u32,
    pub output_rate: u32,
    pub channels: ChannelLayout,
    pub preset: SrcPreset,
    pub chunk_frames: usize,
    pub filter_length: usize,
    pub phases: usize,
    #[serde(deserialize_with = "nullable")]
    pub explicit_pre_scaling_cutoff: Option<f32>,
    pub pre_scaling_cutoff: f32,
    pub drain_zero_calls: usize,
    pub max_output_frames: usize,
    pub guard_time_numerator: u32,
    pub library_max_relative_ratio: f64,
    pub resources: SrcResourcesView,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CompiledStreamStageKindView {
    Island {
        kernel_hash: ProfileObjectHash,
        local_latency_frames: u64,
        emitted_tail_frames: u64,
        bounded_iir_truncation: bool,
        terminal_output_bounds: Vec<f64>,
    },
    Src {
        parameters: SrcParametersView,
        coefficient_fingerprint: [u8; 32],
        parameter_fingerprint: [u8; 32],
        guard_time_denominator: u32,
        maximum_adapter_finish_output_frames: u64,
        #[serde(deserialize_with = "nullable")]
        truncated_library_output_delay_frames: Option<u64>,
    },
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledStreamStageView {
    pub original_nodes: Vec<NodeId>,
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    pub input_peak_bounds: Vec<f64>,
    pub output_peak_bounds: Vec<f64>,
    pub kind: CompiledStreamStageKindView,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "timing_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GraphTimingView {
    SameRate {
        latency_frames: u64,
        tail_frames: u64,
        tail_policy: GraphTailPolicyView,
        terminal_output_bounds: Vec<f64>,
        node_tails: Vec<FilterTailView>,
    },
    Streaming {
        stages: Arc<[CompiledStreamStageView]>,
    },
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GraphPreviewView {
    #[serde(deserialize_with = "nullable")]
    pub profile_application: Option<Arc<ProfileApplicationBindingView>>,
    #[serde(deserialize_with = "nullable")]
    pub backend: Option<BackendObservationView>,
    pub graph_hash: ProfileObjectHash,
    pub kernel_hash: ProfileObjectHash,
    pub implementation_version: String,
    pub binding: GraphBindingView,
    pub source_format: PcmFormat,
    pub native_output_format: PcmFormat,
    pub processing_input_format: PcmFormat,
    pub processing_output_format: PcmFormat,
    pub quantum_frames: usize,
    // The strict timing variant consumes all remaining flattened fields, including unknown keys.
    #[serde(flatten)]
    pub timing: GraphTimingView,
    pub preservation: PreservationDecision,
    pub transformations: Vec<SignalTransformationView>,
    pub resources: GraphResidentUsageView,
    pub channel_peak_bounds: Vec<f64>,
    pub required_headroom_db: f64,
    pub transition: GraphTransitionView,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphPreparedView {
    pub token: PreparedGraphId,
    pub expires_monotonic_ns: u64,
    pub preview: Arc<GraphPreviewView>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphOperationStageView {
    Validating,
    Preparing,
    Activating,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphServiceView {
    pub resident_bytes: u64,
    pub pending_retirements: usize,
    pub preparing_or_candidate: bool,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphInspectionView {
    pub service: GraphServiceView,
    pub recovery_required: bool,
    pub graph_epoch: u64,
    #[serde(deserialize_with = "nullable")]
    pub active: Option<Arc<GraphPreviewView>>,
    #[serde(deserialize_with = "nullable")]
    pub configured: Option<Arc<GraphIntent>>,
    #[serde(deserialize_with = "nullable")]
    pub pending_request_id: Option<RequestId>,
    #[serde(deserialize_with = "nullable")]
    pub pending_stage: Option<GraphOperationStageView>,
    #[serde(deserialize_with = "nullable")]
    pub prepared: Option<GraphPreparedView>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphDiagnosticView {
    pub code: ErrorCode,
    pub message: String,
    #[serde(deserialize_with = "nullable")]
    pub node: Option<NodeId>,
    #[serde(deserialize_with = "nullable")]
    pub edge: Option<Edge>,
}
