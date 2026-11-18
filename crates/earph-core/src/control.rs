// ABOUTME: Defines one versioned command and receipt contract for all controllers.
// ABOUTME: Rejects unsupported features before the session can accept work.
use crate::{
    EngineError, ErrorCode,
    ids::*,
    safety::{FidelityPolicy, VolumeMode},
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub struct ControlLimits {
    pub(crate) message_bytes: usize,
    pub(crate) receipts: usize,
    pub(crate) events: usize,
}
impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            message_bytes: 1048576,
            receipts: 1024,
            events: 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Virtual,
    LocalFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputBackend {
    Alsa,
    CoreAudio,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputRoute {
    System,
    DirectHardware,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfiguration {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub endpoint: String,
    pub device_id: DeviceId,
    pub profile_id: ProfileId,
    pub period_frames: u32,
    pub buffer_frames: u32,
    pub volume_mode: VolumeMode,
    pub fidelity: FidelityPolicy,
}
impl OutputConfiguration {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.endpoint.is_empty()
            || self.endpoint.len() > 256
            || self.endpoint.trim() != self.endpoint
            || self.endpoint.chars().any(char::is_control)
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "select one explicit output endpoint",
            ));
        }
        if !(1..=8192).contains(&self.period_frames)
            || self.buffer_frames < self.period_frames.saturating_mul(2)
            || self.buffer_frames > 65536
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "invalid output period or buffer bound",
            ));
        }
        if self.backend == OutputBackend::Alsa
            && self.route == OutputRoute::DirectHardware
            && (!self.endpoint.starts_with("hw:") || self.endpoint.len() == 3)
        {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "direct ALSA output requires an explicit hw: endpoint",
            ));
        }
        if self.volume_mode == VolumeMode::Hardware {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "hardware volume set and readback are not implemented",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "capabilities.get")]
    Capabilities,
    #[serde(rename = "state.get")]
    State,
    #[serde(rename = "queue.get")]
    QueueGet { offset: usize, limit: usize },
    #[serde(rename = "queue.append")]
    QueueAppend { path: String },
    #[serde(rename = "queue.append_directory")]
    QueueAppendDirectory { path: String },
    #[serde(rename = "queue.remove")]
    QueueRemove { id: QueueEntryId },
    #[serde(rename = "queue.reorder")]
    QueueMove { id: QueueEntryId, new_index: usize },
    #[serde(rename = "queue.select")]
    QueueSelect { id: Option<QueueEntryId> },
    #[serde(rename = "owner.shutdown")]
    OwnerShutdown,
    #[serde(rename = "output.select")]
    OutputSelect { output: OutputConfiguration },
    #[serde(rename = "output.consent")]
    OutputConsent {
        device_id: DeviceId,
        profile_id: ProfileId,
    },
    #[serde(rename = "volume.set")]
    VolumeSet {
        millidecibels: i32,
        ramp_frames: u32,
    },
    #[serde(rename = "volume.caps.set")]
    VolumeCaps {
        software_millidecibels: i32,
        hardware_millidecibels: Option<i32>,
    },
    #[serde(rename = "volume.arm_fixed")]
    FixedArm {
        device_id: DeviceId,
        profile_id: ProfileId,
    },
    #[serde(rename = "sessions.start")]
    Start {
        source: SourceKind,
        device_id: DeviceId,
        takeover: bool,
        seekable: bool,
    },
    #[serde(rename = "sessions.stop")]
    Stop,
    #[serde(rename = "sessions.pause")]
    Pause,
    #[serde(rename = "sessions.resume")]
    Resume,
    #[serde(rename = "sessions.seek")]
    Seek { position_frames: u64 },
    #[serde(rename = "mute.set")]
    Mute { muted: bool },
    #[serde(other)]
    Unsupported,
}
impl Command {
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Capabilities | Self::State | Self::QueueGet { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub request_id: RequestId,
    pub zone_id: ZoneId,
    #[serde(default)]
    pub owner_id: Option<OwnerId>,
    pub session_id: Option<SessionId>,
    pub expected_revision: Option<u64>,
    pub payload: Command,
}
impl CommandEnvelope {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 || matches!(self.payload, Command::Unsupported) {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported command version or method",
            ));
        }
        if let Command::QueueAppend { path } | Command::QueueAppendDirectory { path } =
            &self.payload
        {
            crate::queue::validate_path(path)?;
        }
        if let Command::OutputSelect { output } = &self.payload {
            output.validate()?;
        }
        Ok(())
    }
}
pub fn decode_command(
    bytes: &[u8],
    limits: &ControlLimits,
) -> Result<CommandEnvelope, EngineError> {
    if bytes.len() > limits.message_bytes {
        return Err(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "command exceeds byte limit",
        ));
    }
    let command: CommandEnvelope = serde_json::from_slice(bytes)
        .map_err(|_| EngineError::new(ErrorCode::AssetInvalid, "invalid command document"))?;
    command.validate()?;
    Ok(command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    Applied,
    Rejected,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum QueryResult {
    State(Box<crate::session::SessionSnapshot>),
    Capabilities(Capabilities),
    Queue(crate::queue::QueuePage),
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capabilities {
    pub schema_version: u32,
    pub sources: Vec<SourceKind>,
    pub physical_output: bool,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Receipt {
    pub request_id: RequestId,
    pub zone_id: ZoneId,
    pub session_id: Option<SessionId>,
    pub status: ReceiptStatus,
    pub error: Option<EngineError>,
    pub revision: u64,
    pub epochs: Epochs,
    pub activation_frame: Option<u64>,
    pub result: Option<QueryResult>,
}
