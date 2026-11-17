// ABOUTME: Tests public audio contracts against independent sample vectors.
// ABOUTME: Guards payload preservation, checked sizes, and sample-domain separation.
use earph_core::{audio::*, ids::*};

fn header(format: PcmFormat, frames: u64) -> BlockHeader {
    BlockHeader {
        epochs: Epochs::default(),
        sequence: 0,
        frame_position: 0,
        frame_count: frames,
        format,
        time_domain: TimeDomain::LocalFrames,
    }
}
fn format(representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(48000, ChannelLayout::Mono, representation).unwrap()
}

#[test]
fn integer_payloads_keep_sign_extrema_and_low_bits() {
    let vectors: Vec<(PcmRepresentation, Vec<u8>, Vec<i64>)> = vec![
        (
            PcmRepresentation::Signed16,
            vec![0, 128, 255, 255, 0, 0, 1, 0, 255, 127],
            vec![-32768, -1, 0, 1, 32767],
        ),
        (
            PcmRepresentation::Signed24,
            vec![0, 0, 128, 255, 255, 255, 0, 0, 0, 1, 0, 0, 255, 255, 127],
            vec![-8388608, -1, 0, 1, 8388607],
        ),
        (
            PcmRepresentation::Signed32,
            vec![
                0, 0, 0, 128, 255, 255, 255, 255, 0, 0, 0, 0, 1, 0, 0, 0, 255, 255, 255, 127,
            ],
            vec![-2147483648, -1, 0, 1, 2147483647],
        ),
    ];
    for (representation, bytes, values) in vectors {
        let block = PcmBlock::from_le_bytes(header(format(representation), 5), &bytes).unwrap();
        let got: Vec<_> = (0..5).map(|i| block.integer_sample(i).unwrap()).collect();
        assert_eq!(got, values);
        let mut packed = vec![0; bytes.len()];
        assert_eq!(block.pack_le(&mut packed).unwrap(), bytes.len());
        assert_eq!(packed, bytes);
        let serialized = serde_json::to_vec(&block).unwrap();
        let restored: PcmBlock = serde_json::from_slice(&serialized).unwrap();
        assert_eq!(restored, block);
    }
}

#[test]
fn malformed_blocks_and_formats_fail_before_consumption() {
    assert!(PcmFormat::new(0, ChannelLayout::Stereo, PcmRepresentation::Signed16).is_err());
    let pcm = format(PcmRepresentation::Signed24);
    for samples in [vec![8388608], vec![-8388609]] {
        assert!(PcmBlock::new(header(pcm, 1), PcmSamples::Signed24(samples)).is_err());
    }
    assert!(PcmBlock::new(header(pcm, 0), PcmSamples::Signed24(vec![])).is_err());
    assert!(PcmBlock::new(header(pcm, 2), PcmSamples::Signed24(vec![0])).is_err());
    assert!(PcmBlock::new(header(pcm, 1), PcmSamples::Signed16(vec![0])).is_err());
    assert!(PcmBlock::from_le_bytes(header(pcm, 1), &[0, 0]).is_err());
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(
            PcmBlock::new(
                header(format(PcmRepresentation::Float64), 1),
                PcmSamples::Float64(vec![value])
            )
            .is_err()
        );
    }
    let mut overflow = header(pcm, 2);
    overflow.frame_position = u64::MAX;
    assert!(PcmBlock::new(overflow, PcmSamples::Signed24(vec![0, 0])).is_err());
    assert!(
        serde_json::from_str::<PcmFormat>(
            r#"{"sample_rate_hz":0,"channels":"stereo","representation":"signed16"}"#
        )
        .is_err()
    );
}

#[test]
fn reusable_storage_honors_capacity_and_budget() {
    let pcm = format(PcmRepresentation::Signed32);
    let mut block = PcmBlock::prepare(pcm, 4, Epochs::default(), MemoryBudget::default()).unwrap();
    block.set_integer_sample(0, 2147483647).unwrap();
    block.set_span(Epochs::default(), 2, 10, 1).unwrap();
    assert_eq!(block.integer_sample(0), Some(2147483647));
    assert_eq!(block.integer_sample(1), None);
    let restored: PcmBlock = serde_json::from_slice(&serde_json::to_vec(&block).unwrap()).unwrap();
    assert_eq!(restored.integer_sample(0), Some(2147483647));
    assert_eq!(restored.capacity_frames(), 1);
    assert!(block.set_span(Epochs::default(), 3, 11, 5).is_err());
    assert!(
        PcmBlock::prepare(pcm, usize::MAX, Epochs::default(), MemoryBudget::default()).is_err()
    );
    assert!(MemoryBudget::default().admit(268435457).is_err());
    assert!(PcmBlock::prepare(pcm, 2, Epochs::default(), MemoryBudget::new(4).unwrap()).is_err());
}

#[test]
fn identifiers_and_epoch_overflow_are_checked() {
    for id in ["".to_string(), "bad\nname".into(), "x".repeat(129)] {
        assert!(ZoneId::new(id.clone()).is_err());
        assert!(serde_json::from_value::<SessionId>(serde_json::json!(id)).is_err());
    }
    assert_eq!(RequestId::new("request-1").unwrap().as_str(), "request-1");
    assert!(
        Epochs {
            session: u64::MAX,
            stream: 0
        }
        .next_session()
        .is_err()
    );
    assert!(
        Epochs {
            session: 1,
            stream: u64::MAX
        }
        .next_stream()
        .is_err()
    );
}
