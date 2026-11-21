// ABOUTME: Tracks bounded AudioQueue ownership without equating buffer release with playback.
// ABOUTME: Validates client-rate restart clocks and explicit natural-drain transitions.
use super::*;
use std::{cell::Cell, sync::atomic::AtomicU32};

#[derive(Clone, Copy)]
struct ScheduledSpan {
    placement: NativeEnqueueSpan,
    queue_end: u64,
}
struct Schedule {
    spans: Box<[Option<ScheduledSpan>]>,
    head: usize,
    len: usize,
    enqueued: u64,
    retired: u64,
    queue_end: Option<(u64, f64)>,
    epochs: Epochs,
    returned: Option<QueueScheduleDiagnostic>,
}
impl Schedule {
    fn copy_spans(&self, destination: &mut [NativeEnqueueSpan]) -> Result<usize, CoreAudioError> {
        if destination.len() < self.len {
            return Err(error(
                ErrorCode::ResourceBudgetExceeded,
                "timing span destination is too small",
            ));
        }
        for (offset, target) in destination.iter_mut().take(self.len).enumerate() {
            *target = self.spans[(self.head + offset) % self.spans.len()]
                .expect("live schedule entries remain until retirement")
                .placement;
        }
        Ok(self.len)
    }
    fn new(capacity: u32, epochs: Epochs) -> Self {
        Self {
            // Each outstanding span contains at least one frame within the unchanged enqueue window.
            spans: vec![None; capacity as usize].into_boxed_slice(),
            head: 0,
            len: 0,
            enqueued: 0,
            retired: 0,
            queue_end: None,
            epochs,
            returned: None,
        }
    }
    fn has_room(&self) -> bool {
        self.len < self.spans.len()
    }
    fn admit(&self, epochs: Epochs, start: u64, frames: u32) -> Result<u64, CoreAudioError> {
        let end = start.checked_add(u64::from(frames)).ok_or(error(
            ErrorCode::OutputFault,
            "AudioQueue payload schedule counter exhausted",
        ))?;
        if epochs != self.epochs
            || frames == 0
            || start != self.enqueued
            || !self.has_room()
            || end
                .checked_sub(self.retired)
                .is_none_or(|n| n > self.spans.len() as u64)
        {
            return Err(error(
                ErrorCode::OutputFault,
                "invalid AudioQueue payload schedule span",
            ));
        }
        Ok(end)
    }
    fn append(
        &mut self,
        epochs: Epochs,
        start: u64,
        frames: u32,
        sample: f64,
        flags: u32,
    ) -> Result<(), CoreAudioError> {
        // Keep the exact SDK result even when validation fails after native acceptance.
        self.returned = Some(QueueScheduleDiagnostic {
            sample_time_bits: Some(sample.to_bits()),
            flags,
            payload_start: start,
            frames,
            epochs,
        });
        let payload_end = self.admit(epochs, start, frames)?;
        // The SDK accepted this admitted payload even if its schedule evidence is unusable.
        self.enqueued = payload_end;
        let queue_start = Some(sample)
            .filter(|sample| {
                flags & 1 != 0
                    && sample.is_finite()
                    && *sample >= 0.0
                    && *sample < 9007199254740992.0
            })
            .ok_or(error(
                ErrorCode::OutputFault,
                "AudioQueue returned no precise buffer schedule",
            ))?;
        let start_whole = queue_start.floor() as u64;
        let fraction = queue_start.fract();
        let end_whole = start_whole.checked_add(u64::from(frames)).ok_or(error(
            ErrorCode::OutputFault,
            "AudioQueue schedule endpoint exhausted",
        ))?;
        // Split coordinates preserve a fractional endpoint without rounding it into an earlier whole frame.
        let queue_end = end_whole
            .checked_add(u64::from(fraction > 0.0))
            .filter(|end| *end < 9007199254740992)
            .ok_or(error(
                ErrorCode::OutputFault,
                "AudioQueue buffer schedule exceeds precise range",
            ))?;
        if self.queue_end.is_some_and(|(whole, part)| {
            start_whole < whole || (start_whole == whole && fraction < part)
        }) {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue buffer schedule overlaps its predecessor",
            ));
        }
        let tail = (self.head + self.len) % self.spans.len();
        self.spans[tail] = Some(ScheduledSpan {
            placement: NativeEnqueueSpan {
                epochs,
                accepted_start: start,
                accepted_end: payload_end,
                queue_start: sample,
                queue_end_whole: end_whole,
                queue_end_fraction: fraction,
            },
            queue_end,
        });
        self.len += 1;
        self.queue_end = Some((end_whole, fraction));
        Ok(())
    }
    fn retire(&mut self, epochs: Epochs, queue_frame: u64) -> Result<u64, CoreAudioError> {
        if epochs != self.epochs {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue schedule generation changed",
            ));
        }
        while self.len != 0 {
            let span =
                self.spans[self.head].expect("live schedule entries remain until retirement");
            if queue_frame < span.queue_end {
                break;
            }
            self.retired = span.placement.accepted_end;
            self.spans[self.head] = None;
            self.head = (self.head + 1) % self.spans.len();
            self.len -= 1;
        }
        Ok(self.retired)
    }
    fn drain_frontier(&mut self, drain: &Drain, ready: bool) -> Option<Option<u64>> {
        let bits = drain.bits.load(Ordering::Acquire);
        if bits & 15 == 7 {
            self.retired = self.enqueued;
            self.spans.fill(None);
            self.len = 0;
            Some(Some(self.retired))
        } else if bits & 10 == 2 {
            // Stop(false) resets the timeline; only its completion may retire the remaining payload.
            Some(ready.then_some(self.retired))
        } else {
            None
        }
    }
}

struct Slots {
    capacities: Box<[u32]>,
    frames: Box<[AtomicU32]>,
    acquired: AtomicU64,
    callbacks: AtomicU64,
    fault: &'static FaultSignal,
}
impl Slots {
    fn new(total: u32, period: u32, fault: &'static FaultSignal) -> Self {
        let capacities: Box<[_]> = (0..total.div_ceil(period))
            .map(|i| period.min(total - i * period))
            .collect();
        Self {
            frames: capacities.iter().map(|_| AtomicU32::new(0)).collect(),
            capacities,
            acquired: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            fault,
        }
    }
    fn claim(&self, index: usize, count: u32) -> Result<bool, CoreAudioError> {
        let capacity = self
            .capacities
            .get(index)
            .copied()
            .ok_or(error(ErrorCode::OutputFault, "invalid queue slot"))?;
        if count == 0 || count > capacity {
            return Err(error(ErrorCode::OutputFault, "invalid queue slot span"));
        }
        Ok(self.frames[index]
            .compare_exchange(0, count, Ordering::AcqRel, Ordering::Acquire)
            .is_ok())
    }
    fn release(&self, index: usize) {
        let Some(slot) = self.frames.get(index) else {
            self.fault
                .latch(OutputFaultReason::CallbackBufferPointer, None, None);
            return;
        };
        let count = slot.load(Ordering::Acquire);
        if count == 0 || count > self.capacities[index] {
            self.fault
                .latch(OutputFaultReason::RingCursorInvalid, None, None);
            return;
        }
        if self
            .acquired
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                v.checked_add(u64::from(count))
            })
            .is_err()
            || self
                .callbacks
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
                .is_err()
        {
            self.fault
                .latch(OutputFaultReason::RingCursorInvalid, None, None);
        }
        if slot
            .compare_exchange(count, 0, Ordering::Release, Ordering::Relaxed)
            .is_err()
        {
            self.fault
                .latch(OutputFaultReason::RingCursorInvalid, None, None);
        }
    }
    fn fill(
        &self,
        index: usize,
        ring: &Ring,
        destination: &mut [u8],
        credit: u64,
    ) -> Result<usize, CoreAudioError> {
        let capacity = self
            .capacities
            .get(index)
            .copied()
            .ok_or(error(ErrorCode::OutputFault, "invalid queue slot"))?;
        if destination.len() < capacity as usize * ring.frame_bytes {
            return Err(error(
                ErrorCode::OutputFault,
                "queue buffer is smaller than its admitted slot",
            ));
        }
        let read = ring.read.load(Ordering::Acquire);
        let finished = ring.finished.load(Ordering::Acquire);
        let write = ring.write.load(Ordering::Acquire);
        let available = write
            .checked_sub(read)
            .filter(|v| *v <= ring.capacity)
            .ok_or(error(ErrorCode::OutputFault, "invalid staging cursor"))?;
        let available = available.min(credit);
        if available == 0 || (available < u64::from(capacity) && !finished) {
            return Ok(0);
        }
        let count = available.min(u64::from(capacity)) as u32;
        if !self.claim(index, count)? {
            return Ok(0);
        }
        for frame in 0..count as usize {
            let source = ((read + frame as u64) % ring.capacity) as usize * ring.frame_bytes;
            for byte in 0..ring.frame_bytes {
                destination[frame * ring.frame_bytes + byte] =
                    ring.bytes[source + byte].load(Ordering::Relaxed);
            }
        }
        ring.read.store(read + u64::from(count), Ordering::Release);
        Ok(count as usize)
    }
    fn pending(&self) -> u64 {
        self.frames
            .iter()
            .map(|f| u64::from(f.load(Ordering::Acquire)))
            .sum()
    }
}
#[derive(Clone, Copy)]
struct ClockRead {
    sample_bits: u64,
    flags: u32,
    discontinuity: bool,
    enqueued: u64,
}
#[cfg(target_os = "macos")]
struct FreshQueueRead {
    restart: Option<u64>,
    clock: Option<ClockRead>,
    running_before: Option<bool>,
}
#[cfg(target_os = "macos")]
impl FreshQueueRead {
    fn pending(restart: Option<u64>) -> Self {
        Self {
            restart,
            clock: None,
            running_before: None,
        }
    }
}
struct Clock {
    raw_last: Cell<Option<f64>>,
    read: Cell<Option<ClockRead>>,
}
impl Clock {
    fn validate_read(&self, read: ClockRead) -> Result<Option<u64>, CoreAudioError> {
        self.read.set(Some(read));
        self.observe(
            (read.flags & 1 != 0).then_some(f64::from_bits(read.sample_bits)),
            read.discontinuity,
        )
    }
    fn begin_read(&self) {
        self.read.set(None);
    }
    fn record_read(&self, sample: f64, flags: u32, discontinuity: bool, enqueued: u64) {
        self.read.set(Some(ClockRead {
            sample_bits: sample.to_bits(),
            flags,
            discontinuity,
            enqueued,
        }));
    }
    fn diagnostics(&self, mut diagnostics: QueueDiagnostics) -> QueueDiagnostics {
        if let Some(read) = self.read.get() {
            let sample = f64::from_bits(read.sample_bits);
            diagnostics.sample_bits = Some(read.sample_bits);
            diagnostics.sample_finite = sample.is_finite().then_some(sample);
            diagnostics.flags = Some(read.flags);
            diagnostics.discontinuity = Some(read.discontinuity);
            diagnostics.observed_enqueued = Some(read.enqueued);
        }
        diagnostics.last_valid = self.raw_last.get().map(|sample| sample.floor() as u64);
        diagnostics
    }
    fn new() -> Self {
        Self {
            raw_last: Cell::new(None),
            read: Cell::new(None),
        }
    }
    fn observe(
        &self,
        sample: Option<f64>,
        discontinuity: bool,
    ) -> Result<Option<u64>, CoreAudioError> {
        if discontinuity {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue timeline discontinuity",
            ));
        }
        let Some(sample) = sample else {
            return Ok(None);
        };
        let message = if !sample.is_finite() {
            Some("AudioQueue client clock is non-finite")
        } else if sample < 0.0 {
            Some("AudioQueue client clock is negative")
        } else if sample >= 9007199254740992.0 {
            Some("AudioQueue client clock exceeds exact whole-frame range")
        } else {
            None
        };
        if let Some(message) = message {
            return Err(error(ErrorCode::OutputFault, message));
        }
        let whole = sample.floor() as u64;
        if self.raw_last.get().is_some_and(|last| sample < last) {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue clock moved backwards",
            ));
        }
        self.raw_last.set(Some(sample));
        Ok(Some(whole))
    }
}
struct Drain {
    bits: AtomicU8,
}
impl Drain {
    const fn new() -> Self {
        Self {
            bits: AtomicU8::new(0),
        }
    }
    fn observe(&self, running: bool) {
        let _ = self
            .bits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
                Some(if bits & 8 != 0 {
                    bits
                } else if running {
                    bits | 1
                } else if bits & 3 == 3 {
                    bits | 4
                } else {
                    bits
                })
            });
    }
    fn request(&self) -> bool {
        self.bits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
                (bits & 10 == 0).then_some(bits | 2)
            })
            .is_ok()
    }
    fn cancel(&self) {
        self.bits.store(8, Ordering::Release);
    }
    fn complete(&self) -> bool {
        self.bits.load(Ordering::Acquire) & 15 == 7
    }
}
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(super) mod native;
#[cfg(test)]
mod tests;

pub(super) fn same_device_configuration(opened: &DeviceSnapshot, current: &DeviceSnapshot) -> bool {
    fn format_eq(a: &crate::StreamFormat, b: &crate::StreamFormat) -> bool {
        a.sample_rate_hz == b.sample_rate_hz
            && [
                a.format_id,
                a.format_flags,
                a.bytes_per_packet,
                a.frames_per_packet,
                a.bytes_per_frame,
                a.channels_per_frame,
                a.bits_per_channel,
            ] == [
                b.format_id,
                b.format_flags,
                b.bytes_per_packet,
                b.frames_per_packet,
                b.bytes_per_frame,
                b.channels_per_frame,
                b.bits_per_channel,
            ]
    }
    opened.uid == current.uid
        && opened.object_id == current.object_id
        && current.alive
        && opened.nominal_rate_hz == current.nominal_rate_hz
        && opened.buffer_frames == current.buffer_frames
        && opened.output_streams.len() == current.output_streams.len()
        && opened
            .output_streams
            .iter()
            .zip(&current.output_streams)
            .all(|(a, b)| {
                a.object_id == b.object_id
                    && format_eq(&a.physical, &b.physical)
                    && format_eq(&a.virtual_format, &b.virtual_format)
            })
}

fn enqueue_credit(enqueued: u64, frontier: u64, capacity: u64) -> Result<u64, CoreAudioError> {
    let pending = enqueued
        .checked_sub(frontier)
        .filter(|pending| *pending <= capacity)
        .ok_or(error(
            ErrorCode::OutputFault,
            "AudioQueue client window exceeds its admitted capacity",
        ))?;
    Ok(capacity - pending)
}

const CLOCK_PROGRESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
struct Readiness {
    frozen_prefix: Cell<Option<u64>>,
    ready: Cell<bool>,
    running: Cell<Option<bool>>,
    status: Cell<Option<i32>>,
    advanced_at: Cell<std::time::Duration>,
    frame: Cell<u64>,
}
impl Readiness {
    fn new() -> Self {
        Self {
            frozen_prefix: Cell::new(None),
            ready: Cell::new(false),
            running: Cell::new(None),
            status: Cell::new(None),
            advanced_at: Cell::new(std::time::Duration::ZERO),
            frame: Cell::new(0),
        }
    }
    fn begin(&self, frame: u64) {
        self.ready.set(false);
        self.running.set(None);
        self.status.set(None);
        self.advanced_at.set(std::time::Duration::ZERO);
        self.frame.set(frame);
    }
    fn deadline(&self, elapsed: std::time::Duration) -> Result<(), CoreAudioError> {
        if elapsed.saturating_sub(self.advanced_at.get()) >= CLOCK_PROGRESS_TIMEOUT {
            return Err(CoreAudioError {
                code: ErrorCode::OutputFault,
                message: "AudioQueue made no validated client-clock progress within five seconds",
                os_status: self.status.get().filter(|status| *status != 0),
            });
        }
        Ok(())
    }
    fn before_clock(
        &self,
        running: bool,
        seen_running: bool,
        elapsed: std::time::Duration,
    ) -> Result<bool, CoreAudioError> {
        self.running.set(Some(running));
        if !running && seen_running {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue stopped outside pause or completed drain",
            ));
        }
        if !self.ready.get() || !running {
            self.deadline(elapsed)?;
        }
        Ok(running)
    }
    fn clock_status(
        &self,
        status: i32,
        invalid_run_state: i32,
        elapsed: std::time::Duration,
        startup: bool,
    ) -> Result<bool, CoreAudioError> {
        self.status.set(Some(status));
        if status == 0 {
            return Ok(true);
        }
        if startup && !self.ready.get() && status == invalid_run_state {
            self.deadline(elapsed)?;
            return Ok(false);
        }
        Err(CoreAudioError {
            code: ErrorCode::OutputFault,
            message: "cannot read AudioQueue client clock in its observed run state",
            os_status: Some(status),
        })
    }
    fn pre_origin_pending(
        &self,
        sample: Option<f64>,
        discontinuity: bool,
        elapsed: std::time::Duration,
        startup: bool,
    ) -> Result<bool, CoreAudioError> {
        if startup
            && !self.ready.get()
            && !discontinuity
            && sample.is_some_and(|sample| sample.is_finite() && sample < 0.0)
        {
            self.deadline(elapsed)?;
            return Ok(true);
        }
        Ok(false)
    }
    fn sample(
        &self,
        frame: Option<u64>,
        elapsed: std::time::Duration,
        startup: bool,
    ) -> Result<Option<u64>, CoreAudioError> {
        if let Some(frame) = frame {
            if frame > self.frame.get() {
                self.frame.set(frame);
                self.advanced_at.set(elapsed);
            }
            self.deadline(elapsed)?;
            self.ready.set(true);
            return Ok(Some(frame));
        }
        if !startup || self.ready.get() {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue lost its valid client sample time",
            ));
        }
        self.deadline(elapsed)?;
        Ok(None)
    }
}

fn queue_running(value: u32) -> bool {
    value != 0
}
