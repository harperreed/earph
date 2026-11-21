// ABOUTME: Defines bounded volatile HTTP source authority and measured playback records.
// ABOUTME: Keeps raw origin grants and descriptors out of generic wire observations and Debug.
use crate::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    control::Command,
    ids::*,
    session::{BackendObservation, OutputActivity, PlaybackProcessingObservation},
    source::StagedUriDescriptor,
};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc};

pub const MAX_NETWORK_AUTHORITY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NETWORK_LIFETIME_MS: u64 = 3_600_000;
pub(crate) const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "network authority exceeds storage limits",
);
pub(crate) const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid network source facts");

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkOriginGrant {
    pub origin: String,
    pub pins: Vec<IpAddr>,
    pub roots_der: Vec<Vec<u8>>,
}
impl std::fmt::Debug for NetworkOriginGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NetworkOriginGrant([redacted])")
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkGrantDocument {
    pub origins: Vec<NetworkOriginGrant>,
}
impl std::fmt::Debug for NetworkGrantDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NetworkGrantDocument([redacted])")
    }
}
impl NetworkGrantDocument {
    pub fn retained_bytes(&self) -> Result<usize, EngineError> {
        if self.origins.capacity() > 8 {
            return Err(BUDGET);
        }
        let mut bytes =
            size_of::<Self>() + 16 + self.origins.capacity() * size_of::<NetworkOriginGrant>();
        for origin in &self.origins {
            if origin.origin.capacity() > 8192
                || origin.pins.capacity() > 8
                || origin.roots_der.capacity() > 64
            {
                return Err(BUDGET);
            }
            let root_bytes = origin
                .roots_der
                .iter()
                .try_fold(0_usize, |n, root| n.checked_add(root.capacity()))
                .ok_or(BUDGET)?;
            if root_bytes > 1024 * 1024 {
                return Err(BUDGET);
            }
            bytes = bytes
                .checked_add(
                    origin.origin.capacity()
                        + origin.pins.capacity() * size_of::<IpAddr>()
                        + origin.roots_der.capacity() * size_of::<Vec<u8>>()
                        + root_bytes,
                )
                .ok_or(BUDGET)?;
        }
        Ok(bytes)
    }
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        self.retained_bytes()?;
        if self.origins.is_empty()
            || self
                .origins
                .iter()
                .any(|g| g.origin.is_empty() || g.pins.is_empty())
        {
            return Err(INVALID);
        }
        Ok(())
    }
}

impl crate::control::CommandEnvelope {
    pub(crate) fn network_capacity_bytes(&self) -> Result<usize, EngineError> {
        if !self.payload.is_network()
            && !self.payload.is_renderer()
            && !matches!(self.payload, Command::SourceStageUri { .. })
        {
            return Ok(0);
        }
        let ids = [
            self.request_id.capacity_bytes(),
            self.zone_id.capacity_bytes(),
            self.owner_id.as_ref().map_or(0, OwnerId::capacity_bytes),
            self.session_id
                .as_ref()
                .map_or(0, SessionId::capacity_bytes),
        ];
        if ids.iter().any(|n| *n > 128) {
            return Err(BUDGET);
        }
        // Include the stored receipt, its cloned identities, and both snapshot bindings.
        let mut bytes = size_of::<(Self, crate::control::Receipt)>()
            + 2 * size_of::<usize>()
            + ids.iter().sum::<usize>()
            + ids[0]
            + ids[1]
            + ids[3]
            + 6 * 128;
        if self.payload.is_renderer() {
            bytes += crate::renderer::MAX_RENDERER_CONTROLLERS
                * (size_of::<crate::renderer::RendererControllerSummary>() + 128)
                + 4 * 128;
        }
        match &self.payload {
            Command::SourceStageUri { uri, metadata } => {
                crate::source::validate_storage(uri, metadata)?;
                bytes += uri.capacity() + metadata.capacity();
            }
            Command::NetworkGrantsReplace { grants, .. } => {
                bytes += grants.retained_bytes()?;
            }
            Command::RendererGrantsReplace { grants, .. } => {
                bytes += grants.retained_bytes()?;
            }
            Command::RendererCallbackGrantsReplace {
                controller_id,
                grants,
                ..
            } => {
                if controller_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                bytes += controller_id.capacity_bytes() + grants.retained_bytes()?;
            }
            Command::RendererSubscription {
                controller_id,
                action,
                ..
            } => {
                if controller_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                bytes += controller_id.capacity_bytes() + action.retained_bytes()?;
            }
            Command::RendererGrantsRevoke { controller_id, .. }
            | Command::RendererCallbackGrantsRevoke { controller_id, .. } => {
                if controller_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                bytes += controller_id.capacity_bytes();
            }
            Command::RendererRequest {
                controller_id,
                action,
                ..
            } => {
                if controller_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                bytes += controller_id.capacity_bytes() + action.retained_bytes()?;
            }
            Command::NetworkStartHttpFlac {
                media_id,
                device_id,
                ..
            } => {
                if media_id.capacity_bytes() > 128 || device_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                bytes += media_id.capacity_bytes() + device_id.capacity_bytes();
            }
            _ => {}
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSourceBinding {
    pub owner_id: OwnerId,
    pub session_id: SessionId,
    pub media_id: StagedMediaId,
    #[serde(deserialize_with = "nonzero_generation")]
    pub source_generation: u64,
    #[serde(deserialize_with = "nonzero_generation")]
    pub grant_generation: u64,
}
impl NetworkSourceBinding {
    pub fn validate(&self) -> Result<(), EngineError> {
        if [
            self.owner_id.capacity_bytes(),
            self.session_id.capacity_bytes(),
            self.media_id.capacity_bytes(),
        ]
        .into_iter()
        .any(|n| n > 128)
        {
            return Err(BUDGET);
        }
        if self.source_generation == 0 || self.grant_generation == 0 {
            return Err(INVALID);
        }
        Ok(())
    }
}
fn nonzero_generation<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let generation = u64::deserialize(deserializer)?;
    if generation == 0 {
        return Err(serde::de::Error::custom(
            "network generation must be nonzero",
        ));
    }
    Ok(generation)
}
#[derive(Debug, Clone)]
pub struct NetworkSourceContext {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub descriptor: Arc<StagedUriDescriptor>,
    pub grants: Arc<NetworkGrantDocument>,
    pub expires_monotonic_ns: u64,
}
#[derive(Debug, Clone)]
pub struct NetworkClosingSource {
    pub context: Arc<NetworkSourceContext>,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub reason: Option<ErrorCode>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkLifecycle {
    Preparing,
    Ready,
    Playing,
    Paused,
    Retiring,
    Failed,
    Completed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub enabled: bool,
    pub grant_generation: u64,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub expires_monotonic_ns: Option<u64>,
    pub origin_count: usize,
    pub source_generation: u64,
    pub retained_bytes: u64,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub lifecycle: Option<NetworkLifecycle>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub active: Option<NetworkSourceBinding>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub closing: Option<NetworkSourceBinding>,
    pub seek: bool,
    pub profiles: bool,
    pub strict_preservation: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkReplacementBoundary {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub source_position_frames: u64,
    pub previous_output_epochs: Epochs,
    pub output_epochs: Epochs,
    pub previous_output_format: PcmFormat,
    pub discarded_output_frames: u64,
}
impl NetworkReplacementBoundary {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.binding.validate()?;
        validate_replacement_facts(
            self.source_epochs,
            self.previous_output_epochs,
            self.output_epochs,
            self.previous_output_format,
            self.discarded_output_frames,
        )
    }
}
pub(crate) fn validate_replacement_facts(
    source: Epochs,
    previous: Epochs,
    output: Epochs,
    previous_format: PcmFormat,
    discarded: u64,
) -> Result<(), EngineError> {
    if source.session == 0
        || source.session != previous.session
        || source.session != output.session
        || source.stream > previous.stream
        || previous.stream >= output.stream
        || discarded == 0
    {
        return Err(INVALID);
    }
    crate::audio::MemoryBudget::default().admit(previous_format.storage_bytes(discarded)?)
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NetworkPlaybackObservation {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub lifecycle: NetworkLifecycle,
    pub format: PcmFormat,
    pub output_format: PcmFormat,
    pub total_frames: Option<u64>,
    pub source_position_frames: u64,
    pub audible_source_position_frames: Option<u64>,
    pub submitted_frames: u64,
    pub consumed_frames: Option<u64>,
    pub backend_delay_frames: Option<u64>,
    pub consumption_basis: &'static str,
    pub input_peak: Option<f64>,
    pub output_peak: Option<f64>,
    pub modified: bool,
    pub backend: Option<BackendObservation>,
    pub output_activity: Option<OutputActivity>,
    pub processing: Option<PlaybackProcessingObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_boundary: Option<Arc<NetworkReplacementBoundary>>,
}
impl NetworkPlaybackObservation {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.binding.validate()?;
        if let Some(boundary) = &self.replacement_boundary {
            boundary.validate()?;
            if boundary.binding != self.binding
                || boundary.source_epochs != self.source_epochs
                || boundary.source_position_frames > self.source_position_frames
                || boundary.output_epochs.stream > self.output_epochs.stream
            {
                return Err(INVALID);
            }
        }
        if self.source_epochs.session == 0
            || self.source_epochs.session != self.output_epochs.session
            || self.source_epochs.stream > self.output_epochs.stream
            || self
                .total_frames
                .is_some_and(|n| self.source_position_frames > n)
            || self
                .audible_source_position_frames
                .is_some_and(|n| n > self.source_position_frames)
            || self
                .consumed_frames
                .is_some_and(|n| n > self.submitted_frames)
            || [self.input_peak, self.output_peak]
                .into_iter()
                .flatten()
                .any(|n| !n.is_finite() || n < 0.0)
            || self
                .processing
                .as_ref()
                .is_some_and(|p| p.source_boundary.is_some())
            || !matches!(
                self.consumption_basis,
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
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct NetworkEvent {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub monotonic_ns: u64,
    pub kind: NetworkEventKind,
}
#[derive(Debug, Clone)]
pub enum NetworkEventKind {
    Prepared {
        observation: NetworkPlaybackObservation,
    },
    Progress {
        observation: NetworkPlaybackObservation,
    },
    Paused {
        observation: NetworkPlaybackObservation,
    },
    Started {
        activation_frame: u64,
    },
    Completed {
        observation: NetworkPlaybackObservation,
    },
    Failed {
        code: ErrorCode,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkExecutionFence {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
}
#[derive(Clone)]
pub(crate) struct NetworkActivationBounds {
    pub request_id: RequestId,
    pub previous_output_epochs: Epochs,
    pub activation_output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
}
impl NetworkActivationBounds {
    pub(crate) fn admits(&self, actual: &NetworkExecutionFence) -> bool {
        actual.device_epoch == self.device_epoch
            && (actual.output_epochs == self.previous_output_epochs
                && actual.graph_epoch == self.graph_epoch
                || actual.output_epochs == self.activation_output_epochs
                    && (actual.graph_epoch == self.graph_epoch
                        || self.graph_epoch.checked_add(1) == Some(actual.graph_epoch)))
    }
}
#[derive(Debug, Clone)]
pub struct NetworkRetirementAck {
    pub binding: NetworkSourceBinding,
    pub source_epochs: Epochs,
    pub output_epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub observation: Option<NetworkPlaybackObservation>,
}
#[derive(Debug, Clone)]
pub struct NetworkGraphActivationAck {
    pub request_id: RequestId,
    pub epochs: Epochs,
    pub graph_hash: String,
    pub activation_frame: u64,
    pub observation: NetworkPlaybackObservation,
}
#[derive(Clone, Default)]
pub(crate) struct NetworkSources {
    pub enabled: bool,
    pub generation: u64,
    pub source_generation: u64,
    pub grants: Option<(u64, Arc<NetworkGrantDocument>)>,
    pub active: Option<Arc<NetworkSourceContext>>,
    pub closing: Option<NetworkClosingSource>,
    pub closing_failure_observed: bool,
    pub closing_activation: Option<NetworkActivationBounds>,
    pub closing_frontier: Option<Epochs>,
    pub admission_ns: u64,
}
