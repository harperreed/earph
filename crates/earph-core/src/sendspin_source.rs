// ABOUTME: Describes the Sendspin player source that the Engine playback worker owns.
// ABOUTME: Labels timing as unqualified and reports submitted frames, never an audible position.
use crate::{ErrorCode, ids::Epochs};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendspinTransportPhase {
    /// The output is open. The transport has not yet sent an available player state.
    Preparing,
    /// The server knows this player is available. No stream is active.
    AwaitingStream,
    /// A stream is active. Received audio goes to the output.
    Streaming,
    /// The source released its output and cancelled its connection.
    Closed,
}

/// No physical measurement backs Sendspin timing, so this type has one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendspinTimingQualification {
    Unqualified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinTiming {
    pub qualification: SendspinTimingQualification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinSourceObservation {
    pub phase: SendspinTransportPhase,
    /// Frames the output accepted since this source started. This is no audible position.
    pub submitted_frames: u64,
    pub timing: SendspinTiming,
}
impl SendspinSourceObservation {
    pub fn new(phase: SendspinTransportPhase, submitted_frames: u64) -> Self {
        Self {
            phase,
            submitted_frames,
            timing: SendspinTiming {
                qualification: SendspinTimingQualification::Unqualified,
            },
        }
    }
}

/// Reports a phase change, a refused control, or the reason the source closed.
/// An error with an open phase means the source refused a control and still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinSourceEvent {
    pub epochs: Epochs,
    pub observation: SendspinSourceObservation,
    pub error: Option<ErrorCode>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observation_reports_unqualified_timing_and_no_audible_position() {
        let observation =
            SendspinSourceObservation::new(SendspinTransportPhase::AwaitingStream, 48000);
        assert_eq!(
            serde_json::to_value(observation).unwrap(),
            serde_json::json!({
                "phase": "awaiting_stream",
                "submitted_frames": 48000,
                "timing": {"qualification": "unqualified"},
            })
        );
    }
}
