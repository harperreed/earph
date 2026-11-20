// ABOUTME: Defines a typed DoP carrier that remains outside ordinary PCM processing.
// ABOUTME: Keeps software transport construction separate from device qualification.
use crate::{
    EngineError, ErrorCode,
    audio::{ChannelLayout, DsdBlock, MemoryBudget},
    ids::Epochs,
    safety::{FidelityPolicy, SampleDomain, VolumeMode, VolumePolicy},
};
const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid DSD transport");
const UNSUPPORTED: EngineError =
    EngineError::new(ErrorCode::FormatUnsupported, "unqualified DoP format");
const CONFLICT: EngineError = EngineError::new(
    ErrorCode::FidelityPolicyConflict,
    "DSD requires an unprocessed typed lane",
);
/// Combined DSF scratch, source block, carrier block, and fixed frame remainder.
pub const DSD_WORKING_BYTES: usize = 4096 + 8192 + 12288 + 4;
#[derive(Debug, Clone, Copy)]
pub struct DopStream {
    pub bit_rate_hz: u32,
    pub channels: ChannelLayout,
    pub epochs: Epochs,
    pub source_sample_position: u64,
}
impl DopStream {
    fn validate(self) -> Result<(), EngineError> {
        if !matches!(self.bit_rate_hz, 2_822_400 | 5_644_800)
            || self.channels != ChannelLayout::Stereo
        {
            return Err(UNSUPPORTED);
        }
        if !self.source_sample_position.is_multiple_of(16) {
            return Err(INVALID);
        }
        Ok(())
    }
}
pub enum DsdProcessing {
    SoftwareVolume(f64),
    SoftwareZero,
    Equalization,
    Limiting,
    SampleRateConversion,
}
#[derive(Debug)]
/// Packed three-byte little-endian words: next source byte, older byte, marker.
/// A future native adapter must accept this type explicitly.
/// ```compile_fail
/// use earph_core::{audio::PcmBlock,dop::DopBlock};
/// fn consume(_: &PcmBlock) {}
/// fn wrong(block: &DopBlock) {consume(block);}
/// ```
pub struct DopBlock {
    payload: Vec<u8>,
    carrier_rate_hz: u32,
    channels: ChannelLayout,
    epochs: Epochs,
    sequence: u64,
    frame_position: u64,
}
impl DopBlock {
    pub fn new(
        payload: Vec<u8>,
        carrier_rate_hz: u32,
        channels: ChannelLayout,
        epochs: Epochs,
        sequence: u64,
        frame_position: u64,
    ) -> Result<Self, EngineError> {
        if !matches!(carrier_rate_hz, 176_400 | 352_800) || channels != ChannelLayout::Stereo {
            return Err(UNSUPPORTED);
        }
        if payload.is_empty()
            || payload.len() > 12288
            || payload.capacity() > 12288
            || !payload.len().is_multiple_of(6)
        {
            return Err(INVALID);
        }
        let count = (payload.len() / 6) as u64;
        frame_position
            .checked_add(count)
            .and_then(|n| n.checked_mul(16))
            .ok_or(INVALID)?;
        let mut marker = payload[2];
        if !matches!(marker, 0x05 | 0xfa) {
            return Err(INVALID);
        }
        for frame in payload.as_chunks::<6>().0 {
            if frame[2] != marker || frame[5] != marker {
                return Err(INVALID);
            }
            marker = flip(marker);
        }
        Ok(Self {
            payload,
            carrier_rate_hz,
            channels,
            epochs,
            sequence,
            frame_position,
        })
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn carrier_rate_hz(&self) -> u32 {
        self.carrier_rate_hz
    }
    pub fn channels(&self) -> ChannelLayout {
        self.channels
    }
    pub fn epochs(&self) -> Epochs {
        self.epochs
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn frame_position(&self) -> u64 {
        self.frame_position
    }
    pub fn frame_count(&self) -> u64 {
        (self.payload.len() / 6) as u64
    }
}
pub struct DopPacker {
    stream: DopStream,
    sequence: u64,
    marker: u8,
    pending: [u8; 4],
    pending_len: usize,
    ended: bool,
}
impl DopPacker {
    /// Start an explicit stream boundary at marker 0x05 and sequence zero.
    pub fn new(stream: DopStream, budget: MemoryBudget) -> Result<Self, EngineError> {
        stream.validate()?;
        budget.admit(DSD_WORKING_BYTES)?;
        Ok(Self {
            stream,
            sequence: 0,
            marker: 0x05,
            pending: [0; 4],
            pending_len: 0,
            ended: false,
        })
    }
    /// Append a canonical fragment. Retain at most three unfinished source bytes.
    pub fn push(&mut self, block: &DsdBlock) -> Result<Option<DopBlock>, EngineError> {
        let result = self.pack(block);
        if result.is_err() {
            self.cancel();
        }
        result
    }
    fn pack(&mut self, block: &DsdBlock) -> Result<Option<DopBlock>, EngineError> {
        if self.ended {
            return Err(INVALID);
        }
        block.validate()?;
        if block.epochs != self.stream.epochs
            || block.bit_rate_hz != self.stream.bit_rate_hz
            || block.channels != self.stream.channels
        {
            return Err(INVALID);
        }
        let frames = (self.pending_len + block.payload.len()) / 4;
        let end = self
            .stream
            .source_sample_position
            .checked_add(frames as u64 * 16)
            .ok_or(INVALID)?;
        let next_sequence = if frames == 0 {
            self.sequence
        } else {
            self.sequence.checked_add(1).ok_or(INVALID)?
        };
        let mut payload = Vec::with_capacity(frames * 6);
        for byte in &block.payload {
            self.pending[self.pending_len] = *byte;
            self.pending_len += 1;
            if self.pending_len == 4 {
                payload.extend([
                    self.pending[2],
                    self.pending[0],
                    self.marker,
                    self.pending[3],
                    self.pending[1],
                    self.marker,
                ]);
                self.marker = flip(self.marker);
                self.pending_len = 0;
                self.pending = [0; 4];
            }
        }
        if frames == 0 {
            return Ok(None);
        }
        let block = DopBlock::new(
            payload,
            self.stream.bit_rate_hz / 16,
            self.stream.channels,
            self.stream.epochs,
            self.sequence,
            self.stream.source_sample_position / 16,
        )?;
        self.stream.source_sample_position = end;
        self.sequence = next_sequence;
        Ok(Some(block))
    }
    /// End a complete stream. An unfinished frame is terminal, never padded.
    pub fn finish(&mut self) -> Result<(), EngineError> {
        let valid = !self.ended && self.pending_len == 0;
        self.cancel();
        if valid { Ok(()) } else { Err(INVALID) }
    }
    pub fn cancel(&mut self) {
        self.pending = [0; 4];
        self.pending_len = 0;
        self.ended = true;
    }
    pub fn request_processing(&mut self, request: DsdProcessing) -> Result<(), EngineError> {
        self.cancel();
        match request {
            DsdProcessing::SoftwareVolume(gain) => {
                VolumePolicy::default().validate_gain_constraints(
                    VolumeMode::Software,
                    SampleDomain::Dsd,
                    FidelityPolicy::StrictPreservation,
                    gain,
                    None,
                )?;
            }
            DsdProcessing::SoftwareZero => {
                VolumePolicy::default().validate_software_zero(
                    VolumeMode::Software,
                    SampleDomain::Dsd,
                    FidelityPolicy::StrictPreservation,
                )?;
            }
            _ => {}
        }
        Err(CONFLICT)
    }
}
pub struct DopUnpacker {
    stream: DopStream,
    sequence: u64,
    marker: u8,
    failed: bool,
}
impl DopUnpacker {
    pub fn new(stream: DopStream) -> Result<Self, EngineError> {
        stream.validate()?;
        Ok(Self {
            stream,
            sequence: 0,
            marker: 0x05,
            failed: false,
        })
    }
    pub fn unpack(&mut self, block: &DopBlock) -> Result<DsdBlock, EngineError> {
        let result = self.decode(block);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn decode(&mut self, block: &DopBlock) -> Result<DsdBlock, EngineError> {
        if self.failed
            || block.epochs != self.stream.epochs
            || block.sequence != self.sequence
            || block.frame_position != self.stream.source_sample_position / 16
            || block.carrier_rate_hz != self.stream.bit_rate_hz / 16
            || block.channels != self.stream.channels
            || block.payload[2] != self.marker
        {
            return Err(INVALID);
        }
        let end = self
            .stream
            .source_sample_position
            .checked_add(block.frame_count() * 16)
            .ok_or(INVALID)?;
        let next_sequence = self.sequence.checked_add(1).ok_or(INVALID)?;
        let mut payload = Vec::with_capacity(block.payload.len() / 3 * 2);
        for frame in block.payload.as_chunks::<6>().0 {
            payload.extend([frame[1], frame[4], frame[0], frame[3]]);
            self.marker = flip(self.marker);
        }
        let source = DsdBlock::new(
            payload,
            self.stream.bit_rate_hz,
            self.stream.channels,
            self.stream.epochs,
        )?;
        self.stream.source_sample_position = end;
        self.sequence = next_sequence;
        Ok(source)
    }
}
fn flip(marker: u8) -> u8 {
    if marker == 0x05 { 0xfa } else { 0x05 }
}
