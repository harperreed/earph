// ABOUTME: Pins the typed DoP contract against numeric carrier words.
// ABOUTME: Covers fragment ownership, phase, epochs, and policy refusal.
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, DsdBlock, MemoryBudget},
    dop::{DSD_WORKING_BYTES, DopBlock, DopPacker, DopStream, DopUnpacker, DsdProcessing},
    ids::Epochs,
};

fn stream() -> DopStream {
    DopStream {
        bit_rate_hz: 2_822_400,
        channels: ChannelLayout::Stereo,
        epochs: Epochs::default(),
        source_sample_position: 0,
    }
}
fn source(bytes: &[u8]) -> DsdBlock {
    DsdBlock::new(
        bytes.to_vec(),
        2_822_400,
        ChannelLayout::Stereo,
        Epochs::default(),
    )
    .unwrap()
}
fn packer() -> DopPacker {
    DopPacker::new(stream(), MemoryBudget::default()).unwrap()
}

#[test]
fn numeric_words_and_phase_persist_across_odd_frame_calls() {
    let mut p = packer();
    let first = p.push(&source(&[0x12, 0x34, 0x56, 0x78])).unwrap().unwrap();
    assert_eq!(first.payload(), [0x56, 0x12, 0x05, 0x78, 0x34, 0x05]);
    assert_eq!(first.carrier_rate_hz(), 176_400);
    assert_eq!(
        (
            first.sequence(),
            first.frame_position(),
            first.frame_count()
        ),
        (0, 0, 1)
    );
    let second = p.push(&source(&[0x9a, 0xbc, 0xde, 0xf0])).unwrap().unwrap();
    assert_eq!(second.payload(), [0xde, 0x9a, 0xfa, 0xf0, 0xbc, 0xfa]);
    assert_eq!((second.sequence(), second.frame_position()), (1, 1));
    p.finish().unwrap();
    assert!(p.push(&source(&[1, 2, 3, 4])).is_err());
}

#[test]
fn arbitrary_byte_fragments_keep_one_frame_remainder_without_filler() {
    let mut p = packer();
    assert!(p.push(&source(&[0x12])).unwrap().is_none());
    assert!(p.push(&source(&[0x34, 0x56])).unwrap().is_none());
    let b = p
        .push(&source(&[0x78, 0x9a, 0xbc, 0xde, 0xf0]))
        .unwrap()
        .unwrap();
    assert_eq!(
        b.payload(),
        [
            0x56, 0x12, 0x05, 0x78, 0x34, 0x05, 0xde, 0x9a, 0xfa, 0xf0, 0xbc, 0xfa
        ]
    );
    p.finish().unwrap();
    for n in 1..4 {
        let mut p = packer();
        p.push(&source(&[1, 2, 3][..n])).unwrap();
        assert!(p.finish().is_err());
        assert!(p.push(&source(&[4])).is_err());
    }
}

#[test]
fn forged_dsd_blocks_and_changed_epochs_end_the_stream() {
    let mut rate = source(&[1, 2, 3, 4]);
    rate.bit_rate_hz = 48_000;
    let mut layout = source(&[1, 2, 3, 4]);
    layout.channels = ChannelLayout::Mono;
    let mut empty = source(&[1]);
    empty.payload.clear();
    let mut oversized = source(&[1]);
    oversized.payload = Vec::with_capacity(10000);
    oversized.payload.extend([1, 2, 3, 4]);
    for block in [rate, layout, empty, oversized] {
        let mut p = packer();
        assert!(p.push(&block).is_err());
        assert!(p.push(&source(&[1, 2, 3, 4])).is_err());
    }
    let mut p = packer();
    p.push(&source(&[1])).unwrap();
    let mut changed = source(&[2, 3, 4]);
    changed.epochs.stream = 1;
    assert!(p.push(&changed).is_err());
    let mut fresh = DopPacker::new(
        DopStream {
            epochs: changed.epochs,
            source_sample_position: 32,
            ..stream()
        },
        MemoryBudget::default(),
    )
    .unwrap();
    changed.payload = vec![0x12, 0x34, 0x56, 0x78];
    let block = fresh.push(&changed).unwrap().unwrap();
    assert_eq!(block.payload(), [0x56, 0x12, 0x05, 0x78, 0x34, 0x05]);
    assert_eq!((block.sequence(), block.frame_position()), (0, 2));
    let mut fresh = DopPacker::new(
        DopStream {
            epochs: changed.epochs,
            ..stream()
        },
        MemoryBudget::default(),
    )
    .unwrap();
    assert!(fresh.push(&source(&[1, 2, 3, 4])).is_err());
}

#[test]
fn unsupported_formats_overflow_and_joint_memory_budget_are_refused() {
    for s in [
        DopStream {
            bit_rate_hz: 48_000,
            ..stream()
        },
        DopStream {
            channels: ChannelLayout::Mono,
            ..stream()
        },
        DopStream {
            source_sample_position: 1,
            ..stream()
        },
    ] {
        assert!(DopPacker::new(s, MemoryBudget::default()).is_err());
    }
    assert!(DopPacker::new(stream(), MemoryBudget::new(DSD_WORKING_BYTES - 1).unwrap()).is_err());
    let mut p = DopPacker::new(
        DopStream {
            bit_rate_hz: 5_644_800,
            ..stream()
        },
        MemoryBudget::new(DSD_WORKING_BYTES).unwrap(),
    )
    .unwrap();
    let mut b = source(&[1, 2, 3, 4]);
    b.bit_rate_hz = 5_644_800;
    assert_eq!(p.push(&b).unwrap().unwrap().carrier_rate_hz(), 352_800);
    assert!(
        DopBlock::new(
            vec![1, 2, 5, 3, 4, 5],
            176_400,
            ChannelLayout::Stereo,
            Epochs::default(),
            0,
            u64::MAX
        )
        .is_err()
    );
    assert!(
        DsdBlock::new(
            vec![0; 8193],
            2_822_400,
            ChannelLayout::Stereo,
            Epochs::default()
        )
        .is_err()
    );
    let mut p = DopPacker::new(
        DopStream {
            source_sample_position: u64::MAX - 15,
            ..stream()
        },
        MemoryBudget::default(),
    )
    .unwrap();
    assert!(p.push(&source(&[1, 2, 3, 4])).is_err());
    assert!(p.finish().is_err());
}

#[test]
fn typed_import_and_unpacking_reject_marker_count_order_and_epoch_errors() {
    for bytes in [
        vec![],
        vec![1, 2, 5],
        vec![1, 2, 5, 3, 4, 0xfa],
        vec![1, 2, 7, 3, 4, 7],
        vec![1, 2, 5, 3, 4, 5, 1, 2, 5, 3, 4, 5],
    ] {
        assert!(
            DopBlock::new(
                bytes,
                176_400,
                ChannelLayout::Stereo,
                Epochs::default(),
                0,
                0
            )
            .is_err()
        );
    }
    let make = |bytes, epochs, seq, pos| {
        DopBlock::new(bytes, 176_400, ChannelLayout::Stereo, epochs, seq, pos).unwrap()
    };
    let mut u = DopUnpacker::new(stream()).unwrap();
    let first = make(vec![0x56, 0x12, 5, 0x78, 0x34, 5], Epochs::default(), 0, 0);
    assert_eq!(u.unpack(&first).unwrap().payload, [0x12, 0x34, 0x56, 0x78]);
    let second = make(
        vec![0xde, 0x9a, 0xfa, 0xf0, 0xbc, 0xfa],
        Epochs::default(),
        1,
        1,
    );
    assert_eq!(u.unpack(&second).unwrap().payload, [0x9a, 0xbc, 0xde, 0xf0]);
    for (marker, epochs, seq, pos) in [
        (0xfa, Epochs::default(), 0, 0),
        (5, Epochs::default(), 1, 0),
        (
            5,
            Epochs {
                session: 1,
                stream: 0,
            },
            0,
            0,
        ),
        (5, Epochs::default(), 0, 1),
    ] {
        let mut u = DopUnpacker::new(stream()).unwrap();
        let bad = make(vec![1, 2, marker, 3, 4, marker], epochs, seq, pos);
        assert!(u.unpack(&bad).is_err());
        assert!(u.unpack(&first).is_err());
    }
}

#[test]
fn processing_requests_and_cancel_discard_pending_source() {
    for request in [
        DsdProcessing::SoftwareVolume(-20.0),
        DsdProcessing::SoftwareVolume(0.0),
        DsdProcessing::SoftwareZero,
        DsdProcessing::Equalization,
        DsdProcessing::Limiting,
        DsdProcessing::SampleRateConversion,
    ] {
        let mut p = packer();
        p.push(&source(&[1])).unwrap();
        assert_eq!(
            p.request_processing(request).unwrap_err().code,
            ErrorCode::FidelityPolicyConflict
        );
        assert!(p.push(&source(&[2, 3, 4])).is_err());
    }
    let mut p = packer();
    p.push(&source(&[1])).unwrap();
    p.cancel();
    assert!(p.push(&source(&[2, 3, 4])).is_err());
    assert!(p.finish().is_err());
}
