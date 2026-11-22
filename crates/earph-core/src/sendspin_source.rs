// ABOUTME: Describes the Sendspin player source that the Engine playback worker owns.
// ABOUTME: Reports unqualified scheduling facts and submitted frames, never an audible position.
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

/// This player measures its timing error and applies no drift correction, so this type has one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendspinTimingCorrection {
    None,
}

/// Scheduling facts against the server timeline. Every error is `(write time + backend delay) - target`.
/// These are client estimates. They describe no audible position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinTiming {
    pub qualification: SendspinTimingQualification,
    pub correction: SendspinTimingCorrection,
    /// Whether the stream holds an anchor. A stream start, a stream end, and a long gap drop it.
    pub anchored: bool,
    /// The first error measured after the latest anchor.
    pub start_error_us: Option<i64>,
    pub last_error_us: Option<i64>,
    pub max_abs_error_us: u64,
    /// Silence frames that the scheduler placed before or between chunks.
    pub inserted_silence_frames: u64,
    /// Received frames that arrived too late and never reached the output.
    pub dropped_frames: u64,
    /// Revision of the clock mapping that the last decision used.
    pub mapping_revision: Option<u64>,
    /// Whether the last decision knew the backend delay. An unknown delay counts as zero.
    pub backend_delay_known: bool,
    /// The measurement boundary of the last known backend delay.
    pub backend_delay_basis: Option<&'static str>,
}
impl SendspinTiming {
    /// The timing of a source that has scheduled no chunk.
    pub const UNANCHORED: Self = Self {
        qualification: SendspinTimingQualification::Unqualified,
        correction: SendspinTimingCorrection::None,
        anchored: false,
        start_error_us: None,
        last_error_us: None,
        max_abs_error_us: 0,
        inserted_silence_frames: 0,
        dropped_frames: 0,
        mapping_revision: None,
        backend_delay_known: false,
        backend_delay_basis: None,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinSourceObservation {
    pub phase: SendspinTransportPhase,
    /// Frames the output accepted since this source started, including inserted silence.
    /// This is no audible position.
    pub submitted_frames: u64,
    pub timing: SendspinTiming,
}
impl SendspinSourceObservation {
    pub fn new(phase: SendspinTransportPhase, submitted_frames: u64) -> Self {
        Self {
            phase,
            submitted_frames,
            timing: SendspinTiming::UNANCHORED,
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
    fn observation_reports_unqualified_uncorrected_timing_and_no_audible_position() {
        let mut observation =
            SendspinSourceObservation::new(SendspinTransportPhase::AwaitingStream, 48000);
        assert_eq!(
            serde_json::to_value(observation).unwrap(),
            serde_json::json!({
                "phase": "awaiting_stream",
                "submitted_frames": 48000,
                "timing": {
                    "qualification": "unqualified",
                    "correction": "none",
                    "anchored": false,
                    "start_error_us": null,
                    "last_error_us": null,
                    "max_abs_error_us": 0,
                    "inserted_silence_frames": 0,
                    "dropped_frames": 0,
                    "mapping_revision": null,
                    "backend_delay_known": false,
                    "backend_delay_basis": null,
                },
            })
        );
        observation.timing = SendspinTiming {
            anchored: true,
            start_error_us: Some(-40),
            last_error_us: Some(125),
            max_abs_error_us: 125,
            mapping_revision: Some(3),
            backend_delay_known: true,
            backend_delay_basis: Some("alsa_client_estimate"),
            ..SendspinTiming::UNANCHORED
        };
        let timing = &serde_json::to_value(observation).unwrap()["timing"];
        assert_eq!(timing["start_error_us"], -40);
        assert_eq!(timing["last_error_us"], 125);
        assert_eq!(timing["mapping_revision"], 3);
        assert_eq!(timing["backend_delay_basis"], "alsa_client_estimate");
    }
}
