// ABOUTME: Counts CoreAudioOutput adapter entries across native reopens.
// ABOUTME: Keeps diagnostic overflow and close status separate from playback results.
use earph_core::{
    ids::Epochs,
    session::{OutputActivity, OutputActivityBoundary, OutputActivityScope},
};
use earph_coreaudio::{CoreAudioError, WriteProgress};

// Public counter names retain their existing spelling; writes do not count AudioQueue enqueue calls.
pub(super) struct ActivityTracker {
    sample: OutputActivity,
}
impl ActivityTracker {
    pub(super) fn opened(origin_epochs: Epochs) -> Self {
        Self {
            sample: OutputActivity {
                scope: OutputActivityScope::OutputInstanceAcrossNativeReopens,
                measurement_boundary: OutputActivityBoundary::CoreAudioSdkEntryCalls,
                origin_epochs,
                native_open_successes: 1,
                sdk_write_attempts: 0,
                accepted_frames: 0,
                sdk_start_attempts: 0,
                counter_overflow: false,
                close_confirmed: false,
            },
        }
    }
    pub(super) fn snapshot(&self) -> OutputActivity {
        self.sample
    }
    pub(super) fn write(
        &mut self,
        enter: impl FnOnce() -> Result<WriteProgress, CoreAudioError>,
    ) -> Result<WriteProgress, CoreAudioError> {
        accumulate(
            &mut self.sample.sdk_write_attempts,
            1,
            &mut self.sample.counter_overflow,
        );
        let result = enter();
        if let Ok(WriteProgress::Accepted { frames }) = result {
            accumulate(
                &mut self.sample.accepted_frames,
                frames as u64,
                &mut self.sample.counter_overflow,
            );
        }
        result
    }
    pub(super) fn start(
        &mut self,
        enter: impl FnOnce() -> Result<(), CoreAudioError>,
    ) -> Result<(), CoreAudioError> {
        accumulate(
            &mut self.sample.sdk_start_attempts,
            1,
            &mut self.sample.counter_overflow,
        );
        enter()
    }
    pub(super) fn closed(&mut self) {
        self.sample.close_confirmed = true;
    }
    pub(super) fn reopened(&mut self) {
        accumulate(
            &mut self.sample.native_open_successes,
            1,
            &mut self.sample.counter_overflow,
        );
        self.sample.close_confirmed = false;
    }
}
fn accumulate(total: &mut u64, amount: u64, overflow: &mut bool) {
    let (value, exceeded) = total.overflowing_add(amount);
    *total = if exceeded { u64::MAX } else { value };
    *overflow |= exceeded;
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{ErrorCode, ids::Epochs};
    use earph_coreaudio::{CoreAudioError, WriteProgress};

    #[test]
    fn activity_keeps_entries_results_and_origin_across_reopens() {
        let origin = Epochs {
            session: 7,
            stream: 9,
        };
        let mut activity = ActivityTracker::opened(origin);
        let failure =
            CoreAudioError::new(ErrorCode::SafetyPolicyBlocked, "controlled denied entry");
        assert_eq!(activity.start(|| Err(failure)).unwrap_err(), failure);
        assert_eq!(
            activity
                .write(|| Ok(WriteProgress::Accepted { frames: 23 }))
                .unwrap(),
            WriteProgress::Accepted { frames: 23 }
        );
        assert_eq!(
            activity.write(|| Ok(WriteProgress::WouldBlock)).unwrap(),
            WriteProgress::WouldBlock
        );
        assert_eq!(activity.write(|| Err(failure)).unwrap_err(), failure);
        activity.closed();
        assert!(activity.snapshot().close_confirmed);
        activity.reopened();
        let sample = activity.snapshot();
        assert_eq!(sample.origin_epochs, origin);
        assert_eq!(sample.native_open_successes, 2);
        assert_eq!(sample.sdk_write_attempts, 3);
        assert_eq!(sample.accepted_frames, 23);
        assert_eq!(sample.sdk_start_attempts, 1);
        assert!(!sample.counter_overflow);
        assert!(!sample.close_confirmed);
        assert_eq!(activity.snapshot(), sample);
    }

    #[test]
    fn activity_overflow_never_changes_accepted_results_and_stays_sticky() {
        let mut activity = ActivityTracker::opened(Epochs::default());
        activity.sample.native_open_successes = u64::MAX;
        activity.sample.sdk_write_attempts = u64::MAX;
        activity.sample.accepted_frames = u64::MAX - 2;
        activity.sample.sdk_start_attempts = u64::MAX;
        assert_eq!(
            activity
                .write(|| Ok(WriteProgress::Accepted { frames: 17 }))
                .unwrap(),
            WriteProgress::Accepted { frames: 17 }
        );
        activity.start(|| Ok(())).unwrap();
        activity.closed();
        activity.reopened();
        let sample = activity.snapshot();
        assert_eq!(sample.native_open_successes, u64::MAX);
        assert_eq!(sample.sdk_write_attempts, u64::MAX);
        assert_eq!(sample.accepted_frames, u64::MAX);
        assert_eq!(sample.sdk_start_attempts, u64::MAX);
        assert!(sample.counter_overflow);
        assert!(!sample.close_confirmed);
    }
}
