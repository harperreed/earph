// ABOUTME: Decodes bounded IEEE floating correction WAV files without changing their gain.
// ABOUTME: Derives immutable byte identity and preserves authored tap and channel geometry.
use earph_core::{
    EngineError, ErrorCode,
    audio::ChannelLayout,
    dsp_nodes::{FirAssetReference, FirPurpose, FirSha256},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Read, Seek},
    path::Path,
};
use symphonia::core::{
    audio::{Audio, GenericAudioBufferRef},
    codecs::audio::well_known::{CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F64LE},
    common::Limit,
    formats::{TrackType, probe::Hint},
    io::MediaSourceStream,
    meta::MetadataOptions,
};

pub const MAX_FIR_WAV_BYTES: usize = 5 * 1024 * 1024;
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "correction WAV is corrupt or differs from its declared geometry",
);
const UNSUPPORTED: EngineError = EngineError::new(
    ErrorCode::FormatUnsupported,
    "correction WAV requires mono or stereo IEEE float32 or float64 PCM",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "correction WAV exceeds its byte or tap limit",
);

pub struct FirCorrection {
    pub reference: FirAssetReference,
    pub taps: Vec<Vec<f64>>,
}

/// Read an owned source without modifying it. Admission rejects special files before reading.
pub fn read_fir_source(path: &Path) -> Result<Vec<u8>, EngineError> {
    let metadata = std::fs::metadata(path).map_err(|_| INVALID)?;
    if metadata.len() > MAX_FIR_WAV_BYTES as u64 {
        return Err(BUDGET);
    }
    read_fir_file(crate::admission::open(path)?)
}

/// Read a previously admitted regular file without growing its prepared byte span.
pub fn read_fir_file(mut file: std::fs::File) -> Result<Vec<u8>, EngineError> {
    let metadata = file.metadata().map_err(|_| INVALID)?;
    if !metadata.is_file() {
        return Err(INVALID);
    }
    let size = usize::try_from(metadata.len()).map_err(|_| BUDGET)?;
    if size > MAX_FIR_WAV_BYTES {
        return Err(BUDGET);
    }
    file.rewind().map_err(|_| INVALID)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| BUDGET)?;
    if bytes.capacity() > MAX_FIR_WAV_BYTES {
        return Err(BUDGET);
    }
    bytes.resize(size, 0);
    file.read_exact(&mut bytes).map_err(|_| INVALID)?;
    if file.read(&mut [0_u8; 1]).map_err(|_| INVALID)? != 0 {
        return Err(INVALID);
    }
    Ok(bytes)
}

/// Decode the supplied immutable bytes with the pinned WAV and PCM library.
pub fn decode_fir_wav(bytes: &[u8]) -> Result<FirCorrection, EngineError> {
    preflight(bytes)?;
    let stream = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let metadata = MetadataOptions::default()
        .limit_tag_bytes(Limit::Maximum(64 * 1024))
        .limit_visual_bytes(Limit::Maximum(0));
    let mut reader = symphonia::default::get_probe()
        .probe(&Hint::new(), stream, Default::default(), metadata)
        .map_err(|_| UNSUPPORTED)?;
    let track = reader.default_track(TrackType::Audio).ok_or(UNSUPPORTED)?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or(UNSUPPORTED)?;
    if params.codec != CODEC_ID_PCM_F32LE && params.codec != CODEC_ID_PCM_F64LE {
        return Err(UNSUPPORTED);
    }
    let channels = match params.channels.as_ref().ok_or(UNSUPPORTED)?.count() {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        _ => return Err(UNSUPPORTED),
    };
    let rate = params.sample_rate.ok_or(UNSUPPORTED)?;
    let taps = usize::try_from(track.num_frames.ok_or(INVALID)?).map_err(|_| BUDGET)?;
    if !(1..=262144).contains(&taps) {
        return Err(BUDGET);
    }
    if params.max_frames_per_packet.is_none_or(|n| n > 1152)
        || params.extra_data.as_ref().is_some_and(|v| v.len() > 65536)
    {
        return Err(BUDGET);
    }
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .map_err(|_| UNSUPPORTED)?;
    let mut values: Vec<Vec<f64>> = (0..channels.count())
        .map(|_| Vec::with_capacity(taps))
        .collect();
    let mut count = 0_usize;
    while let Some(packet) = reader.next_packet().map_err(|_| INVALID)? {
        if packet.track_id != track_id || packet.data.len() > 1024 * 1024 {
            return Err(INVALID);
        }
        let audio = decoder.decode(&packet).map_err(|_| INVALID)?;
        let frames = audio.frames();
        if frames == 0
            || audio.num_planes() != channels.count()
            || audio.spec().rate() != rate
            || count.checked_add(frames).is_none_or(|n| n > taps)
        {
            return Err(INVALID);
        }
        for (ch, destination) in values.iter_mut().enumerate() {
            for frame in 0..frames {
                let value = match &audio {
                    GenericAudioBufferRef::F32(buffer) => {
                        f64::from(buffer.plane(ch).ok_or(INVALID)?[frame])
                    }
                    GenericAudioBufferRef::F64(buffer) => buffer.plane(ch).ok_or(INVALID)?[frame],
                    _ => return Err(UNSUPPORTED),
                };
                if !value.is_finite() {
                    return Err(INVALID);
                }
                destination.push(value);
            }
        }
        count += frames;
    }
    if count != taps {
        return Err(INVALID);
    }
    let hash = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let reference = FirAssetReference {
        sha256: FirSha256::new(hash)?,
        sample_rate_hz: rate,
        channels,
        taps_per_channel: taps,
        purpose: FirPurpose::Correction,
    };
    reference.validate()?;
    Ok(FirCorrection {
        reference,
        taps: values,
    })
}

fn preflight(bytes: &[u8]) -> Result<(), EngineError> {
    if bytes.len() > MAX_FIR_WAV_BYTES {
        return Err(BUDGET);
    }
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(UNSUPPORTED);
    }
    let length = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| INVALID)?) as usize;
    if length.checked_add(8) != Some(bytes.len()) {
        return Err(INVALID);
    }
    let (mut position, mut metadata, mut count, mut format, mut data) =
        (12_usize, 0_usize, 0_usize, None, false);
    while position < bytes.len() {
        count += 1;
        if count > 256 {
            return Err(BUDGET);
        }
        let header = bytes.get(position..position + 8).ok_or(INVALID)?;
        let size = u32::from_le_bytes(header[4..8].try_into().map_err(|_| INVALID)?) as usize;
        let start = position + 8;
        let end = start.checked_add(size).ok_or(INVALID)?;
        let payload = bytes.get(start..end).ok_or(INVALID)?;
        match &header[..4] {
            b"fmt " => {
                if format.is_some() || data || ![16, 18].contains(&size) {
                    return Err(INVALID);
                }
                if u16::from_le_bytes(payload[..2].try_into().map_err(|_| INVALID)?) != 3 {
                    return Err(UNSUPPORTED);
                }
                let channels = u16::from_le_bytes(payload[2..4].try_into().map_err(|_| INVALID)?);
                let rate = u32::from_le_bytes(payload[4..8].try_into().map_err(|_| INVALID)?);
                let byte_rate = u32::from_le_bytes(payload[8..12].try_into().map_err(|_| INVALID)?);
                let align = u16::from_le_bytes(payload[12..14].try_into().map_err(|_| INVALID)?);
                let bits = u16::from_le_bytes(payload[14..16].try_into().map_err(|_| INVALID)?);
                if ![1, 2].contains(&channels)
                    || ![32, 64].contains(&bits)
                    || rate == 0
                    || align != channels * (bits / 8)
                    || rate.checked_mul(u32::from(align)) != Some(byte_rate)
                    || (size == 18 && payload[16..18] != [0, 0])
                {
                    return Err(UNSUPPORTED);
                }
                format = Some(usize::from(align));
            }
            b"data" => {
                let align = format.ok_or(INVALID)?;
                if data || size == 0 || !size.is_multiple_of(align) || size / align > 262144 {
                    return Err(INVALID);
                }
                data = true;
            }
            b"JUNK" | b"PAD " | b"fact" => {
                if &header[..4] == b"fact" && size != 4 {
                    return Err(INVALID);
                }
                metadata = metadata.checked_add(size).ok_or(BUDGET)?;
                if metadata > 1024 * 1024 {
                    return Err(BUDGET);
                }
            }
            // Tag objects can use substantially more memory than their encoded byte count.
            _ => return Err(UNSUPPORTED),
        }
        position = end.checked_add(size % 2).ok_or(INVALID)?;
        if position > bytes.len() {
            return Err(INVALID);
        }
    }
    if format.is_none() || !data {
        return Err(INVALID);
    }
    Ok(())
}
