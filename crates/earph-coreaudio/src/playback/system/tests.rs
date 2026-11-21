// ABOUTME: Exercises real bounded producer storage, slot reuse, and queue lifecycle state.
// ABOUTME: Rejects acquisition-based progress and invalid sample-time restart frontiers.
use super::super::tests::{block, producer};
use super::*;
#[test]
fn partial_slots_reuse_only_after_acquisition_and_do_not_advance_restart_clock() {
    let slots = Slots::new(5, 2, FAULT_SIGNALS.reserve().unwrap());
    assert_eq!(&*slots.capacities, &[2, 2, 1]);
    assert!(slots.claim(0, 2).unwrap());
    assert!(!slots.claim(0, 1).unwrap());
    assert!(slots.claim(2, 1).unwrap());
    assert_eq!(slots.pending(), 3);
    slots.release(0);
    assert_eq!(slots.acquired.load(Ordering::Acquire), 2);
    assert_eq!(slots.pending(), 1);
    assert!(slots.claim(0, 1).unwrap());
    assert!(slots.claim(2, 2).is_err());
    assert_eq!(Clock::new().observe(Some(0.0), false).unwrap(), Some(0));
}
#[test]
fn duplicate_release_and_counter_overflow_latch_faults() {
    let slots = Slots::new(2, 2, FAULT_SIGNALS.reserve().unwrap());
    slots.release(0);
    assert!(slots.fault.is_set());
    let slots = Slots::new(2, 2, FAULT_SIGNALS.reserve().unwrap());
    slots.claim(0, 1).unwrap();
    slots.acquired.store(u64::MAX, Ordering::Release);
    slots.release(0);
    assert!(slots.fault.is_set());
    assert_eq!(slots.acquired.load(Ordering::Acquire), u64::MAX);
}
#[test]
fn clock_keeps_strict_queue_time_separate_from_frozen_payload_prefix() {
    let clock = Clock::new();
    assert_eq!(clock.observe(Some(44.9), false).unwrap(), Some(44));
    assert_eq!(clock.observe(None, false).unwrap(), None);
    assert!(clock.observe(Some(43.0), false).is_err());
    let readiness = Readiness::new();
    readiness.frozen_prefix.set(Some(44));
    assert_eq!(readiness.frozen_prefix.get(), Some(44));
    assert!(clock.observe(Some(99.0), true).is_err());
}
#[test]
fn queue_time_can_exceed_payload_and_raw_fractional_regressions_stay_strict() {
    let clock = Clock::new();
    assert_eq!(clock.observe(Some(3293.0), false).unwrap(), Some(3293));
    assert_eq!(clock.observe(Some(3293.9), false).unwrap(), Some(3293));
    assert!(clock.observe(Some(3293.8), false).is_err());
}
#[test]
fn schedule_maps_nonzero_origins_and_gaps_to_whole_payload_spans() {
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(256, epochs);
    schedule.append(epochs, 0, 64, 100.0, 1).unwrap();
    schedule.append(epochs, 64, 64, 1000.0, 1).unwrap();
    assert_eq!(schedule.retire(epochs, 163).unwrap(), 0);
    assert_eq!(schedule.retire(epochs, 164).unwrap(), 64);
    assert_eq!(schedule.retire(epochs, 999).unwrap(), 64);
    assert_eq!(schedule.retire(epochs, 1063).unwrap(), 64);
    assert_eq!(schedule.retire(epochs, 3293).unwrap(), 128);
    assert_eq!(schedule.enqueued, 128);
    assert_eq!(schedule.retired, 128);
}
#[test]
fn schedule_rejects_invalid_spans_without_committing_payload_or_overwriting_history() {
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(2, epochs);
    schedule.append(epochs, 0, 1, 100.0, 1).unwrap();
    for (start, frames, sample) in [(2, 1, 101.0), (1, 0, 101.0), (1, 2, 101.0)] {
        assert!(schedule.append(epochs, start, frames, sample, 1).is_err());
        assert_eq!(schedule.enqueued, 1);
        assert_eq!(schedule.len, 1);
    }
    assert!(
        schedule
            .append(epochs.next_stream().unwrap(), 1, 1, 101.0, 1)
            .is_err()
    );
    schedule.append(epochs, 1, 1, 200.0, 1).unwrap();
    assert!(!schedule.has_room());
    assert!(schedule.admit(epochs, 2, 1).is_err());
    assert_eq!(schedule.enqueued, 2);
    assert_eq!(schedule.retire(epochs, 101).unwrap(), 1);
    assert!(schedule.has_room());
    schedule.append(epochs, 2, 1, 201.0, 1).unwrap();
    assert_eq!(schedule.retire(epochs, 202).unwrap(), 3);
    assert!(schedule.retire(epochs.next_stream().unwrap(), 202).is_err());
    let mut exhausted = Schedule::new(2, epochs);
    exhausted.enqueued = u64::MAX;
    exhausted.retired = u64::MAX;
    assert!(exhausted.admit(epochs, u64::MAX, 1).is_err());
}
#[test]
fn accepted_native_enqueue_keeps_its_count_when_returned_schedule_faults() {
    let epochs = Epochs::default();
    for (sample, flags) in [
        (100.5, 1),
        (-522.0, 1),
        (f64::NAN, 1),
        (f64::INFINITY, 1),
        (9007199254740991.0, 1),
        (101.0, 0),
    ] {
        let mut schedule = Schedule::new(4, epochs);
        schedule.append(epochs, 0, 1, 100.0, 1).unwrap();
        schedule.admit(epochs, 1, 1).unwrap();
        // The caller invokes append only after the SDK has accepted this admitted buffer.
        assert!(schedule.append(epochs, 1, 1, sample, flags).is_err());
        assert_eq!(schedule.enqueued, 2);
        assert_eq!(schedule.retired, 0);
        assert_eq!(schedule.len, 1);
        assert_eq!(schedule.spans[schedule.head].unwrap().payload_end, 1);
        let returned = schedule.returned.unwrap();
        assert_eq!(returned.sample_time_bits, Some(sample.to_bits()));
        assert_eq!(returned.flags, flags);
        assert_eq!(returned.payload_start, 1);
    }
}
#[test]
fn fractional_schedules_retire_only_at_conservative_whole_clock_thresholds() {
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(256, epochs);
    schedule.append(epochs, 0, 64, 1e-20, 1).unwrap();
    assert_eq!(schedule.enqueued, 64);
    assert_eq!(schedule.retired, 0);
    assert_eq!(schedule.retire(epochs, 64).unwrap(), 0);
    assert_eq!(schedule.retire(epochs, 65).unwrap(), 64);
    let mut schedule = Schedule::new(256, epochs);
    schedule.append(epochs, 0, 64, 0.5, 1).unwrap();
    assert_eq!(schedule.retire(epochs, 64).unwrap(), 0);
    assert_eq!(schedule.retire(epochs, 65).unwrap(), 64);
    let mut schedule = Schedule::new(4, epochs);
    schedule.append(epochs, 0, 2, 0.1, 1).unwrap();
    assert_eq!(schedule.retire(epochs, 2).unwrap(), 0);
    assert_eq!(schedule.retire(epochs, 3).unwrap(), 2);
    schedule.append(epochs, 2, 2, 2.1, 1).unwrap();
    assert_eq!(schedule.retire(epochs, 4).unwrap(), 2);
    assert_eq!(schedule.retire(epochs, 5).unwrap(), 4);
    let mut overlap = Schedule::new(4, epochs);
    overlap.append(epochs, 0, 2, 0.1, 1).unwrap();
    assert!(overlap.append(epochs, 2, 2, 2.09, 1).is_err());
    assert_eq!(overlap.enqueued, 4);
    assert_eq!(overlap.retired, 0);
    assert_eq!(overlap.len, 1);
}
#[test]
fn callback_reuse_keeps_fractional_geometry_and_tiny_eof_schedules_bounded() {
    let epochs = Epochs::default();
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    let slots = Slots::new(5, 2, output.ring.fault);
    let mut schedule = Schedule::new(5, epochs);
    output
        .write(&block(0, &[1, -1, 2, -2, 3, -3, 4, -4]), 0, 4)
        .unwrap();
    output.finish().unwrap();
    let mut bytes = [0; 16];
    for frame in 0..4 {
        assert!(schedule.has_room());
        let credit = enqueue_credit(schedule.enqueued, schedule.retired, 5).unwrap();
        assert_eq!(slots.fill(2, &output.ring, &mut bytes, credit).unwrap(), 1);
        schedule
            .append(epochs, frame, 1, 100.0 + frame as f64, 1)
            .unwrap();
        slots.release(2);
        assert_eq!(schedule.retired, 0);
    }
    assert_eq!(schedule.len, 4);
    assert_eq!(slots.pending(), 0);
    assert_eq!(schedule.retire(epochs, 102).unwrap(), 2);
    assert_eq!(schedule.len, 2);
    assert_eq!(schedule.retire(epochs, 3293).unwrap(), 4);
}
#[test]
fn pause_freezes_mapped_payload_and_resume_keeps_the_running_clock_origin() {
    let epochs = Epochs::default();
    let clock = Clock::new();
    let mut schedule = Schedule::new(256, epochs);
    schedule.append(epochs, 0, 64, 1000.0, 1).unwrap();
    schedule.append(epochs, 64, 64, 2000.0, 1).unwrap();
    let running = clock.observe(Some(1065.5), false).unwrap().unwrap();
    let frozen = schedule.retire(epochs, running).unwrap();
    assert_eq!(frozen, 64);
    let readiness = Readiness::new();
    readiness.frozen_prefix.set(Some(frozen));
    // A post-pause diagnostic sample cannot retire the still-scheduled second buffer.
    clock.record_read(3293.0, 1, false, 128);
    assert_eq!(readiness.frozen_prefix.get(), Some(64));
    assert_eq!(schedule.retired, 64);
    readiness.frozen_prefix.set(None);
    assert!(clock.observe(Some(1065.25), false).is_err());
    assert!(clock.observe(Some(2064.0), true).is_err());
    let resumed = clock.observe(Some(2064.0), false).unwrap().unwrap();
    assert_eq!(schedule.retire(epochs, resumed).unwrap(), 128);
}
#[test]
fn natural_drain_retires_short_eof_without_reading_the_reset_clock_and_cancel_cannot_finish() {
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(4, epochs);
    let clock = Clock::new();
    schedule.append(epochs, 0, 3, 1000.0, 1).unwrap();
    let raw = clock.observe(Some(1002.0), false).unwrap().unwrap();
    assert_eq!(schedule.retire(epochs, raw).unwrap(), 0);
    let drain = Drain::new();
    drain.observe(false);
    assert!(drain.request());
    assert_eq!(schedule.drain_frontier(&drain, true), Some(Some(0)));
    drain.observe(false);
    assert_eq!(schedule.drain_frontier(&drain, false), Some(None));
    drain.observe(true);
    drain.observe(false);
    assert_eq!(schedule.drain_frontier(&drain, false), Some(Some(3)));
    assert_eq!(clock.raw_last.get(), Some(1002.0));
    assert_eq!(schedule.len, 0);
    // A reset-to-zero clock would fail active continuity; the completed drain never reads it.
    assert!(clock.observe(Some(0.0), false).is_err());
    let mut cancelled = Schedule::new(4, epochs);
    cancelled.append(epochs, 0, 3, 1000.0, 1).unwrap();
    let drain = Drain::new();
    drain.observe(true);
    drain.request();
    drain.cancel();
    drain.observe(false);
    assert_eq!(cancelled.drain_frontier(&drain, true), None);
    assert_eq!(cancelled.retired, 0);
}
#[test]
#[cfg(not(target_os = "macos"))]
fn schedule_retirement_and_reuse_allocate_nothing_after_preparation() {
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(2, epochs);
    let (_, counts) = super::super::tests::allocations::tracked(|| {
        for frame in 0..10 {
            schedule
                .append(epochs, frame, 1, 100.0 + frame as f64, 1)
                .unwrap();
            assert_eq!(schedule.retire(epochs, 101 + frame).unwrap(), frame + 1);
        }
    });
    assert_eq!(
        counts,
        super::super::tests::allocations::AllocationCounts::default()
    );
}
#[test]
fn clock_rejects_invalid_or_discontinuous_queue_time() {
    for sample in [f64::NAN, f64::INFINITY, -1.0, u64::MAX as f64] {
        assert!(Clock::new().observe(Some(sample), false).is_err());
    }
    assert!(Clock::new().observe(Some(10.0), true).is_err());
}
#[test]
fn drain_requires_request_and_observed_running_to_stopped_transition() {
    let drain = Drain::new();
    drain.observe(false);
    assert!(drain.request());
    assert!(!drain.request());
    drain.observe(false);
    assert!(!drain.complete());
    drain.observe(true);
    assert!(!drain.complete());
    drain.observe(false);
    assert!(drain.complete());
    drain.cancel();
    assert!(!drain.complete());
}
#[test]
fn cancelled_generation_cannot_become_natural_eof() {
    let drain = Drain::new();
    drain.observe(true);
    drain.request();
    drain.cancel();
    drain.observe(false);
    assert!(!drain.complete());
}
#[test]
fn real_producer_retains_short_write_prefix_and_rejects_stale_epochs() {
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    let mut input = block(0, &[1, -1, 2, -2, 3, -3, 4, -4, 5, -5]);
    assert_eq!(
        output.write(&input, 0, 5).unwrap(),
        WriteProgress::Accepted { frames: 4 }
    );
    assert_eq!(
        output.write(&input, 4, 1).unwrap(),
        WriteProgress::WouldBlock
    );
    input
        .set_span(
            Epochs {
                stream: 1,
                ..Epochs::default()
            },
            0,
            4,
            1,
        )
        .unwrap();
    assert_eq!(
        output.write(&input, 0, 1).unwrap_err().code,
        ErrorCode::StaleSession
    );
    assert_eq!(output.submitted, 4);
}

#[test]
fn sdk_staging_reads_real_float_payload_and_keeps_short_tail_until_eof() {
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    let input = block(0, &[8388607, -8388608, 0, 0, 4194304, -4194304]);
    output.write(&input, 0, 3).unwrap();
    let slots = Slots::new(4, 2, output.ring.fault);
    let mut bytes = [0_u8; 16];
    assert_eq!(
        slots.fill(0, &output.ring, &mut bytes, u64::MAX).unwrap(),
        2
    );
    let values: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect();
    assert_eq!(values, [0.9999999, -1.0, 0.0, 0.0]);
    assert_eq!(output.ring.queued_frames(), 1);
    assert_eq!(
        slots.fill(1, &output.ring, &mut bytes, u64::MAX).unwrap(),
        0
    );
    output.finish().unwrap();
    assert_eq!(
        slots.fill(1, &output.ring, &mut bytes, u64::MAX).unwrap(),
        1
    );
    assert_eq!(&bytes[..8], &[0, 0, 0, 63, 0, 0, 0, 191]);
    assert_eq!(output.ring.consumed.load(Ordering::Acquire), 0);
    assert_eq!(slots.pending(), 3);
    assert_eq!(output.ring.queued_frames(), 0);
}

#[test]
#[cfg(not(target_os = "macos"))]
fn slot_callback_accounting_and_clock_observation_allocate_nothing() {
    let slots = Slots::new(4, 2, FAULT_SIGNALS.reserve().unwrap());
    slots.claim(0, 2).unwrap();
    let clock = Clock::new();
    let (_, counts) = super::super::tests::allocations::tracked(|| {
        slots.release(0);
        assert_eq!(clock.observe(Some(1.5), false).unwrap(), Some(1));
    });
    assert_eq!(
        counts,
        super::super::tests::allocations::AllocationCounts::default()
    );
    assert_eq!(slots.acquired.load(Ordering::Acquire), 2);
}

#[test]
fn invalid_staging_and_counter_exhaustion_preserve_unaccepted_pcm() {
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    let input = block(0, &[1, -1]);
    output.submitted = u64::MAX;
    assert_eq!(
        output.write(&input, 0, 1).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(output.ring.queued_frames(), 0);
    output.submitted = 0;
    output.write(&input, 0, 1).unwrap();
    let slots = Slots::new(2, 2, output.ring.fault);
    assert!(slots.fill(0, &output.ring, &mut [0; 1], u64::MAX).is_err());
    assert_eq!(output.ring.queued_frames(), 1);
    assert_eq!(slots.pending(), 0);
}

#[test]
fn independent_buffer_callbacks_publish_complete_bounded_acquisition_totals() {
    let slots = Arc::new(Slots::new(16, 1, FAULT_SIGNALS.reserve().unwrap()));
    for index in 0..16 {
        assert!(slots.claim(index, 1).unwrap());
    }
    let callbacks: Vec<_> = (0..16)
        .map(|index| {
            let slots = slots.clone();
            std::thread::spawn(move || slots.release(index))
        })
        .collect();
    for callback in callbacks {
        callback.join().unwrap();
    }
    assert_eq!(slots.pending(), 0);
    assert_eq!(slots.acquired.load(Ordering::Acquire), 16);
    assert_eq!(slots.callbacks.load(Ordering::Acquire), 16);
    assert!(!slots.fault.is_set());
}

#[test]
fn released_slots_cannot_grow_the_native_playback_window_beyond_its_admitted_capacity() {
    assert_eq!(enqueue_credit(256, 0, 256).unwrap(), 0);
    assert_eq!(enqueue_credit(256, 63, 256).unwrap(), 63);
    assert_eq!(enqueue_credit(256, 256, 256).unwrap(), 256);
    assert!(enqueue_credit(257, 0, 256).is_err());
    assert!(enqueue_credit(10, 11, 256).is_err());
}

#[test]
fn staging_waits_for_client_clock_credit_even_when_a_slot_is_free() {
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    output.write(&block(0, &[1, -1, 2, -2]), 0, 2).unwrap();
    let slots = Slots::new(2, 2, output.ring.fault);
    let mut bytes = [0_u8; 16];
    assert_eq!(slots.fill(0, &output.ring, &mut bytes, 0).unwrap(), 0);
    assert_eq!(output.ring.queued_frames(), 2);
    assert_eq!(slots.fill(0, &output.ring, &mut bytes, 1).unwrap(), 0);
    output.finish().unwrap();
    assert_eq!(slots.fill(0, &output.ring, &mut bytes, 1).unwrap(), 1);
    assert_eq!(output.ring.queued_frames(), 1);
}

#[test]
fn asynchronous_start_waits_for_running_and_first_clock_without_a_guessed_frontier() {
    use std::time::Duration;
    let readiness = Readiness::new();
    assert!(
        !readiness
            .before_clock(false, false, Duration::ZERO)
            .unwrap()
    );
    assert_eq!(readiness.running.get(), Some(false));
    assert_eq!(readiness.status.get(), None);
    assert!(
        readiness
            .before_clock(true, false, Duration::from_millis(2))
            .unwrap()
    );
    assert!(
        !readiness
            .clock_status(-66678, -66678, Duration::from_millis(2), true)
            .unwrap()
    );
    assert!(!readiness.ready.get());
    assert!(
        readiness
            .clock_status(0, -66678, Duration::from_millis(3), true)
            .unwrap()
    );
    assert_eq!(
        readiness
            .sample(Some(0), Duration::from_millis(3), true)
            .unwrap(),
        Some(0)
    );
    assert!(readiness.ready.get());
    assert!(
        readiness
            .clock_status(-66678, -66678, Duration::from_millis(4), true)
            .is_err()
    );
}
#[test]
fn startup_pending_never_resets_the_five_second_deadline_or_suppresses_other_failures() {
    use std::time::Duration;
    let readiness = Readiness::new();
    for millis in [1, 1000, 4999] {
        assert!(
            !readiness
                .clock_status(-66678, -66678, Duration::from_millis(millis), true)
                .unwrap()
        );
    }
    let failure = readiness
        .clock_status(-66678, -66678, Duration::from_secs(5), true)
        .unwrap_err();
    assert_eq!(failure.os_status, Some(-66678));
    assert_eq!(readiness.status.get(), Some(-66678));
    assert!(
        Readiness::new()
            .clock_status(-66680, -66678, Duration::ZERO, true)
            .is_err()
    );
    assert!(
        Readiness::new()
            .clock_status(-66678, -66678, Duration::ZERO, false)
            .is_err()
    );
    assert!(
        Readiness::new()
            .before_clock(false, true, Duration::ZERO)
            .is_err()
    );
}
#[test]
fn valid_clock_that_stops_advancing_faults_without_releasing_more_enqueue_credit() {
    use std::time::Duration;
    let readiness = Readiness::new();
    assert_eq!(
        readiness
            .sample(Some(64), Duration::from_secs(1), true)
            .unwrap(),
        Some(64)
    );
    assert_eq!(
        readiness
            .sample(Some(64), Duration::from_secs(5), true)
            .unwrap(),
        Some(64)
    );
    assert!(
        readiness
            .sample(Some(64), Duration::from_secs(6), true)
            .is_err()
    );
    assert!(
        readiness
            .sample(None, Duration::from_secs(5), true)
            .is_err()
    );
    assert_eq!(enqueue_credit(256, 64, 256).unwrap(), 64);
}

#[test]
fn resume_starts_a_bounded_readiness_episode_without_resetting_the_client_frame_origin() {
    use std::time::Duration;
    let readiness = Readiness::new();
    readiness.ready.set(true);
    readiness.frame.set(128);
    readiness.begin(128);
    assert!(!readiness.ready.get());
    assert_eq!(readiness.frame.get(), 128);
    assert!(
        !readiness
            .before_clock(false, false, Duration::ZERO)
            .unwrap()
    );
    assert!(
        !readiness
            .clock_status(-66678, -66678, Duration::from_millis(1), true)
            .unwrap()
    );
    assert_eq!(
        readiness
            .sample(Some(129), Duration::from_millis(2), true)
            .unwrap(),
        Some(129)
    );
    assert!(readiness.ready.get());
}

#[test]
fn every_nonzero_sdk_running_value_can_advance_clock_readiness() {
    use std::time::Duration;
    assert!(!queue_running(0));
    for raw in [1, 2, u32::MAX] {
        assert!(
            Readiness::new()
                .before_clock(queue_running(raw), false, Duration::ZERO)
                .unwrap()
        );
    }
}

#[test]
fn clock_rejection_classification_distinguishes_invalid_queue_coordinates() {
    for (sample, message) in [
        (f64::NAN, "AudioQueue client clock is non-finite"),
        (f64::INFINITY, "AudioQueue client clock is non-finite"),
        (-1.0, "AudioQueue client clock is negative"),
        (
            9007199254740992.0,
            "AudioQueue client clock exceeds exact whole-frame range",
        ),
    ] {
        let clock = Clock::new();
        let failure = clock.observe(Some(sample), false).unwrap_err();
        assert_eq!(failure.code, ErrorCode::OutputFault);
        assert_eq!(failure.message, message);
        assert_eq!(clock.raw_last.get(), None);
    }
    assert_eq!(Clock::new().observe(Some(100.0), false).unwrap(), Some(100));
}

#[test]
#[cfg(not(target_os = "macos"))]
fn raw_clock_diagnostics_survive_real_pcm_staging_and_lossless_json_serialization() {
    let mut producer = producer(PlaybackRoute::Float32Compatibility);
    producer
        .write(&block(0, &[1, -1, 2, -2, 3, -3, 4, -4]), 0, 4)
        .unwrap();
    let slots = Slots::new(4, 2, producer.ring.fault);
    assert_eq!(slots.fill(0, &producer.ring, &mut [0; 16], 2).unwrap(), 2);
    slots.release(0);
    let output = CoreAudioOutput {
        report: OutputReport {
            requested: producer.request.clone(),
            observed: super::super::tests::device(),
            queue_client_format: None,
            evidence_level: "owned_pcm_model",
            payload_preservation: "modified_integer_to_float32",
            downstream_transformations: "unknown",
        },
        producer,
        state: OutputState::Running,
    };
    let clock = Clock::new();
    clock.begin_read();
    clock.record_read(-3.0, 1, false, 2);
    assert!(clock.observe(Some(-3.0), false).is_err());
    let diagnostics = clock.diagnostics(QueueDiagnostics {
        running: Some(true),
        status: Some(0),
        ready: Some(false),
        enqueued: Some(2),
        ..QueueDiagnostics::default()
    });
    let progress = output.progress().with_queue_diagnostics(diagnostics);
    let bytes = serde_json::to_vec(&progress).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["submitted_frames"], 4);
    assert_eq!(slots.acquired.load(Ordering::Acquire), 2);
    assert_eq!(value["queue_enqueued_frames"], 2);
    assert_eq!(value["queue_clock_observed_enqueued_frames"], 2);
    assert_eq!(
        value["queue_clock_sample_time_bits"].as_u64(),
        Some(0xc008000000000000)
    );
    assert_eq!(value["queue_clock_sample_time_finite"], -3.0);
    assert_eq!(value["queue_clock_flags"], 1);
    assert_eq!(value["queue_clock_discontinuity"], false);
    assert!(value["queue_clock_last_valid_frame"].is_null());
    for bits in [0x7ff8000000000042, 0x7ff0000000000000, 0xfff0000000000000] {
        let sample = f64::from_bits(bits);
        clock.record_read(sample, 1, false, 2);
        let progress = output
            .progress()
            .with_queue_diagnostics(clock.diagnostics(QueueDiagnostics::default()));
        let value: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&progress).unwrap()).unwrap();
        assert_eq!(value["queue_clock_sample_time_bits"].as_u64(), Some(bits));
        assert!(value["queue_clock_sample_time_finite"].is_null());
    }
    clock.observe(Some(1.0), false).unwrap();
    clock.record_read(-1.0, 0, true, 2);
    let diagnostics = clock.diagnostics(QueueDiagnostics::default());
    assert_eq!(diagnostics.last_valid, Some(1));
    assert_eq!(diagnostics.flags, Some(0));
    assert_eq!(diagnostics.discontinuity, Some(true));
    clock.begin_read();
    assert!(
        clock
            .diagnostics(QueueDiagnostics::default())
            .sample_bits
            .is_none()
    );
    assert_eq!(
        clock.diagnostics(QueueDiagnostics::default()).last_valid,
        Some(1)
    );
    let epochs = Epochs {
        session: u64::MAX,
        stream: 17,
    };
    for (bits, flags) in [
        (0xc080500000000000, 1),
        (0x7ff8000000000042, 1),
        (0x7ff0000000000000, 1),
        (0x40a9ba0000000000, 0),
    ] {
        let mut schedule = Schedule::new(4, epochs);
        assert!(
            schedule
                .append(epochs, 0, 2, f64::from_bits(bits), flags)
                .is_err()
        );
        assert_eq!(schedule.enqueued, 2);
        assert_eq!(schedule.retired, 0);
        assert_eq!(schedule.len, 0);
        let progress = output.progress().with_queue_diagnostics(QueueDiagnostics {
            schedule: schedule.returned,
            retired: Some(schedule.retired),
            enqueued: Some(schedule.enqueued),
            ..QueueDiagnostics::default()
        });
        let value: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&progress).unwrap()).unwrap();
        assert_eq!(
            value["queue_schedule"]["sample_time_bits"].as_u64(),
            Some(bits)
        );
        assert_eq!(value["queue_schedule"]["flags"], flags);
        assert_eq!(value["queue_schedule"]["payload_start"], 0);
        assert_eq!(value["queue_schedule"]["frames"], 2);
        assert_eq!(
            value["queue_schedule"]["epochs"]["session"].as_u64(),
            Some(u64::MAX)
        );
        assert_eq!(value["queue_retired_frames"], 0);
        assert_eq!(value["queue_enqueued_frames"], 2);
    }
}

#[test]
fn observed_negative_first_clock_waits_unknown_then_accepts_zero_and_positive_frames() {
    use std::time::Duration;
    let readiness = Readiness::new();
    let clock = Clock::new();
    let epochs = Epochs::default();
    let mut schedule = Schedule::new(256, epochs);
    schedule.append(epochs, 0, 64, 0.0, 1).unwrap();
    let observed = f64::from_bits(0xc080500000000000);
    assert_eq!(observed, -522.0);
    assert!(
        readiness
            .clock_status(0, -66678, Duration::ZERO, true)
            .unwrap()
    );
    clock.record_read(observed, 7, false, 256);
    assert!(
        readiness
            .pre_origin_pending(Some(observed), false, Duration::from_millis(1), true)
            .unwrap()
    );
    assert!(!readiness.ready.get());
    assert_eq!(clock.raw_last.get(), None);
    assert_eq!(
        clock.diagnostics(QueueDiagnostics::default()).last_valid,
        None
    );
    assert_eq!(enqueue_credit(256, schedule.retired, 256).unwrap(), 0);
    assert!(clock.observe(Some(observed), false).is_err());
    for (sample, frame, millis) in [(0.0, 0, 2), (32.75, 0, 3), (64.0, 64, 4)] {
        let elapsed = Duration::from_millis(millis);
        assert!(
            !readiness
                .pre_origin_pending(Some(sample), false, elapsed, true)
                .unwrap()
        );
        let frontier = clock
            .observe(Some(sample), false)
            .unwrap()
            .map(|raw| schedule.retire(epochs, raw).unwrap());
        assert_eq!(
            readiness.sample(frontier, elapsed, true).unwrap(),
            Some(frame)
        );
    }
    assert!(readiness.ready.get());
    assert_eq!(enqueue_credit(256, schedule.retired, 256).unwrap(), 64);
    assert!(
        !readiness
            .pre_origin_pending(Some(-1.0), false, Duration::from_millis(4), true)
            .unwrap()
    );
    assert_eq!(
        clock.observe(Some(-1.0), false).unwrap_err().message,
        "AudioQueue client clock is negative"
    );
    assert_eq!(clock.raw_last.get(), Some(64.0));
}
#[test]
fn advancing_negative_startup_samples_never_extend_the_deadline_or_bypass_other_guards() {
    use std::time::Duration;
    let readiness = Readiness::new();
    for (sample, millis) in [(-522.0, 1), (-20.0, 1000), (-0.25, 4999)] {
        assert!(
            readiness
                .pre_origin_pending(Some(sample), false, Duration::from_millis(millis), true)
                .unwrap()
        );
        assert_eq!(readiness.advanced_at.get(), Duration::ZERO);
        assert_eq!(readiness.frame.get(), 0);
    }
    assert!(
        readiness
            .pre_origin_pending(Some(-0.125), false, Duration::from_secs(5), true)
            .is_err()
    );
    for (sample, discontinuity, startup) in [
        (Some(-522.0), true, true),
        (Some(-522.0), false, false),
        (None, false, true),
        (Some(f64::NEG_INFINITY), false, true),
        (Some(f64::NAN), false, true),
        (Some(9007199254740992.0), false, true),
        (Some(257.0), false, true),
    ] {
        assert!(
            !Readiness::new()
                .pre_origin_pending(sample, discontinuity, Duration::ZERO, startup)
                .unwrap()
        );
    }
    let clock = Clock::new();
    assert!(clock.observe(Some(-522.0), true).is_err());
    assert_eq!(clock.observe(Some(257.0), false).unwrap(), Some(257));
    assert!(clock.observe(Some(f64::NEG_INFINITY), false).is_err());
    assert!(clock.observe(Some(9007199254740992.0), false).is_err());
}

#[test]
fn pre_origin_resume_wait_keeps_the_previous_valid_client_origin_strict() {
    use std::time::Duration;
    let clock = Clock::new();
    assert_eq!(clock.observe(Some(128.0), false).unwrap(), Some(128));
    let readiness = Readiness::new();
    readiness.begin(128);
    assert!(
        readiness
            .pre_origin_pending(Some(-522.0), false, Duration::from_millis(1), true)
            .unwrap()
    );
    assert_eq!(clock.raw_last.get(), Some(128.0));
    assert_eq!(
        clock.diagnostics(QueueDiagnostics::default()).last_valid,
        Some(128)
    );
    assert_eq!(
        enqueue_credit(256, readiness.frame.get(), 256).unwrap(),
        128
    );
    assert!(
        !readiness
            .pre_origin_pending(Some(0.0), false, Duration::from_millis(2), true)
            .unwrap()
    );
    assert!(clock.observe(Some(0.0), false).is_err());
    let frame = clock.observe(Some(129.0), false).unwrap();
    assert_eq!(
        readiness
            .sample(frame, Duration::from_millis(3), true)
            .unwrap(),
        Some(129)
    );
}
