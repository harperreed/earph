// ABOUTME: Owns bounded PCM submission and explicit Core Audio playback policy.
// ABOUTME: Separates callback delivery from unverified downstream presentation.
#[cfg(any(target_os = "macos", test))]
use crate::assess;
use crate::{CoreAudioError, DeviceSnapshot, PrecisionRequest};
use earph_core::{
    ErrorCode,
    audio::{PcmBlock, PcmFormat},
    ids::Epochs,
};
use serde::{Deserialize, Serialize};
mod fault;
use fault::FaultSignal;
pub use fault::{OutputFaultDetail, OutputFaultReason};
#[cfg(any(target_os = "macos", test))]
use std::sync::atomic::AtomicUsize;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackRoute {
    StrictInteger,
    Float32Compatibility,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputRequest {
    pub device_uid: String,
    pub format: PcmFormat,
    pub buffer_frames: u32,
    pub ring_frames: u32,
    pub route: PlaybackRoute,
}
impl OutputRequest {
    pub fn validate(&self) -> Result<(), CoreAudioError> {
        self.precision().validate()?;
        if self.ring_frames < self.buffer_frames || self.ring_frames > 65536 {
            return Err(error(
                ErrorCode::ResourceBudgetExceeded,
                "ring must hold one callback through 65536 frames",
            ));
        }
        Ok(())
    }
    fn precision(&self) -> PrecisionRequest {
        PrecisionRequest {
            schema_version: 1,
            device_uid: self.device_uid.clone(),
            format: self.format,
            buffer_frames: self.buffer_frames,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalPermission {
    Denied,
    Granted,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Prepared,
    Running,
    Paused,
    Stopped,
    Failed,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteProgress {
    Accepted { frames: usize },
    WouldBlock,
}
#[derive(Debug, Clone, Serialize)]
pub struct OutputReport {
    pub requested: OutputRequest,
    pub observed: DeviceSnapshot,
    pub evidence_level: &'static str,
    pub payload_preservation: &'static str,
    pub downstream_transformations: &'static str,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct OutputProgress {
    pub state: OutputState,
    pub submitted_frames: u64,
    pub callback_delivered_frames: u64,
    pub queued_frames: u64,
    pub callbacks_observed: u64,
    pub measurement_boundary: &'static str,
    pub audible_position_frames: Option<u64>,
    pub backend_delay_frames: Option<u64>,
    pub fault: bool,
    pub fault_detail: Option<OutputFaultDetail>,
}
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CallbackTiming {
    pub generation: u64,
    pub output_host_time_ticks: Option<u64>,
    pub output_sample_time_frames: Option<f64>,
    pub measurement_boundary: &'static str,
}
impl CallbackTiming {
    fn unknown(generation: u64) -> Self {
        Self {
            generation,
            output_host_time_ticks: None,
            output_sample_time_frames: None,
            measurement_boundary: "sdk_output_timestamp_not_dac_presentation",
        }
    }
    fn for_state(self, state: OutputState) -> Self {
        if state == OutputState::Running {
            self
        } else {
            Self::unknown(self.generation)
        }
    }
}
fn error(code: ErrorCode, message: &'static str) -> CoreAudioError {
    CoreAudioError::new(code, message)
}
fn permission(permission: SignalPermission) -> Result<(), CoreAudioError> {
    if permission != SignalPermission::Granted {
        Err(error(
            ErrorCode::SafetyPolicyBlocked,
            "Core Audio start requires explicit signal permission",
        ))
    } else {
        Ok(())
    }
}
#[cfg(any(target_os = "macos", test))]
fn negotiate(request: &OutputRequest, device: &DeviceSnapshot) -> Result<usize, CoreAudioError> {
    request.validate()?;
    let assessment = assess(&request.precision(), device)?;
    if request.route == PlaybackRoute::StrictInteger {
        if !assessment.matches_current_format {
            return Err(error(
                ErrorCode::FidelityPolicyConflict,
                "observed physical or virtual format differs from strict integer request",
            ));
        }
        return Ok(request
            .format
            .payload_bytes(1)
            .expect("validated mono/stereo format"));
    }
    if !device.alive
        || device.buffer_frames != request.buffer_frames
        || device.nominal_rate_hz != f64::from(request.format.sample_rate_hz())
        || device.output_streams.len() != 1
    {
        return Err(error(
            ErrorCode::FormatUnsupported,
            "compatibility route requires the observed rate, buffer and one output stream",
        ));
    }
    let virtual_format = &device.output_streams[0].virtual_format;
    let physical = &device.output_streams[0].physical;
    let channels = request.format.channels().count() as u32;
    if virtual_format.format_id != u32::from_be_bytes(*b"lpcm")
        || virtual_format.format_flags & !64 != 9
        || virtual_format.sample_rate_hz != f64::from(request.format.sample_rate_hz())
        || virtual_format.channels_per_frame != channels
        || virtual_format.bits_per_channel != 32
        || virtual_format.bytes_per_frame != channels * 4
        || virtual_format.frames_per_packet != 1
        || virtual_format.bytes_per_packet != channels * 4
        || physical.sample_rate_hz != f64::from(request.format.sample_rate_hz())
        || physical.channels_per_frame != channels
    {
        return Err(error(
            ErrorCode::FormatUnsupported,
            "compatibility route requires exact interleaved float32 virtual PCM without resampling",
        ));
    }
    Ok(channels as usize * 4)
}

// Each cursor has exactly one writer. Release publication protects complete frames.
// Atomic bytes keep storage safe even while a terminal fault silences concurrent work.
#[cfg(any(target_os = "macos", test))]
struct FaultSignals<const N: usize> {
    slots: [FaultSignal; N],
    next: AtomicUsize,
}
#[cfg(any(target_os = "macos", test))]
impl<const N: usize> FaultSignals<N> {
    const fn new() -> Self {
        Self {
            slots: [const { FaultSignal::new() }; N],
            next: AtomicUsize::new(0),
        }
    }
    fn reserve(&'static self) -> Result<&'static FaultSignal, CoreAudioError> {
        let index = self
            .next
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                (next < N).then_some(next + 1)
            })
            .map_err(|_| {
                error(
                    ErrorCode::ResourceBudgetExceeded,
                    "Core Audio lifetime notification budget exhausted; restart the process",
                )
            })?;
        Ok(&self.slots[index])
    }
}
// SDK listener removal does not document callback quiescence. Slots are never reused or freed.
// The 65536 eight-byte slots retain 512 KiB of lifetime metadata, separate from ring audio storage.
#[cfg(any(target_os = "macos", test))]
static FAULT_SIGNALS: FaultSignals<65536> = FaultSignals::new();

struct Ring {
    bytes: Box<[AtomicU8]>,
    frame_bytes: usize,
    capacity: u64,
    read: AtomicU64,
    write: AtomicU64,
    consumed: AtomicU64,
    callbacks: AtomicU64,
    enabled: AtomicBool,
    finished: AtomicBool,
    fault: &'static FaultSignal,
    #[cfg(any(target_os = "macos", test))]
    rendering: AtomicBool,
}
impl Ring {
    fn audio_storage_bytes(&self) -> Result<u64, CoreAudioError> {
        self.bytes
            .len()
            .checked_mul(size_of::<AtomicU8>())
            .and_then(|bytes| u64::try_from(bytes).ok())
            .ok_or(error(
                ErrorCode::ResourceBudgetExceeded,
                "audio storage count exceeds u64",
            ))
    }
    #[cfg(any(target_os = "macos", test))]
    fn new(frames: u32, frame_bytes: usize) -> Result<Self, CoreAudioError> {
        let fault = FAULT_SIGNALS.reserve()?;
        Ok(Self {
            bytes: (0..frames as usize * frame_bytes)
                .map(|_| AtomicU8::new(0))
                .collect(),
            frame_bytes,
            capacity: u64::from(frames),
            read: AtomicU64::new(0),
            write: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            callbacks: AtomicU64::new(0),
            enabled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            fault,
            rendering: AtomicBool::new(false),
        })
    }
    fn queued_frames(&self) -> u64 {
        // Read first: the consumer cannot overtake the subsequently observed producer cursor.
        let read = self.read.load(Ordering::Acquire);
        self.write
            .load(Ordering::Acquire)
            .saturating_sub(read)
            .min(self.capacity)
    }
    #[cfg(any(target_os = "macos", test))]
    fn fail(&self, reason: OutputFaultReason) {
        self.fail_with_frames(reason, None, None);
    }
    #[cfg(any(target_os = "macos", test))]
    fn fail_with_frames(
        &self,
        reason: OutputFaultReason,
        requested: Option<u64>,
        queued: Option<u64>,
    ) {
        self.fault.latch(reason, requested, queued);
        self.enabled.store(false, Ordering::Release);
    }
    #[cfg(any(target_os = "macos", test))]
    fn render(&self, destination: &mut [u8]) {
        destination.fill(0);
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        if !self.enabled.load(Ordering::Acquire) || self.fault.is_set() {
            return;
        }
        if self
            .rendering
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.fail(OutputFaultReason::RingReentry);
            return;
        }
        self.render_locked(destination);
        self.rendering.store(false, Ordering::Release);
    }
    #[cfg(any(target_os = "macos", test))]
    fn render_locked(&self, destination: &mut [u8]) {
        if !destination.len().is_multiple_of(self.frame_bytes)
            || destination.len() / self.frame_bytes > 8192
        {
            self.fail(OutputFaultReason::CallbackBufferSize);
            return;
        }
        let requested = (destination.len() / self.frame_bytes) as u64;
        let read = self.read.load(Ordering::Relaxed);
        // Observe EOF before publication so a racing final write cannot become a false empty tail.
        let finished = self.finished.load(Ordering::Acquire);
        let write = self.write.load(Ordering::Acquire);
        let available = write.saturating_sub(read);
        if available > self.capacity || (!finished && available < requested) {
            let reason = if write < read || available > self.capacity {
                OutputFaultReason::RingCursorInvalid
            } else {
                OutputFaultReason::RingUnderrun
            };
            self.fail_with_frames(reason, Some(requested), Some(available));
            return;
        }
        let frames = available.min(requested);
        for frame in 0..frames as usize {
            let start = ((read + frame as u64) % self.capacity) as usize * self.frame_bytes;
            for byte in 0..self.frame_bytes {
                destination[frame * self.frame_bytes + byte] =
                    self.bytes[start + byte].load(Ordering::Relaxed);
            }
        }
        self.read.store(read + frames, Ordering::Release);
        self.consumed.fetch_add(frames, Ordering::Release);
    }
    // Call only after successful SDK stop, or before the first start.
    fn cancel(&self) {
        self.enabled.store(false, Ordering::Release);
        self.read
            .store(self.write.load(Ordering::Acquire), Ordering::Release);
    }
}
struct Producer {
    request: OutputRequest,
    epochs: Epochs,
    ring: Arc<Ring>,
    next_position: Option<u64>,
    submitted: u64,
    cancelled: bool,
}
impl Producer {
    #[cfg(any(target_os = "macos", test))]
    fn new(request: OutputRequest, epochs: Epochs) -> Result<Self, CoreAudioError> {
        request.validate()?;
        let frame_bytes = if request.route == PlaybackRoute::StrictInteger {
            request
                .format
                .payload_bytes(1)
                .expect("validated PCM format")
        } else {
            request.format.channels().count() * 4
        };
        Ok(Self {
            ring: Arc::new(Ring::new(request.ring_frames, frame_bytes)?),
            request,
            epochs,
            next_position: None,
            submitted: 0,
            cancelled: false,
        })
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<WriteProgress, CoreAudioError> {
        if self.cancelled || self.ring.fault.is_set() || self.ring.finished.load(Ordering::Acquire)
        {
            return Err(error(
                ErrorCode::OutputFault,
                "output is terminal or finished; open a new stream explicitly",
            ));
        }
        let header = block.header();
        if header.epochs != self.epochs {
            return Err(error(
                ErrorCode::StaleSession,
                "PCM belongs to another output epoch",
            ));
        }
        if header.format != self.request.format {
            return Err(error(
                ErrorCode::FormatUnsupported,
                "PCM format differs from the opened stream",
            ));
        }
        if frames == 0
            || offset
                .checked_add(frames)
                .is_none_or(|end| end as u64 > header.frame_count)
        {
            return Err(error(ErrorCode::AssetInvalid, "invalid PCM write span"));
        }
        let position = header
            .frame_position
            .checked_add(offset as u64)
            .ok_or(error(ErrorCode::AssetInvalid, "PCM position overflow"))?;
        if self
            .next_position
            .is_some_and(|expected| expected != position)
        {
            return Err(error(
                ErrorCode::AssetInvalid,
                "PCM span does not follow accepted frames",
            ));
        }
        let write = self.ring.write.load(Ordering::Relaxed);
        let read = self.ring.read.load(Ordering::Acquire);
        let count = frames.min(
            self.ring
                .capacity
                .saturating_sub(write.saturating_sub(read)) as usize,
        );
        if count == 0 {
            return Ok(WriteProgress::WouldBlock);
        }
        let next = write.checked_add(count as u64).ok_or(error(
            ErrorCode::ResourceBudgetExceeded,
            "ring cursor exhausted",
        ))?;
        self.submitted.checked_add(count as u64).ok_or(error(
            ErrorCode::ResourceBudgetExceeded,
            "submitted counter exhausted",
        ))?;
        let width = self.request.format.representation().bytes();
        let channels = self.request.format.channels().count();
        for frame in 0..count {
            let destination =
                ((write + frame as u64) % self.ring.capacity) as usize * self.ring.frame_bytes;
            for channel in 0..channels {
                let value = block
                    .integer_sample((offset + frame) * channels + channel)
                    .ok_or(error(
                        ErrorCode::AssetInvalid,
                        "expected integer PCM samples",
                    ))?;
                if self.request.route == PlaybackRoute::StrictInteger {
                    for (byte, value) in value.to_le_bytes()[..width].iter().enumerate() {
                        self.ring.bytes[destination + channel * width + byte]
                            .store(*value, Ordering::Relaxed);
                    }
                } else {
                    let scale = (1_u64 << (width * 8 - 1)) as f64;
                    let sample = (value as f64 / scale) as f32;
                    for (byte, value) in sample.to_le_bytes().iter().enumerate() {
                        self.ring.bytes[destination + channel * 4 + byte]
                            .store(*value, Ordering::Relaxed);
                    }
                }
            }
        }
        self.ring.write.store(next, Ordering::Release);
        self.submitted += count as u64;
        self.next_position = Some(position + count as u64);
        Ok(WriteProgress::Accepted { frames: count })
    }
    fn finish(&mut self) -> Result<(), CoreAudioError> {
        if self.cancelled || self.ring.fault.is_set() {
            return Err(error(ErrorCode::OutputFault, "output is terminal"));
        }
        self.ring.finished.store(true, Ordering::Release);
        Ok(())
    }
    fn cancel(&mut self) {
        self.cancelled = true;
        self.ring.cancel();
    }
}

pub struct CoreAudioOutput {
    producer: Producer,
    report: OutputReport,
    state: OutputState,
    #[cfg(target_os = "macos")]
    native: native::Registration,
}
impl CoreAudioOutput {
    pub fn open(request: OutputRequest, epochs: Epochs) -> Result<Self, CoreAudioError> {
        request.validate()?;
        #[cfg(not(target_os = "macos"))]
        {
            let _ = epochs;
            Err(error(
                ErrorCode::CapabilityUnsupported,
                "Core Audio playback requires macOS",
            ))
        }
        #[cfg(target_os = "macos")]
        {
            let observed = crate::inspect(&request.device_uid)?;
            negotiate(&request, &observed)?;
            let producer = Producer::new(request.clone(), epochs)?;
            let native = native::Registration::new(
                observed.object_id,
                observed.output_streams[0].object_id,
                producer.ring.clone(),
                request.format.channels().count() as u32,
            )?;
            Ok(Self {
                producer,
                native,
                state: OutputState::Prepared,
                report: OutputReport {
                    requested: request.clone(),
                    observed,
                    evidence_level: "registered_not_started",
                    payload_preservation: if request.route == PlaybackRoute::StrictInteger {
                        "unverified"
                    } else {
                        "modified_integer_to_float32"
                    },
                    downstream_transformations: "unknown",
                },
            })
        }
    }
    pub fn report(&self) -> &OutputReport {
        &self.report
    }
    /// Actual Rust-owned audio capacity, excluding metadata, allocator overhead, and SDK storage.
    pub fn audio_storage_bytes(&self) -> Result<u64, CoreAudioError> {
        self.producer.ring.audio_storage_bytes()
    }
    pub fn state(&self) -> OutputState {
        if self.producer.ring.fault.is_set() {
            OutputState::Failed
        } else {
            self.state
        }
    }
    pub fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<WriteProgress, CoreAudioError> {
        self.producer.write(block, offset, frames)
    }
    pub fn finish(&mut self) -> Result<(), CoreAudioError> {
        self.producer.finish()
    }
    pub fn start(&mut self, approved: SignalPermission) -> Result<(), CoreAudioError> {
        permission(approved)?;
        if self.state() != OutputState::Prepared {
            return Err(error(
                ErrorCode::OutputFault,
                "start requires a prepared output",
            ));
        }
        self.start_native()
    }
    pub fn resume(&mut self, approved: SignalPermission) -> Result<(), CoreAudioError> {
        permission(approved)?;
        if self.state() != OutputState::Paused {
            return Err(error(
                ErrorCode::OutputFault,
                "resume requires a paused output",
            ));
        }
        self.start_native()
    }
    fn start_native(&mut self) -> Result<(), CoreAudioError> {
        #[cfg(target_os = "macos")]
        {
            let current = crate::inspect(&self.report.requested.device_uid)?;
            if current.object_id != self.report.observed.object_id {
                self.producer
                    .ring
                    .fail(OutputFaultReason::DeviceIdentityChanged);
                return Err(error(
                    ErrorCode::DeviceUnavailable,
                    "selected device identity changed",
                ));
            }
            if let Err(error) = negotiate(&self.report.requested, &current) {
                self.producer
                    .ring
                    .fail(OutputFaultReason::DeviceConfigurationChanged);
                return Err(error);
            }
            self.producer.ring.enabled.store(true, Ordering::Release);
            if let Err(error) = self.native.start() {
                self.producer.ring.fail(OutputFaultReason::NativeStart);
                return Err(error);
            }
            self.state = OutputState::Running;
            self.report.evidence_level = "started_callback_boundary_only";
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err(error(
                ErrorCode::CapabilityUnsupported,
                "Core Audio playback requires macOS",
            ))
        }
    }
    pub fn pause(&mut self) -> Result<(), CoreAudioError> {
        if self.state() != OutputState::Running {
            return Err(error(
                ErrorCode::OutputFault,
                "pause requires a running output",
            ));
        }
        self.producer.ring.enabled.store(false, Ordering::Release);
        #[cfg(target_os = "macos")]
        if let Err(error) = self.native.stop() {
            self.producer.ring.fail(OutputFaultReason::NativeStop);
            return Err(error);
        }
        self.state = OutputState::Paused;
        Ok(())
    }
    pub fn progress(&self) -> OutputProgress {
        let ring = &self.producer.ring;
        let fault_detail = ring.fault.detail();
        OutputProgress {
            state: if fault_detail.is_some() {
                OutputState::Failed
            } else {
                self.state
            },
            submitted_frames: self.producer.submitted,
            callback_delivered_frames: ring.consumed.load(Ordering::Acquire),
            queued_frames: ring.queued_frames(),
            callbacks_observed: ring.callbacks.load(Ordering::Acquire),
            measurement_boundary: "core_audio_ioproc_delivery",
            audible_position_frames: None,
            backend_delay_frames: None,
            fault: fault_detail.is_some(),
            fault_detail,
        }
    }
    pub fn callback_timing(&self) -> CallbackTiming {
        #[cfg(target_os = "macos")]
        let timing = self.native.callback_timing();
        #[cfg(not(target_os = "macos"))]
        let timing = CallbackTiming::unknown(0);
        timing.for_state(self.state())
    }
    pub fn scheduled_output_complete(&self) -> Result<Option<bool>, CoreAudioError> {
        let ring = &self.producer.ring;
        if self.state() != OutputState::Running
            || !ring.finished.load(Ordering::Acquire)
            || ring.queued_frames() != 0
            || ring.consumed.load(Ordering::Acquire) != self.producer.submitted
        {
            return Ok(None);
        }
        #[cfg(target_os = "macos")]
        {
            match self
                .native
                .scheduled_output_complete(self.producer.submitted)
            {
                Ok(value) if self.state() == OutputState::Running => Ok(value),
                Ok(_) => Ok(None),
                Err(mut error) => {
                    ring.fail(OutputFaultReason::NativePresentation);
                    error.code = ErrorCode::OutputFault;
                    Err(error)
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(None)
        }
    }
    pub fn stop(&mut self) -> Result<(), CoreAudioError> {
        self.producer.ring.enabled.store(false, Ordering::Release);
        #[cfg(target_os = "macos")]
        if let Err(error) = self.native.stop() {
            self.producer.ring.fail(OutputFaultReason::NativeStop);
            return Err(error);
        }
        self.producer.cancel();
        if self.state != OutputState::Closed {
            self.state = OutputState::Stopped;
        }
        Ok(())
    }
    pub fn close(&mut self) -> Result<(), CoreAudioError> {
        self.stop()?;
        #[cfg(target_os = "macos")]
        if let Err(error) = self.native.close() {
            self.producer.ring.fail(OutputFaultReason::NativeClose);
            return Err(error);
        }
        self.state = OutputState::Closed;
        Ok(())
    }
}
#[cfg(any(target_os = "macos", test))]
mod presentation;
#[cfg(test)]
mod tests;

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod native;
