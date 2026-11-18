// ABOUTME: Qualifies correction WAV decoding without integer playback conversion.
// ABOUTME: Preserves exact channel order, authored zeros, and immutable byte identity.
use earph_media::decode_fir_wav;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

thread_local! {
    static TRACK:Cell<bool>=const{Cell::new(false)};
    static LIVE:Cell<usize>=const{Cell::new(0)};
    static PEAK:Cell<usize>=const{Cell::new(0)};
}
struct Allocator;
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            record(layout.size(), false);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        record(layout.size(), true);
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let n = unsafe { System.realloc(p, layout, size) };
        if !n.is_null() {
            record(layout.size(), true);
            record(size, false);
        }
        n
    }
}
fn record(size: usize, free: bool) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        LIVE.with(|live| {
            let value = if free {
                live.get().saturating_sub(size)
            } else {
                live.get() + size
            };
            live.set(value);
            PEAK.with(|peak| peak.set(peak.get().max(value)));
        });
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn wav(bits: u16, samples: &[f64]) -> Vec<u8> {
    let data: Vec<u8> = samples
        .iter()
        .flat_map(|v| {
            if bits == 32 {
                (*v as f32).to_le_bytes().to_vec()
            } else {
                v.to_le_bytes().to_vec()
            }
        })
        .collect();
    let mut b = Vec::new();
    b.extend(b"RIFF");
    b.extend((36 + data.len() as u32).to_le_bytes());
    b.extend(b"WAVEfmt ");
    b.extend(16u32.to_le_bytes());
    b.extend(3u16.to_le_bytes());
    b.extend(2u16.to_le_bytes());
    b.extend(48000u32.to_le_bytes());
    b.extend((48000u32 * 2 * u32::from(bits / 8)).to_le_bytes());
    b.extend((2 * (bits / 8)).to_le_bytes());
    b.extend(bits.to_le_bytes());
    b.extend(b"data");
    b.extend((data.len() as u32).to_le_bytes());
    b.extend(data);
    b
}
#[test]
fn floating_corrections_preserve_signed_gain_channels_and_authored_zeros() {
    for bits in [32, 64] {
        let raw = wav(bits, &[1.25, -0.5, 0.25, -0.125, 0.0, 0.0]);
        let correction = decode_fir_wav(&raw).unwrap();
        assert_eq!(correction.reference.taps_per_channel, 3);
        assert_eq!(correction.reference.sample_rate_hz, 48000);
        assert_eq!(
            correction.taps,
            vec![vec![1.25, 0.25, 0.0], vec![-0.5, -0.125, 0.0]]
        );
        assert_eq!(
            decode_fir_wav(&raw).unwrap().reference,
            correction.reference
        );
    }
}
#[test]
fn malformed_and_nonfinite_corrections_fail_before_a_reference_is_returned() {
    for samples in [vec![], vec![f64::NAN, 0.0], vec![f64::INFINITY, 0.0]] {
        assert!(decode_fir_wav(&wav(64, &samples)).is_err());
    }
    let mut truncated = wav(64, &[0.5, 0.25]);
    truncated.pop();
    assert!(decode_fir_wav(&truncated).is_err());
    let mut wrong = wav(64, &[0.5, 0.25]);
    wrong[8..12].copy_from_slice(b"AIFF");
    assert!(decode_fir_wav(&wrong).is_err());
    let mut oversize = wav(64, &[0.5, 0.25]);
    oversize.resize(6 * 1024 * 1024, 0);
    assert!(decode_fir_wav(&oversize).is_err());
}

#[test]
fn inconsistent_ieee_frame_geometry_is_rejected_without_tolerant_decoder_repair() {
    let mut bytes = wav(64, &[0.5, 0.25]);
    bytes[32..34].copy_from_slice(&2_u16.to_le_bytes());
    assert!(decode_fir_wav(&bytes).is_err());
}

#[test]
fn embedded_tag_containers_require_separate_import_qualification() {
    let original = wav(64, &[0.5, 0.25]);
    let mut bytes = original[..36].to_vec();
    bytes.extend(b"LIST");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(b"INFOINAM");
    bytes.extend(4_u32.to_le_bytes());
    bytes.extend(b"test");
    bytes.extend(&original[36..]);
    let len = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    assert!(decode_fir_wav(&bytes).is_err());
}

#[test]
fn maximum_stereo_decode_fits_the_declared_resolution_scratch_allowance() {
    let data = wav(64, &vec![0.25; 2 * 262144]);
    let extra = earph_media::MAX_FIR_WAV_BYTES - data.len() - 8;
    let mut bytes = data[..36].to_vec();
    bytes.extend(b"JUNK");
    bytes.extend((extra as u32).to_le_bytes());
    bytes.resize(bytes.len() + extra, 0);
    bytes.extend_from_slice(&data[36..]);
    let length = bytes.len() as u32 - 8;
    bytes[4..8].copy_from_slice(&length.to_le_bytes());
    LIVE.with(|x| x.set(0));
    PEAK.with(|x| x.set(0));
    TRACK.with(|x| x.set(true));
    let correction = decode_fir_wav(&bytes);
    TRACK.with(|x| x.set(false));
    let peak = PEAK.with(Cell::get);
    let correction = correction.unwrap();
    let resident: usize = correction.taps.iter().map(|v| v.capacity() * 8).sum();
    let transient = peak.saturating_sub(resident);
    println!(
        "requested_decode_peak_bytes={peak} retained_tap_bytes={resident} transient_decode_bytes={transient}"
    );
    assert!(
        earph_media::MAX_FIR_WAV_BYTES + transient <= 8 * 1024 * 1024,
        "decoder requested peak {peak}, retained taps {resident}"
    );
    assert_eq!(correction.reference.taps_per_channel, 262144);
}

#[test]
fn optional_fact_count_must_match_actual_data_frames() {
    let original = wav(64, &[0.5, 0.25, 0.0, 0.0]);
    for frames in [1_u32, 2, 3] {
        let mut bytes = original[..36].to_vec();
        bytes.extend(b"fact");
        bytes.extend(4_u32.to_le_bytes());
        bytes.extend(frames.to_le_bytes());
        bytes.extend(&original[36..]);
        let size = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        if frames == 2 {
            assert_eq!(
                decode_fir_wav(&bytes).unwrap().reference.taps_per_channel,
                2
            );
        } else {
            assert!(
                decode_fir_wav(&bytes).is_err(),
                "inconsistent fact count {frames} was accepted"
            );
        }
    }
}

#[test]
fn fir_file_admission_applies_its_own_optional_payload_bound() {
    use std::{fs, os::unix::fs::DirBuilderExt};
    let directory = std::env::temp_dir().join(format!(
        "earph-fir-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let original = wav(64, &[0.5, 0.25]);
    for optional in [1024 * 1024, 1024 * 1024 + 2] {
        let mut bytes = original[..36].to_vec();
        bytes.extend(b"JUNK");
        bytes.extend((optional as u32).to_le_bytes());
        bytes.resize(bytes.len() + optional, 0);
        bytes.extend(&original[36..]);
        let size = bytes.len() as u32 - 8;
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        let path = directory.join(format!("optional-{optional}.wav"));
        fs::write(&path, &bytes).unwrap();
        let read = earph_media::read_fir_source(&path);
        if optional == 1024 * 1024 {
            let read =
                read.expect("FIR-specific payload bound should admit exact optional maximum");
            assert_eq!(read, bytes);
            assert_eq!(decode_fir_wav(&read).unwrap().reference.taps_per_channel, 1);
        } else {
            if let Ok(read) = read {
                assert!(decode_fir_wav(&read).is_err());
            }
        }
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
