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
    /// The server connection was lost after the server knew this player.
    /// The Host retries it, and this phase lasts until the server knows this player again.
    Reconnecting,
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
/// That delay includes an applied device latency.
/// These are client estimates. They describe no audible position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinTiming {
    pub qualification: SendspinTimingQualification,
    pub correction: SendspinTimingCorrection,
    /// Whether the stream holds an anchor.
    /// A stream start, a full clear, a stream end, an output underrun, a long gap, and idle silence drop it.
    pub anchored: bool,
    /// Times a chunk took an anchor since this source started.
    pub anchors: u64,
    /// Output underruns that this source recovered from since it started.
    pub underruns: u64,
    /// The first error measured after the latest anchor.
    pub start_error_us: Option<i64>,
    pub last_error_us: Option<i64>,
    pub max_abs_error_us: u64,
    /// Silence frames that the scheduler placed before or between chunks.
    pub inserted_silence_frames: u64,
    /// Silence frames that kept the output running while a stream had no audio due.
    /// They grow with waiting time, so they stay apart from the scheduler's bounded silence.
    pub idle_silence_frames: u64,
    /// Received frames that arrived too late and never reached the output.
    pub dropped_frames: u64,
    /// Contiguous chunks that were due more than 20 ms before the output could play them.
    /// The output ran dry or fell behind. The chunk still plays; nothing corrects or bounds the lateness.
    pub late_chunks: u64,
    /// Frames of output time that passed with nothing queued since this source started.
    /// Audio that was due in that time is lost, and no other counter holds it.
    /// `None` means that the output reports no such count: it fails instead, as an ALSA underrun does.
    pub starved_frames: Option<u64>,
    /// Scheduled frames that the output still held at the latest full clear.
    /// The output kept running, so these frames of the cleared audio played on.
    /// `None` means no full clear yet, or an output that reported no held frames.
    /// The device latency lies behind these frames and plays out as well.
    pub clear_held_frames: Option<u64>,
    /// The most frames that a full clear left to play on since this source started.
    pub max_clear_held_frames: u64,
    /// Scheduled frames that the output still held when a stream end or a lost player role released it.
    /// The release dropped them, and no other counter holds them.
    /// Frames behind an unknown hold are not counted.
    pub output_discarded_frames: u64,
    /// The head start of the first chunk after the latest stream start, clear, underrun, long gap, or idle silence.
    /// It is the chunk's local target minus the time the source first saw it. No output delay is subtracted.
    /// The schedule needs more than the backend delay, or it drops the difference as late.
    pub first_lead_us: Option<i64>,
    /// The smallest first lead since this source started.
    pub min_first_lead_us: Option<i64>,
    /// The time from the first sight of that chunk to its first scheduling decision.
    /// The source decides only on a running output, so this includes the output's start time.
    pub first_wait_us: Option<u64>,
    /// The longest first wait since this source started.
    pub max_first_wait_us: u64,
    /// Revision of the clock mapping that the last decision used.
    pub mapping_revision: Option<u64>,
    /// Whether the last decision knew the frames that the output held. Unknown frames count as zero.
    /// A backend without a delay report counts the accepted frames past its restart cursor.
    pub backend_delay_known: bool,
    /// The measurement boundary of the last known backend delay.
    /// It names the device latency when the scheduler adds that latency to the delay.
    pub backend_delay_basis: Option<&'static str>,
    /// The latency that the output device reports beyond the backend delay, in stream frames.
    /// `None` means unknown, and unknown latency counts as zero. It is a device report, not a measurement.
    pub device_latency_frames: Option<u64>,
    /// Whether the scheduler adds the reported device latency to the backend delay.
    /// It refuses a report above its limit, and it has nothing to add to an unknown latency.
    pub device_latency_applied: bool,
}
impl SendspinTiming {
    /// The timing of a source that has scheduled no chunk.
    pub const UNANCHORED: Self = Self {
        qualification: SendspinTimingQualification::Unqualified,
        correction: SendspinTimingCorrection::None,
        anchored: false,
        anchors: 0,
        underruns: 0,
        start_error_us: None,
        last_error_us: None,
        max_abs_error_us: 0,
        inserted_silence_frames: 0,
        idle_silence_frames: 0,
        dropped_frames: 0,
        late_chunks: 0,
        starved_frames: None,
        clear_held_frames: None,
        max_clear_held_frames: 0,
        output_discarded_frames: 0,
        first_lead_us: None,
        min_first_lead_us: None,
        first_wait_us: None,
        max_first_wait_us: 0,
        mapping_revision: None,
        backend_delay_known: false,
        backend_delay_basis: None,
        device_latency_frames: None,
        device_latency_applied: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SendspinSourceObservation {
    pub phase: SendspinTransportPhase,
    /// Frames the output accepted since this source started, including inserted and idle silence.
    /// This is no audible position.
    pub submitted_frames: u64,
    /// Times the Host restored a lost server connection since this session started.
    /// Each reconnect starts a new source, so the other facts count from that reconnect.
    pub reconnects: u64,
    pub timing: SendspinTiming,
}
impl SendspinSourceObservation {
    pub fn new(phase: SendspinTransportPhase, submitted_frames: u64) -> Self {
        Self {
            phase,
            submitted_frames,
            reconnects: 0,
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
                "reconnects": 0,
                "timing": {
                    "qualification": "unqualified",
                    "correction": "none",
                    "anchored": false,
                    "anchors": 0,
                    "underruns": 0,
                    "start_error_us": null,
                    "last_error_us": null,
                    "max_abs_error_us": 0,
                    "inserted_silence_frames": 0,
                    "idle_silence_frames": 0,
                    "dropped_frames": 0,
                    "late_chunks": 0,
                    "starved_frames": null,
                    "clear_held_frames": null,
                    "max_clear_held_frames": 0,
                    "output_discarded_frames": 0,
                    "first_lead_us": null,
                    "min_first_lead_us": null,
                    "first_wait_us": null,
                    "max_first_wait_us": 0,
                    "mapping_revision": null,
                    "backend_delay_known": false,
                    "backend_delay_basis": null,
                    "device_latency_frames": null,
                    "device_latency_applied": false,
                },
            })
        );
        observation.timing = SendspinTiming {
            anchored: true,
            anchors: 2,
            underruns: 1,
            start_error_us: Some(-40),
            last_error_us: Some(125),
            max_abs_error_us: 125,
            late_chunks: 3,
            starved_frames: Some(128),
            clear_held_frames: Some(1440),
            max_clear_held_frames: 1500,
            output_discarded_frames: 37,
            first_lead_us: Some(360_000),
            min_first_lead_us: Some(-12_000),
            first_wait_us: Some(410_000),
            max_first_wait_us: 410_000,
            mapping_revision: Some(3),
            backend_delay_known: true,
            backend_delay_basis: Some("alsa_client_estimate"),
            device_latency_frames: Some(9600),
            device_latency_applied: true,
            ..SendspinTiming::UNANCHORED
        };
        let timing = &serde_json::to_value(observation).unwrap()["timing"];
        assert_eq!(timing["anchors"], 2);
        assert_eq!(timing["underruns"], 1);
        assert_eq!(timing["start_error_us"], -40);
        assert_eq!(timing["last_error_us"], 125);
        assert_eq!(timing["late_chunks"], 3);
        assert_eq!(timing["starved_frames"], 128);
        assert_eq!(timing["clear_held_frames"], 1440);
        assert_eq!(timing["max_clear_held_frames"], 1500);
        assert_eq!(timing["output_discarded_frames"], 37);
        assert_eq!(timing["first_lead_us"], 360_000);
        assert_eq!(timing["min_first_lead_us"], -12_000);
        assert_eq!(timing["first_wait_us"], 410_000);
        assert_eq!(timing["max_first_wait_us"], 410_000);
        assert_eq!(timing["mapping_revision"], 3);
        assert_eq!(timing["backend_delay_basis"], "alsa_client_estimate");
        assert_eq!(timing["device_latency_frames"], 9600);
        assert_eq!(timing["device_latency_applied"], true);
    }
}
