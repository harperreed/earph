// ABOUTME: Exercises the bounded native-output replay ledger with real sample spans.
// ABOUTME: Checks partial writes, retained history, counter resets, and failure atomicity.
#[path = "support/allocations.rs"]
mod allocations;
#[path = "../src/output_replay.rs"]
mod output_replay;

use earph_core::{ErrorCode, ids::Epochs};
use output_replay::OutputReplay;

#[test]
fn replay_processing_holds_errors_reset_and_cancel_allocate_and_free_nothing() {
    use allocations::{AllocationCounts, tracked};
    let (calibration, counts) = tracked(|| std::hint::black_box(vec![0_u8; 32]));
    assert!(counts.allocations > 0);
    assert!(tracked(|| drop(calibration)).1.frees > 0);
    let mut replay = OutputReplay::prepare(2, 8, epochs(0), 128).unwrap();
    let mut output = [0.0; 8];
    let (_, counts) = tracked(|| {
        replay.append(&[0.5; 8]).unwrap();
        replay.copy_pending(&mut output).unwrap();
        replay.submitted(3).unwrap();
        replay.observe(3, 1).unwrap();
        replay.hold(3, 1, epochs(1)).unwrap();
        assert!(replay.observe(4, 4).is_err());
        assert!(replay.append(&[f64::INFINITY; 2]).is_err());
        assert!(replay.append(&[0.5; 16]).is_err());
        replay.reset(epochs(2)).unwrap();
        replay.cancel();
        assert!(replay.append(&[0.0; 2]).is_err());
        assert!(replay.copy_pending(&mut output).is_err());
    });
    assert_eq!(counts, AllocationCounts::default());
}

fn epochs(stream: u64) -> Epochs {
    Epochs { session: 1, stream }
}

#[test]
fn retained_prevolume_output_survives_wrapping_partial_writes_and_repeated_holds() {
    let mut replay = OutputReplay::prepare(2, 5, epochs(0), 80).unwrap();
    assert_eq!(replay.resident_bytes(), 80);
    replay
        .append(&[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0])
        .unwrap();
    let mut output = [99.0; 6];
    assert_eq!(replay.copy_pending(&mut output).unwrap(), 3);
    assert_eq!(output, [1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    replay.submitted(3).unwrap();
    replay.observe(3, 2).unwrap();
    assert_eq!(replay.available_frames(), 3);
    replay.append(&[5.0, -5.0, 6.0, -6.0, 7.0, -7.0]).unwrap();
    let before = replay.state();
    assert_eq!(
        replay.append(&[8.0, -8.0]).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(replay.state(), before);
    replay.hold(3, 2, epochs(1)).unwrap();
    assert_eq!(replay.pending_frames(), 5);
    assert_eq!(replay.copy_pending(&mut output).unwrap(), 3);
    assert_eq!(output, [3.0, -3.0, 4.0, -4.0, 5.0, -5.0]);
    replay.submitted(1).unwrap();
    replay.hold(1, 0, epochs(2)).unwrap();
    assert_eq!(replay.copy_pending(&mut output).unwrap(), 3);
    assert_eq!(output, [3.0, -3.0, 4.0, -4.0, 5.0, -5.0]);
    replay.submitted(3).unwrap();
    replay.observe(3, 3).unwrap();
    assert_eq!(replay.pending_frames(), 2);
    assert_eq!(replay.copy_pending(&mut output).unwrap(), 2);
    assert_eq!(&output[..4], &[6.0, -6.0, 7.0, -7.0]);
    assert_eq!(&output[4..], &[5.0, -5.0]);
}

#[test]
fn invalid_restart_facts_epochs_geometry_and_samples_preserve_the_ledger() {
    let mut replay = OutputReplay::prepare(1, 4, epochs(0), 32).unwrap();
    replay.append(&[0.25, 0.5, 0.75]).unwrap();
    replay.submitted(2).unwrap();
    replay.observe(2, 1).unwrap();
    let before = replay.state();
    for (submitted, restart) in [(1, 1), (2, 3), (2, 0), (u64::MAX, u64::MAX)] {
        assert_eq!(
            replay.observe(submitted, restart).unwrap_err().code,
            ErrorCode::OutputFault
        );
        assert_eq!(replay.state(), before);
    }
    for next in [
        epochs(0),
        Epochs {
            session: 2,
            stream: 1,
        },
    ] {
        assert_eq!(
            replay.hold(2, 1, next).unwrap_err().code,
            ErrorCode::StaleSession
        );
        assert_eq!(replay.state(), before);
    }
    assert_eq!(
        replay.submitted(2).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        replay.append(&[f64::NAN]).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(replay.state(), before);
    let mut empty = [];
    assert_eq!(replay.copy_pending(&mut empty).unwrap(), 0);
    let mut stereo = OutputReplay::prepare(2, 4, epochs(0), 64).unwrap();
    assert_eq!(
        stereo.append(&[0.1]).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        stereo.copy_pending(&mut [0.0; 3]).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert!(OutputReplay::prepare(3, 4, epochs(0), 96).is_err());
    assert!(OutputReplay::prepare(2, 4, epochs(0), 63).is_err());
    assert!(OutputReplay::prepare(1, 0, epochs(0), 32).is_err());
}

#[test]
fn newer_source_reset_clears_history_and_cancellation_remains_permanent() {
    let mut replay = OutputReplay::prepare(1, 4, epochs(0), 32).unwrap();
    replay.append(&[0.25, 0.5]).unwrap();
    replay.submitted(1).unwrap();
    assert!(replay.reset(epochs(0)).is_err());
    replay
        .reset(Epochs {
            session: 2,
            stream: 0,
        })
        .unwrap();
    assert_eq!(replay.pending_frames(), 0);
    assert_eq!(replay.available_frames(), 4);
    assert_eq!(replay.state().generated, 0);
    assert_eq!(replay.state().backend_origin, 0);
    replay.append(&[0.75]).unwrap();
    replay.cancel();
    assert_eq!(
        replay.append(&[0.5]).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        replay.copy_pending(&mut [0.0]).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        replay.submitted(1).unwrap_err().code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        replay
            .hold(
                0,
                0,
                Epochs {
                    session: 2,
                    stream: 1
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(
        replay
            .reset(Epochs {
                session: 3,
                stream: 0
            })
            .unwrap_err()
            .code,
        ErrorCode::OperationCancelled
    );
    assert_eq!(replay.resident_bytes(), 32);
}

#[test]
fn real_stream_output_replays_without_resetting_src_history_or_losing_final_padding() {
    use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
    use earph_dsp::{
        resample::SrcDesign,
        stream::{PreparedStream, StreamBudget, StreamPlan, StreamStagePlan},
    };
    use std::sync::{Arc, atomic::AtomicBool};
    let design = SrcDesign::analyze(
        48000,
        44100,
        ChannelLayout::Stereo,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let input: Vec<_> = (0..514).map(|n| (n as f64 * 0.13).sin() * 0.25).collect();
    let mut reference = design.instantiate(16 << 20).unwrap();
    let mut expected = Vec::new();
    let mut offset = 0;
    let mut span = [0.0; 34];
    for _ in 0..10000 {
        let step = reference.process(&input[offset..], &mut span).unwrap();
        offset += step.consumed_frames * 2;
        expected.extend_from_slice(&span[..step.produced_frames * 2]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    reference.begin_finish().unwrap();
    for _ in 0..10000 {
        let step = reference.finish(&mut span).unwrap();
        expected.extend_from_slice(&span[..step.produced_frames * 2]);
        if step.terminal {
            break;
        }
    }
    let plan = StreamPlan::new(
        vec![StreamStagePlan::Src(design)],
        &[1.0; 2],
        StreamBudget::default(),
    )
    .unwrap();
    let mut stream =
        PreparedStream::prepare(plan, epochs(0), Arc::new(AtomicBool::new(false))).unwrap();
    let mut replay = OutputReplay::prepare(2, 4096, epochs(0), 65536).unwrap();
    offset = 0;
    for _ in 0..10000 {
        let end = (offset + 26).min(input.len());
        let step = stream.pump(&input[offset..end], &mut span).unwrap();
        offset += step.consumed_frames * 2;
        replay.append(&span[..step.produced_frames * 2]).unwrap();
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    stream.begin_finish().unwrap();
    let mut terminal = false;
    for _ in 0..10000 {
        let step = stream.finish(&mut span).unwrap();
        replay.append(&span[..step.produced_frames * 2]).unwrap();
        if step.terminal {
            terminal = true;
            break;
        }
    }
    assert!(terminal);
    assert_eq!(stream.stats().source_frames, 257);
    assert_eq!(replay.pending_frames() * 2, expected.len());
    let before = stream.stats();
    let mut discarded = [0.0; 34];
    assert_eq!(replay.copy_pending(&mut discarded).unwrap(), 17);
    replay.submitted(17).unwrap();
    replay.hold(17, 0, epochs(1)).unwrap();
    let mut actual = Vec::new();
    let mut accepted = 0;
    while replay.pending_frames() != 0 {
        let count = replay.copy_pending(&mut span).unwrap().min(3);
        actual.extend_from_slice(&span[..count * 2]);
        replay.submitted(count).unwrap();
        accepted += count as u64;
        replay.observe(accepted, accepted).unwrap();
    }
    assert_eq!(actual, expected);
    assert_eq!(stream.stats(), before);
    assert_eq!(stream.src_stats(0).unwrap().real_input_frames, 257);
    assert_eq!(replay.available_frames(), 4096);
}
