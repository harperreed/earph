// ABOUTME: Checks the Sendspin scheduling decisions against hand-computed frame and microsecond values.
// ABOUTME: Covers anchoring, locked error statistics, gaps, the monotonic clamp, idle silence pacing, and numeric faults.
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

#[test]
fn anchors_count_each_anchored_write_and_no_wait_or_dropped_chunk() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    // A wait and a wholly late chunk take no anchor.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 900_000, None)),
        Ok(Decision::Wait { until_us: 995_000 })
    );
    assert_eq!(
        scheduler.decide(chunk(0, 1_000_000, None)),
        Ok(Decision::DropChunk)
    );
    assert_eq!(scheduler.snapshot().anchors, 0);
    assert_eq!(scheduler.decide(chunk(0, 0, None)), Ok(write(0, 0)));
    assert_eq!(scheduler.snapshot().anchors, 1);
    // A locked chunk and a short gap keep the first anchor.
    let short = ChunkFacts {
        gap_frames: 480,
        ..chunk(20_000, 10_000, None)
    };
    assert_eq!(scheduler.decide(short), Ok(write(480, 0)));
    assert_eq!(scheduler.snapshot().anchors, 1);
    // A reset alone takes no anchor. The next written chunk does, padded or trimmed.
    scheduler.reset();
    assert_eq!(scheduler.snapshot().anchors, 1);
    assert_eq!(scheduler.decide(chunk(1_000, 0, None)), Ok(write(48, 0)));
    assert_eq!(scheduler.snapshot().anchors, 2);
    let long = ChunkFacts {
        gap_frames: 4_801,
        ..chunk(2_000_000, 2_001_000, None)
    };
    assert_eq!(scheduler.decide(long), Ok(write(0, 48)));
    assert_eq!(scheduler.snapshot().anchors, 3);
}

/// Keeps 30 ms in an output that plays 48 frames each millisecond.
#[test]
fn first_lead_is_the_head_start_of_the_first_chunk_after_each_reset_before_the_delay() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, None);
    assert_eq!(snapshot.min_first_lead_us, None);
    assert_eq!(snapshot.first_wait_us, None);
    assert_eq!(snapshot.max_first_wait_us, 0);
    // 100 ms of head start. The 4_800 delay frames use all of it, and the first lead excludes them.
    assert_eq!(
        scheduler.decide(chunk(1_000_000, 900_000, Some(4_800))),
        Ok(write(0, 0))
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(100_000));
    assert_eq!(snapshot.min_first_lead_us, Some(100_000));
    // No sight preceded this decision, so it waited for nothing.
    assert_eq!(snapshot.first_wait_us, Some(0));
    // A locked chunk changes nothing.
    assert_eq!(
        scheduler.decide(chunk(1_010_000, 950_000, Some(2_880))),
        Ok(write(0, 0))
    );
    assert_eq!(scheduler.snapshot().first_lead_us, Some(100_000));
    // A reset keeps the last value until the next chunk replaces it.
    scheduler.reset();
    assert_eq!(scheduler.snapshot().first_lead_us, Some(100_000));
    // A late first chunk has a negative head start. Only the first decision counts.
    assert_eq!(
        scheduler.decide(chunk(2_000_000, 2_030_000, None)),
        Ok(Decision::DropChunk)
    );
    assert_eq!(
        scheduler.decide(chunk(2_050_000, 2_030_000, None)),
        Ok(Decision::Wait {
            until_us: 2_045_000
        })
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(-30_000));
    assert_eq!(snapshot.min_first_lead_us, Some(-30_000));
    // A larger head start replaces the last value and keeps the minimum.
    scheduler.reset();
    assert_eq!(
        scheduler.decide(chunk(3_000_000, 2_640_000, None)),
        Ok(Decision::Wait {
            until_us: 2_995_000
        })
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(360_000));
    assert_eq!(snapshot.min_first_lead_us, Some(-30_000));
}

#[test]
fn a_sight_records_the_head_start_before_the_first_decision_and_the_wait_between_them() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    // The chunk arrives 360 ms early. The output starts for 400 ms, so no decision runs meanwhile.
    scheduler.sight(1_360_000, 1_000_000);
    // A later sight of the waiting chunk changes nothing.
    scheduler.sight(1_360_000, 1_200_000);
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(360_000));
    assert_eq!(snapshot.first_wait_us, None);
    // The first decision finds the chunk 40 ms late: 1_920 of its frames are gone.
    assert_eq!(
        scheduler.decide(ChunkFacts {
            frames: 4_800,
            ..chunk(1_360_000, 1_400_000, None)
        }),
        Ok(write(0, 1_920))
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(360_000));
    assert_eq!(snapshot.first_wait_us, Some(400_000));
    assert_eq!(snapshot.max_first_wait_us, 400_000);
    // A shorter wait after the next reset replaces the last value and keeps the maximum.
    scheduler.reset();
    scheduler.sight(2_300_000, 2_000_000);
    assert_eq!(
        scheduler.decide(chunk(2_300_000, 2_010_000, None)),
        Ok(Decision::Wait {
            until_us: 2_295_000
        })
    );
    // Only the first decision after the sight measures the wait.
    assert_eq!(
        scheduler.decide(chunk(2_300_000, 2_295_000, None)),
        Ok(write(240, 0))
    );
    let snapshot = scheduler.snapshot();
    assert_eq!(snapshot.first_lead_us, Some(300_000));
    assert_eq!(snapshot.min_first_lead_us, Some(300_000));
    assert_eq!(snapshot.first_wait_us, Some(10_000));
    assert_eq!(snapshot.max_first_wait_us, 400_000);
}

#[test]
fn a_long_gap_and_an_extreme_lead_also_record_a_first_lead() {
    let mut scheduler = locked(RATE);
    assert_eq!(scheduler.snapshot().first_lead_us, Some(0));
    // A long gap anchors again, so its chunk is the first of a new attempt.
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
    assert_eq!(scheduler.snapshot().first_lead_us, Some(100_000));
    // A head start beyond the integer range saturates and fails no decision.
    scheduler.reset();
    assert_eq!(
        scheduler.decide(chunk(i64::MAX, i64::MIN, None)),
        Ok(Decision::Wait {
            until_us: i64::MAX - 5_000
        })
    );
    assert_eq!(scheduler.snapshot().first_lead_us, Some(i64::MAX));
    assert_eq!(scheduler.snapshot().min_first_lead_us, Some(0));
}

fn pacer() -> SilencePacer {
    SilencePacer::new(RATE, 1_440).unwrap()
}

#[test]
fn idle_silence_fills_an_empty_output_to_the_depth_and_then_follows_the_clock() {
    let mut pacer = pacer();
    assert_eq!(pacer.owed(0, None, false), 1_440);
    pacer.accepted(0, 1_440);
    assert_eq!(pacer.owed(0, None, false), 0);
    // 10 ms plays 480 frames.
    assert_eq!(pacer.owed(10_000, None, false), 480);
    // 19_999 us remain: 959.952 frames round up to 960 held frames.
    assert_eq!(pacer.owed(10_001, None, false), 480);
    // 19_979 us remain: 958.992 frames round up to 959 held frames.
    assert_eq!(pacer.owed(10_021, None, false), 481);
    pacer.accepted(10_000, 480);
    assert_eq!(pacer.owed(10_000, None, false), 0);
    // An output that played everything is empty, however long ago that was.
    assert_eq!(pacer.owed(5_000_000, None, false), 1_440);
    pacer.accepted(5_000_000, 100);
    assert_eq!(pacer.owed(5_000_000, None, false), 1_340);
    assert_eq!(
        SilencePacer::new(0, 1_440).err(),
        Some(ScheduleError::ZeroSampleRate)
    );
}

#[test]
fn audio_written_ahead_of_the_clock_owes_no_idle_silence_until_it_has_played() {
    let mut pacer = pacer();
    // One second of audio reaches an output without a clock at once.
    pacer.accepted(0, 48_000);
    assert_eq!(pacer.owed(900_000, None, false), 0);
    assert_eq!(pacer.owed(970_000, None, false), 0);
    assert_eq!(pacer.owed(980_000, None, false), 480);
    assert_eq!(pacer.owed(1_000_000, None, false), 1_440);
}

#[test]
fn a_reported_depth_corrects_the_estimate_and_a_zero_report_does_not() {
    let mut pacer = pacer();
    pacer.accepted(0, 1_440);
    // The output started late and still holds 80 ms.
    pacer.correct(0, 3_840);
    assert_eq!(pacer.owed(50_000, None, false), 0);
    assert_eq!(pacer.owed(60_000, None, false), 480);
    // The output clock ran fast and holds only 10 ms.
    pacer.correct(60_000, 480);
    assert_eq!(pacer.owed(60_000, None, false), 960);
    // An output without a clock reports zero right after a write. Only the clock paces it.
    pacer.accepted(60_000, 960);
    pacer.correct(60_000, 0);
    assert_eq!(pacer.owed(60_000, None, false), 0);
    pacer.reset();
    assert_eq!(pacer.owed(60_000, None, false), 1_440);
}

#[test]
fn idle_silence_fits_before_the_limit_of_a_waiting_head_chunk() {
    let pacer = pacer();
    // 5 ms fit 240 frames.
    assert_eq!(pacer.owed(1_000, Some(6_000), false), 240);
    // 20 us fit 0.96 frames, so no whole frame fits.
    assert_eq!(pacer.owed(1_000, Some(1_020), false), 0);
    assert_eq!(pacer.owed(1_000, Some(1_021), false), 1);
    assert_eq!(pacer.owed(1_000, Some(500), false), 0);
    assert_eq!(pacer.owed(1_000, Some(10_000_000), false), 1_440);
    assert_eq!(pacer.owed(i64::MIN, Some(i64::MAX), false), 1_440);
}

#[test]
fn contiguous_audio_gets_idle_silence_only_below_two_thirds_of_the_depth() {
    let mut pacer = pacer();
    pacer.accepted(0, 1_440);
    // A short dip between two chunks moves no audio.
    assert_eq!(pacer.owed(5_000, None, true), 0);
    // 960 held frames are exactly two thirds.
    assert_eq!(pacer.owed(10_000, None, true), 0);
    assert_eq!(pacer.owed(10_000, None, false), 480);
    // 958.992 frames round up to 959 held frames, so the output starves.
    assert_eq!(pacer.owed(10_021, None, true), 481);
    assert_eq!(pacer.owed(10_021, Some(10_021 + 5_000), true), 240);
}

#[test]
fn a_reported_device_latency_within_the_limit_joins_the_counted_delay() {
    let latency = LatencyAdmission::new(RATE, Some(9_600));
    assert_eq!(latency.reported_frames(), Some(9_600));
    assert!(latency.applied());
    // The output holds 1_536 frames, and the device reports 200 ms behind them.
    assert_eq!(latency.delay_frames(Some(1_536)), Some(11_136));
    assert_eq!(latency.delay_frames(Some(0)), Some(9_600));
    // An unknown counted delay stays unknown.
    assert_eq!(latency.delay_frames(None), None);
    assert_eq!(latency.delay_frames(Some(u64::MAX)), Some(u64::MAX));
    // One second is the limit, and the limit itself applies.
    assert!(LatencyAdmission::new(RATE, Some(48_000)).applied());
    // A device that reports zero latency adds nothing, and the report still applies.
    let zero = LatencyAdmission::new(RATE, Some(0));
    assert!(zero.applied());
    assert_eq!(zero.delay_frames(Some(7)), Some(7));
}

#[test]
fn an_unknown_or_absurd_device_latency_is_reported_and_never_applied() {
    let unknown = LatencyAdmission::new(RATE, None);
    assert_eq!(unknown.reported_frames(), None);
    assert!(!unknown.applied());
    assert_eq!(unknown.delay_frames(Some(5)), Some(5));
    assert_eq!(unknown.lead_time_ms(), 0);
    // One frame beyond one second.
    for absurd in [48_001, u64::MAX] {
        let latency = LatencyAdmission::new(RATE, Some(absurd));
        assert_eq!(latency.reported_frames(), Some(absurd));
        assert!(!latency.applied());
        assert_eq!(latency.delay_frames(Some(5)), Some(5));
        assert_eq!(latency.delay_frames(None), None);
        assert_eq!(latency.lead_time_ms(), 0);
    }
    // 44_101 frames exceed one second at 44.1 kHz and fit inside it at 48 kHz.
    assert!(!LatencyAdmission::new(44_100, Some(44_101)).applied());
    assert!(LatencyAdmission::new(RATE, Some(44_101)).applied());
    // No rate turns frames into time, so nothing applies.
    assert!(!LatencyAdmission::new(0, Some(1)).applied());
    assert_eq!(LatencyAdmission::new(0, Some(1)).lead_time_ms(), 0);
}

#[test]
fn the_applied_device_latency_rounds_up_to_whole_lead_milliseconds() {
    let lead = |frames| LatencyAdmission::new(RATE, Some(frames)).lead_time_ms();
    assert_eq!(lead(0), 0);
    assert_eq!(lead(1), 1);
    assert_eq!(lead(48), 1);
    // 49 frames last 1.0208 ms.
    assert_eq!(lead(49), 2);
    assert_eq!(lead(9_600), 200);
    assert_eq!(lead(48_000), 1_000);
}

#[test]
fn a_device_latency_longer_than_the_server_lead_drops_audio_until_the_lead_covers_it() {
    let mut scheduler = Scheduler::new(RATE).unwrap();
    let latency = LatencyAdmission::new(RATE, Some(9_600));
    // The output holds 32 ms of idle silence, and the device reports 200 ms: 232 ms of delay.
    let delay = latency.delay_frames(Some(1_536));
    // A chunk that arrives 150 ms ahead is 82 ms late, so every such chunk drops.
    assert_eq!(
        scheduler.decide(chunk(150_000, 0, delay)),
        Ok(Decision::DropChunk)
    );
    assert_eq!(scheduler.snapshot().dropped_frames, 480);
    assert!(!scheduler.snapshot().anchored);
    // A lead of 150 ms plus the latency leaves 118 ms: wait until 5 ms remain.
    let ahead_us = (150 + latency.lead_time_ms() as i64) * 1_000;
    assert_eq!(ahead_us, 350_000);
    assert_eq!(
        scheduler.decide(chunk(ahead_us, 0, delay)),
        Ok(Decision::Wait { until_us: 113_000 })
    );
    // At the wake time the chunk anchors behind 240 frames of silence and drops nothing more.
    assert_eq!(
        scheduler.decide(chunk(ahead_us, 113_000, delay)),
        Ok(write(240, 0))
    );
    assert!(scheduler.snapshot().anchored);
    assert_eq!(scheduler.snapshot().dropped_frames, 480);
}
