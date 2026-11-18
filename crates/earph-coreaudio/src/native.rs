// ABOUTME: Reads bounded Core Audio properties and owns never-started callback registrations.
// ABOUTME: Preserves device settings and releases returned Core Foundation objects.
use crate::*;
use earph_core::ErrorCode;
use objc2_core_audio as ca;
use objc2_core_audio_types::{
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp, AudioValueRange,
};
use objc2_core_foundation::{CFRetained, CFString};
use std::{
    ffi::c_void,
    marker::PhantomData,
    mem,
    ptr::{self, NonNull},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

fn checked(status: i32, message: &'static str) -> Result<(), CoreAudioError> {
    if status == 0 {
        Ok(())
    } else {
        Err(CoreAudioError::native(message, status))
    }
}
fn invalid(message: &'static str) -> CoreAudioError {
    CoreAudioError::new(ErrorCode::AssetInvalid, message)
}
fn address(selector: u32, scope: u32) -> ca::AudioObjectPropertyAddress {
    ca::AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: ca::kAudioObjectPropertyElementMain,
    }
}

/// # Safety
/// Every bit pattern, including zero, must be valid; values must contain no owned allocations or references.
unsafe trait PropertyValue: Copy {}
// SAFETY: These SDK values contain only numbers or a raw pointer; every bit pattern is a valid Rust value.
unsafe impl PropertyValue for u32 {}
unsafe impl PropertyValue for i32 {}
unsafe impl PropertyValue for f64 {}
unsafe impl PropertyValue for *mut CFString {}
unsafe impl PropertyValue for AudioValueRange {}
unsafe impl PropertyValue for AudioStreamBasicDescription {}
unsafe impl PropertyValue for ca::AudioStreamRangedDescription {}

fn read<T: PropertyValue>(object: u32, selector: u32, scope: u32) -> Result<T, CoreAudioError> {
    let mut property = address(selector, scope);
    // SAFETY: PropertyValue guarantees zero initialization is valid.
    let mut value: T = unsafe { mem::zeroed() };
    let mut size = mem::size_of::<T>() as u32;
    // SAFETY: All pointers refer to live, correctly sized allocations. No qualifier is supplied.
    checked(
        unsafe {
            ca::AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut property),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut value).cast(),
            )
        },
        "cannot read selected Core Audio property",
    )?;
    if size != mem::size_of::<T>() as u32 {
        return Err(invalid(
            "Core Audio property size changed or has an unsupported layout",
        ));
    }
    Ok(value)
}
fn array<T: PropertyValue>(
    object: u32,
    selector: u32,
    scope: u32,
    maximum: usize,
) -> Result<Vec<T>, CoreAudioError> {
    let mut property = address(selector, scope);
    let mut size = 0;
    // SAFETY: Addresses and the returned size point to valid stack values.
    checked(
        unsafe {
            ca::AudioObjectGetPropertyDataSize(
                object,
                NonNull::from(&mut property),
                0,
                ptr::null(),
                NonNull::from(&mut size),
            )
        },
        "cannot size selected Core Audio property",
    )?;
    if !(size as usize).is_multiple_of(mem::size_of::<T>())
        || size as usize / mem::size_of::<T>() > maximum
    {
        return Err(CoreAudioError::new(
            ErrorCode::ResourceBudgetExceeded,
            "Core Audio property exceeds its bounded element count",
        ));
    }
    let count = size as usize / mem::size_of::<T>();
    if count == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: PropertyValue permits zero bits and has no owned allocations.
    let mut values = vec![unsafe { mem::zeroed::<T>() }; count];
    let admitted = size;
    // SAFETY: The writable vector spans exactly the admitted byte count.
    checked(
        unsafe {
            ca::AudioObjectGetPropertyData(
                object,
                NonNull::from(&mut property),
                0,
                ptr::null(),
                NonNull::from(&mut size),
                NonNull::new(values.as_mut_ptr())
                    .expect("nonempty property vector")
                    .cast(),
            )
        },
        "cannot read Core Audio array; device properties may have changed",
    )?;
    if size > admitted || !(size as usize).is_multiple_of(mem::size_of::<T>()) {
        return Err(invalid("Core Audio array changed to an unsupported size"));
    }
    values.truncate(size as usize / mem::size_of::<T>());
    Ok(values)
}
fn present(object: u32, selector: u32, scope: u32) -> bool {
    let mut property = address(selector, scope);
    // SAFETY: The property address remains live throughout this synchronous query.
    unsafe { ca::AudioObjectHasProperty(object, NonNull::from(&mut property)) }
}
fn settable(object: u32, selector: u32) -> Result<bool, CoreAudioError> {
    let mut property = address(selector, ca::kAudioObjectPropertyScopeGlobal);
    let mut value = 0;
    // SAFETY: The property address and SDK Boolean output are valid stack values.
    checked(
        unsafe {
            ca::AudioObjectIsPropertySettable(
                object,
                NonNull::from(&mut property),
                NonNull::from(&mut value),
            )
        },
        "cannot inspect Core Audio property permissions",
    )?;
    Ok(value != 0)
}
fn string(object: u32, selector: u32) -> Result<String, CoreAudioError> {
    let pointer = read::<*mut CFString>(object, selector, ca::kAudioObjectPropertyScopeGlobal)?;
    let pointer =
        NonNull::new(pointer).ok_or(invalid("Core Audio returned an empty string object"))?;
    // SAFETY: These name and UID properties return a CFString with +1 ownership, as documented by Apple.
    let owned = unsafe { CFRetained::from_raw(pointer) };
    let value = owned.to_string();
    if value.len() > 4096 {
        return Err(CoreAudioError::new(
            ErrorCode::ResourceBudgetExceeded,
            "Core Audio string exceeds 4096 bytes",
        ));
    }
    Ok(value)
}
fn format(raw: AudioStreamBasicDescription) -> Result<StreamFormat, CoreAudioError> {
    if !raw.mSampleRate.is_finite() {
        return Err(invalid("Core Audio reported a nonfinite stream rate"));
    }
    Ok(StreamFormat {
        sample_rate_hz: raw.mSampleRate,
        format_id: raw.mFormatID,
        format_flags: raw.mFormatFlags,
        bytes_per_packet: raw.mBytesPerPacket,
        frames_per_packet: raw.mFramesPerPacket,
        bytes_per_frame: raw.mBytesPerFrame,
        channels_per_frame: raw.mChannelsPerFrame,
        bits_per_channel: raw.mBitsPerChannel,
    })
}
fn range(raw: AudioValueRange) -> Result<ValueRange, CoreAudioError> {
    if !raw.mMinimum.is_finite() || !raw.mMaximum.is_finite() || raw.mMinimum > raw.mMaximum {
        return Err(invalid("Core Audio reported an invalid capability range"));
    }
    Ok(ValueRange {
        minimum: raw.mMinimum,
        maximum: raw.mMaximum,
    })
}
fn formats(object: u32, selector: u32) -> Result<Vec<RangedFormat>, CoreAudioError> {
    array::<ca::AudioStreamRangedDescription>(
        object,
        selector,
        ca::kAudioObjectPropertyScopeGlobal,
        256,
    )?
    .into_iter()
    .map(|raw| {
        Ok(RangedFormat {
            format: format(raw.mFormat)?,
            sample_rates: range(raw.mSampleRateRange)?,
        })
    })
    .collect()
}
fn stream(object: u32) -> Result<StreamSnapshot, CoreAudioError> {
    let scope = ca::kAudioObjectPropertyScopeGlobal;
    Ok(StreamSnapshot {
        object_id: object,
        physical: format(read(object, ca::kAudioStreamPropertyPhysicalFormat, scope)?)?,
        virtual_format: format(read(object, ca::kAudioStreamPropertyVirtualFormat, scope)?)?,
        physical_settable: settable(object, ca::kAudioStreamPropertyPhysicalFormat)?,
        virtual_settable: settable(object, ca::kAudioStreamPropertyVirtualFormat)?,
        available_physical: formats(object, ca::kAudioStreamPropertyAvailablePhysicalFormats)?,
        available_virtual: formats(object, ca::kAudioStreamPropertyAvailableVirtualFormats)?,
    })
}
fn snapshot(object: u32, streams: Vec<u32>) -> Result<DeviceSnapshot, CoreAudioError> {
    let scope = ca::kAudioObjectPropertyScopeGlobal;
    let nominal_rate_hz = read::<f64>(object, ca::kAudioDevicePropertyNominalSampleRate, scope)?;
    if !nominal_rate_hz.is_finite() {
        return Err(invalid("Core Audio reported a nonfinite nominal rate"));
    }
    let hog_present = present(object, ca::kAudioDevicePropertyHogMode, scope);
    Ok(DeviceSnapshot {
        object_id: object,
        uid: string(object, ca::kAudioDevicePropertyDeviceUID)?,
        name: string(object, ca::kAudioObjectPropertyName)?,
        alive: read::<u32>(object, ca::kAudioDevicePropertyDeviceIsAlive, scope)? != 0,
        nominal_rate_hz,
        nominal_rate_settable: settable(object, ca::kAudioDevicePropertyNominalSampleRate)?,
        buffer_frames: read(object, ca::kAudioDevicePropertyBufferFrameSize, scope)?,
        buffer_frame_range: range(read(
            object,
            ca::kAudioDevicePropertyBufferFrameSizeRange,
            scope,
        )?)?,
        hog_owner_pid: if hog_present {
            Some(read(object, ca::kAudioDevicePropertyHogMode, scope)?)
        } else {
            None
        },
        hog_mode_settable: if hog_present {
            Some(settable(object, ca::kAudioDevicePropertyHogMode)?)
        } else {
            None
        },
        output_streams: streams.into_iter().map(stream).collect::<Result<_, _>>()?,
    })
}

pub fn devices() -> Result<DeviceInventory, CoreAudioError> {
    let ids = array::<u32>(
        ca::kAudioObjectSystemObject as u32,
        ca::kAudioHardwarePropertyDevices,
        ca::kAudioObjectPropertyScopeGlobal,
        64,
    )?;
    let mut inventory = DeviceInventory {
        devices: Vec::new(),
        failures: Vec::new(),
    };
    for id in ids {
        let result = array::<u32>(
            id,
            ca::kAudioDevicePropertyStreams,
            ca::kAudioObjectPropertyScopeOutput,
            16,
        )
        .and_then(|streams| {
            if streams.is_empty() {
                Ok(None)
            } else {
                snapshot(id, streams).map(Some)
            }
        });
        match result {
            Ok(Some(device)) => inventory.devices.push(device),
            Ok(None) => {}
            Err(error) => inventory.failures.push(DeviceFailure {
                object_id: id,
                error,
            }),
        }
    }
    inventory.devices.sort_by(|a, b| a.uid.cmp(&b.uid));
    Ok(inventory)
}
pub fn inspect(uid: &str) -> Result<DeviceSnapshot, CoreAudioError> {
    if uid.is_empty() || uid.len() > 1024 || uid.trim() != uid || uid.chars().any(char::is_control)
    {
        return Err(invalid(
            "select one explicit device UID without control characters",
        ));
    }
    let inventory = devices()?;
    if let Some(device) = inventory
        .devices
        .into_iter()
        .find(|device| device.uid == uid)
    {
        return Ok(device);
    }
    Err(inventory
        .failures
        .first()
        .map(|failure| failure.error)
        .unwrap_or(CoreAudioError::new(
            ErrorCode::DeviceUnavailable,
            "selected UID is unavailable; no default output or fallback was selected",
        )))
}

unsafe extern "C-unwind" fn silent_callback(
    _: u32,
    _: NonNull<AudioTimeStamp>,
    _: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    _: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    context: *mut c_void,
) -> i32 {
    // This spike never starts its registration. Core Audio supplies zeroed output buffers if unexpectedly called.
    if let Some(context) = NonNull::new(context.cast::<AtomicU64>()) {
        // SAFETY: The registration owns this unique counter until successful destruction.
        unsafe { context.as_ref() }.fetch_add(1, Ordering::Relaxed);
    }
    0
}
enum RegistrationState {
    Registered(ca::AudioDeviceIOProcID),
    Closed,
    Failed(CoreAudioError),
}
pub struct SilentRegistration {
    device: DeviceSnapshot,
    state: RegistrationState,
    context: Option<Box<AtomicU64>>,
    _owner: PhantomData<Rc<()>>,
}
impl SilentRegistration {
    pub fn new(uid: &str) -> Result<Self, CoreAudioError> {
        let device = inspect(uid)?;
        if !device.alive {
            return Err(CoreAudioError::new(
                ErrorCode::DeviceUnavailable,
                "selected Core Audio device is not alive",
            ));
        }
        let mut id = None;
        let mut context = Box::new(AtomicU64::new(0));
        // SAFETY: The callback is static. This unique context remains owned until registration destruction succeeds.
        checked(
            unsafe {
                ca::AudioDeviceCreateIOProcID(
                    device.object_id,
                    Some(silent_callback),
                    NonNull::from(context.as_mut()).as_ptr().cast(),
                    NonNull::from(&mut id),
                )
            },
            "cannot register a silent callback on the selected device",
        )?;
        if id.is_none() {
            // The SDK reported success without a usable ID. Retain its context until process exit.
            Box::leak(context);
            return Err(CoreAudioError::new(
                ErrorCode::InternalFault,
                "Core Audio returned no callback registration",
            ));
        }
        Ok(Self {
            device,
            state: RegistrationState::Registered(id),
            context: Some(context),
            _owner: PhantomData,
        })
    }
    pub fn report(&self) -> RegistrationReport {
        RegistrationReport {
            device: self.device.clone(),
            evidence_level: "registered_not_started",
            signals_submitted: 0,
        }
    }
    pub fn callbacks_observed(&self) -> u64 {
        self.context
            .as_ref()
            .expect("guard retains callback context until drop")
            .load(Ordering::Relaxed)
    }
    pub fn close(&mut self) -> Result<(), CoreAudioError> {
        match mem::replace(&mut self.state, RegistrationState::Closed) {
            RegistrationState::Closed => Ok(()),
            RegistrationState::Failed(error) => {
                self.state = RegistrationState::Failed(error);
                Err(error)
            }
            RegistrationState::Registered(id) => {
                // SAFETY: This guard owns exactly this never-started registration on its recorded device.
                let status = unsafe { ca::AudioDeviceDestroyIOProcID(self.device.object_id, id) };
                if status != 0 {
                    let mut error = CoreAudioError::native(
                        "cannot destroy owned callback registration; restart the probe process before retrying",
                        status,
                    );
                    error.code = ErrorCode::OutputFault;
                    self.state = RegistrationState::Failed(error);
                    return Err(error);
                }
                Ok(())
            }
        }
    }
}
impl Drop for SilentRegistration {
    fn drop(&mut self) {
        if self.close().is_err() {
            // A failed native destruction may leave the registration alive. Keep its data alive too.
            if let Some(context) = self.context.take() {
                Box::leak(context);
            }
        }
    }
}
