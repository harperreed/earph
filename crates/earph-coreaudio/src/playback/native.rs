// ABOUTME: Owns playback IOProc registration and bounded SDK buffer access.
// ABOUTME: Retains callback storage whenever native lifecycle completion is uncertain.
use super::*;
use objc2_core_audio as ca;
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use std::{ffi::c_void, marker::PhantomData, ptr::NonNull, rc::Rc};
mod timing;
use timing::Timing;

struct Context {
    ring: Arc<Ring>,
    channels: u32,
    timing: Timing,
}
unsafe extern "C-unwind" fn changed(
    _: u32,
    _: u32,
    _: NonNull<ca::AudioObjectPropertyAddress>,
    context: *mut c_void,
) -> i32 {
    if let Some(signal) = NonNull::new(context.cast::<AtomicU8>()) {
        // SAFETY: Each listener points only to one never-reused process-lifetime fault slot.
        unsafe { signal.as_ref() }.store(1, Ordering::Release);
    }
    0
}
unsafe extern "C-unwind" fn callback(
    _: u32,
    _: NonNull<AudioTimeStamp>,
    _: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    mut output: NonNull<AudioBufferList>,
    output_time: NonNull<AudioTimeStamp>,
    context: *mut c_void,
) -> i32 {
    let Some(context) = NonNull::new(context.cast::<Context>()) else {
        return -50;
    };
    // SAFETY: Registration retains this unique immutable context until SDK destruction succeeds.
    let context = unsafe { context.as_ref() };
    // SAFETY: The SDK supplies a writable AudioBufferList and its declared trailing buffers.
    let output = unsafe { output.as_mut() };
    if output.mNumberBuffers > 16 {
        context.ring.fail();
        return -50;
    }
    // SAFETY: The SDK owns mNumberBuffers valid trailing AudioBuffers for this invocation.
    let buffers = unsafe {
        std::slice::from_raw_parts_mut(output.mBuffers.as_mut_ptr(), output.mNumberBuffers as usize)
    };
    let mut valid = buffers.len() == 1;
    for buffer in buffers.iter_mut() {
        let size = buffer.mDataByteSize as usize;
        if size > 8192 * 8 || (size > 0 && buffer.mData.is_null()) {
            context.ring.fail();
            return -50;
        }
        if buffer.mNumberChannels != context.channels {
            valid = false;
        }
        if size > 0 {
            // SAFETY: The SDK supplies the writable byte count. Access stays within this bounded buffer.
            unsafe { std::slice::from_raw_parts_mut(buffer.mData.cast::<u8>(), size) }.fill(0);
        }
    }
    if !valid {
        context.ring.fail();
        return 0;
    }
    let buffer = &mut buffers[0];
    if buffer.mDataByteSize == 0 {
        return 0;
    }
    // SAFETY: The writable SDK allocation and bound were validated above. No references escape this invocation.
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(buffer.mData.cast::<u8>(), buffer.mDataByteSize as usize)
    };
    if context.ring.enabled.load(Ordering::Acquire)
        && context.ring.fault.load(Ordering::Acquire) == 0
    {
        // SAFETY: The SDK owns this output timestamp for the callback's duration.
        context.timing.record(unsafe { output_time.as_ref() });
    }
    context.ring.render(bytes);
    0
}

pub(super) struct Registration {
    device: u32,
    id: ca::AudioDeviceIOProcID,
    context: Option<Box<Context>>,
    started: bool,
    closed: bool,
    terminal: Option<CoreAudioError>,
    listeners: Vec<(u32, ca::AudioObjectPropertyAddress)>,
    _owner: PhantomData<Rc<()>>,
}
impl Registration {
    pub(super) fn new(
        device: u32,
        stream: u32,
        ring: Arc<Ring>,
        channels: u32,
    ) -> Result<Self, CoreAudioError> {
        let context = Box::new(Context {
            ring,
            channels,
            timing: Timing::new(),
        });
        let mut id = None;
        // SAFETY: The static callback uses this unique owned context until checked destruction.
        let status = unsafe {
            ca::AudioDeviceCreateIOProcID(
                device,
                Some(callback),
                std::ptr::from_ref(context.as_ref()).cast_mut().cast(),
                NonNull::from(&mut id),
            )
        };
        if status != 0 || id.is_none() {
            // SDK failure can leave registration outcome uncertain. Never release potentially referenced data.
            Box::leak(context);
            return Err(if status != 0 {
                CoreAudioError::native(
                    "playback registration failed; callback context retained until process exit",
                    status,
                )
            } else {
                error(
                    ErrorCode::InternalFault,
                    "SDK returned no playback registration; callback context retained",
                )
            });
        }
        let mut registration = Self {
            device,
            id,
            context: Some(context),
            started: false,
            closed: false,
            terminal: None,
            listeners: Vec::with_capacity(6),
            _owner: PhantomData,
        };
        for (object, selector, scope) in [
            (
                device,
                ca::kAudioDevicePropertyDeviceIsAlive,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device,
                ca::kAudioDevicePropertyNominalSampleRate,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device,
                ca::kAudioDevicePropertyBufferFrameSize,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                device,
                ca::kAudioDevicePropertyStreams,
                ca::kAudioObjectPropertyScopeOutput,
            ),
            (
                stream,
                ca::kAudioStreamPropertyPhysicalFormat,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
            (
                stream,
                ca::kAudioStreamPropertyVirtualFormat,
                ca::kAudioObjectPropertyScopeGlobal,
            ),
        ] {
            let mut address = ca::AudioObjectPropertyAddress {
                mSelector: selector,
                mScope: scope,
                mElement: ca::kAudioObjectPropertyElementMain,
            };
            let context = registration
                .context
                .as_ref()
                .expect("live registration owns context");
            // SAFETY: This listener references only its never-reused process-lifetime fault slot.
            let status = unsafe {
                ca::AudioObjectAddPropertyListener(
                    object,
                    NonNull::from(&mut address),
                    Some(changed),
                    std::ptr::from_ref(context.ring.fault).cast_mut().cast(),
                )
            };
            if status != 0 {
                return Err(registration.fail(
                    "Core Audio listener registration uncertain; callback context retained",
                    status,
                ));
            }
            registration.listeners.push((object, address));
        }
        Ok(registration)
    }
    fn check(&self) -> Result<(), CoreAudioError> {
        if let Some(error) = self.terminal {
            return Err(error);
        }
        if self.closed {
            return Err(error(
                ErrorCode::OutputFault,
                "callback registration is closed",
            ));
        }
        Ok(())
    }
    fn fail(&mut self, message: &'static str, status: i32) -> CoreAudioError {
        let mut error = CoreAudioError::native(message, status);
        error.code = ErrorCode::OutputFault;
        self.terminal = Some(error);
        self.context
            .as_ref()
            .expect("live registration owns context")
            .ring
            .fail();
        error
    }
    pub(super) fn start(&mut self) -> Result<(), CoreAudioError> {
        self.check()?;
        self.context
            .as_ref()
            .expect("live registration owns context")
            .timing
            .invalidate()?;
        // SAFETY: This guard owns the live registration and context. The caller checks explicit permission first.
        let status = unsafe { ca::AudioDeviceStart(self.device, self.id) };
        if status != 0 {
            return Err(self.fail(
                "Core Audio start outcome uncertain; callback context retained",
                status,
            ));
        }
        self.started = true;
        Ok(())
    }
    pub(super) fn stop(&mut self) -> Result<(), CoreAudioError> {
        if self.closed {
            return Ok(());
        }
        self.check()?;
        if self.started {
            // SAFETY: Stop only this guard's registration, never a null/global hardware registration.
            let status = unsafe { ca::AudioDeviceStop(self.device, self.id) };
            if status != 0 {
                return Err(self.fail(
                    "Core Audio stop outcome uncertain; callback context retained",
                    status,
                ));
            }
            self.started = false;
            self.context
                .as_ref()
                .expect("live registration owns context")
                .timing
                .invalidate()?;
        }
        Ok(())
    }
    pub(super) fn callback_timing(&self) -> CallbackTiming {
        self.context
            .as_ref()
            .expect("registration retains context until drop")
            .timing
            .snapshot()
    }
    pub(super) fn close(&mut self) -> Result<(), CoreAudioError> {
        if self.closed {
            return Ok(());
        }
        self.stop()?;
        while let Some((object, mut address)) = self.listeners.pop() {
            let context = self
                .context
                .as_ref()
                .expect("live registration owns context");
            // SAFETY: The exact listener uses a static fault slot that outlives uncertain removal.
            let status = unsafe {
                ca::AudioObjectRemovePropertyListener(
                    object,
                    NonNull::from(&mut address),
                    Some(changed),
                    std::ptr::from_ref(context.ring.fault).cast_mut().cast(),
                )
            };
            if status != 0 {
                return Err(self.fail(
                    "Core Audio listener removal uncertain; callback context retained",
                    status,
                ));
            }
        }
        // SAFETY: SDK stop completed, or this registration was never started. This guard owns the exact ID.
        let status = unsafe { ca::AudioDeviceDestroyIOProcID(self.device, self.id) };
        if status != 0 {
            return Err(self.fail(
                "Core Audio destruction outcome uncertain; callback context retained",
                status,
            ));
        }
        self.closed = true;
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(context) = &self.context {
            context.ring.enabled.store(false, Ordering::Release);
        }
        if self.close().is_err()
            && let Some(context) = self.context.take()
        {
            Box::leak(context);
        }
    }
}
#[cfg(test)]
mod tests;
