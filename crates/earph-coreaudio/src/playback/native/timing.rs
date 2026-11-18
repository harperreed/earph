// ABOUTME: Records bounded atomic snapshots of SDK output timestamps.
// ABOUTME: Invalidates prior observations without claiming hardware presentation.
use super::*;
use objc2_core_audio_types::AudioTimeStampFlags;

pub(super) struct Timing {
    generation: AtomicU64,
    recorded_generation: AtomicU64,
    sequence: AtomicU64,
    writer: AtomicBool,
    host_time: AtomicU64,
    sample_time: AtomicU64,
    flags: AtomicU8,
}
impl Timing {
    pub(super) fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            recorded_generation: AtomicU64::new(u64::MAX),
            sequence: AtomicU64::new(0),
            writer: AtomicBool::new(false),
            host_time: AtomicU64::new(0),
            sample_time: AtomicU64::new(0),
            flags: AtomicU8::new(0),
        }
    }
    pub(super) fn invalidate(&self) -> Result<(), CoreAudioError> {
        self.generation
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| {
                error(
                    ErrorCode::ResourceBudgetExceeded,
                    "SDK timestamp generation exhausted",
                )
            })
    }
    pub(super) fn record(&self, timestamp: &AudioTimeStamp) {
        // A reentrant callback skips this observation rather than waiting for another writer.
        if self
            .writer
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.recorded_generation
            .store(self.generation.load(Ordering::SeqCst), Ordering::SeqCst);
        self.host_time.store(timestamp.mHostTime, Ordering::SeqCst);
        self.sample_time
            .store(timestamp.mSampleTime.to_bits(), Ordering::SeqCst);
        let mut flags = 0;
        if timestamp
            .mFlags
            .contains(AudioTimeStampFlags::HostTimeValid)
        {
            flags |= 1;
        }
        if timestamp
            .mFlags
            .contains(AudioTimeStampFlags::SampleTimeValid)
            && timestamp.mSampleTime.is_finite()
        {
            flags |= 2;
        }
        self.flags.store(flags, Ordering::SeqCst);
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.writer.store(false, Ordering::SeqCst);
    }
    pub(super) fn snapshot(&self) -> CallbackTiming {
        for _ in 0..4 {
            let generation = self.generation.load(Ordering::SeqCst);
            let before = self.sequence.load(Ordering::SeqCst);
            if before & 1 != 0 {
                continue;
            }
            let recorded = self.recorded_generation.load(Ordering::SeqCst);
            let host = self.host_time.load(Ordering::SeqCst);
            let sample = self.sample_time.load(Ordering::SeqCst);
            let flags = self.flags.load(Ordering::SeqCst);
            if before != self.sequence.load(Ordering::SeqCst)
                || generation != self.generation.load(Ordering::SeqCst)
            {
                continue;
            }
            let mut result = CallbackTiming::unknown(generation);
            if recorded == generation {
                result.output_host_time_ticks = (flags & 1 != 0).then_some(host);
                result.output_sample_time_frames =
                    (flags & 2 != 0).then_some(f64::from_bits(sample));
            }
            return result;
        }
        CallbackTiming::unknown(self.generation.load(Ordering::SeqCst))
    }
}
