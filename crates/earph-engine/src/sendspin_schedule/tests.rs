// ABOUTME: Checks the Sendspin scheduling decisions against hand-computed frame and microsecond values.
// ABOUTME: Covers anchoring, locked error statistics, gaps, the monotonic clamp, and numeric faults.
use super::*;

const RATE: u32 = 48_000;

fn chunk(target_us: i64, now_us: i64, delay_frames: Option<u64>) -> ChunkFacts {
    ChunkFacts {
        target_us,
        mapping_revision: 1,
        frames: 480,
        now_us,
        delay_frames,
        gap_frames: 0,
    }
}

fn write(prepend_silence_frames: u64, skip_frames: u64) -> Decision {
    Decision::Write {
        prepend_silence_frames,
        skip_frames,
    }
}

/// Anchors exactly on time, so later chunks exercise the locked state.
fn locked(rate: u32) -> Scheduler {
    let mut scheduler = Scheduler::new(rate).unwrap();
    assert_eq!(scheduler.decide(chunk(0, 0, None)), Ok(write(0, 0)));
    scheduler
}

#[test]
fn early_chunk_waits_until_the_pad_window_and_never_past_the_target() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    // 100 ms early: wait until 5 ms remain.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 900_000, None)),
        Ok(Decision::Wait { until_us: 995_000 })
    );
    // One microsecond over the limit still makes progress.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 994_999, None)),
        Ok(Decision::Wait { until_us: 995_000 })
    );
    // One delay frame is 20.8333 us: 94_979.1667 us of excess rounds up to 94_980.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 900_000, Some(1))),
        Ok(Decision::Wait { until_us: 994_980 })
    );
    // The widest representable lead still ends before the target.
    assert_eq!(
        scheduler.decide(chunk(i64::MAX, i64::MIN, None)),
        Ok(Decision::Wait {
            until_us: i64::MAX - 5_000
        })
    );
    assert!(!scheduler.snapshot().anchored);
    // At the wake time the lead is 4_999.1667 us: 239.96 frames round to 240.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 994_980, Some(1))),
        Ok(write(240, 0))
    );
    assert!(scheduler.snapshot().anchored);
    assert_eq!(scheduler.snapshot().inserted_silence_frames, 240);
}

#[test]
fn short_lead_pads_to_the_nearest_frame() {
    // (lead in us, expected frames) at 48 kHz, where one frame is 20.8333 us.
    for (lead_us, frames) in [
        (5_000, 240), // the limit itself pads
        (31, 1),      // 1.488 frames
        (32, 2),      // 1.536 frames
        (10, 0),      // 0.48 frames
        (11, 1),      // 0.528 frames
    ] {
        let mut scheduler = Scheduler::new(RATE).unwrap();
        assert_eq!(
            scheduler.decide(chunk(1_000_000, 1_000_000 - lead_us, None)),
            Ok(write(frames, 0)),
            "lead {lead_us}"
        );
        assert_eq!(scheduler.snapshot().inserted_silence_frames, frames);
    }
    // At 50 kHz one frame is 20 us, so 10 us and 30 us are exact ties and round up.
    for (lead_us, frames) in [(10, 1), (30, 2), (9, 0)] {
        let mut scheduler = Scheduler::new(50_000).unwrap();
        assert_eq!(
            scheduler.decide(chunk(1_000_000, 1_000_000 - lead_us, None)),
            Ok(write(frames, 0)),
            "lead {lead_us}"
        );
    }
}

#[test]
fn late_chunk_drops_whole_chunks_then_frames() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    // 15 ms late is 720 frames: the whole 480-frame chunk goes.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 1_015_000, None)),
        Ok(Decision::DropChunk)
    );
    assert!(!scheduler.snapshot().anchored);
    assert_eq!(scheduler.snapshot().dropped_frames, 480);
    // The next chunk starts 10 ms later, so it is 5 ms late: 240 frames.
    assert_eq!(
        scheduler.decide(chunk(1_010_000, 1_015_000, None)),
        Ok(write(0, 240))
    );
    assert!(scheduler.snapshot().anchored);
    assert_eq!(scheduler.snapshot().dropped_frames, 720);
}

#[test]
fn late_frames_round_up() {
    // (late us, expected decision) for a 480-frame chunk.
    for (late_us, expected) in [
        (0, write(0, 0)),
        (1, write(0, 1)),              // 0.048 frames
        (21, write(0, 2)),             // 1.008 frames
        (9_979, write(0, 479)),        // 478.992 frames
        (10_000, Decision::DropChunk), // exactly the whole chunk
    ] {
        let mut scheduler = Scheduler::new(RATE).unwrap();
        assert_eq!(
            scheduler.decide(chunk(1_000_000, 1_000_000 + late_us, None)),
            Ok(expected),
            "late {late_us}"
        );
    }
}

#[test]
fn delay_counts_once_and_unknown_delay_is_zero_and_flagged() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    assert!(!scheduler.snapshot().delay_known);
    // 10 ms before the target with no reported delay: wait.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 990_000, None)),
        Ok(Decision::Wait { until_us: 995_000 })
    );
    assert!(!scheduler.snapshot().delay_known);
    // 240 delay frames are 5 ms, which leaves a 5 ms lead: 240 frames of silence.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 990_000, Some(240))),
        Ok(write(240, 0))
    );
    assert!(scheduler.snapshot().delay_known);
    // Locked: (1_004_000 + 10_000) - 1_010_000 = 4_000.
    assert_eq!(
        scheduler.decide(chunk(1_010_000, 1_004_000, Some(480))),
        Ok(write(0, 0))
    );
    assert_eq!(scheduler.snapshot().last_error_us, Some(4_000));
}

#[test]
fn mapping_revision_change_is_recorded_without_a_new_anchor() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    assert_eq!(scheduler.snapshot().mapping_revision, None);
    let first = ChunkFacts {
        mapping_revision: 7,
        ..chunk(1_000_000, 1_000_000, None)
    };
    assert_eq!(scheduler.decide(first), Ok(write(0, 0)));
    assert_eq!(scheduler.snapshot().mapping_revision, Some(7));
    // The new revision moves the target 2 ms; the chunk still writes contiguously.
    let second = ChunkFacts {
        mapping_revision: 8,
        ..chunk(1_012_000, 1_010_000, None)
    };
    assert_eq!(scheduler.decide(second), Ok(write(0, 0)));
    let snapshot = scheduler.snapshot();
    assert!(snapshot.anchored);
    assert_eq!(snapshot.mapping_revision, Some(8));
    assert_eq!(snapshot.start_error_us, Some(-2_000));
    assert_eq!(
        (snapshot.inserted_silence_frames, snapshot.dropped_frames),
        (0, 0)
    );
}

#[test]
fn monotonic_clamp_never_decreases() {
    let mut clamp = MonotonicClamp::default();
    let outputs = [10, 5, 12, 11, -3, i64::MAX, i64::MIN].map(|value| clamp.clamp(value));
    assert_eq!(outputs, [10, 10, 12, 12, 12, i64::MAX, i64::MAX]);
    assert_eq!(MonotonicClamp::default().clamp(-9), -9);
}

#[test]
fn short_gap_becomes_silence_and_long_gap_anchors_again() {
    let mut scheduler = locked(RATE);
    // 4_800 frames are exactly 100 ms. The first frame leaves after that silence.
    let short = ChunkFacts {
        gap_frames: 4_800,
        ..chunk(1_200_000, 1_100_000, None)
    };
    assert_eq!(scheduler.decide(short), Ok(write(4_800, 0)));
    let snapshot = scheduler.snapshot();
    assert!(snapshot.anchored);
    assert_eq!(snapshot.inserted_silence_frames, 4_800);
    assert_eq!(snapshot.start_error_us, Some(0));
    // One frame more than 100 ms anchors again against the chunk's own target.
    let long = ChunkFacts {
        gap_frames: 4_801,
        ..chunk(2_000_000, 1_900_000, None)
    };
    assert_eq!(
        scheduler.decide(long),
        Ok(Decision::Wait {
            until_us: 1_995_000
        })
    );
    let snapshot = scheduler.snapshot();
    assert!(!snapshot.anchored);
    assert_eq!(snapshot.start_error_us, None);
    assert_eq!(snapshot.last_error_us, Some(0));
    assert_eq!(snapshot.inserted_silence_frames, 4_800);
}

#[test]
fn errors_track_start_last_and_maximum_across_a_reset() {
    let mut scheduler = locked(RATE);
    assert_eq!(scheduler.snapshot().start_error_us, None);
    assert_eq!(scheduler.snapshot().last_error_us, None);
    for (target_us, now_us) in [(10_000, 10_300), (20_000, 19_300), (30_000, 30_200)] {
        assert_eq!(
            scheduler.decide(chunk(target_us, now_us, None)),
            Ok(write(0, 0))
        );
    }
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.start_error_us, Some(300));
    assert_eq!(snapshot.last_error_us, Some(200));
    assert_eq!(snapshot.max_abs_error_us, 700);

    scheduler.reset();
    let snapshot = scheduler.snapshot();
    assert!(!snapshot.anchored);
    assert_eq!(snapshot.start_error_us, None);
    assert_eq!(snapshot.last_error_us, Some(200));
    assert_eq!(snapshot.max_abs_error_us, 700);

    // Anchoring pads 48 frames (1 ms) and records no error of its own.
    assert_eq!(
        scheduler.decide(chunk(41_000, 40_000, None)),
        Ok(write(48, 0))
    );
    assert_eq!(scheduler.snapshot().start_error_us, None);
    assert_eq!(
        scheduler.decide(chunk(51_000, 50_950, None)),
        Ok(write(0, 0))
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.start_error_us, Some(-50));
    assert_eq!(snapshot.last_error_us, Some(-50));
    assert_eq!(snapshot.max_abs_error_us, 700);
    assert_eq!(snapshot.inserted_silence_frames, 48);
    assert_eq!(snapshot.correction.as_str(), "none");
    assert_eq!(snapshot.qualification.as_str(), "unqualified");
}

#[test]
fn locked_error_rounds_to_the_nearest_microsecond_away_from_zero() {
    // One delay frame at 48 kHz is 20.8333 us.
    let mut scheduler = locked(RATE);
    scheduler.decide(chunk(10_000, 10_000, Some(1))).unwrap();
    assert_eq!(scheduler.snapshot().last_error_us, Some(21));
    scheduler.decide(chunk(20_000, 19_958, Some(1))).unwrap();
    assert_eq!(scheduler.snapshot().last_error_us, Some(-21)); // -21.1667
    // One delay frame at 400 kHz is 2.5 us: both ties leave zero.
    let mut scheduler = locked(400_000);
    scheduler.decide(chunk(10_000, 10_000, Some(1))).unwrap();
    assert_eq!(scheduler.snapshot().last_error_us, Some(3));
    scheduler.decide(chunk(20_000, 19_995, Some(1))).unwrap();
    assert_eq!(scheduler.snapshot().last_error_us, Some(-3)); // -2.5
}

#[test]
fn zero_rate_and_overflow_return_errors() {
    assert_eq!(Scheduler::new(0).err(), Some(ScheduleError::ZeroSampleRate));

    // At 1 Hz, u64::MAX delay frames are far more microseconds than i64 holds.
    let mut scheduler = locked(1);
    let before = scheduler.snapshot();
    assert_eq!(
        scheduler.decide(chunk(0, 0, Some(u64::MAX))),
        Err(ScheduleError::Overflow)
    );
    // A failed decision changes nothing.
    assert_eq!(scheduler.snapshot(), before);

    // The dropped-frame counter is full after one drop, so the next drop overflows it.
    let mut scheduler = Scheduler::new(u32::MAX).unwrap();
    let hopeless = ChunkFacts {
        frames: u64::MAX,
        ..chunk(i64::MIN, i64::MAX, Some(u64::MAX))
    };
    assert_eq!(scheduler.decide(hopeless), Ok(Decision::DropChunk));
    assert_eq!(scheduler.snapshot().dropped_frames, u64::MAX);
    let before = scheduler.snapshot();
    assert_eq!(
        scheduler.decide(ChunkFacts {
            frames: 1,
            ..hopeless
        }),
        Err(ScheduleError::Overflow)
    );
    assert_eq!(scheduler.snapshot(), before);
}
