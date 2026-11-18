// ABOUTME: Exercises real file decoding, exact seeks, and malformed inputs.
// ABOUTME: Uses generated integer signals with independently known samples.
use earph_core::{ErrorCode, ids::Epochs};
use earph_media::LocalFile;
use std::{fs, path::PathBuf};

fn directory() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/media-tests")
        .join(format!(
            "{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn sample(index: usize, bits: u16) -> i32 {
    let limit = 1_i64 << (bits - 1);
    ((index as i64 * 7919 % (2 * limit)) - limit) as i32
}

fn wave(name: &str, bits: u16, channels: u16, frames: usize) -> PathBuf {
    let path = directory().join(name);
    let size = frames * channels as usize * (bits / 8) as usize;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + size as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend((48000 * channels as u32 * (bits / 8) as u32).to_le_bytes());
    bytes.extend((channels * bits / 8).to_le_bytes());
    bytes.extend(bits.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((size as u32).to_le_bytes());
    for i in 0..frames * channels as usize {
        bytes.extend(&sample(i, bits).to_le_bytes()[..(bits / 8) as usize]);
    }
    fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn wav_integer_samples_frames_and_seek_are_exact() {
    for bits in [16, 24, 32] {
        for channels in [1, 2] {
            let path = wave(&format!("{bits}-{channels}.wav"), bits, channels, 8199);
            let original = fs::read(&path).unwrap();
            let mut file = LocalFile::open(&path, Epochs::default()).unwrap();
            assert_eq!(file.total_frames(), 8199);
            let mut frames = 0;
            while let Some(block) = file.next_block().unwrap() {
                assert_eq!(block.header().frame_position, frames);
                for i in 0..block.header().frame_count as usize * channels as usize {
                    assert_eq!(
                        block.integer_sample(i),
                        Some(sample(frames as usize * channels as usize + i, bits) as i64)
                    );
                }
                frames += block.header().frame_count;
            }
            assert_eq!(frames, 8199);
            let epochs = Epochs {
                session: 1,
                stream: 3,
            };
            file.seek(4097, epochs).unwrap();
            let block = file.next_block().unwrap().unwrap();
            assert_eq!(block.header().epochs, epochs);
            assert_eq!(block.header().frame_position, 4097);
            assert_eq!(
                block.integer_sample(0),
                Some(sample(4097 * channels as usize, bits) as i64)
            );
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }
}

#[test]
fn missing_truncated_and_multichannel_files_fail_with_typed_errors() {
    assert_eq!(
        LocalFile::open(&directory().join("absent"), Epochs::default())
            .err()
            .unwrap()
            .code,
        ErrorCode::AssetInvalid
    );
    let multi = wave("six-channel.wav", 24, 6, 40);
    assert_eq!(
        LocalFile::open(&multi, Epochs::default())
            .err()
            .unwrap()
            .code,
        ErrorCode::FormatUnsupported
    );
    let path = wave("truncated.wav", 24, 2, 8199);
    let mut bytes = fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 99);
    fs::write(&path, bytes).unwrap();
    let failed = match LocalFile::open(&path, Epochs::default()) {
        Err(_) => true,
        Ok(mut file) => loop {
            match file.next_block() {
                Err(_) => break true,
                Ok(None) => break false,
                _ => {}
            }
        },
    };
    assert!(
        failed,
        "truncated payload must never report a complete render"
    );
}

#[test]
fn dop_carrier_is_rejected_before_exposing_pcm() {
    let path = wave("dop.wav", 24, 2, 40);
    let mut bytes = fs::read(&path).unwrap();
    for frame in 0..40 {
        for ch in 0..2 {
            bytes[44 + frame * 6 + ch * 3 + 2] = if frame % 2 == 0 { 0x05 } else { 0xfa };
        }
    }
    fs::write(&path, bytes).unwrap();
    assert_eq!(
        LocalFile::open(&path, Epochs::default())
            .err()
            .unwrap()
            .code,
        ErrorCode::FidelityPolicyConflict
    );
}

#[test]
fn lossless_containers_and_seek_match_original_integers() {
    for bits in [16, 24, 32] {
        let source = wave(&format!("source-{bits}.wav"), bits, 2, 8199);
        for extension in ["flac", "aiff"] {
            if bits == 32 && extension == "flac" {
                continue;
            } // FFmpeg 6 encodes FLAC at up to 24 bits.
            let path = source.with_extension(extension);
            let codec = if extension == "flac" {
                "flac".to_owned()
            } else {
                format!("pcm_s{bits}be")
            };
            let result = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-y", "-i"])
                .arg(&source)
                .args(["-c:a", &codec])
                .arg(&path)
                .output()
                .expect("install FFmpeg to generate lossless acceptance fixtures");
            assert!(
                result.status.success(),
                "fixture producer: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let mut file = LocalFile::open(&path, Epochs::default()).unwrap();
            let mut frames = 0;
            while let Some(block) = file.next_block().unwrap() {
                for i in 0..block.header().frame_count as usize * 2 {
                    assert_eq!(
                        block.integer_sample(i),
                        Some(sample(frames * 2 + i, bits) as i64)
                    );
                }
                frames += block.header().frame_count as usize;
            }
            assert_eq!(frames, 8199);
            for frame in [0, 1, 4097, 8198, 8199] {
                file.seek(
                    frame,
                    Epochs {
                        session: 2,
                        stream: frame + 1,
                    },
                )
                .unwrap();
                let block = file.next_block().unwrap();
                if frame == 8199 {
                    assert!(block.is_none());
                } else {
                    let block = block.unwrap();
                    assert_eq!(block.header().frame_position, frame);
                    assert_eq!(
                        block.integer_sample(0),
                        Some(sample(frame as usize * 2, bits) as i64)
                    );
                }
            }
        }
    }
}

#[test]
fn dop_in_a_32_bit_container_is_rejected() {
    let path = wave("dop-32.wav", 32, 2, 40);
    let mut bytes = fs::read(&path).unwrap();
    for frame in 0..40 {
        for ch in 0..2 {
            bytes[44 + frame * 8 + ch * 4 + 3] = if frame % 2 == 0 { 0x05 } else { 0xfa };
        }
    }
    fs::write(&path, bytes).unwrap();
    assert_eq!(
        LocalFile::open(&path, Epochs::default())
            .err()
            .expect("DoP must fail before PCM exposure")
            .code,
        ErrorCode::FidelityPolicyConflict
    );
}

fn flac(name: &str) -> PathBuf {
    let wav = wave(&format!("{name}.wav"), 16, 2, 8199);
    let path = wav.with_extension("flac");
    let output = std::process::Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(wav)
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    path
}

#[test]
fn a_forged_short_frame_count_cannot_hide_audio() {
    let path = flac("short-count");
    let mut bytes = fs::read(&path).unwrap();
    let info = u64::from_be_bytes(bytes[18..26].try_into().unwrap());
    bytes[18..26].copy_from_slice(&((info & !((1_u64 << 36) - 1)) | 4608).to_be_bytes());
    fs::write(&path, bytes).unwrap();
    let mut file = LocalFile::open(&path, Epochs::default()).unwrap();
    assert!(file.next_block().unwrap().is_some());
    assert_eq!(
        file.next_block().expect_err("extra frames must fail").code,
        ErrorCode::AssetInvalid
    );
}

#[test]
fn oversized_container_metadata_is_rejected_before_probe() {
    let path = flac("large-metadata");
    let bytes = fs::read(&path).unwrap();
    let mut expanded = bytes[..42].to_vec();
    expanded.extend([2, 0x10, 0, 1]);
    expanded.resize(expanded.len() + 1048577, 0);
    expanded.extend(&bytes[42..]);
    fs::write(&path, expanded).unwrap();
    assert_eq!(
        LocalFile::open(&path, Epochs::default())
            .err()
            .expect("metadata budget must apply before library allocations")
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
}
