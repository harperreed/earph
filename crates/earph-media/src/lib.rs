// ABOUTME: Reads local lossless audio without modifying original files.
// ABOUTME: Preserves integer samples at the worker-to-engine boundary.
#![forbid(unsafe_code)]
mod admission;
pub mod correction_text;
mod fir;
mod fir_manifest;
mod render;
use earph_core::{
    EngineError, ErrorCode,
    audio::{
        BlockHeader, ChannelLayout, PcmBlock, PcmFormat, PcmRepresentation, PcmSamples, TimeDomain,
    },
    ids::Epochs,
};
pub use fir::{FirCorrection, MAX_FIR_WAV_BYTES, decode_fir_wav, read_fir_file, read_fir_source};
pub use fir_manifest::{FirCorrectionManifest, MAX_FIR_MANIFEST_BYTES, read_fir_manifest};
pub use render::{RenderReport, render};
use std::path::Path;
use symphonia::core::{
    audio::{Audio, GenericAudioBufferRef},
    codecs::audio::AudioDecoder,
    common::Limit,
    formats::{FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
    units::{TimeBase, Timestamp},
};

const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "local media is missing, corrupt, or incomplete",
);
const UNSUPPORTED: EngineError = EngineError::new(
    ErrorCode::FormatUnsupported,
    "local media format is not qualified",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "local media packet budget exceeded",
);
const MAX_FRAMES: usize = 65536;

/// A worker-side reader. Never call this API from an output callback.
pub struct LocalFile {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    format: PcmFormat,
    total_frames: u64,
    position: u64,
    discard_until: u64,
    sequence: u64,
    epochs: Epochs,
    pending: Option<PcmBlock>,
    state: ReadState,
}

#[derive(Clone, Copy)]
enum ReadState {
    Reading,
    SeekedEnd,
    Ended,
    Failed,
}

impl LocalFile {
    pub fn open(path: &Path, epochs: Epochs) -> Result<Self, EngineError> {
        let file = admission::open(path)?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let metadata = MetadataOptions::default()
            .limit_tag_bytes(Limit::Maximum(64 * 1024))
            .limit_visual_bytes(Limit::Maximum(0));
        let reader = symphonia::default::get_probe()
            .probe(&Hint::new(), stream, Default::default(), metadata)
            .map_err(|_| UNSUPPORTED)?;
        let track = reader.default_track(TrackType::Audio).ok_or(UNSUPPORTED)?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or(UNSUPPORTED)?;
        let rate = params.sample_rate.ok_or(UNSUPPORTED)?;
        let channels = match params.channels.as_ref().ok_or(UNSUPPORTED)?.count() {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ => return Err(UNSUPPORTED),
        };
        let representation = match params.bits_per_sample {
            Some(16) => PcmRepresentation::Signed16,
            Some(24) => PcmRepresentation::Signed24,
            Some(32) => PcmRepresentation::Signed32,
            _ => return Err(UNSUPPORTED),
        };
        let format = PcmFormat::new(rate, channels, representation)?;
        let total_frames = track.num_frames.ok_or(UNSUPPORTED)?;
        if total_frames > i64::MAX as u64
            || track.start_ts != Timestamp::ZERO
            || track.time_base != TimeBase::try_new(1, rate)
        {
            return Err(UNSUPPORTED);
        }
        if params
            .max_frames_per_packet
            .is_some_and(|n| n > MAX_FRAMES as u64)
            || params.extra_data.as_ref().is_some_and(|b| b.len() > 65536)
        {
            return Err(BUDGET);
        }
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &Default::default())
            .map_err(|_| UNSUPPORTED)?;
        let track_id = track.id;
        let mut file = Self {
            reader,
            decoder,
            track_id,
            format,
            total_frames,
            position: 0,
            discard_until: 0,
            sequence: 0,
            epochs,
            pending: None,
            state: ReadState::Reading,
        };
        file.pending = file.decode_block()?;
        Ok(file)
    }
    pub fn format(&self) -> PcmFormat {
        self.format
    }
    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }
    pub fn next_block(&mut self) -> Result<Option<PcmBlock>, EngineError> {
        match self.state {
            ReadState::Failed => return Err(INVALID),
            ReadState::SeekedEnd | ReadState::Ended => return Ok(None),
            ReadState::Reading => {}
        }
        if let Some(block) = self.pending.take() {
            return Ok(Some(block));
        }
        let result = self.decode_block();
        if result.is_err() {
            self.state = ReadState::Failed;
        }
        result
    }
    pub fn seek(&mut self, frame: u64, epochs: Epochs) -> Result<(), EngineError> {
        if frame > self.total_frames {
            return Err(INVALID);
        }
        if matches!(self.state, ReadState::Failed) {
            return Err(INVALID);
        }
        // A failed native seek may have changed reader state. Never expose the old pending block.
        self.pending = None;
        self.state = ReadState::Failed;
        if frame < self.total_frames {
            let result = self
                .reader
                .seek(
                    SeekMode::Accurate,
                    SeekTo::Timestamp {
                        ts: Timestamp::new(frame as i64),
                        track_id: self.track_id,
                    },
                )
                .map_err(|_| INVALID)?;
            let actual = u64::try_from(result.actual_ts.get()).map_err(|_| INVALID)?;
            if actual > frame {
                return Err(INVALID);
            }
            self.position = actual;
        } else {
            self.position = frame;
        }
        self.decoder.reset();
        self.discard_until = frame;
        self.epochs = epochs;
        self.sequence = 0;
        self.state = if frame == self.total_frames {
            ReadState::SeekedEnd
        } else {
            ReadState::Reading
        };
        Ok(())
    }
    fn decode_block(&mut self) -> Result<Option<PcmBlock>, EngineError> {
        if self.position == self.total_frames {
            if self.reader.next_packet().map_err(|_| INVALID)?.is_some() {
                return Err(INVALID);
            }
            self.state = ReadState::Ended;
            return Ok(None);
        }
        loop {
            let packet = self
                .reader
                .next_packet()
                .map_err(|_| INVALID)?
                .ok_or(INVALID)?;
            if packet.track_id != self.track_id {
                return Err(UNSUPPORTED);
            }
            if packet.data.len() > 1024 * 1024 || packet.block_dur().get() > MAX_FRAMES as u64 {
                return Err(BUDGET);
            }
            if packet.pts.get() != self.position as i64 {
                return Err(INVALID);
            }
            let audio = self.decoder.decode(&packet).map_err(|_| INVALID)?;
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
                .filter(|n| *n <= self.total_frames)
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
                continue;
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
            let block = PcmBlock::new(
                BlockHeader {
                    epochs: self.epochs,
                    sequence: self.sequence,
                    frame_position: start,
                    frame_count: end - start,
                    format: self.format,
                    time_domain: TimeDomain::LocalFrames,
                },
                samples,
            )?;
            self.sequence = self.sequence.checked_add(1).ok_or(BUDGET)?;
            return Ok(Some(block));
        }
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
