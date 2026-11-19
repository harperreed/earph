// ABOUTME: Decodes owner playback and backend observations without inventing device evidence.
// ABOUTME: Keeps nullable accounting separate from explicitly omitted processing observations.
use super::{graph::*, nullable, present};
use earph_core::{audio::PcmFormat, control::OutputBackend, ids::*, profiles::ProfileObjectHash};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedSampleRepresentationView {
    Signed16,
    Signed24,
    Signed32,
    Float32,
    Float64,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedStreamFormatView {
    pub sample_rate_hz: f64,
    pub channels: u32,
    pub representation: ObservedSampleRepresentationView,
    pub bytes_per_frame: u32,
    pub interleaved: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservationStateView {
    Unverified,
    Modified,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendObservationView {
    pub backend: OutputBackend,
    #[serde(deserialize_with = "nullable")]
    pub device_identity_sha256: Option<ProfileObjectHash>,
    pub audio_storage_bytes: u64,
    pub client_format: ObservedStreamFormatView,
    #[serde(deserialize_with = "nullable")]
    pub device_format: Option<ObservedStreamFormatView>,
    pub measurement_boundary: String,
    pub preservation: PreservationStateView,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputActivityScopeView {
    OutputInstanceAcrossNativeReopens,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputActivityBoundaryView {
    CoreAudioSdkEntryCalls,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputActivityCapabilitiesView {
    pub backends: Vec<OutputBackend>,
    pub scope: OutputActivityScopeView,
    pub measurement_boundary: OutputActivityBoundaryView,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputActivityView {
    pub scope: OutputActivityScopeView,
    pub measurement_boundary: OutputActivityBoundaryView,
    pub origin_epochs: Epochs,
    pub native_open_successes: u64,
    pub sdk_write_attempts: u64,
    pub accepted_frames: u64,
    pub sdk_start_attempts: u64,
    pub counter_overflow: bool,
    pub close_confirmed: bool,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaybackObservationView {
    #[serde(deserialize_with = "nullable")]
    pub processing: Option<PlaybackProcessingView>,
    pub format: PcmFormat,
    pub submitted_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub consumed_frames: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub backend_delay_frames: Option<u64>,
    pub consumption_basis: String,
    pub source_entry_id: QueueEntryId,
    pub source_position_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub audible_entry_id: Option<QueueEntryId>,
    #[serde(deserialize_with = "nullable")]
    pub audible_source_position_frames: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub input_peak: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub output_peak: Option<f64>,
    pub modified: bool,
    #[serde(deserialize_with = "nullable")]
    pub backend: Option<BackendObservationView>,
    #[serde(deserialize_with = "nullable")]
    pub output_activity: Option<OutputActivityView>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlaybackProcessingView {
    pub resident_bytes: u64,
    #[serde(deserialize_with = "nullable")]
    pub audio_payload_bytes: Option<usize>,
    #[serde(deserialize_with = "nullable")]
    pub failure_node: Option<NodeId>,
    pub graph_hash: ProfileObjectHash,
    // The strict timing variant consumes all remaining flattened fields, including unknown keys.
    #[serde(flatten)]
    pub timing: PlaybackTimingView,
    #[serde(default, deserialize_with = "present")]
    pub streaming: Option<StreamingRuntimeView>,
    #[serde(default, deserialize_with = "present")]
    pub source_boundary: Option<Arc<StreamingReplacementView>>,
    pub saturation_count: u64,
    #[serde(deserialize_with = "nullable")]
    pub graph_peak: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub prequantization_peak: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "timing_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlaybackTimingView {
    SameRate {
        latency_frames: u64,
        tail_frames: u64,
    },
    Streaming {
        stages: Arc<[CompiledStreamStageView]>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingRuntimeView {
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub source_accepted_frames: u64,
    pub generated_output_frames: u64,
    pub submission_frontier_frames: u64,
    pub backend_origin_frames: u64,
    pub retained_start_frames: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamingReplacementView {
    pub source_entry_id: QueueEntryId,
    pub source_position_frames: u64,
    pub source_epochs: Epochs,
    pub discarded_output_frames: u64,
}
