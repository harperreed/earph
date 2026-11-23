// ABOUTME: Reads local lossless audio without modifying original files.
// ABOUTME: Preserves integer samples at the worker-to-engine boundary.
#![forbid(unsafe_code)]
mod admission;
pub use admission::is_dsd;
pub mod cli_io;
pub mod correction_text;
pub mod dsd;
mod fir;
mod fir_manifest;
mod pcm;
mod render;
mod streaming_flac;
use earph_core::{
    EngineError, ErrorCode,
    audio::{BlockHeader, ChannelLayout, PcmBlock, PcmFormat, PcmRepresentation, TimeDomain},
    ids::Epochs,
};
pub use fir::{FirCorrection, MAX_FIR_WAV_BYTES, decode_fir_wav, read_fir_file, read_fir_source};
pub use fir_manifest::{FirCorrectionManifest, MAX_FIR_MANIFEST_BYTES, read_fir_manifest};
pub use pcm::DecodedPcmFrame;
use pcm::PcmState;
pub use render::{RenderReport, render};
use std::path::Path;
pub use streaming_flac::StreamingFlac;
use symphonia::core::{
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
    pcm: PcmState,
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
            pcm: PcmState::new(format, Some(total_frames)),
            epochs,
            pending: None,
            state: ReadState::Reading,
        };
        file.pending = file.decode_block()?;
        Ok(file)
    }
    pub fn format(&self) -> PcmFormat {
        self.pcm.format
    }
    pub fn total_frames(&self) -> u64 {
        self.pcm
            .total_frames
            .expect("local files require known duration")
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
        if frame > self.total_frames() {
            return Err(INVALID);
        }
        if matches!(self.state, ReadState::Failed) {
            return Err(INVALID);
        }
        // A failed native seek may have changed reader state. Never expose the old pending block.
        self.pending = None;
        self.state = ReadState::Failed;
        if frame < self.total_frames() {
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
            self.pcm.position = actual;
        } else {
            self.pcm.position = frame;
        }
        self.decoder.reset();
        self.pcm.discard_until = frame;
        self.epochs = epochs;
        self.pcm.sequence = 0;
        self.state = if frame == self.total_frames() {
            ReadState::SeekedEnd
        } else {
            ReadState::Reading
        };
        Ok(())
    }
    fn decode_block(&mut self) -> Result<Option<PcmBlock>, EngineError> {
        if self.pcm.position == self.total_frames() {
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
            if packet.pts.get() != self.pcm.position as i64 {
                return Err(INVALID);
            }
            let audio = self.decoder.decode(&packet).map_err(|_| INVALID)?;
            let Some(frame) = self.pcm.convert(audio)? else {
                continue;
            };
            let block = PcmBlock::new(
                BlockHeader {
                    epochs: self.epochs,
                    sequence: frame.sequence(),
                    frame_position: frame.frame_position(),
                    frame_count: frame.frame_count(),
                    format: self.pcm.format,
                    time_domain: TimeDomain::LocalFrames,
                },
                frame.into_samples(),
            )?;
            return Ok(Some(block));
        }
    }
}
