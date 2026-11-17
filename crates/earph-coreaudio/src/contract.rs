// ABOUTME: Compares explicit integer PCM requests with observed Core Audio settings.
// ABOUTME: Keeps format matches separate from ownership, opening, and hardware proof.
use earph_core::{
    ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrecisionRequest {
    pub schema_version: u32,
    pub device_uid: String,
    pub format: PcmFormat,
    pub buffer_frames: u32,
}
impl PrecisionRequest {
    pub fn validate(&self) -> Result<(), CoreAudioError> {
        if self.schema_version != 1 {
            return Err(CoreAudioError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported Core Audio request version",
            ));
        }
        if self.device_uid.is_empty()
            || self.device_uid.len() > 1024
            || self.device_uid.trim() != self.device_uid
            || self.device_uid.chars().any(char::is_control)
        {
            return Err(CoreAudioError::new(
                ErrorCode::AssetInvalid,
                "select one explicit device UID without control characters",
            ));
        }
        if self.format.representation() == PcmRepresentation::Float64 {
            return Err(CoreAudioError::new(
                ErrorCode::FormatUnsupported,
                "precision assessment accepts signed integer PCM only",
            ));
        }
        if !(1..=8192).contains(&self.buffer_frames) {
            return Err(CoreAudioError::new(
                ErrorCode::ResourceBudgetExceeded,
                "buffer must contain 1..8192 frames",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamFormat {
    pub sample_rate_hz: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
}
impl StreamFormat {
    fn matches(&self, format: PcmFormat) -> bool {
        // Core Audio defines signed, packed PCM as flags 2 and 3. Bit 6 marks nonmixable data.
        // Reject every other flag, including float, big-endian, alignment, planar, and unknown extensions.
        self.format_id == u32::from_be_bytes(*b"lpcm")
            && self.format_flags & !64 == 12
            && self.sample_rate_hz == f64::from(format.sample_rate_hz())
            && self.channels_per_frame == format.channels().count() as u32
            && self.bits_per_channel == format.representation().bytes() as u32 * 8
            && self.bytes_per_frame == format.payload_bytes(1).unwrap_or(0) as u32
            && self.frames_per_packet == 1
            && self.bytes_per_packet == self.bytes_per_frame
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValueRange {
    pub minimum: f64,
    pub maximum: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangedFormat {
    pub format: StreamFormat,
    pub sample_rates: ValueRange,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamSnapshot {
    pub object_id: u32,
    pub physical: StreamFormat,
    pub virtual_format: StreamFormat,
    pub physical_settable: bool,
    pub virtual_settable: bool,
    pub available_physical: Vec<RangedFormat>,
    pub available_virtual: Vec<RangedFormat>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    pub object_id: u32,
    pub uid: String,
    pub name: String,
    pub alive: bool,
    pub nominal_rate_hz: f64,
    pub nominal_rate_settable: bool,
    pub buffer_frames: u32,
    pub buffer_frame_range: ValueRange,
    pub hog_owner_pid: Option<i32>,
    pub hog_mode_settable: Option<bool>,
    pub output_streams: Vec<StreamSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrecisionAssessment {
    pub requested: PrecisionRequest,
    pub observed: DeviceSnapshot,
    pub matches_current_format: bool,
    pub conflicts: Vec<&'static str>,
    pub ownership: &'static str,
    pub evidence_level: &'static str,
    pub payload_preservation: &'static str,
    pub opened: bool,
}
pub fn assess(
    request: &PrecisionRequest,
    device: &DeviceSnapshot,
) -> Result<PrecisionAssessment, CoreAudioError> {
    request.validate()?;
    if request.device_uid != device.uid {
        return Err(CoreAudioError::new(
            ErrorCode::DeviceUnavailable,
            "observed UID differs from the selected device",
        ));
    }
    let mut conflicts = Vec::new();
    if !device.alive {
        conflicts.push("selected device is not alive");
    }
    if device.nominal_rate_hz != f64::from(request.format.sample_rate_hz()) {
        conflicts.push("nominal sample rate differs");
    }
    if device.buffer_frames != request.buffer_frames {
        conflicts.push("buffer frame size differs");
    }
    if device.output_streams.len() != 1 {
        conflicts.push("precision spike requires exactly one output stream");
    } else {
        let stream = &device.output_streams[0];
        if !stream.physical.matches(request.format) {
            conflicts.push("physical stream format differs from exact integer PCM");
        }
        if !stream.virtual_format.matches(request.format) {
            conflicts.push("virtual stream format differs from exact integer PCM");
        }
    }
    Ok(PrecisionAssessment {
        requested: request.clone(),
        observed: device.clone(),
        matches_current_format: conflicts.is_empty(),
        conflicts,
        ownership: match (device.hog_owner_pid, device.hog_mode_settable) {
            (Some(pid), _) if pid >= 0 => "owned_by_process",
            (Some(-1), Some(true)) => "available_not_acquired",
            (Some(-1), Some(false)) => "not_acquirable",
            _ => "unknown",
        },
        evidence_level: "observed_current_settings",
        payload_preservation: "unverified",
        opened: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CoreAudioError {
    pub code: ErrorCode,
    pub message: &'static str,
    pub os_status: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct DeviceFailure {
    pub object_id: u32,
    pub error: CoreAudioError,
}
#[derive(Debug, Serialize)]
pub struct DeviceInventory {
    pub devices: Vec<DeviceSnapshot>,
    pub failures: Vec<DeviceFailure>,
}
#[derive(Debug, Serialize)]
pub struct RegistrationReport {
    pub device: DeviceSnapshot,
    pub evidence_level: &'static str,
    pub signals_submitted: u64,
}
impl CoreAudioError {
    pub const fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            os_status: None,
        }
    }
    #[cfg(target_os = "macos")]
    pub(crate) fn native(message: &'static str, status: i32) -> Self {
        Self {
            code: ErrorCode::DeviceUnavailable,
            message,
            os_status: Some(status),
        }
    }
}
impl std::fmt::Display for CoreAudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:?}: {} (OSStatus {:?})",
            self.code, self.message, self.os_status
        )
    }
}
impl std::error::Error for CoreAudioError {}
