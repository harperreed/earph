// ABOUTME: Defines explicit routes, bounded requests, and output observations.
// ABOUTME: Distinguishes client negotiation from downstream hardware proof.
use earph_core::{
    ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    DirectHardware,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenRequest {
    pub schema_version: u32,
    pub device: String,
    pub route: Route,
    pub format: PcmFormat,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

impl OpenRequest {
    pub fn validate(&self) -> Result<(), AlsaError> {
        if self.schema_version != 1 {
            return Err(AlsaError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported ALSA request version",
            ));
        }
        if self.device.is_empty()
            || self.device.len() > 256
            || self.device.trim() != self.device
            || self.device.chars().any(char::is_control)
        {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "select one explicit ALSA endpoint without control characters",
            ));
        }
        if self.route == Route::DirectHardware
            && (!self.device.starts_with("hw:") || self.device.len() == 3)
        {
            return Err(AlsaError::new(
                ErrorCode::FidelityPolicyConflict,
                "direct output requires an explicit hw: endpoint; no fallback is allowed",
            ));
        }
        if self.format.representation() == PcmRepresentation::Float64 {
            return Err(AlsaError::new(
                ErrorCode::FormatUnsupported,
                "this spike accepts signed integer PCM only",
            ));
        }
        if !(1..=8192).contains(&self.period_frames)
            || self.buffer_frames < self.period_frames.saturating_mul(2)
            || self.buffer_frames > 65536
        {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "period must be 1..8192 frames; buffer must hold two periods and at most 65536 frames",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AlsaError {
    pub code: ErrorCode,
    pub message: &'static str,
    pub native_errno: Option<i32>,
}
impl AlsaError {
    pub const fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            native_errno: None,
        }
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn native(code: ErrorCode, message: &'static str, result: i32) -> Self {
        Self {
            code,
            message,
            native_errno: Some(-result),
        }
    }
}
impl std::fmt::Display for AlsaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}: {} (errno {:?})",
            self.code, self.message, self.native_errno
        )
    }
}
impl std::error::Error for AlsaError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Prepared,
    Running,
    Draining,
    Stopped,
    Failed,
}

/// The caller must obtain approval for the selected route before granting this permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalPermission {
    Denied,
    Granted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WriteProgress {
    Accepted { frames: usize },
    WouldBlock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainProgress {
    Complete,
    WouldBlock,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservedFormat {
    pub format: PcmFormat,
    pub alsa_format: String,
    pub access: String,
    pub rate_numerator: u32,
    pub rate_denominator: u32,
    pub significant_bits: u32,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceIdentity {
    pub card_index: i32,
    pub device_index: u32,
    pub subdevice_index: u32,
    pub pcm_id: String,
    pub pcm_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct OpenReport {
    pub schema_version: u32,
    pub requested: OpenRequest,
    pub observed: ObservedFormat,
    pub pcm_type: String,
    pub identity: Option<DeviceIdentity>,
    pub alsa_version: String,
    pub evidence_level: &'static str,
    pub downstream_transformations: &'static str,
    pub payload_preservation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceHint {
    pub device: String,
    pub description: Option<String>,
    pub evidence_level: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct OutputProgress {
    pub state: OutputState,
    pub submitted_frames: u64,
    pub backend_delay_frames: u64,
    pub estimated_consumed_frames: u64,
    pub measurement_boundary: &'static str,
}
