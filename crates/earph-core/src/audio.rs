// ABOUTME: Separates integer PCM, prepared f64 storage, and DSD payloads.
// ABOUTME: Validates frames and sample representations without hidden conversion.
use crate::{EngineError, ErrorCode, ids::Epochs};
use serde::{Deserialize, Serialize};

const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid audio block");
const FORMAT: EngineError = EngineError::new(
    ErrorCode::FormatUnsupported,
    "unsupported sample representation",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "audio buffer budget exceeded",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelLayout {
    Mono,
    Stereo,
}
impl ChannelLayout {
    pub fn count(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PcmRepresentation {
    Signed16,
    Signed24,
    Signed32,
    Float64,
}
impl PcmRepresentation {
    pub fn bytes(self) -> usize {
        match self {
            Self::Signed16 => 2,
            Self::Signed24 => 3,
            Self::Signed32 => 4,
            Self::Float64 => 8,
        }
    }
    pub fn integer_bounds(self) -> Result<(i64, i64), EngineError> {
        match self {
            Self::Signed16 => Ok((-32768, 32767)),
            Self::Signed24 => Ok((-8388608, 8388607)),
            Self::Signed32 => Ok((-2147483648, 2147483647)),
            Self::Float64 => Err(FORMAT),
        }
    }
    fn storage_bytes(self) -> usize {
        if self == Self::Signed24 {
            4
        } else {
            self.bytes()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FormatDocument")]
pub struct PcmFormat {
    sample_rate_hz: u32,
    channels: ChannelLayout,
    representation: PcmRepresentation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormatDocument {
    sample_rate_hz: u32,
    channels: ChannelLayout,
    representation: PcmRepresentation,
}
impl TryFrom<FormatDocument> for PcmFormat {
    type Error = EngineError;
    fn try_from(d: FormatDocument) -> Result<Self, Self::Error> {
        Self::new(d.sample_rate_hz, d.channels, d.representation)
    }
}
impl PcmFormat {
    pub fn new(
        sample_rate_hz: u32,
        channels: ChannelLayout,
        representation: PcmRepresentation,
    ) -> Result<Self, EngineError> {
        if sample_rate_hz == 0 {
            return Err(FORMAT);
        }
        Ok(Self {
            sample_rate_hz,
            channels,
            representation,
        })
    }
    pub fn sample_rate_hz(self) -> u32 {
        self.sample_rate_hz
    }
    pub fn channels(self) -> ChannelLayout {
        self.channels
    }
    pub fn representation(self) -> PcmRepresentation {
        self.representation
    }
    pub fn scalar_count(self, frames: u64) -> Result<usize, EngineError> {
        usize::try_from(frames)
            .ok()
            .and_then(|n| n.checked_mul(self.channels.count()))
            .ok_or(BUDGET)
    }
    pub fn payload_bytes(self, frames: u64) -> Result<usize, EngineError> {
        self.scalar_count(frames)?
            .checked_mul(self.representation.bytes())
            .ok_or(BUDGET)
    }
    pub fn storage_bytes(self, frames: u64) -> Result<usize, EngineError> {
        self.scalar_count(frames)?
            .checked_mul(self.representation.storage_bytes())
            .ok_or(BUDGET)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryBudget {
    bytes: usize,
}
impl Default for MemoryBudget {
    fn default() -> Self {
        Self { bytes: 268435456 }
    }
}
impl MemoryBudget {
    pub fn new(bytes: usize) -> Result<Self, EngineError> {
        if bytes == 0 {
            Err(BUDGET)
        } else {
            Ok(Self { bytes })
        }
    }
    pub fn admit(self, required_bytes: usize) -> Result<(), EngineError> {
        if required_bytes > self.bytes {
            Err(BUDGET)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeDomain {
    HttpMediaFrames,
    LocalFrames,
    OfflineFrames,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockHeader {
    pub epochs: Epochs,
    pub sequence: u64,
    pub frame_position: u64,
    pub frame_count: u64,
    pub format: PcmFormat,
    pub time_domain: TimeDomain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "representation",
    content = "samples",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PcmSamples {
    Signed16(Vec<i16>),
    Signed24(Vec<i32>),
    Signed32(Vec<i32>),
    Float64(Vec<f64>),
}
impl PcmSamples {
    fn len(&self) -> usize {
        match self {
            Self::Signed16(v) => v.len(),
            Self::Signed24(v) | Self::Signed32(v) => v.len(),
            Self::Float64(v) => v.len(),
        }
    }
    fn representation(&self) -> PcmRepresentation {
        match self {
            Self::Signed16(_) => PcmRepresentation::Signed16,
            Self::Signed24(_) => PcmRepresentation::Signed24,
            Self::Signed32(_) => PcmRepresentation::Signed32,
            Self::Float64(_) => PcmRepresentation::Float64,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(try_from = "BlockDocument")]
pub struct PcmBlock {
    header: BlockHeader,
    samples: PcmSamples,
}
#[derive(Serialize)]
#[serde(tag = "representation", content = "samples", rename_all = "snake_case")]
enum SamplesView<'a> {
    Signed16(&'a [i16]),
    Signed24(&'a [i32]),
    Signed32(&'a [i32]),
    Float64(&'a [f64]),
}
impl Serialize for PcmBlock {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let count = self
            .header
            .format
            .scalar_count(self.header.frame_count)
            .map_err(serde::ser::Error::custom)?;
        let samples = match &self.samples {
            PcmSamples::Signed16(v) => SamplesView::Signed16(&v[..count]),
            PcmSamples::Signed24(v) => SamplesView::Signed24(&v[..count]),
            PcmSamples::Signed32(v) => SamplesView::Signed32(&v[..count]),
            PcmSamples::Float64(v) => SamplesView::Float64(&v[..count]),
        };
        let mut state = serializer.serialize_struct("PcmBlock", 2)?;
        state.serialize_field("header", &self.header)?;
        state.serialize_field("samples", &samples)?;
        state.end()
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BlockDocument {
    header: BlockHeader,
    samples: PcmSamples,
}
impl TryFrom<BlockDocument> for PcmBlock {
    type Error = EngineError;
    fn try_from(d: BlockDocument) -> Result<Self, Self::Error> {
        Self::new(d.header, d.samples)
    }
}

impl PcmBlock {
    pub fn new(header: BlockHeader, samples: PcmSamples) -> Result<Self, EngineError> {
        if header.frame_count == 0
            || header
                .frame_position
                .checked_add(header.frame_count)
                .is_none()
            || header.format.scalar_count(header.frame_count)? != samples.len()
            || header.format.representation != samples.representation()
        {
            return Err(INVALID);
        }
        if matches!(&samples, PcmSamples::Signed24(v) if v.iter().any(|n| !(-8388608..=8388607).contains(n)))
            || matches!(&samples, PcmSamples::Float64(v) if v.iter().any(|n| !n.is_finite()))
        {
            return Err(INVALID);
        }
        Ok(Self { header, samples })
    }
    pub fn prepare(
        format: PcmFormat,
        capacity_frames: usize,
        epochs: Epochs,
        budget: MemoryBudget,
    ) -> Result<Self, EngineError> {
        let count = format.scalar_count(capacity_frames as u64)?;
        budget.admit(format.storage_bytes(capacity_frames as u64)?)?;
        let samples = match format.representation {
            PcmRepresentation::Signed16 => PcmSamples::Signed16(vec![0; count]),
            PcmRepresentation::Signed24 => PcmSamples::Signed24(vec![0; count]),
            PcmRepresentation::Signed32 => PcmSamples::Signed32(vec![0; count]),
            PcmRepresentation::Float64 => PcmSamples::Float64(vec![0.0; count]),
        };
        Self::new(
            BlockHeader {
                epochs,
                sequence: 0,
                frame_position: 0,
                frame_count: capacity_frames as u64,
                format,
                time_domain: TimeDomain::OfflineFrames,
            },
            samples,
        )
    }
    pub fn header(&self) -> BlockHeader {
        self.header
    }
    pub fn capacity_frames(&self) -> usize {
        self.samples.len() / self.header.format.channels.count()
    }
    pub fn set_span(
        &mut self,
        epochs: Epochs,
        sequence: u64,
        frame_position: u64,
        frame_count: u64,
    ) -> Result<(), EngineError> {
        if frame_count == 0
            || frame_position.checked_add(frame_count).is_none()
            || frame_count > self.capacity_frames() as u64
        {
            return Err(INVALID);
        }
        self.header.epochs = epochs;
        self.header.sequence = sequence;
        self.header.frame_position = frame_position;
        self.header.frame_count = frame_count;
        Ok(())
    }
    pub fn integer_sample(&self, index: usize) -> Option<i64> {
        if index
            >= self
                .header
                .format
                .scalar_count(self.header.frame_count)
                .ok()?
        {
            return None;
        }
        match &self.samples {
            PcmSamples::Signed16(v) => v.get(index).map(|n| i64::from(*n)),
            PcmSamples::Signed24(v) | PcmSamples::Signed32(v) => {
                v.get(index).map(|n| i64::from(*n))
            }
            _ => None,
        }
    }
    pub fn set_integer_sample(&mut self, index: usize, value: i64) -> Result<(), EngineError> {
        let (min, max) = self.header.format.representation.integer_bounds()?;
        if index >= self.samples.len() || !(min..=max).contains(&value) {
            return Err(INVALID);
        }
        match &mut self.samples {
            PcmSamples::Signed16(v) => v[index] = value as i16,
            PcmSamples::Signed24(v) | PcmSamples::Signed32(v) => v[index] = value as i32,
            _ => return Err(FORMAT),
        }
        Ok(())
    }
    pub fn from_le_bytes(header: BlockHeader, bytes: &[u8]) -> Result<Self, EngineError> {
        header.format.representation.integer_bounds()?;
        if bytes.len() != header.format.payload_bytes(header.frame_count)? {
            return Err(INVALID);
        }
        let mut block = Self::prepare(
            header.format,
            usize::try_from(header.frame_count).map_err(|_| BUDGET)?,
            header.epochs,
            MemoryBudget::default(),
        )?;
        for (i, chunk) in bytes
            .chunks_exact(header.format.representation.bytes())
            .enumerate()
        {
            let mut word = [if chunk[chunk.len() - 1] & 0x80 != 0 {
                255
            } else {
                0
            }; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            block.set_integer_sample(i, i64::from_le_bytes(word))?;
        }
        block.set_span(
            header.epochs,
            header.sequence,
            header.frame_position,
            header.frame_count,
        )?;
        block.header.time_domain = header.time_domain;
        Ok(block)
    }
    pub fn pack_le(&self, destination: &mut [u8]) -> Result<usize, EngineError> {
        self.header.format.representation.integer_bounds()?;
        let bytes = self.header.format.payload_bytes(self.header.frame_count)?;
        if destination.len() < bytes {
            return Err(INVALID);
        }
        let width = self.header.format.representation.bytes();
        for (i, chunk) in destination[..bytes].chunks_exact_mut(width).enumerate() {
            chunk.copy_from_slice(&self.integer_sample(i).ok_or(INVALID)?.to_le_bytes()[..width]);
        }
        Ok(bytes)
    }
}

/// An opaque DSD lane cannot enter a PCM consumer.
/// ```compile_fail
/// use earph_core::audio::{DsdBlock, PcmBlock};
/// fn consume(_: &PcmBlock) {}
/// fn wrong_domain(block: &DsdBlock) { consume(block); }
/// ```
#[derive(Debug)]
pub struct DsdBlock {
    pub payload: Vec<u8>,
    pub bit_rate_hz: u32,
    pub channels: ChannelLayout,
    pub epochs: Epochs,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stereo_byte_counts_reject_overflow() {
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed32).unwrap();
        assert!(format.payload_bytes(u64::MAX).is_err());
        assert_eq!(format.payload_bytes(3).unwrap(), 24);
    }
}
