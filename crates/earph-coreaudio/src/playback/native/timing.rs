// ABOUTME: Records bounded atomic snapshots of SDK output timestamps.
// ABOUTME: Invalidates prior observations without claiming hardware presentation.
use super::super::presentation::{PayloadBuffer, SampleClock};
use super::*;
use objc2_core_audio_types::AudioTimeStampFlags;

pub(super) struct Observation {
    pub callback: CallbackTiming,
    pub payload: Option<PayloadBuffer>,
}

pub(super) struct Timing {
    generation: AtomicU64,
    recorded_generation: AtomicU64,
    sequence: AtomicU64,
    writer: AtomicBool,
    host_time: AtomicU64,
    sample_time: AtomicU64,
    flags: AtomicU8,
    payload_generation: AtomicU64,
    payload_total: AtomicU64,
    payload_host: AtomicU64,
    payload_start: AtomicU64,
    payload_end: AtomicU64,
    payload_valid: AtomicBool,
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
            payload_generation: AtomicU64::new(u64::MAX),
            payload_total: AtomicU64::new(0),
            payload_host: AtomicU64::new(0),
            payload_start: AtomicU64::new(0),
            payload_end: AtomicU64::new(0),
            payload_valid: AtomicBool::new(false),
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
    pub(super) fn record_buffer(
        &self,
        timestamp: &AudioTimeStamp,
        delivered: u64,
        total: u64,
        buffer_frames: usize,
    ) {
        // A reentrant callback skips this observation rather than waiting for another writer.
        if self
            .writer
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.sequence.fetch_add(1, Ordering::SeqCst);
        let generation = self.generation.load(Ordering::SeqCst);
        self.recorded_generation.store(generation, Ordering::SeqCst);
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
        if delivered > 0 {
            let end = timestamp.mSampleTime + buffer_frames as f64;
            let valid = flags == 3
                && (1..=8192).contains(&buffer_frames)
                && delivered <= buffer_frames as u64
                && end.is_finite()
                && end > timestamp.mSampleTime;
            self.payload_generation.store(generation, Ordering::SeqCst);
            self.payload_total.store(total, Ordering::SeqCst);
            self.payload_host
                .store(timestamp.mHostTime, Ordering::SeqCst);
            self.payload_start
                .store(timestamp.mSampleTime.to_bits(), Ordering::SeqCst);
            self.payload_end.store(end.to_bits(), Ordering::SeqCst);
            self.payload_valid.store(valid, Ordering::SeqCst);
        }
        self.sequence.fetch_add(1, Ordering::SeqCst);
        self.writer.store(false, Ordering::SeqCst);
    }
    pub(super) fn snapshot(&self) -> CallbackTiming {
        self.observation().callback
    }
    pub(super) fn observation(&self) -> Observation {
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
            let payload_generation = self.payload_generation.load(Ordering::SeqCst);
            let payload_total = self.payload_total.load(Ordering::SeqCst);
            let payload_host = self.payload_host.load(Ordering::SeqCst);
            let payload_start = self.payload_start.load(Ordering::SeqCst);
            let payload_end = self.payload_end.load(Ordering::SeqCst);
            let payload_valid = self.payload_valid.load(Ordering::SeqCst);
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
            let payload =
                (recorded == generation && payload_generation == generation && payload_valid)
                    .then_some(PayloadBuffer {
                        delivered_frames: payload_total,
                        start: SampleClock {
                            sample_frames: f64::from_bits(payload_start),
                            host_ticks: payload_host,
                        },
                        end_sample_frames: f64::from_bits(payload_end),
                    });
            return Observation {
                callback: result,
                payload,
            };
        }
        Observation {
            callback: CallbackTiming::unknown(self.generation.load(Ordering::SeqCst)),
            payload: None,
        }
    }
}
