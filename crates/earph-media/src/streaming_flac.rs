// ABOUTME: Decodes admitted nonseekable FLAC with exact raw byte accounting.
// ABOUTME: Bounds retained input and preserves observed failures across calls.
use crate::{DecodedPcmFrame, MAX_FRAMES, pcm::PcmState};
use earph_core::{
    EngineError, ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
};
use std::{
    io::{self, Cursor, Read},
    sync::{Arc, Mutex, MutexGuard},
};
use symphonia::core::{
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    errors::Error,
    formats::{FormatReader, TrackType},
    io::{MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource},
    units::{TimeBase, Timestamp},
};
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "streaming FLAC is corrupt or incomplete",
);
const UNSUPPORTED: EngineError = EngineError::new(
    ErrorCode::FormatUnsupported,
    "streaming FLAC format is not qualified",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "streaming FLAC input budget exceeded",
);
const RAW_BYTES: usize = 2 * 1024 * 1024;

struct Guard {
    bytes: Box<[u8]>,
    start: usize,
    len: usize,
    remaining: usize,
    eof: bool,
    failure: Option<EngineError>,
}

impl Guard {
    fn new() -> Self {
        Self {
            bytes: vec![0; RAW_BYTES].into_boxed_slice(),
            start: 0,
            len: 0,
            remaining: RAW_BYTES,
            eof: false,
            failure: None,
        }
    }
    fn healthy(&self) -> Result<(), EngineError> {
        self.failure.map_or(Ok(()), Err)
    }
    fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() <= self.len
            && bytes
                .iter()
                .enumerate()
                .all(|(i, byte)| self.bytes[(self.start + i) % RAW_BYTES] == *byte)
    }
    fn drain(&mut self, len: usize) {
        self.start = (self.start + len) % RAW_BYTES;
        self.len -= len;
    }
}
fn lock(guard: &Mutex<Guard>) -> MutexGuard<'_, Guard> {
    guard.lock().unwrap_or_else(|poison| poison.into_inner())
}
struct Guarded<R> {
    source: R,
    guard: Arc<Mutex<Guard>>,
}
impl<R: Read> Read for Guarded<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let allowed = {
            let mut guard = lock(&self.guard);
            if guard.failure.is_some() {
                return Err(io::Error::other("FLAC source failed"));
            }
            if guard.eof {
                return Ok(0);
            }
            let allowed = out.len().min(guard.remaining).min(RAW_BYTES - guard.len);
            if allowed == 0 {
                guard.failure = Some(BUDGET);
                return Err(io::Error::other("FLAC input budget exceeded"));
            }
            allowed
        };
        // The caller can inspect or cancel its source without contending on this guard.
        let result = self.source.read(&mut out[..allowed]);
        let mut guard = lock(&self.guard);
        match result {
            Ok(0) => {
                guard.eof = true;
                Ok(0)
            }
            Ok(count) => {
                for byte in &out[..count] {
                    let index = (guard.start + guard.len) % RAW_BYTES;
                    guard.bytes[index] = *byte;
                    guard.len += 1;
                }
                guard.remaining -= count;
                Ok(count)
            }
            Err(error) => {
                guard.failure.get_or_insert(INVALID);
                Err(error)
            }
        }
    }
}

fn admit(reader: &mut impl Read) -> Result<Vec<u8>, EngineError> {
    let mut magic = [0; 4];
    reader.read_exact(&mut magic).map_err(|_| INVALID)?;
    if &magic != b"fLaC" {
        return Err(INVALID);
    }
    let mut prefix = magic.to_vec();
    for block in 0..256 {
        let mut header = [0; 4];
        reader.read_exact(&mut header).map_err(|_| INVALID)?;
        let kind = header[0] & 0x7f;
        let size =
            (usize::from(header[1]) << 16) | (usize::from(header[2]) << 8) | usize::from(header[3]);
        if (block == 0 && (kind != 0 || size != 34)) || (block > 0 && kind == 0) || kind == 127 {
            return Err(INVALID);
        }
        let end = prefix.len() + 4 + size;
        if size > 65536 || end > 1048576 {
            return Err(BUDGET);
        }
        prefix.extend(header);
        let start = prefix.len();
        prefix.resize(end, 0);
        reader
            .read_exact(&mut prefix[start..])
            .map_err(|_| INVALID)?;
        if header[0] & 0x80 != 0 {
            return Ok(prefix);
        }
    }
    Err(BUDGET)
}

/// Worker-side raw FLAC decoding. Engine publication needs its own transport and epoch fence.
pub struct StreamingFlac {
    reader: symphonia::default::formats::FlacReader<'static>,
    decoder: Box<dyn AudioDecoder>,
    guard: Arc<Mutex<Guard>>,
    pcm: PcmState,
    track_id: u32,
    ended: bool,
    failure: Option<EngineError>,
}
impl StreamingFlac {
    pub fn open<R: Read + Send + Sync + 'static>(mut source: R) -> Result<Self, EngineError> {
        let prefix = admit(&mut source)?;
        let bits = ((prefix[20] & 1) << 4 | (prefix[21] >> 4)) + 1;
        if !matches!(bits, 16 | 24) {
            return Err(UNSUPPORTED);
        }
        let guard = Arc::new(Mutex::new(Guard::new()));
        let guarded = Guarded {
            source,
            guard: guard.clone(),
        };
        let replay = Cursor::new(prefix).chain(guarded);
        let stream = MediaSourceStream::new(
            Box::new(ReadOnlySource::new(replay)),
            MediaSourceStreamOptions { buffer_len: 65536 },
        );
        let result = symphonia::default::formats::FlacReader::try_new(stream, Default::default());
        lock(&guard).healthy()?;
        let reader = result.map_err(|_| INVALID)?;
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
            _ => return Err(UNSUPPORTED),
        };
        let format = PcmFormat::new(rate, channels, representation)?;
        if track.num_frames.is_some_and(|n| n > i64::MAX as u64)
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
            .make_audio_decoder(params, &AudioDecoderOptions::default().verify(true))
            .map_err(|_| UNSUPPORTED)?;
        let track_id = track.id;
        let pcm = PcmState::new(format, track.num_frames);
        Ok(Self {
            reader,
            decoder,
            guard,
            pcm,
            track_id,
            ended: false,
            failure: None,
        })
    }
    pub fn format(&self) -> PcmFormat {
        self.pcm.format
    }
    pub fn total_frames(&self) -> Option<u64> {
        self.pcm.total_frames
    }
    pub fn next_frame(&mut self) -> Result<Option<DecodedPcmFrame>, EngineError> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.ended {
            return Ok(None);
        }
        lock(&self.guard).remaining = RAW_BYTES;
        let result = self.decode_frame();
        if let Err(error) = result {
            self.failure = Some(error);
        }
        result
    }
    fn decode_frame(&mut self) -> Result<Option<DecodedPcmFrame>, EngineError> {
        let result = self.reader.next_packet();
        lock(&self.guard).healthy()?;
        let Some(packet) = result.map_err(|_| INVALID)? else {
            let guard = lock(&self.guard);
            if !guard.eof
                || guard.len != 0
                || self
                    .pcm
                    .total_frames
                    .is_some_and(|n| n != self.pcm.position)
            {
                return Err(INVALID);
            }
            drop(guard);
            if self.decoder.finalize().verify_ok == Some(false) {
                return Err(INVALID);
            }
            self.ended = true;
            return Ok(None);
        };
        if packet.data.len() > 1048576 || packet.block_dur().get() > MAX_FRAMES as u64 {
            return Err(BUDGET);
        }
        if packet.track_id != self.track_id
            || packet.pts.get() != self.pcm.position as i64
            || packet.trim_start.get() != 0
            || packet.trim_end.get() != 0
        {
            return Err(INVALID);
        }
        if !lock(&self.guard).matches(&packet.data) || packet.data.len() < 3 {
            return Err(INVALID);
        }
        let mut borrowed = packet.as_packet_ref();
        borrowed.data = &packet.data[..packet.data.len() - 3];
        if !matches!(self.decoder.decode_ref(&borrowed), Err(Error::IoError(_))) {
            return Err(INVALID);
        }
        borrowed.data = &packet.data[..packet.data.len() - 2];
        let audio = self.decoder.decode_ref(&borrowed).map_err(|_| INVALID)?;
        if audio.frames() as u64 != packet.block_dur().get() {
            return Err(INVALID);
        }
        let frame = self.pcm.convert(audio)?.ok_or(INVALID)?;
        lock(&self.guard).drain(packet.data.len());
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_mirror_wraps_and_latches_each_budget_independently() {
        for mirror_full in [false, true] {
            let guard = Arc::new(Mutex::new(Guard::new()));
            let bytes: Vec<u8> = (0..3 * RAW_BYTES).map(|n| (n % 251) as u8).collect();
            let mut reader = Guarded {
                source: Cursor::new(bytes.clone()),
                guard: guard.clone(),
            };
            let mut out = vec![0; RAW_BYTES];
            reader.read_exact(&mut out[..RAW_BYTES / 2]).unwrap();
            assert!(lock(&guard).matches(&bytes[..RAW_BYTES / 2]));
            lock(&guard).drain(RAW_BYTES / 2);
            lock(&guard).remaining = RAW_BYTES;
            reader.read_exact(&mut out).unwrap();
            let mut state = lock(&guard);
            assert!(state.matches(&bytes[RAW_BYTES / 2..RAW_BYTES / 2 + RAW_BYTES]));
            assert_eq!(state.bytes.len(), RAW_BYTES);
            if mirror_full {
                state.remaining = RAW_BYTES;
            } else {
                state.drain(RAW_BYTES);
            }
            drop(state);
            assert!(reader.read(&mut out[..1]).is_err());
            assert_eq!(lock(&guard).failure, Some(BUDGET));
            let before = reader.source.position();
            lock(&guard).remaining = RAW_BYTES;
            assert!(reader.read(&mut out[..1]).is_err());
            assert_eq!(reader.source.position(), before);
            assert_eq!(lock(&guard).bytes.len(), RAW_BYTES);
        }
    }

    #[test]
    fn prefix_capacity_and_guard_storage_are_measured_at_admission_limit() {
        let mut bytes = b"fLaC\0\0\0\x22".to_vec();
        bytes.resize(42, 0);
        for _ in 0..15 {
            bytes.extend([1, 1, 0, 0]);
            bytes.resize(bytes.len() + 65536, 0);
        }
        let last = 1048576 - bytes.len() - 4;
        bytes.extend([0x81, 0, (last >> 8) as u8, last as u8]);
        bytes.resize(1048576, 0);
        let prefix = admit(&mut Cursor::new(bytes)).unwrap();
        let guard = Guard::new();
        println!(
            "retained capacity: admitted_prefix_len={} admitted_prefix_capacity={} fixed_raw_storage={}",
            prefix.len(),
            prefix.capacity(),
            guard.bytes.len()
        );
        assert_eq!(prefix.len(), 1048576);
        assert_eq!(guard.bytes.len(), RAW_BYTES);
    }

    #[test]
    fn source_error_is_latched_without_holding_the_guard_during_read() {
        struct Source {
            guard: Arc<Mutex<Guard>>,
            calls: usize,
        }
        impl Read for Source {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                assert!(self.guard.try_lock().is_ok());
                self.calls += 1;
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "owned read failure",
                ))
            }
        }
        let guard = Arc::new(Mutex::new(Guard::new()));
        let mut reader = Guarded {
            source: Source {
                guard: guard.clone(),
                calls: 0,
            },
            guard: guard.clone(),
        };
        assert!(reader.read(&mut [0]).is_err());
        assert_eq!(lock(&guard).failure, Some(INVALID));
        lock(&guard).remaining = RAW_BYTES;
        assert!(reader.read(&mut [0]).is_err());
        assert_eq!(reader.source.calls, 1);
        assert_eq!(lock(&guard).failure, Some(INVALID));
    }
}
