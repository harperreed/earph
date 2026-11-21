// ABOUTME: Owns a selected-device AudioQueue and fixed source-rate Float32 buffers.
// ABOUTME: Pumps only on the worker and retains callback storage until synchronous disposal succeeds.
use super::*;
use objc2_audio_toolbox as aq;
use objc2_core_audio as ca;
use objc2_core_audio_types::{AudioStreamBasicDescription, AudioTimeStamp, AudioTimeStampFlags};
use objc2_core_foundation::{CFRetained, CFString};
use std::{
    cell::RefCell,
    ffi::c_void,
    marker::PhantomData,
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::AtomicPtr,
};

struct Context {
    ring: Arc<Ring>,
    slots: Slots,
    buffers: Box<[AtomicPtr<aq::AudioQueueBuffer>]>,
    drain: Drain,
}
unsafe extern "C-unwind" fn released(
    context: *mut c_void,
    _: aq::AudioQueueRef,
    buffer: aq::AudioQueueBufferRef,
) {
    let (Some(context), Some(buffer)) = (
        NonNull::new(context.cast::<Context>()),
        NonNull::new(buffer),
    ) else {
        return;
    };
    // SAFETY: Synchronous queue disposal precedes context release. The SDK supplies its own buffer.
    let context = unsafe { context.as_ref() };
    // SAFETY: mUserData stores a checked slot index, not a dereferenced application pointer.
    let index = unsafe { buffer.as_ref() }.mUserData as usize;
    let Some(index) = index.checked_sub(1) else {
        context.ring.fail(OutputFaultReason::CallbackBufferPointer);
        return;
    };
    if context
        .buffers
        .get(index)
        .is_none_or(|p| p.load(Ordering::Acquire) != buffer.as_ptr())
    {
        context.ring.fail(OutputFaultReason::CallbackBufferPointer);
        return;
    }
    context.slots.release(index);
}
unsafe extern "C-unwind" fn running_changed(
    context: *mut c_void,
    queue: aq::AudioQueueRef,
    _: u32,
) {
    let Some(context) = NonNull::new(context.cast::<Context>()) else {
        return;
    };
    // SAFETY: The listener remains owned by this queue until synchronous disposal returns.
    let context = unsafe { context.as_ref() };
    let mut running = 0_u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: The queue is live during its listener. The outputs have the exact property sizes.
    let status = unsafe {
        aq::AudioQueueGetProperty(
            queue,
            aq::kAudioQueueProperty_IsRunning,
            NonNull::from(&mut running).cast(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size != size_of::<u32>() as u32 {
        context.ring.fail(OutputFaultReason::NativePresentation);
        return;
    }
    context.drain.observe(queue_running(running));
}
unsafe extern "C-unwind" fn device_changed(
    _: u32,
    _: u32,
    _: NonNull<ca::AudioObjectPropertyAddress>,
    context: *mut c_void,
) -> i32 {
    if let Some(signal) = NonNull::new(context.cast::<FaultSignal>()) {
        // SAFETY: Device listeners reference only a never-reused process-lifetime fault slot.
        unsafe { signal.as_ref() }.latch(OutputFaultReason::DevicePropertyChanged, None, None);
    }
    0
}

pub(in crate::playback) struct Registration {
    queue: aq::AudioQueueRef,
    timeline: aq::AudioQueueTimelineRef,
    context: Option<Box<Context>>,
    client: crate::StreamFormat,
    uid: String,
    schedule: RefCell<Schedule>,
    clock: Clock,
    readiness: Readiness,
    requested_at: Option<std::time::Instant>,
    started: bool,
    paused: bool,
    closed: bool,
    cancel_result: Option<Result<(), CoreAudioError>>,
    dispose_attempted: bool,
    terminal: Cell<Option<CoreAudioError>>,
    listeners: Vec<(u32, ca::AudioObjectPropertyAddress)>,
    _owner: PhantomData<Rc<()>>,
}
impl Registration {
    pub(in crate::playback) fn new(
        request: &OutputRequest,
        device: &DeviceSnapshot,
        ring: Arc<Ring>,
        epochs: Epochs,
    ) -> Result<Self, CoreAudioError> {
        let slots = Slots::new(request.ring_frames, request.buffer_frames, ring.fault);
        let buffers = slots
            .capacities
            .iter()
            .map(|_| AtomicPtr::new(ptr::null_mut()))
            .collect();
        let context = Box::new(Context {
            ring,
            slots,
            buffers,
            drain: Drain::new(),
        });
        let channels = request.format.channels().count() as u32;
        let mut asbd = AudioStreamBasicDescription {
            mSampleRate: f64::from(request.format.sample_rate_hz()),
            mFormatID: u32::from_be_bytes(*b"lpcm"),
            mFormatFlags: 9,
            mBytesPerPacket: channels * 4,
            mFramesPerPacket: 1,
            mBytesPerFrame: channels * 4,
            mChannelsPerFrame: channels,
            mBitsPerChannel: 32,
            mReserved: 0,
        };
        let mut queue = ptr::null_mut();
        // SAFETY: The ASBD is initialized. This unique context outlives successful synchronous disposal.
        let status = unsafe {
            aq::AudioQueueNewOutput(
                NonNull::from(&mut asbd),
                Some(released),
                ptr::from_ref(context.as_ref()).cast_mut().cast(),
                None,
                None,
                0,
                NonNull::from(&mut queue),
            )
        };
        if status != 0 || queue.is_null() {
            Box::leak(context);
            return Err(native_failure(
                "AudioQueue creation uncertain; callback storage retained",
                status,
            ));
        }
        let client = stream_format(&asbd);
        let mut registration = Self {
            queue,
            timeline: ptr::null_mut(),
            context: Some(context),
            client,
            uid: request.device_uid.clone(),
            schedule: RefCell::new(Schedule::new(request.ring_frames, epochs)),
            clock: Clock::new(),
            readiness: Readiness::new(),
            requested_at: None,
            started: false,
            paused: false,
            closed: false,
            cancel_result: None,
            dispose_attempted: false,
            terminal: Cell::new(None),
            listeners: Vec::with_capacity(6),
            _owner: PhantomData,
        };
        // Drop synchronously disposes every partially configured queue, including all SDK buffers.
        registration.configure(request, device, &asbd)?;
        Ok(registration)
    }
    fn context(&self) -> &Context {
        self.context
            .as_ref()
            .expect("queue retains callback context until drop")
    }
    fn configure(
        &mut self,
        request: &OutputRequest,
        device: &DeviceSnapshot,
        expected: &AudioStreamBasicDescription,
    ) -> Result<(), CoreAudioError> {
        let uid = CFString::from_str(&request.device_uid);
        let mut pointer: *const CFString = &*uid;
        // SAFETY: CurrentDevice takes one CFString reference. The queue retains its property value.
        self.checked(
            unsafe {
                aq::AudioQueueSetProperty(
                    self.queue,
                    aq::kAudioQueueProperty_CurrentDevice,
                    NonNull::from(&mut pointer).cast(),
                    size_of::<*const CFString>() as u32,
                )
            },
            OutputFaultReason::DeviceIdentityChanged,
            "cannot bind selected AudioQueue device",
        )?;
        self.verify_uid(&request.device_uid)?;
        let actual: AudioStreamBasicDescription =
            self.property(aq::kAudioQueueProperty_StreamDescription, *expected)?;
        if actual.mSampleRate != expected.mSampleRate
            || actual.mFormatID != expected.mFormatID
            || actual.mFormatFlags != expected.mFormatFlags
            || actual.mBytesPerPacket != expected.mBytesPerPacket
            || actual.mFramesPerPacket != 1
            || actual.mBytesPerFrame != expected.mBytesPerFrame
            || actual.mChannelsPerFrame != expected.mChannelsPerFrame
            || actual.mBitsPerChannel != 32
        {
            return Err(self.fail(
                OutputFaultReason::DeviceConfigurationChanged,
                "AudioQueue changed the client PCM format",
                None,
            ));
        }
        self.client = stream_format(&actual);
        let mut timeline = ptr::null_mut();
        // SAFETY: The queue owns this timeline, and its output pointer is initialized and writable.
        self.checked(
            unsafe { aq::AudioQueueCreateTimeline(self.queue, NonNull::from(&mut timeline)) },
            OutputFaultReason::NativePresentation,
            "cannot create AudioQueue timeline",
        )?;
        self.timeline = timeline;
        if self.timeline.is_null() {
            return Err(self.fail(
                OutputFaultReason::NativePresentation,
                "AudioQueue returned no timeline",
                None,
            ));
        }
        // SAFETY: This context stays alive until worker-side synchronous queue disposal completes.
        self.checked(
            unsafe {
                aq::AudioQueueAddPropertyListener(
                    self.queue,
                    aq::kAudioQueueProperty_IsRunning,
                    Some(running_changed),
                    ptr::from_ref(self.context()).cast_mut().cast(),
                )
            },
            OutputFaultReason::NativeListenerRegistration,
            "cannot register AudioQueue running listener",
        )?;
        for index in 0..self.context().slots.capacities.len() {
            let bytes = self.context().slots.capacities[index] * expected.mBytesPerFrame;
            let mut buffer = ptr::null_mut();
            // SAFETY: This queue owns each fixed allocation until disposal. The output pointer is valid.
            self.checked(
                unsafe {
                    aq::AudioQueueAllocateBuffer(self.queue, bytes, NonNull::from(&mut buffer))
                },
                OutputFaultReason::CallbackBufferPointer,
                "cannot allocate bounded AudioQueue buffer",
            )?;
            let Some(mut buffer) = NonNull::new(buffer) else {
                return Err(self.fail(
                    OutputFaultReason::CallbackBufferPointer,
                    "AudioQueue returned no buffer",
                    None,
                ));
            };
            // SAFETY: This allocated buffer has never been enqueued, and belongs exclusively to this owner.
            let native = unsafe { buffer.as_mut() };
            if native.mAudioDataBytesCapacity != bytes {
                return Err(self.fail(
                    OutputFaultReason::CallbackBufferSize,
                    "AudioQueue allocation differs from admitted capacity",
                    None,
                ));
            }
            native.mUserData = (index + 1) as *mut c_void;
            self.context().buffers[index].store(buffer.as_ptr(), Ordering::Release);
        }
        for (object, selector, scope) in [
            (
                device.object_id,
                ca::kAudioDevicePropertyDeviceIsAlive,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device.object_id,
                ca::kAudioDevicePropertyNominalSampleRate,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device.object_id,
                ca::kAudioDevicePropertyBufferFrameSize,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device.object_id,
                ca::kAudioDevicePropertyStreams,
                ca::kAudioObjectPropertyScopeOutput,
            ),
            (
                device.output_streams[0].object_id,
                ca::kAudioStreamPropertyPhysicalFormat,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device.output_streams[0].object_id,
                ca::kAudioStreamPropertyVirtualFormat,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
        ] {
            let mut address = ca::AudioObjectPropertyAddress {
                mSelector: selector,
                mScope: scope,
                mElement: ca::kAudioObjectPropertyElementMain,
            };
            // SAFETY: This exact listener references a never-reused fault slot, even if removal fails.
            self.checked(
                unsafe {
                    ca::AudioObjectAddPropertyListener(
                        object,
                        NonNull::from(&mut address),
                        Some(device_changed),
                        ptr::from_ref(self.context().ring.fault).cast_mut().cast(),
                    )
                },
                OutputFaultReason::NativeListenerRegistration,
                "cannot register selected-device listener",
            )?;
            self.listeners.push((object, address));
        }
        Ok(())
    }
    fn fail(
        &self,
        reason: OutputFaultReason,
        message: &'static str,
        status: Option<i32>,
    ) -> CoreAudioError {
        self.context().ring.fail(reason);
        let failure = CoreAudioError {
            code: ErrorCode::OutputFault,
            message,
            os_status: status,
        };
        if self.terminal.get().is_none() {
            self.terminal.set(Some(failure));
        }
        failure
    }
    fn checked(
        &self,
        status: i32,
        reason: OutputFaultReason,
        message: &'static str,
    ) -> Result<(), CoreAudioError> {
        if status == 0 {
            Ok(())
        } else {
            Err(self.fail(reason, message, Some(status)))
        }
    }
    fn check(&self) -> Result<(), CoreAudioError> {
        if let Some(failure) = self.terminal.get() {
            return Err(failure);
        }
        if self.closed || self.context().ring.fault.is_set() {
            return Err(error(
                ErrorCode::OutputFault,
                "AudioQueue is closed or faulted",
            ));
        }
        Ok(())
    }
    fn property<T: Copy>(&self, key: u32, mut value: T) -> Result<T, CoreAudioError> {
        let mut size = size_of::<T>() as u32;
        // SAFETY: Every caller pairs the property with its documented output type and exact size.
        self.checked(
            unsafe {
                aq::AudioQueueGetProperty(
                    self.queue,
                    key,
                    NonNull::from(&mut value).cast(),
                    NonNull::from(&mut size),
                )
            },
            OutputFaultReason::NativePresentation,
            "cannot read AudioQueue property",
        )?;
        if size != size_of::<T>() as u32 {
            return Err(self.fail(
                OutputFaultReason::NativePresentation,
                "AudioQueue property size changed",
                None,
            ));
        }
        Ok(value)
    }
    fn verify_uid(&self, expected: &str) -> Result<(), CoreAudioError> {
        let pointer: *mut CFString =
            self.property(aq::kAudioQueueProperty_CurrentDevice, ptr::null_mut())?;
        let Some(pointer) = NonNull::new(pointer) else {
            return Err(self.fail(
                OutputFaultReason::DeviceIdentityChanged,
                "AudioQueue returned no selected UID",
                None,
            ));
        };
        // SAFETY: AudioQueueGetProperty duplicates CF objects under the Create Rule, despite its Get name.
        let readback = unsafe { CFRetained::from_raw(pointer) };
        if readback.to_string() != expected {
            return Err(self.fail(
                OutputFaultReason::DeviceIdentityChanged,
                "AudioQueue selected UID differs from request",
                None,
            ));
        }
        Ok(())
    }
    pub(in crate::playback) fn client_format(&self) -> crate::StreamFormat {
        self.client.clone()
    }
    pub(in crate::playback) fn audio_storage_bytes(&self) -> u64 {
        if self.closed {
            return 0;
        }
        self.context()
            .slots
            .capacities
            .iter()
            .map(|frames| u64::from(*frames) * u64::from(self.client.bytes_per_frame))
            .sum()
    }
    pub(in crate::playback) fn queued_frames(&self) -> u64 {
        if self.closed {
            0
        } else {
            self.context().slots.pending()
        }
    }
    pub(in crate::playback) fn acquisition(&self) -> (u64, u64) {
        (
            self.context().slots.acquired.load(Ordering::Acquire),
            self.context().slots.callbacks.load(Ordering::Acquire),
        )
    }
    pub(in crate::playback) fn pump(&self) -> Result<(), CoreAudioError> {
        self.check()?;
        self.verify_uid(&self.uid)?;
        if self.context().drain.bits.load(Ordering::Acquire) & 10 != 0 {
            return Ok(());
        }
        let ring = &self.context().ring;
        let frontier = self
            .restart_cursor_frames()?
            .unwrap_or(self.schedule.borrow().retired);
        let mut credit = enqueue_credit(self.schedule.borrow().enqueued, frontier, ring.capacity)
            .map_err(|_| {
            self.fail(
                OutputFaultReason::RingCursorInvalid,
                "AudioQueue client playback window exceeded",
                None,
            )
        })?;
        for (index, pointer) in self.context().buffers.iter().enumerate() {
            if !self.schedule.borrow().has_room() {
                break;
            }
            if self.context().slots.frames[index].load(Ordering::Acquire) != 0 {
                continue;
            }
            let Some(mut pointer) = NonNull::new(pointer.load(Ordering::Acquire)) else {
                return Err(self.fail(
                    OutputFaultReason::CallbackBufferPointer,
                    "AudioQueue slot has no buffer",
                    None,
                ));
            };
            // SAFETY: The callback's release publication returned exclusive access to this fixed buffer.
            let buffer = unsafe { pointer.as_mut() };
            // SAFETY: Allocation size was verified at preparation. No callback touches PCM or buffer metadata after release.
            let destination = unsafe {
                std::slice::from_raw_parts_mut(
                    buffer.mAudioData.as_ptr().cast::<u8>(),
                    buffer.mAudioDataBytesCapacity as usize,
                )
            };
            let count = self
                .context()
                .slots
                .fill(index, ring, destination, credit)
                .map_err(|_| {
                    self.fail(
                        OutputFaultReason::RingCursorInvalid,
                        "cannot stage AudioQueue PCM",
                        None,
                    )
                })?;
            if count == 0 {
                continue;
            }
            credit -= count as u64;
            let (epochs, payload_start) = {
                let schedule = self.schedule.borrow();
                schedule
                    .admit(schedule.epochs, schedule.enqueued, count as u32)
                    .map_err(|failure| self.readiness_error(failure))?;
                (schedule.epochs, schedule.enqueued)
            };
            buffer.mAudioDataByteSize = (count * ring.frame_bytes) as u32;
            // SAFETY: Numeric timestamp fields accept zero initialization. No returned field is trusted before validation.
            let mut actual_start: AudioTimeStamp = unsafe { std::mem::zeroed() };
            // SAFETY: This exact buffer belongs to this queue and contains whole initialized interleaved PCM frames.
            self.checked(
                unsafe {
                    aq::AudioQueueEnqueueBufferWithParameters(
                        self.queue,
                        pointer.as_ptr(),
                        0,
                        ptr::null(),
                        0,
                        0,
                        0,
                        ptr::null(),
                        ptr::null(),
                        &mut actual_start,
                    )
                },
                OutputFaultReason::NativeStart,
                "AudioQueue enqueue failed after accepted PCM",
            )?;
            self.schedule
                .borrow_mut()
                .append(
                    epochs,
                    payload_start,
                    count as u32,
                    actual_start.mSampleTime,
                    actual_start.mFlags.0,
                )
                .map_err(|failure| self.readiness_error(failure))?;
        }
        Ok(())
    }
    pub(in crate::playback) fn start(&mut self, uid: &str) -> Result<(), CoreAudioError> {
        self.check()?;
        self.verify_uid(uid)?;
        self.pump()?;
        if self.paused {
            self.readiness.frozen_prefix.set(None);
            self.context().drain.bits.store(0, Ordering::Release);
        }
        self.readiness.begin(self.schedule.borrow().retired);
        self.requested_at = Some(std::time::Instant::now());
        // SAFETY: The caller checked explicit permission and identity before this sole start operation.
        self.checked(
            unsafe { aq::AudioQueueStart(self.queue, ptr::null()) },
            OutputFaultReason::NativeStart,
            "AudioQueue start outcome uncertain",
        )?;
        self.started = true;
        self.paused = false;
        let running: u32 = self.property(aq::kAudioQueueProperty_IsRunning, 0_u32)?;
        self.readiness.running.set(Some(queue_running(running)));
        self.context().drain.observe(queue_running(running));
        Ok(())
    }
    pub(in crate::playback) fn restart_cursor_frames(&self) -> Result<Option<u64>, CoreAudioError> {
        Ok(self.fresh_queue_read()?.restart)
    }
    fn fresh_queue_read(&self) -> Result<FreshQueueRead, CoreAudioError> {
        self.check()?;
        if !self.started {
            return Ok(FreshQueueRead::pending(Some(0)));
        }
        if self.context().drain.complete() {
            return Ok(FreshQueueRead::pending(
                self.schedule
                    .borrow_mut()
                    .drain_frontier(&self.context().drain, self.readiness.ready.get())
                    .expect("completed drain owns its frontier"),
            ));
        }
        if let Some(frozen) = self.readiness.frozen_prefix.get() {
            return Ok(FreshQueueRead::pending(Some(frozen)));
        }
        let elapsed = self
            .requested_at
            .expect("started queue owns its request time")
            .elapsed();
        let running: u32 = self.property(aq::kAudioQueueProperty_IsRunning, 0_u32)?;
        self.readiness.running.set(Some(queue_running(running)));
        self.context().drain.observe(queue_running(running));
        if let Some(frontier) = self
            .schedule
            .borrow_mut()
            .drain_frontier(&self.context().drain, self.readiness.ready.get())
        {
            return Ok(FreshQueueRead::pending(frontier));
        }
        if !self.paused
            && !self
                .readiness
                .before_clock(
                    queue_running(running),
                    self.context().drain.bits.load(Ordering::Acquire) & 1 != 0,
                    elapsed,
                )
                .map_err(|failure| self.readiness_error(failure))?
        {
            return Ok(FreshQueueRead {
                running_before: Some(queue_running(running)),
                ..FreshQueueRead::pending(None)
            });
        }
        self.clock.begin_read();
        // SAFETY: AudioTimeStamp contains only numeric fields, including zero-valid timestamp flags.
        let mut timestamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
        let mut discontinuity = 0;
        // SAFETY: Queue and timeline are live. The timestamp and discontinuity outputs are initialized stack values.
        let status = unsafe {
            aq::AudioQueueGetCurrentTime(
                self.queue,
                self.timeline,
                &mut timestamp,
                &mut discontinuity,
            )
        };
        let read = ClockRead {
            sample_bits: timestamp.mSampleTime.to_bits(),
            flags: timestamp.mFlags.0,
            discontinuity: discontinuity != 0,
            enqueued: self.schedule.borrow().enqueued,
        };
        if status == 0 {
            self.clock.record_read(
                timestamp.mSampleTime,
                timestamp.mFlags.0,
                discontinuity != 0,
                self.schedule.borrow().enqueued,
            );
        }
        if !self
            .readiness
            .clock_status(
                status,
                aq::kAudioQueueErr_InvalidRunState,
                elapsed,
                !self.paused,
            )
            .map_err(|failure| self.readiness_error(failure))?
        {
            return Ok(FreshQueueRead {
                running_before: Some(queue_running(running)),
                ..FreshQueueRead::pending(None)
            });
        }
        let sample = timestamp
            .mFlags
            .contains(AudioTimeStampFlags::SampleTimeValid)
            .then_some(timestamp.mSampleTime);
        if self
            .readiness
            .pre_origin_pending(sample, discontinuity != 0, elapsed, !self.paused)
            .map_err(|failure| self.readiness_error(failure))?
        {
            return Ok(FreshQueueRead {
                clock: Some(read),
                running_before: Some(queue_running(running)),
                ..FreshQueueRead::pending(None)
            });
        }
        let frame = self
            .clock
            .validate_read(read)
            .map_err(|failure| self.readiness_error(failure))?;
        let frame = frame
            .map(|queue_frame| {
                let mut schedule = self.schedule.borrow_mut();
                let epochs = schedule.epochs;
                schedule.retire(epochs, queue_frame)
            })
            .transpose()
            .map_err(|failure| self.readiness_error(failure))?;
        let restart = self
            .readiness
            .sample(frame, elapsed, !self.paused)
            .map_err(|failure| self.readiness_error(failure))?;
        Ok(FreshQueueRead {
            restart,
            clock: Some(read),
            running_before: Some(queue_running(running)),
        })
    }
    pub(in crate::playback) fn timing_observation(
        &self,
        spans: &mut [NativeEnqueueSpan],
        mut observation: TimingObservation,
    ) -> Result<TimingObservation, CoreAudioError> {
        if spans.len() < self.schedule.borrow().len {
            return Err(error(
                ErrorCode::ResourceBudgetExceeded,
                "timing span destination is too small",
            ));
        }
        let fresh = self.fresh_queue_read()?;
        let running_after = if fresh.clock.is_some() {
            Some(queue_running(
                self.property(aq::kAudioQueueProperty_IsRunning, 0_u32)?,
            ))
        } else {
            None
        };
        let schedule = self.schedule.borrow();
        observation.initialized_spans = schedule.copy_spans(spans)?;
        observation.enqueued_frames = Some(schedule.enqueued);
        observation.restart_prefix = fresh.restart;
        observation.running_before = fresh.running_before;
        observation.running_after = running_after;
        observation.availability = if self.context().drain.bits.load(Ordering::Acquire) & 2 != 0 {
            TimingAvailability::Draining
        } else if self.paused {
            TimingAvailability::Paused
        } else if !self.started {
            TimingAvailability::Prepared
        } else {
            TimingAvailability::Priming
        };
        if let Some(read) = fresh.clock {
            observation.flags = Some(read.flags);
            observation.discontinuity = Some(read.discontinuity);
            if fresh.restart.is_some()
                && fresh.running_before == Some(true)
                && running_after == Some(true)
            {
                observation.queue_sample = Some(f64::from_bits(read.sample_bits));
                observation.availability = TimingAvailability::Available;
            }
        }
        Ok(observation)
    }
    fn readiness_error(&self, failure: CoreAudioError) -> CoreAudioError {
        self.fail(
            OutputFaultReason::NativePresentation,
            failure.message,
            failure.os_status,
        )
    }
    pub(in crate::playback) fn initial_restart_cursor_pending(&self) -> bool {
        self.started
            && !self.paused
            && !self.closed
            && !self.context().ring.fault.is_set()
            && !self.context().drain.complete()
            && !self.readiness.ready.get()
    }
    pub(in crate::playback) fn clock_diagnostics(&self) -> QueueDiagnostics {
        let schedule = self.schedule.borrow();
        self.clock.diagnostics(QueueDiagnostics {
            running: self.readiness.running.get(),
            status: self.readiness.status.get(),
            ready: Some(self.readiness.ready.get()),
            enqueued: Some(schedule.enqueued),
            schedule: schedule.returned,
            retired: Some(schedule.retired),
            ..QueueDiagnostics::default()
        })
    }
    pub(in crate::playback) fn pause(&mut self) -> Result<(), CoreAudioError> {
        self.check()?;
        let frontier = self.restart_cursor_frames()?.ok_or_else(|| {
            self.fail(
                OutputFaultReason::NativePresentation,
                "paused AudioQueue has no validated restart frontier",
                None,
            )
        })?;
        self.context().drain.cancel();
        // SAFETY: Pause preserves buffers. Publish the proven running prefix only after this call succeeds.
        self.checked(
            unsafe { aq::AudioQueuePause(self.queue) },
            OutputFaultReason::NativeStop,
            "AudioQueue pause outcome uncertain",
        )?;
        self.paused = true;
        self.readiness.frozen_prefix.set(Some(frontier));
        Ok(())
    }
    pub(in crate::playback) fn scheduled_output_complete(
        &self,
    ) -> Result<Option<bool>, CoreAudioError> {
        self.check()?;
        self.pump()?;
        if !self.started
            || self.paused
            || !self.context().ring.finished.load(Ordering::Acquire)
            || self.context().ring.queued_frames() != 0
        {
            return Ok(None);
        }
        if self.context().drain.request() {
            // SAFETY: All accepted PCM is enqueued. Request asynchronous EOF drain exactly once for this queue generation.
            self.checked(
                unsafe { aq::AudioQueueStop(self.queue, false) },
                OutputFaultReason::NativeStop,
                "AudioQueue drain outcome uncertain",
            )?;
        }
        Ok(Some(self.context().drain.complete()))
    }
    pub(in crate::playback) fn stop(&mut self) -> Result<(), CoreAudioError> {
        if self.closed {
            return Ok(());
        }
        if let Some(result) = self.cancel_result {
            return result;
        }
        self.context().drain.cancel();
        // SAFETY: Immediate stop is cancellation. Attempt it once, including when PCM was enqueued before start.
        let status = unsafe { aq::AudioQueueStop(self.queue, true) };
        let result = self.checked(
            status,
            OutputFaultReason::NativeStop,
            "AudioQueue cancellation outcome uncertain",
        );
        self.cancel_result = Some(result);
        result?;
        self.started = false;
        for frames in &self.context().slots.frames {
            frames.store(0, Ordering::Release);
        }
        Ok(())
    }
    pub(in crate::playback) fn close(&mut self) -> Result<(), CoreAudioError> {
        if self.closed {
            return Ok(());
        }
        if self.dispose_attempted {
            return Err(self.terminal.get().unwrap_or(error(
                ErrorCode::OutputFault,
                "AudioQueue disposal outcome is uncertain",
            )));
        }
        // Cleanup attempts synchronous disposal even after a latched fault or failed stop.
        let mut failure = self.stop().err();
        while let Some((object, mut address)) = self.listeners.pop() {
            // SAFETY: The listener points to a process-lifetime fault slot; uncertainty never creates a dangling context.
            let status = unsafe {
                ca::AudioObjectRemovePropertyListener(
                    object,
                    NonNull::from(&mut address),
                    Some(device_changed),
                    ptr::from_ref(self.context().ring.fault).cast_mut().cast(),
                )
            };
            if status != 0 {
                failure = Some(self.fail(
                    OutputFaultReason::NativeListenerRemoval,
                    "AudioQueue device listener removal uncertain",
                    Some(status),
                ));
            }
        }
        self.dispose_attempted = true;
        // SAFETY: This worker owns the live queue. This synchronous call is never made from an SDK callback.
        let status = unsafe { aq::AudioQueueDispose(self.queue, true) };
        if status != 0 {
            return Err(self.fail(
                OutputFaultReason::NativeDestruction,
                "AudioQueue disposal uncertain; callback storage retained",
                Some(status),
            ));
        }
        self.closed = true;
        self.timeline = ptr::null_mut();
        self.context().ring.cancel();
        if let Some(failure) = failure {
            return Err(failure);
        }
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let _ = self.close();
        if !self.closed
            && let Some(context) = self.context.take()
        {
            Box::leak(context);
        }
    }
}
fn native_failure(message: &'static str, status: i32) -> CoreAudioError {
    CoreAudioError {
        code: ErrorCode::OutputFault,
        message,
        os_status: Some(status),
    }
}
fn stream_format(asbd: &AudioStreamBasicDescription) -> crate::StreamFormat {
    crate::StreamFormat {
        sample_rate_hz: asbd.mSampleRate,
        format_id: asbd.mFormatID,
        format_flags: asbd.mFormatFlags,
        bytes_per_packet: asbd.mBytesPerPacket,
        frames_per_packet: asbd.mFramesPerPacket,
        bytes_per_frame: asbd.mBytesPerFrame,
        channels_per_frame: asbd.mChannelsPerFrame,
        bits_per_channel: asbd.mBitsPerChannel,
    }
}
