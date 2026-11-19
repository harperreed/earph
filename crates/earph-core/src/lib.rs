// ABOUTME: Owns the audio player's stable domain and control contracts.
// ABOUTME: Keeps platform handles and adapter types outside the core.
#![forbid(unsafe_code)]
pub mod audio;
pub mod control;
pub mod devices;
pub mod dsp_nodes;
pub mod gain;
pub mod graph;
pub mod graph_control;
pub mod ids;
pub mod profile_control;
pub mod profile_packages;
pub mod profiles;
pub mod queue;
pub mod safety;
pub mod session;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    CapabilityUnsupported,
    DeviceUnavailable,
    FormatUnsupported,
    FidelityPolicyConflict,
    SourceBusy,
    StaleRevision,
    StaleSession,
    GraphInvalid,
    ResourceBudgetExceeded,
    AssetInvalid,
    SafetyPolicyBlocked,
    AuthRequired,
    AuthDenied,
    OperationCancelled,
    OutputFault,
    InternalFault,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EngineError {
    pub code: ErrorCode,
    pub message: &'static str,
}
impl EngineError {
    pub const fn new(code: ErrorCode, message: &'static str) -> Self {
        Self { code, message }
    }
}
impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for EngineError {}
