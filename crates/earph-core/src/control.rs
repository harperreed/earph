// ABOUTME: Defines one versioned command and receipt contract for all controllers.
// ABOUTME: Rejects unsupported features before the session can accept work.
use crate::{EngineError, ErrorCode, ids::*};
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "capabilities.get")]
    Capabilities,
    #[serde(rename = "state.get")]
    State,
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
        matches!(self, Self::Capabilities | Self::State)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub request_id: RequestId,
    pub zone_id: ZoneId,
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
    State(crate::session::SessionSnapshot),
    Capabilities(Capabilities),
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
