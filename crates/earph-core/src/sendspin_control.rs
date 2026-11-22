// ABOUTME: Defines the control-surface facts for the one Sendspin player connection.
// ABOUTME: Keeps the connection fence apart from session state so retirement can outlive a session.
use crate::{
    ErrorCode,
    ids::{Epochs, SessionId},
    sendspin_source::{SendspinSourceObservation, SendspinTransportPhase},
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendspinPhase {
    /// No connection exists. A start is possible.
    Disconnected,
    /// The session exists. The source has not opened its output yet.
    Connecting,
    Preparing,
    AwaitingStream,
    Streaming,
    /// The session released the connection. The Host still retires its workers.
    Closing,
}

/// The fences that every event from the current connection must carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendspinConnection {
    pub generation: u64,
    /// The session epochs at start. The source keeps them until it closes.
    pub epochs: Epochs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SendspinSnapshot {
    /// The owner was launched with a Sendspin server configuration.
    pub configured: bool,
    pub phase: SendspinPhase,
    /// Counts starts in this owner. Zero means no start happened.
    pub connection_generation: u64,
    pub session_id: Option<SessionId>,
    pub observation: Option<SendspinSourceObservation>,
    /// Why the latest connection failed. A new start clears it.
    pub error: Option<ErrorCode>,
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveConnection {
    pub fence: SendspinConnection,
    pub session_id: SessionId,
    /// False after the session released this connection.
    pub open: bool,
    /// True after the server learned that this player is available.
    pub prepared: bool,
    pub observation: Option<SendspinSourceObservation>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SendspinControl {
    pub configured: bool,
    pub generation: u64,
    pub connection: Option<ActiveConnection>,
    pub error: Option<ErrorCode>,
}
impl SendspinControl {
    pub fn phase(&self) -> SendspinPhase {
        let Some(connection) = &self.connection else {
            return SendspinPhase::Disconnected;
        };
        if !connection.open {
            return SendspinPhase::Closing;
        }
        match connection.observation.map(|observation| observation.phase) {
            None => SendspinPhase::Connecting,
            Some(SendspinTransportPhase::Preparing) => SendspinPhase::Preparing,
            Some(SendspinTransportPhase::AwaitingStream) => SendspinPhase::AwaitingStream,
            Some(SendspinTransportPhase::Streaming) => SendspinPhase::Streaming,
            Some(SendspinTransportPhase::Closed) => SendspinPhase::Closing,
        }
    }
    pub fn snapshot(&self) -> SendspinSnapshot {
        SendspinSnapshot {
            configured: self.configured,
            phase: self.phase(),
            connection_generation: self.generation,
            session_id: self
                .connection
                .as_ref()
                .filter(|connection| connection.open)
                .map(|connection| connection.session_id.clone()),
            observation: self
                .connection
                .as_ref()
                .and_then(|connection| connection.observation),
            error: self.error,
        }
    }
}
