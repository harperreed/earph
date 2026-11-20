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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPlaybackObservationView {
    pub binding: earph_core::network_source::NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub lifecycle: earph_core::network_source::NetworkLifecycle,
    pub format: PcmFormat,
    pub output_format: PcmFormat,
    #[serde(deserialize_with = "nullable")]
    pub total_frames: Option<u64>,
    pub source_position_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub audible_source_position_frames: Option<u64>,
    pub submitted_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub consumed_frames: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub backend_delay_frames: Option<u64>,
    pub consumption_basis: String,
    #[serde(deserialize_with = "nullable")]
    pub input_peak: Option<f64>,
    #[serde(deserialize_with = "nullable")]
    pub output_peak: Option<f64>,
    pub modified: bool,
    #[serde(deserialize_with = "nullable")]
    pub backend: Option<BackendObservationView>,
    #[serde(deserialize_with = "nullable")]
    pub output_activity: Option<OutputActivityView>,
    #[serde(deserialize_with = "nullable")]
    pub processing: Option<PlaybackProcessingView>,
    #[serde(default, deserialize_with = "present")]
    pub replacement_boundary: Option<Arc<earph_core::network_source::NetworkReplacementBoundary>>,
}
impl NetworkPlaybackObservationView {
    pub(super) fn valid(&self) -> bool {
        // Match Core's observation contract without binding retained facts to live authority.
        self.binding.source_generation > 0
            && self.binding.grant_generation > 0
            && self.source_epochs.session > 0
            && self.source_epochs.session == self.output_epochs.session
            && self.source_epochs.stream <= self.output_epochs.stream
            && self
                .total_frames
                .is_none_or(|n| self.source_position_frames <= n)
            && self
                .audible_source_position_frames
                .is_none_or(|n| n <= self.source_position_frames)
            && self
                .consumed_frames
                .is_none_or(|n| n <= self.submitted_frames)
            && [self.input_peak, self.output_peak]
                .into_iter()
                .flatten()
                .all(|n| n.is_finite() && n >= 0.0)
            && self
                .processing
                .as_ref()
                .is_none_or(|p| p.source_boundary.is_none())
            && self.replacement_boundary.as_ref().is_none_or(|boundary| {
                boundary.validate().is_ok()
                    && boundary.binding == self.binding
                    && boundary.source_epochs == self.source_epochs
                    && boundary.source_position_frames <= self.source_position_frames
                    && self
                        .total_frames
                        .is_none_or(|total| boundary.source_position_frames <= total)
                    && boundary.output_epochs.session == self.output_epochs.session
                    && boundary.output_epochs.stream <= self.output_epochs.stream
            })
            && matches!(
                self.consumption_basis.as_str(),
                "unknown"
                    | "backend_consumed"
                    | "backend_delay"
                    | "submitted"
                    | "alsa_status"
                    | "core_audio_rendered"
                    | "alsa_client_estimate"
                    | "callback_delivery_not_dac"
                    | "core_audio_ioproc_delivery"
            )
    }
}

// These borrowed facts are derived for display; the owner's source records retain authority.
pub(crate) struct PlaybackFacts<'a> {
    pub format: PcmFormat,
    pub source_position_frames: u64,
    pub audible_source_position_frames: Option<u64>,
    pub submitted_frames: u64,
    pub consumed_frames: Option<u64>,
    pub backend_delay_frames: Option<u64>,
    pub consumption_basis: &'a str,
    pub input_peak: Option<f64>,
    pub output_peak: Option<f64>,
    pub modified: bool,
    pub backend: Option<&'a BackendObservationView>,
    pub output_activity: Option<&'a OutputActivityView>,
    pub processing: Option<&'a PlaybackProcessingView>,
}
impl super::StateView {
    pub(crate) fn graph_source_available(&self) -> bool {
        use earph_core::network_source::NetworkLifecycle;
        self.session_id.is_some()
            && self.source.is_some()
            && self.playback_facts().is_some()
            && self.network_observation().is_none_or(|p| {
                matches!(
                    p.lifecycle,
                    NetworkLifecycle::Ready | NetworkLifecycle::Playing | NetworkLifecycle::Paused
                )
            })
    }
    pub(crate) fn network_observation(&self) -> Option<&NetworkPlaybackObservationView> {
        use earph_core::control::SourceKind;
        if self.source == Some(SourceKind::HttpFlac)
            || self.source.is_none() && self.playback.is_none()
        {
            self.network_playback.as_ref()
        } else {
            None
        }
    }
    pub(crate) fn playback_facts(&self) -> Option<PlaybackFacts<'_>> {
        if let Some(p) = self.network_observation() {
            return Some(PlaybackFacts {
                format: p.format,
                source_position_frames: p.source_position_frames,
                audible_source_position_frames: p.audible_source_position_frames,
                submitted_frames: p.submitted_frames,
                consumed_frames: p.consumed_frames,
                backend_delay_frames: p.backend_delay_frames,
                consumption_basis: &p.consumption_basis,
                input_peak: p.input_peak,
                output_peak: p.output_peak,
                modified: p.modified,
                backend: p.backend.as_ref(),
                output_activity: p.output_activity.as_ref(),
                processing: p.processing.as_ref(),
            });
        }
        if self.source == Some(earph_core::control::SourceKind::HttpFlac) {
            return None;
        }
        self.playback.as_ref().map(|p| PlaybackFacts {
            format: p.format,
            source_position_frames: p.source_position_frames,
            audible_source_position_frames: p.audible_source_position_frames,
            submitted_frames: p.submitted_frames,
            consumed_frames: p.consumed_frames,
            backend_delay_frames: p.backend_delay_frames,
            consumption_basis: &p.consumption_basis,
            input_peak: p.input_peak,
            output_peak: p.output_peak,
            modified: p.modified,
            backend: p.backend.as_ref(),
            output_activity: p.output_activity.as_ref(),
            processing: p.processing.as_ref(),
        })
    }
}
