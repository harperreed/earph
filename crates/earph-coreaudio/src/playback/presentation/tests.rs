// ABOUTME: Verifies conservative handoff decisions from paired SDK clock observations.
// ABOUTME: Covers the measured final-buffer gap and startup epoch disagreement.
use super::*;
fn payload() -> PayloadBuffer {
    PayloadBuffer {
        delivered_frames: 36352,
        start: SampleClock {
            sample_frames: 34704.0,
            host_ticks: 10_000_000,
        },
        end_sample_frames: 35216.0,
    }
}
fn start() -> SampleClock {
    payload().start
}
fn end() -> SampleClock {
    SampleClock {
        sample_frames: 35216.0,
        host_ticks: 10_256_000,
    }
}
fn current() -> SampleClock {
    SampleClock {
        sample_frames: 34970.0,
        host_ticks: 10_133_000,
    }
}
#[test]
fn two_more_callbacks_do_not_prove_the_last_buffer_crossed_the_device_clock() {
    assert_eq!(
        scheduled_handoff(payload(), 36352, current(), start(), end(), current()),
        Some(false)
    );
    assert_eq!(
        scheduled_handoff(payload(), 36352, end(), start(), end(), end()),
        Some(true)
    );
    let host_only = SampleClock {
        sample_frames: 35215.0,
        host_ticks: 10_256_000,
    };
    assert_eq!(
        scheduled_handoff(payload(), 36352, host_only, start(), end(), host_only),
        Some(false)
    );
}
#[test]
fn epoch_disagreement_invalid_translation_and_wrong_delivery_totals_are_unknown() {
    assert_eq!(
        scheduled_handoff(payload(), 36353, current(), start(), end(), current()),
        None
    );
    let wrong_epoch = SampleClock {
        sample_frames: 30_875_409_907.0,
        host_ticks: 10_133_000,
    };
    let translated_wrong = SampleClock {
        sample_frames: wrong_epoch.sample_frames,
        host_ticks: 99_000_000,
    };
    assert_eq!(
        scheduled_handoff(
            payload(),
            36352,
            wrong_epoch,
            start(),
            end(),
            translated_wrong
        ),
        None
    );
    let shifted_start = SampleClock {
        sample_frames: 34704.0,
        host_ticks: 9_999_000,
    };
    assert_eq!(
        scheduled_handoff(payload(), 36352, current(), shifted_start, end(), current()),
        None
    );
    let wrong_end = SampleClock {
        sample_frames: 35217.0,
        ..end()
    };
    assert_eq!(
        scheduled_handoff(payload(), 36352, current(), start(), wrong_end, current()),
        None
    );
    let nonfinite = SampleClock {
        sample_frames: f64::NAN,
        ..current()
    };
    assert_eq!(
        scheduled_handoff(payload(), 36352, nonfinite, start(), end(), current()),
        None
    );
    let backwards = SampleClock {
        host_ticks: 9_999_999,
        ..end()
    };
    assert_eq!(
        scheduled_handoff(payload(), 36352, current(), start(), backwards, current()),
        None
    );
}
