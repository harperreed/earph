// ABOUTME: Qualifies nonseekable FLAC sample preservation and strict byte ownership.
// ABOUTME: Uses real encoder frames and independently generated malformed inputs.
#[path = "support/streaming.rs"]
mod support;
use earph_core::{EngineError, ErrorCode, audio::PcmSamples};
use earph_media::StreamingFlac;
use std::{
    io::{self, Cursor, Read},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use support::*;

struct Split {
    bytes: Cursor<Vec<u8>>,
    sizes: Vec<usize>,
    index: usize,
}
impl Read for Split {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let len = out.len().min(self.sizes[self.index % self.sizes.len()]);
        self.index += 1;
        self.bytes.read(&mut out[..len])
    }
}
fn values(samples: &PcmSamples) -> Vec<i64> {
    match samples {
        PcmSamples::Signed16(v) => v.iter().map(|n| i64::from(*n)).collect(),
        PcmSamples::Signed24(v) | PcmSamples::Signed32(v) => {
            v.iter().map(|n| i64::from(*n)).collect()
        }
        _ => panic!("integer PCM required"),
    }
}
fn verify(
    bytes: Vec<u8>,
    bits: u16,
    channels: u16,
    pattern: &str,
    known: bool,
    frames: u64,
    sizes: Vec<usize>,
) {
    let mut reader = StreamingFlac::open(Split {
        bytes: Cursor::new(bytes),
        sizes,
        index: 0,
    })
    .unwrap();
    assert_eq!(reader.total_frames(), known.then_some(frames));
    let mut position = 0;
    let mut sequence = 0;
    while let Some(frame) = reader.next_frame().unwrap() {
        assert_eq!(frame.format(), reader.format());
        assert_eq!(frame.frame_position(), position);
        assert_eq!(frame.sequence(), sequence);
        if frames == 8199 && position == 8192 {
            assert_eq!(frame.frame_count(), 7);
        }
        let expected: Vec<i64> = (position as usize * channels as usize
            ..(position + frame.frame_count()) as usize * channels as usize)
            .map(|i| i64::from(sample(i, bits, pattern)))
            .collect();
        assert_eq!(values(frame.samples()), expected);
        position += frame.frame_count();
        sequence += 1;
        assert_eq!(values(&frame.into_samples()), expected);
    }
    assert_eq!(position, frames);
    assert_eq!(reader.total_frames(), known.then_some(frames));
    assert!(reader.next_frame().unwrap().is_none());
}
fn reject(bytes: Vec<u8>) -> EngineError {
    match StreamingFlac::open(Cursor::new(bytes)) {
        Err(error) => error,
        Ok(mut stream) => loop {
            match stream.next_frame() {
                Err(error) => {
                    assert_eq!(stream.next_frame().unwrap_err(), error);
                    return error;
                }
                Ok(None) => panic!("corrupt input reported clean EOF"),
                Ok(Some(_)) => {}
            }
        },
    }
}
#[test]
fn exact_integer_samples_known_unknown_md5_and_fragmented_reads() {
    for bits in [16, 24] {
        for channels in [1, 2] {
            for pattern in ["constant", "verbatim", "fixed", "lpc"] {
                let bytes = fixture(bits, channels, 8199, pattern);
                for known in [true, false] {
                    for md5 in [true, false] {
                        for sizes in [vec![1], vec![3, 17, 2, 4096, 11]] {
                            verify(
                                metadata(&bytes, known, md5),
                                bits,
                                channels,
                                pattern,
                                known,
                                8199,
                                sizes,
                            );
                        }
                    }
                }
            }
        }
    }
    for bits in [16, 24] {
        let bytes = fixture(bits, 2, 8199, "padding");
        for known in [true, false] {
            for md5 in [true, false] {
                verify(
                    metadata(&bytes, known, md5),
                    bits,
                    2,
                    "padding",
                    known,
                    8199,
                    vec![1],
                );
            }
        }
    }
    println!(
        "exact matrix: genuine_fixtures=18 variants=136 packets=408 frames=1115064 integer_samples=1705392"
    );
}
#[test]
fn partial_tails_crc_damage_and_skipped_bytes_never_report_eof() {
    let original = fixture(16, 1, 8199, "verbatim");
    let raw = packets(&original);
    let start = audio_offset(&original);
    let prefix = metadata(&original[..start], false, false);
    for tail in [1, 2, 3, raw[2].len() - 1] {
        let mut damaged = metadata(&original, false, false);
        damaged.truncate(damaged.len() - tail);
        assert_eq!(reject(damaged).code, ErrorCode::AssetInvalid);
    }
    for index in [start, start + raw[0].len(), original.len() - 1] {
        let mut damaged = metadata(&original, false, false);
        damaged[index] ^= 1;
        assert_eq!(reject(damaged).code, ErrorCode::AssetInvalid);
    }
    for extra in [vec![0xff], vec![0xff, 0xf8], vec![0, 0, 0], vec![0xff; 31]] {
        let mut damaged = metadata(&original, false, false);
        damaged.extend(extra);
        assert_eq!(reject(damaged).code, ErrorCode::AssetInvalid);
    }
    let mut shorter = prefix;
    shorter.extend(&raw[0]);
    shorter.extend(&raw[1]);
    verify(shorter.clone(), 16, 1, "verbatim", false, 8192, vec![1]);
    let mut missing_md5 = shorter.clone();
    missing_md5[26..42].copy_from_slice(&original[26..42]);
    assert_eq!(reject(missing_md5).code, ErrorCode::AssetInvalid);
    shorter[22..26].copy_from_slice(&original[22..26]);
    assert_eq!(reject(shorter).code, ErrorCode::AssetInvalid);
    let mut bad_md5 = metadata(&original, false, true);
    bad_md5[26] ^= 1;
    assert_eq!(reject(bad_md5).code, ErrorCode::AssetInvalid);
}
#[test]
fn crc_valid_trailing_and_missing_footer_candidates_fail_before_pcm() {
    let original = fixture(16, 1, 8199, "verbatim");
    let raw = packets(&original);
    let prefix = metadata(&original[..audio_offset(&original)], false, false);
    for trailing in [true, false] {
        let mut candidate = raw[0].clone();
        if trailing {
            candidate.extend([0xa5, 0x5a]);
            candidate.extend(crc16(&candidate).to_be_bytes());
        } else {
            candidate.truncate(candidate.len() - 2);
            let last = candidate.len() - 2;
            let crc = crc16(&candidate[..last]).to_be_bytes();
            candidate[last..].copy_from_slice(&crc);
        }
        assert_eq!(crc16(&candidate), 0);
        let mut damaged = prefix.clone();
        damaged.extend(&candidate);
        assert_eq!(packets(&damaged), vec![candidate]);
        assert_eq!(ordinary_first_frames(&damaged), 4096);
        let mut reader = StreamingFlac::open(Cursor::new(damaged)).unwrap();
        assert_eq!(
            reader.next_frame().unwrap_err().code,
            ErrorCode::AssetInvalid
        );
        assert_eq!(
            reader.next_frame().unwrap_err().code,
            ErrorCode::AssetInvalid
        );
    }
}
struct Counted {
    bytes: Cursor<Vec<u8>>,
    reads: Arc<AtomicUsize>,
}
impl Read for Counted {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.read(out)?;
        self.reads.fetch_add(count, Ordering::Relaxed);
        Ok(count)
    }
}
fn admission_reject(bytes: Vec<u8>, expected_read: usize, code: ErrorCode) {
    let reads = Arc::new(AtomicUsize::new(0));
    let result = StreamingFlac::open(Counted {
        bytes: Cursor::new(bytes),
        reads: reads.clone(),
    });
    assert_eq!(result.err().expect("admission must fail").code, code);
    assert_eq!(reads.load(Ordering::Relaxed), expected_read);
}
#[test]
fn metadata_limits_apply_before_over_budget_body_reads() {
    let original = fixture(16, 1, 8199, "constant");
    let mut prefix = original[..42].to_vec();
    prefix[4] &= 0x7f;
    let mut body = prefix.clone();
    body.extend([0x81, 1, 0, 1]);
    admission_reject(body, 46, ErrorCode::ResourceBudgetExceeded);
    let mut blocks = prefix.clone();
    for _ in 1..256 {
        blocks.extend([1, 0, 0, 0]);
    }
    blocks.extend([0x81, 0, 0, 0]);
    admission_reject(blocks, 1062, ErrorCode::ResourceBudgetExceeded);
    let mut total = prefix.clone();
    for _ in 0..15 {
        total.extend([1, 1, 0, 0]);
        total.resize(total.len() + 65536, 0);
    }
    total.extend([0x81, 1, 0, 0]);
    let expected = total.len();
    admission_reject(total, expected, ErrorCode::ResourceBudgetExceeded);
    let mut duplicate = prefix.clone();
    duplicate.extend([0x80, 0, 0, 34]);
    admission_reject(duplicate, 46, ErrorCode::AssetInvalid);
    let mut reserved = prefix;
    reserved.extend([0xff, 0, 0, 0]);
    admission_reject(reserved, 46, ErrorCode::AssetInvalid);
    admission_reject(b"fLaC\x80\0\0\x21".to_vec(), 8, ErrorCode::AssetInvalid);
    for len in 0..42 {
        let bytes = original[..len].to_vec();
        admission_reject(bytes, len, ErrorCode::AssetInvalid);
    }
}
#[test]
fn unsupported_depth_and_dop_fail_without_exposing_pcm() {
    let original = fixture(24, 2, 8199, "fixed");
    let mut unsupported = original;
    unsupported[20] = (unsupported[20] & !1) | 1;
    unsupported[21] |= 0xf0;
    assert_eq!(reject(unsupported).code, ErrorCode::FormatUnsupported);
    let dop = fixture(24, 2, 8199, "dop");
    let mut stream = StreamingFlac::open(Cursor::new(dop)).unwrap();
    assert_eq!(
        stream.next_frame().unwrap_err().code,
        ErrorCode::FidelityPolicyConflict
    );
    assert_eq!(
        stream.next_frame().unwrap_err().code,
        ErrorCode::FidelityPolicyConflict
    );
}

#[test]
fn constructor_resync_budget_and_actual_source_errors_remain_terminal() {
    let original = fixture(16, 1, 8199, "verbatim");
    let mut junk = metadata(&original[..audio_offset(&original)], false, false);
    junk.resize(junk.len() + 2 * 1024 * 1024 + 1, 0x7b);
    assert_eq!(reject(junk).code, ErrorCode::ResourceBudgetExceeded);
    struct FailsAtEnd {
        source: Cursor<Vec<u8>>,
        calls: Arc<AtomicUsize>,
    }
    impl Read for FailsAtEnd {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if self.source.position() == self.source.get_ref().len() as u64 {
                return Err(io::Error::other("owned source fault"));
            }
            self.source.read(out)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let source = FailsAtEnd {
        source: Cursor::new(original),
        calls: calls.clone(),
    };
    let mut stream = StreamingFlac::open(source).unwrap();
    loop {
        match stream.next_frame() {
            Err(error) => {
                assert_eq!(error.code, ErrorCode::AssetInvalid);
                let before = calls.load(Ordering::Relaxed);
                assert_eq!(stream.next_frame().unwrap_err(), error);
                assert_eq!(calls.load(Ordering::Relaxed), before);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("source fault became EOF"),
        }
    }
}

#[test]
fn large_genuine_packets_and_ring_wrap_preserve_every_sample() {
    let bytes = fixture_with_block(24, 2, 400001, "verbatim", 65535);
    let raw = packets(&bytes);
    println!(
        "capacity qualification: largest real packet={} raw audio bytes={}",
        raw.iter().map(Vec::len).max().unwrap(),
        bytes.len() - audio_offset(&bytes)
    );
    assert!(raw.iter().map(Vec::len).max().unwrap() > 390000);
    verify(bytes, 24, 2, "verbatim", true, 400001, vec![32768]);
}

#[test]
fn oversized_packets_and_packet_assembly_exhaustion_fail_with_budget_errors() {
    let bytes = fixture(16, 1, 8199, "verbatim");
    let packet = packets(&bytes).remove(0);
    let prefix = metadata(&bytes[..audio_offset(&bytes)], false, false);
    for size in [1048577, 2 * 1024 * 1024 + 1] {
        let mut candidate = packet.clone();
        candidate.resize(size - 2, 0x7b);
        candidate.extend(crc16(&candidate).to_be_bytes());
        let mut body = prefix.clone();
        body.extend(candidate);
        assert_eq!(reject(body).code, ErrorCode::ResourceBudgetExceeded);
    }
}

#[test]
fn nested_metadata_lengths_and_missing_final_flag_fail() {
    let original = fixture(16, 1, 8199, "constant");
    for (kind, body) in [(4, vec![255; 4]), (6, vec![0, 0, 0, 3, 255, 255, 255, 255])] {
        let mut bytes = original[..42].to_vec();
        bytes[4] &= 0x7f;
        bytes.extend([0x80 | kind, 0, 0, body.len() as u8]);
        bytes.extend(body);
        bytes.extend(&original[audio_offset(&original)..]);
        assert_eq!(reject(bytes).code, ErrorCode::AssetInvalid);
    }
    let mut prefix = original[..42].to_vec();
    prefix[4] &= 0x7f;
    assert_eq!(reject(prefix).code, ErrorCode::AssetInvalid);
}
