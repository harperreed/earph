// ABOUTME: Shares checked integer PCM conversion between file and stream readers.
// ABOUTME: Keeps decoded samples separate from engine publication identities.
use crate::{BUDGET, INVALID, MAX_FRAMES, UNSUPPORTED};
use earph_core::{
    EngineError, ErrorCode,
    audio::{PcmFormat, PcmRepresentation, PcmSamples},
};
use symphonia::core::audio::{Audio, GenericAudioBufferRef};

/// Decoded PCM before the engine assigns epochs or a clock domain.
#[derive(Debug)]
pub struct DecodedPcmFrame {
    format: PcmFormat,
    frame_position: u64,
    sequence: u64,
    frame_count: u64,
    samples: PcmSamples,
}
impl DecodedPcmFrame {
    pub fn format(&self) -> PcmFormat {
        self.format
    }
    pub fn frame_position(&self) -> u64 {
        self.frame_position
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
    pub fn samples(&self) -> &PcmSamples {
        &self.samples
    }
    pub fn into_samples(self) -> PcmSamples {
        self.samples
    }
}

pub(crate) struct PcmState {
    pub(crate) format: PcmFormat,
    pub(crate) total_frames: Option<u64>,
    pub(crate) position: u64,
    pub(crate) discard_until: u64,
    pub(crate) sequence: u64,
}
impl PcmState {
    pub(crate) fn new(format: PcmFormat, total_frames: Option<u64>) -> Self {
        Self {
            format,
            total_frames,
            position: 0,
            discard_until: 0,
            sequence: 0,
        }
    }
    pub(crate) fn convert(
        &mut self,
        audio: GenericAudioBufferRef<'_>,
    ) -> Result<Option<DecodedPcmFrame>, EngineError> {
        let frames = audio.frames();
        let channels = self.format.channels().count();
        if frames == 0
            || frames > MAX_FRAMES
            || audio.spec().rate() != self.format.sample_rate_hz()
            || audio.num_planes() != channels
        {
            return Err(UNSUPPORTED);
        }
        let end = self
            .position
            .checked_add(frames as u64)
            .filter(|n| self.total_frames.is_none_or(|total| *n <= total))
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(INVALID)?;
        let bits = (self.format.representation().bytes() * 8) as u32;
        let mut values = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            for ch in 0..channels {
                let value = match &audio {
                    GenericAudioBufferRef::S16(b) if bits <= 16 => {
                        i64::from(b.plane(ch).ok_or(INVALID)?[frame]) >> (16 - bits)
                    }
                    GenericAudioBufferRef::S24(b) if bits <= 24 => {
                        i64::from(b.plane(ch).ok_or(INVALID)?[frame].0) >> (24 - bits)
                    }
                    GenericAudioBufferRef::S32(b) => {
                        i64::from(b.plane(ch).ok_or(INVALID)?[frame]) >> (32 - bits)
                    }
                    _ => return Err(UNSUPPORTED),
                };
                values.push(value);
            }
        }
        if bits >= 24 && detected_dop(&values, channels, bits) {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "DoP transport cannot enter the PCM lane",
            ));
        }
        let skip = self
            .discard_until
            .saturating_sub(self.position)
            .min(frames as u64) as usize;
        let start = self.position + skip as u64;
        self.position = end;
        if skip == frames {
            return Ok(None);
        }
        let values = &values[skip * channels..];
        let samples = match self.format.representation() {
            PcmRepresentation::Signed16 => {
                PcmSamples::Signed16(values.iter().map(|v| *v as i16).collect())
            }
            PcmRepresentation::Signed24 => {
                PcmSamples::Signed24(values.iter().map(|v| *v as i32).collect())
            }
            PcmRepresentation::Signed32 => {
                PcmSamples::Signed32(values.iter().map(|v| *v as i32).collect())
            }
            _ => return Err(UNSUPPORTED),
        };
        let result = DecodedPcmFrame {
            format: self.format,
            frame_position: start,
            sequence: self.sequence,
            frame_count: end - start,
            samples,
        };
        self.sequence = self.sequence.checked_add(1).ok_or(BUDGET)?;
        Ok(Some(result))
    }
}

fn detected_dop(values: &[i64], channels: usize, bits: u32) -> bool {
    let mut previous = None;
    let mut run = 0;
    for frame in values.chunks_exact(channels) {
        let marker = ((frame[0] >> (bits - 8)) & 255) as u8;
        if matches!(marker, 0x05 | 0xfa)
            && frame
                .iter()
                .all(|s| ((s >> (bits - 8)) & 255) as u8 == marker)
        {
            run = if previous.is_some_and(|p| p != marker) {
                run + 1
            } else {
                1
            };
            if run >= 16 {
                return true;
            }
            previous = Some(marker);
        } else {
            run = 0;
            previous = None;
        }
    }
    false
}
