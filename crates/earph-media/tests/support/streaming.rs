// ABOUTME: Generates owned FLAC fixtures with independent integer sample values.
// ABOUTME: Preserves each producer input and output beneath the task target directory.
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use symphonia::core::{
    formats::FormatReader,
    io::{MediaSourceStream, ReadOnlySource},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub fn directory() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/streaming-flac/fixtures")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&path).unwrap();
    path
}
pub fn sample(index: usize, bits: u16, pattern: &str) -> i32 {
    let limit = 1_i64 << (bits - 1);
    match pattern {
        "constant" => 137,
        "padding" => 123 + (index % 2) as i32,
        "lpc" => ((index as f64 * 0.019).sin() * limit as f64 * 0.45) as i32,
        "dop" => {
            if (index / 2).is_multiple_of(2) {
                0x05_0039
            } else {
                -0x05_ffc7
            }
        }
        "verbatim" => {
            let mut x = index as u32 + 1;
            x ^= x >> 16;
            x = x.wrapping_mul(0x7feb352d);
            x ^= x >> 15;
            x = x.wrapping_mul(0x846ca68b);
            x ^= x >> 16;
            ((i64::from(x) % (2 * limit)) - limit) as i32
        }
        _ => ((index as i64 * 7919 % (2 * limit)) - limit) as i32,
    }
}
pub fn fixture(bits: u16, channels: u16, frames: usize, pattern: &str) -> Vec<u8> {
    fixture_with_block(bits, channels, frames, pattern, 4096)
}
pub fn fixture_with_block(
    bits: u16,
    channels: u16,
    frames: usize,
    pattern: &str,
    block: usize,
) -> Vec<u8> {
    let root = directory();
    let source = root.join("source.wav");
    let size = frames * channels as usize * (bits / 8) as usize;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + size as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend((48000 * channels as u32 * u32::from(bits / 8)).to_le_bytes());
    bytes.extend((channels * bits / 8).to_le_bytes());
    bytes.extend(bits.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((size as u32).to_le_bytes());
    for i in 0..frames * channels as usize {
        bytes.extend(&sample(i, bits, pattern).to_le_bytes()[..usize::from(bits / 8)]);
    }
    fs::File::create_new(&source)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    let output = root.join("source.flac");
    let mut command = std::process::Command::new("ffmpeg");
    command
        .args(["-nostdin", "-v", "error", "-n", "-i"])
        .arg(source)
        .args(["-c:a", "flac", "-frame_size"])
        .arg(block.to_string());
    if pattern == "verbatim" {
        command.args(["-lpc_type", "none", "-prediction_order_method", "search"]);
    }
    if pattern == "lpc" {
        command.args([
            "-lpc_type",
            "levinson",
            "-min_prediction_order",
            "8",
            "-max_prediction_order",
            "8",
        ]);
    }
    if pattern == "padding" {
        command.args(["-lpc_type", "fixed", "-ch_mode", "left_side"]);
    }
    let result = command.arg(&output).output().unwrap();
    fs::File::create_new(root.join("encoder.stderr"))
        .unwrap()
        .write_all(&result.stderr)
        .unwrap();
    assert!(
        result.status.success(),
        "encoder failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    fs::read(output).unwrap()
}
pub fn metadata(bytes: &[u8], known: bool, md5: bool) -> Vec<u8> {
    let mut copy = bytes.to_vec();
    if !known {
        copy[22] &= 0xf0;
        copy[23..26].fill(0);
    }
    if !md5 {
        copy[26..42].fill(0);
    }
    copy
}
pub fn audio_offset(bytes: &[u8]) -> usize {
    let mut offset = 4;
    loop {
        let last = bytes[offset] & 0x80 != 0;
        let size = (usize::from(bytes[offset + 1]) << 16)
            | (usize::from(bytes[offset + 2]) << 8)
            | usize::from(bytes[offset + 3]);
        offset += 4 + size;
        if last {
            return offset;
        }
    }
}
pub fn packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let source = ReadOnlySource::new(std::io::Cursor::new(bytes.to_vec()));
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut reader =
        symphonia::default::formats::FlacReader::try_new(stream, Default::default()).unwrap();
    let mut packets = Vec::new();
    while let Some(packet) = reader.next_packet().unwrap() {
        packets.push(packet.data.to_vec());
    }
    packets
}
pub fn ordinary_first_frames(bytes: &[u8]) -> usize {
    use symphonia::core::formats::TrackType;
    let source = ReadOnlySource::new(std::io::Cursor::new(bytes.to_vec()));
    let stream = MediaSourceStream::new(Box::new(source), Default::default());
    let mut reader =
        symphonia::default::formats::FlacReader::try_new(stream, Default::default()).unwrap();
    let track = reader.default_track(TrackType::Audio).unwrap();
    let params = track.codec_params.as_ref().unwrap().audio().unwrap();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &Default::default())
        .unwrap();
    decoder
        .decode(&reader.next_packet().unwrap().unwrap())
        .unwrap()
        .frames()
}
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0_u16;
    for byte in bytes {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x8005
            } else {
                crc << 1
            };
        }
    }
    crc
}
