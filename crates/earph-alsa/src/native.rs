// ABOUTME: Owns ALSA pointers and checks every native result at the boundary.
// ABOUTME: Rejects plugin substitution and closes streams without draining on shutdown.
use crate::{
    AlsaError, DeviceHint, DeviceIdentity, DrainProgress, ObservedFormat, OpenRequest, Route,
    WriteProgress,
};
use alsa_sys as sys;
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    marker::PhantomData,
    ptr,
    rc::Rc,
};

fn check(result: i32, code: ErrorCode, message: &'static str) -> Result<(), AlsaError> {
    if result < 0 {
        Err(AlsaError::native(code, message, result))
    } else {
        Ok(())
    }
}

// All callers pass a library-owned, NUL-terminated string or a null optional field.
unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    // SAFETY: The caller guarantees a live ALSA C string for this copy.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

pub fn library_version() -> String {
    // SAFETY: ALSA returns a static, NUL-terminated version string.
    unsafe { text(sys::snd_asoundlib_version()) }
}

macro_rules! parameters {
    ($name:ident, $native:ty, $allocate:ident, $free:ident) => {
        struct $name(*mut $native);
        impl $name {
            fn new() -> Result<Self, AlsaError> {
                let mut pointer = ptr::null_mut();
                // SAFETY: The out pointer is writable. Successful allocation transfers ownership.
                check(
                    unsafe { sys::$allocate(&mut pointer) },
                    ErrorCode::ResourceBudgetExceeded,
                    "cannot allocate ALSA parameters",
                )?;
                if pointer.is_null() {
                    return Err(AlsaError::new(
                        ErrorCode::InternalFault,
                        "ALSA returned null parameters",
                    ));
                }
                Ok(Self(pointer))
            }
        }
        impl Drop for $name {
            fn drop(&mut self) {
                // SAFETY: This object owns exactly one allocation from the matching ALSA allocator.
                unsafe { sys::$free(self.0) };
            }
        }
    };
}
parameters!(
    Hardware,
    sys::snd_pcm_hw_params_t,
    snd_pcm_hw_params_malloc,
    snd_pcm_hw_params_free
);
parameters!(
    Software,
    sys::snd_pcm_sw_params_t,
    snd_pcm_sw_params_malloc,
    snd_pcm_sw_params_free
);
parameters!(
    Info,
    sys::snd_pcm_info_t,
    snd_pcm_info_malloc,
    snd_pcm_info_free
);

pub(crate) struct Pcm {
    pointer: *mut sys::snd_pcm_t,
    // Keep the owned native stream on its owner thread; no concurrent native calls are allowed.
    _owner: PhantomData<Rc<()>>,
}
impl Pcm {
    pub(crate) fn open(request: &OpenRequest) -> Result<Self, AlsaError> {
        request.validate()?;
        let device = CString::new(request.device.as_bytes())
            .map_err(|_| AlsaError::new(ErrorCode::AssetInvalid, "invalid ALSA endpoint"))?;
        let mut pointer = ptr::null_mut();
        let mut flags = sys::SND_PCM_NONBLOCK;
        if request.route == Route::DirectHardware {
            flags |= sys::SND_PCM_NO_AUTO_RESAMPLE
                | sys::SND_PCM_NO_AUTO_CHANNELS
                | sys::SND_PCM_NO_AUTO_FORMAT
                | sys::SND_PCM_NO_SOFTVOL;
        }
        // SAFETY: Device is a live C string; pointer receives one owned playback handle.
        check(
            unsafe {
                sys::snd_pcm_open(
                    &mut pointer,
                    device.as_ptr(),
                    sys::SND_PCM_STREAM_PLAYBACK,
                    flags as i32,
                )
            },
            ErrorCode::DeviceUnavailable,
            "cannot open selected endpoint; check access, availability, and other applications; no fallback attempted",
        )?;
        if pointer.is_null() {
            return Err(AlsaError::new(
                ErrorCode::InternalFault,
                "ALSA returned a null stream",
            ));
        }
        let pcm = Self {
            pointer,
            _owner: PhantomData,
        };
        if request.route == Route::DirectHardware && pcm.kind() != sys::SND_PCM_TYPE_HW {
            return Err(AlsaError::new(
                ErrorCode::FidelityPolicyConflict,
                "selected hw: endpoint opened a plugin; direct hardware output is required",
            ));
        }
        Ok(pcm)
    }

    fn kind(&self) -> sys::snd_pcm_type_t {
        // SAFETY: The stream remains owned and live for this borrow.
        unsafe { sys::snd_pcm_type(self.pointer) }
    }
    pub(crate) fn type_name(&self) -> String {
        // SAFETY: ALSA supplies a static name for the live stream's type.
        unsafe { text(sys::snd_pcm_type_name(self.kind())) }
    }
    pub(crate) fn identity(&self) -> Result<Option<DeviceIdentity>, AlsaError> {
        let info = Info::new()?;
        // SAFETY: The handle and allocated Info remain live throughout all queries.
        unsafe {
            check(
                sys::snd_pcm_info(self.pointer, info.0),
                ErrorCode::DeviceUnavailable,
                "cannot inspect selected endpoint identity",
            )?;
            let card = sys::snd_pcm_info_get_card(info.0);
            if card < 0 {
                return Ok(None);
            }
            Ok(Some(DeviceIdentity {
                card_index: card,
                device_index: sys::snd_pcm_info_get_device(info.0),
                subdevice_index: sys::snd_pcm_info_get_subdevice(info.0),
                pcm_id: text(sys::snd_pcm_info_get_id(info.0)),
                pcm_name: text(sys::snd_pcm_info_get_name(info.0)),
            }))
        }
    }

    pub(crate) fn configure(&self, request: &OpenRequest) -> Result<ObservedFormat, AlsaError> {
        let hardware = Hardware::new()?;
        let software = Software::new()?;
        let format = match request.format.representation() {
            PcmRepresentation::Signed16 => sys::SND_PCM_FORMAT_S16_LE,
            PcmRepresentation::Signed24 => sys::SND_PCM_FORMAT_S24_3LE,
            PcmRepresentation::Signed32 => sys::SND_PCM_FORMAT_S32_LE,
            PcmRepresentation::Float64 => {
                return Err(AlsaError::new(
                    ErrorCode::FormatUnsupported,
                    "integer PCM required",
                ));
            }
        };
        let exact = |result| {
            check(
                result,
                ErrorCode::FormatUnsupported,
                "selected endpoint rejected an exact rate, format, channel count, or buffer; no conversion attempted",
            )
        };
        // SAFETY: All pointers are owned allocations. Frame sizes were bounded before opening.
        unsafe {
            exact(sys::snd_pcm_hw_params_any(self.pointer, hardware.0))?;
            exact(sys::snd_pcm_hw_params_set_rate_resample(
                self.pointer,
                hardware.0,
                0,
            ))?;
            exact(sys::snd_pcm_hw_params_set_access(
                self.pointer,
                hardware.0,
                sys::SND_PCM_ACCESS_RW_INTERLEAVED,
            ))?;
            exact(sys::snd_pcm_hw_params_set_format(
                self.pointer,
                hardware.0,
                format,
            ))?;
            exact(sys::snd_pcm_hw_params_set_channels(
                self.pointer,
                hardware.0,
                request.format.channels().count() as u32,
            ))?;
            exact(sys::snd_pcm_hw_params_set_rate(
                self.pointer,
                hardware.0,
                request.format.sample_rate_hz(),
                0,
            ))?;
            exact(sys::snd_pcm_hw_params_set_period_size(
                self.pointer,
                hardware.0,
                request.period_frames.into(),
                0,
            ))?;
            exact(sys::snd_pcm_hw_params_set_buffer_size(
                self.pointer,
                hardware.0,
                request.buffer_frames.into(),
            ))?;
            exact(sys::snd_pcm_hw_params(self.pointer, hardware.0))?;
            exact(sys::snd_pcm_hw_params_current(self.pointer, hardware.0))?;
            let (mut rate, mut channels, mut numerator, mut denominator) = (0, 0, 0, 0);
            let (mut period, mut buffer, mut observed_format, mut access, mut direction) =
                (0, 0, 0, 0, 0);
            exact(sys::snd_pcm_hw_params_get_rate(
                hardware.0,
                &mut rate,
                &mut direction,
            ))?;
            exact(sys::snd_pcm_hw_params_get_rate_numden(
                hardware.0,
                &mut numerator,
                &mut denominator,
            ))?;
            exact(sys::snd_pcm_hw_params_get_channels(
                hardware.0,
                &mut channels,
            ))?;
            exact(sys::snd_pcm_hw_params_get_format(
                hardware.0,
                &mut observed_format,
            ))?;
            exact(sys::snd_pcm_hw_params_get_access(hardware.0, &mut access))?;
            exact(sys::snd_pcm_hw_params_get_period_size(
                hardware.0,
                &mut period,
                ptr::null_mut(),
            ))?;
            exact(sys::snd_pcm_hw_params_get_buffer_size(
                hardware.0,
                &mut buffer,
            ))?;
            let significant_bits = sys::snd_pcm_hw_params_get_sbits(hardware.0);
            let expected_bits = request.format.representation().bytes() as u32 * 8;
            if rate != request.format.sample_rate_hz()
                || channels != request.format.channels().count() as u32
                || observed_format != format
                || access != sys::SND_PCM_ACCESS_RW_INTERLEAVED
                || period != u64::from(request.period_frames) as sys::snd_pcm_uframes_t
                || buffer != u64::from(request.buffer_frames) as sys::snd_pcm_uframes_t
                || denominator == 0
                || u64::from(numerator) != u64::from(rate) * u64::from(denominator)
                || significant_bits < 0
                || significant_bits as u32 != expected_bits
            {
                return Err(AlsaError::new(
                    ErrorCode::FormatUnsupported,
                    "installed ALSA parameters differ from the exact request",
                ));
            }
            exact(sys::snd_pcm_sw_params_current(self.pointer, software.0))?;
            exact(sys::snd_pcm_sw_params_set_start_threshold(
                self.pointer,
                software.0,
                1,
            ))?;
            exact(sys::snd_pcm_sw_params_set_avail_min(
                self.pointer,
                software.0,
                period,
            ))?;
            exact(sys::snd_pcm_sw_params(self.pointer, software.0))?;
            Ok(ObservedFormat {
                format: PcmFormat::new(
                    rate,
                    if channels == 1 {
                        ChannelLayout::Mono
                    } else {
                        ChannelLayout::Stereo
                    },
                    request.format.representation(),
                )
                .map_err(|_| {
                    AlsaError::new(ErrorCode::FormatUnsupported, "invalid installed PCM format")
                })?,
                alsa_format: text(sys::snd_pcm_format_name(observed_format)),
                access: text(sys::snd_pcm_access_name(access)),
                rate_numerator: numerator,
                rate_denominator: denominator,
                significant_bits: significant_bits as u32,
                period_frames: period as u32,
                buffer_frames: buffer as u32,
            })
        }
    }

    pub(crate) fn write(
        &self,
        bytes: &[u8],
        frames: usize,
        frame_bytes: usize,
    ) -> Result<WriteProgress, AlsaError> {
        if frames == 0 || frames.checked_mul(frame_bytes) != Some(bytes.len()) {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "native write requires complete interleaved frames",
            ));
        }
        // SAFETY: The buffer contains exactly the requested number of frames in the installed format.
        let result = unsafe {
            sys::snd_pcm_writei(
                self.pointer,
                bytes.as_ptr().cast(),
                frames as sys::snd_pcm_uframes_t,
            )
        };
        if result == -(libc::EAGAIN as sys::snd_pcm_sframes_t) {
            return Ok(WriteProgress::WouldBlock);
        }
        if result < 0 {
            return Err(AlsaError::native(
                ErrorCode::OutputFault,
                "ALSA write failed; output stopped without recovery or fallback",
                result as i32,
            ));
        }
        if result == 0 || result as usize > frames {
            return Err(AlsaError::new(
                ErrorCode::OutputFault,
                "ALSA reported an invalid write count",
            ));
        }
        Ok(WriteProgress::Accepted {
            frames: result as usize,
        })
    }
    pub(crate) fn delay(&self) -> Result<u64, AlsaError> {
        let mut delay = 0;
        // SAFETY: The stream is live and delay is a writable native frame counter.
        check(
            unsafe { sys::snd_pcm_delay(self.pointer, &mut delay) },
            ErrorCode::OutputFault,
            "cannot observe ALSA delay; output stopped",
        )?;
        u64::try_from(delay).map_err(|_| {
            AlsaError::new(
                ErrorCode::OutputFault,
                "ALSA reported negative delay; output stopped",
            )
        })
    }
    pub(crate) fn stop(&self) -> Result<(), AlsaError> {
        // SAFETY: The stream is live. Drop stops immediately and never drains or emits queued samples.
        check(
            unsafe { sys::snd_pcm_drop(self.pointer) },
            ErrorCode::OutputFault,
            "ALSA could not stop the selected stream",
        )
    }
    pub(crate) fn prepare(&self) -> Result<(), AlsaError> {
        // SAFETY: This owner holds the live stream after dropping queued frames.
        check(
            unsafe { sys::snd_pcm_prepare(self.pointer) },
            ErrorCode::OutputFault,
            "ALSA could not reprepare the selected stream",
        )
    }
    pub(crate) fn drain(&self) -> Result<DrainProgress, AlsaError> {
        // SAFETY: The stream was opened nonblocking and remains owned on this thread.
        let result = unsafe { sys::snd_pcm_drain(self.pointer) };
        if result == -libc::EAGAIN {
            return Ok(DrainProgress::WouldBlock);
        }
        check(
            result,
            ErrorCode::OutputFault,
            "ALSA drain failed without recovery or fallback",
        )?;
        Ok(DrainProgress::Complete)
    }
    pub(crate) fn drain_progress(&self) -> Result<DrainProgress, AlsaError> {
        // SAFETY: This thread owns the configured stream throughout the drain transaction.
        match unsafe { sys::snd_pcm_state(self.pointer) } {
            sys::SND_PCM_STATE_SETUP => Ok(DrainProgress::Complete),
            sys::SND_PCM_STATE_DRAINING => Ok(DrainProgress::WouldBlock),
            // Some plugins still need to commit buffered frames before starting their slave's drain.
            sys::SND_PCM_STATE_RUNNING | sys::SND_PCM_STATE_PREPARED => self.drain(),
            _ => Err(AlsaError::new(
                ErrorCode::OutputFault,
                "ALSA stream left its valid drain states; output stopped",
            )),
        }
    }
    pub(crate) fn wait_ready(&self, timeout_ms: u32) -> Result<bool, AlsaError> {
        // SAFETY: The caller validates this bounded timeout; the stream remains live.
        let result = unsafe { sys::snd_pcm_wait(self.pointer, timeout_ms as i32) };
        check(
            result,
            ErrorCode::OutputFault,
            "ALSA wait failed without recovery or fallback",
        )?;
        Ok(result > 0)
    }
}
impl Drop for Pcm {
    fn drop(&mut self) {
        // SAFETY: This object owns the stream. Stop before closing; never drain during cleanup.
        // Explicit stop reports errors. Destructors must still close after any native stop failure.
        unsafe {
            sys::snd_pcm_drop(self.pointer);
            sys::snd_pcm_close(self.pointer);
        }
    }
}

struct Hints(*mut *mut c_void);
impl Drop for Hints {
    fn drop(&mut self) {
        // SAFETY: This object owns the hint list allocated by ALSA.
        unsafe {
            sys::snd_device_name_free_hint(self.0);
        }
    }
}
unsafe fn hint_field(hint: *const c_void, key: &CStr) -> Result<Option<String>, AlsaError> {
    // SAFETY: The hint remains live in its owned list and key is a C string.
    let pointer = unsafe { sys::snd_device_name_get_hint(hint, key.as_ptr()) };
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: ALSA returns a malloc-owned C string; copy it before freeing it once.
    let result = (|| {
        let maximum = earph_core::devices::MAX_DEVICE_TEXT_BYTES;
        let mut length = 0;
        // SAFETY: ALSA owns a NUL-terminated allocation. Stop at its terminator or the admitted bound.
        while unsafe { *pointer.add(length) } != 0 {
            if length == maximum {
                return Err(AlsaError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "ALSA hint text exceeds discovery limit",
                ));
            }
            length += 1;
        }
        // SAFETY: The bounded scan established that these bytes belong to the live allocation.
        let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
        let value = std::str::from_utf8(bytes)
            .map_err(|_| AlsaError::new(ErrorCode::AssetInvalid, "ALSA hint is not UTF-8"))?;
        Ok(value.to_owned())
    })();
    unsafe {
        libc::free(pointer.cast());
    }
    result.map(Some)
}
pub fn devices() -> Result<Vec<DeviceHint>, AlsaError> {
    let mut pointer = ptr::null_mut();
    // SAFETY: The output pointer receives an ALSA-owned null-terminated list.
    check(
        unsafe { sys::snd_device_name_hint(-1, c"pcm".as_ptr(), &mut pointer) },
        ErrorCode::DeviceUnavailable,
        "cannot enumerate ALSA playback hints",
    )?;
    if pointer.is_null() {
        return Ok(Vec::new());
    }
    let hints = Hints(pointer);
    let mut result = Vec::new();
    let mut index = 0;
    loop {
        // SAFETY: ALSA guarantees a null-terminated list. The list remains live until this function returns.
        let hint = unsafe { *hints.0.add(index) };
        if hint.is_null() {
            break;
        }
        if index == earph_core::devices::MAX_DEVICES {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "ALSA hint count exceeds discovery limit",
            ));
        }
        index += 1;
        // SAFETY: Each queried hint belongs to the live list; fields are copied and freed separately.
        unsafe {
            if hint_field(hint, c"IOID")?.as_deref() == Some("Input") {
                continue;
            }
            if let Some(device) = hint_field(hint, c"NAME")? {
                result.push(DeviceHint {
                    device,
                    description: hint_field(hint, c"DESC")?,
                    evidence_level: "advertised",
                });
            }
        }
    }
    result.sort_by(|a, b| a.device.cmp(&b.device));
    result.dedup_by(|a, b| a.device == b.device);
    Ok(result)
}
