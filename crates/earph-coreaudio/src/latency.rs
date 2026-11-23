// ABOUTME: Holds the latency that a Core Audio output device reports about itself.
// ABOUTME: Converts those frames to a client rate and claims no audible position.
use serde::Serialize;

/// The latency properties of one output device and one of its output streams.
/// Every count is in frames at `nominal_rate_hz`.
/// `None` means that the device has no such property or that the read failed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct OutputLatency {
    pub nominal_rate_hz: f64,
    /// `kAudioDevicePropertyLatency` in the output scope.
    pub device_frames: Option<u32>,
    /// `kAudioDevicePropertySafetyOffset` in the output scope.
    pub safety_offset_frames: Option<u32>,
    /// `kAudioStreamPropertyLatency` of the output stream.
    pub stream_frames: Option<u32>,
    /// `kAudioDevicePropertyBufferFrameSize`: the frames in one device IO buffer.
    pub io_buffer_frames: u32,
}
impl OutputLatency {
    /// Returns the reported latency in frames at `client_rate_hz`, rounded to the nearest frame.
    /// An unreadable property counts as zero. `None` means that no latency property was readable.
    /// Pass `with_io_buffer` when the caller's own delay stops before the device IO buffer.
    pub fn frames_at(&self, client_rate_hz: u32, with_io_buffer: bool) -> Option<u64> {
        let reported = [
            self.device_frames,
            self.safety_offset_frames,
            self.stream_frames,
        ];
        if reported.iter().all(Option::is_none)
            || !self.nominal_rate_hz.is_finite()
            || self.nominal_rate_hz < 1.0
        {
            return None;
        }
        let buffer = with_io_buffer.then_some(self.io_buffer_frames);
        let frames: u64 = reported
            .into_iter()
            .chain([buffer])
            .flatten()
            .map(u64::from)
            .sum();
        // Four `u32` counts stay exact in an `f64`. The cast saturates.
        Some((frames as f64 * f64::from(client_rate_hz) / self.nominal_rate_hz).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn latency() -> OutputLatency {
        OutputLatency {
            nominal_rate_hz: 48000.0,
            device_frames: Some(300),
            safety_offset_frames: Some(100),
            stream_frames: Some(80),
            io_buffer_frames: 512,
        }
    }

    #[test]
    fn reported_frames_add_up_and_the_io_buffer_counts_only_on_request() {
        assert_eq!(latency().frames_at(48000, false), Some(480));
        assert_eq!(latency().frames_at(48000, true), Some(992));
    }
    #[test]
    fn device_rate_frames_convert_to_the_client_rate() {
        let device = OutputLatency {
            nominal_rate_hz: 44100.0,
            device_frames: Some(441),
            safety_offset_frames: None,
            stream_frames: None,
            io_buffer_frames: 512,
        };
        // 441 frames at 44.1 kHz last 10 ms: 480 frames at 48 kHz.
        assert_eq!(device.frames_at(48000, false), Some(480));
        // 442 frames are 481.09 client frames, and 443 frames are 482.18.
        let odd = |frames| OutputLatency {
            device_frames: Some(frames),
            ..device
        };
        assert_eq!(odd(442).frames_at(48000, false), Some(481));
        assert_eq!(odd(443).frames_at(48000, false), Some(482));
    }
    #[test]
    fn an_unreadable_property_counts_as_zero_and_no_readable_property_is_unknown() {
        let partial = OutputLatency {
            safety_offset_frames: None,
            ..latency()
        };
        assert_eq!(partial.frames_at(48000, false), Some(380));
        let unknown = OutputLatency {
            device_frames: None,
            safety_offset_frames: None,
            stream_frames: None,
            ..latency()
        };
        assert_eq!(unknown.frames_at(48000, false), None);
        // The IO buffer alone is no latency report.
        assert_eq!(unknown.frames_at(48000, true), None);
        let zero = OutputLatency {
            device_frames: Some(0),
            safety_offset_frames: Some(0),
            stream_frames: Some(0),
            ..latency()
        };
        assert_eq!(zero.frames_at(48000, false), Some(0));
    }
    #[test]
    fn an_unusable_nominal_rate_is_unknown() {
        for nominal_rate_hz in [0.0, -48000.0, f64::NAN, f64::INFINITY] {
            let device = OutputLatency {
                nominal_rate_hz,
                ..latency()
            };
            assert_eq!(device.frames_at(48000, false), None);
        }
        let largest = OutputLatency {
            nominal_rate_hz: 1.0,
            device_frames: Some(u32::MAX),
            safety_offset_frames: Some(u32::MAX),
            stream_frames: Some(u32::MAX),
            io_buffer_frames: u32::MAX,
        };
        assert_eq!(largest.frames_at(u32::MAX, true), Some(u64::MAX));
    }
}
