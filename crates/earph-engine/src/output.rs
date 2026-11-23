// ABOUTME: Gives the playback worker one bounded output interface.
// ABOUTME: Preserves backend timing uncertainty instead of inventing DAC positions.
use earph_core::{
    EngineError, ErrorCode,
    audio::{PcmBlock, PcmFormat},
    control::{OutputBackend, OutputConfiguration, OutputRoute},
    ids::Epochs,
    session::{BackendObservation, ObservedStreamFormat, PreservationState},
};
#[cfg(any(target_os = "macos", test))]
mod coreaudio;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Counters {
    pub submitted: u64,
    pub consumed: Option<u64>,
    pub restart_cursor: Option<u64>,
    pub delay: Option<u64>,
    pub basis: &'static str,
}
/// The latency that an output device reports beyond the frames that `Counters` cover.
/// It is a device report, never an audible position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeviceLatency {
    /// Frames at the rate of the opened format.
    pub frames: u64,
    /// The measurement boundary of a delay that includes these frames.
    pub basis: &'static str,
}
pub(crate) trait Output {
    /// Actual Rust-owned adapter audio capacity; excludes metadata and opaque native storage.
    fn audio_storage_bytes(&self) -> Result<u64, EngineError>;
    fn is_running(&self) -> bool;
    fn observation(&self) -> Result<BackendObservation, EngineError>;
    fn activity(&self) -> Option<earph_core::session::OutputActivity> {
        None
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError>;
    /// True only while CoreAudio awaits its first valid client clock within its startup deadline.
    fn initial_restart_cursor_pending(&self) -> bool {
        false
    }
    fn progress(&mut self) -> Result<Counters, EngineError>;
    /// Returns the latency that the device reported when this output last opened.
    /// `None` means that the backend reports none.
    fn device_latency(&self) -> Option<DeviceLatency> {
        None
    }
    fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError>;
    /// Drops unplayed audio and reprepares the output for other audio.
    /// Unlike `suspend`, it reports no position, so it also works while the output still starts.
    fn discard(&mut self, epochs: Epochs) -> Result<(), EngineError> {
        self.suspend(epochs).map(|_| ())
    }
    /// Reprepares the same stream after a native underrun failed the last call.
    /// Returns false when that failure was no underrun. The output then stays failed.
    fn recover(&mut self, _epochs: Epochs) -> Result<bool, EngineError> {
        Ok(false)
    }
    fn finish(&mut self) -> Result<Option<Counters>, EngineError>;
    fn wait(&mut self) -> Result<(), EngineError>;
    fn stop(&mut self) -> Result<(), EngineError>;
}

/// A separate typed owner with close-only controls. It has no PCM processing API.
pub(crate) struct DsdOutput {
    #[cfg(target_os = "linux")]
    native: earph_alsa::DopOutput,
}
impl DsdOutput {
    pub fn open(
        config: &OutputConfiguration,
        stream: earph_core::dop::DopStream,
    ) -> Result<Self, EngineError> {
        #[cfg(target_os = "linux")]
        {
            if config.backend != OutputBackend::Alsa {
                return Err(DSD_UNSUPPORTED);
            }
            let format = PcmFormat::new(
                stream.bit_rate_hz / 16,
                stream.channels,
                earph_core::audio::PcmRepresentation::Signed24,
            )?;
            let native = earph_alsa::DopOutput::open(
                earph_alsa::OpenRequest {
                    schema_version: 1,
                    device: config.endpoint.clone(),
                    route: match config.route {
                        OutputRoute::System => earph_alsa::Route::System,
                        OutputRoute::DirectHardware => earph_alsa::Route::DirectHardware,
                    },
                    format,
                    period_frames: config.period_frames,
                    buffer_frames: config.buffer_frames,
                },
                stream,
            )
            .map_err(error)?;
            native.audio_storage_bytes().map_err(error)?;
            if native.report().observed.format != format {
                return Err(DSD_UNSUPPORTED);
            }
            Ok(Self { native })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (config, stream);
            Err(DSD_UNSUPPORTED)
        }
    }
    pub fn submitted(&self) -> u64 {
        #[cfg(target_os = "linux")]
        {
            self.native.submitted_frames()
        }
        #[cfg(not(target_os = "linux"))]
        {
            0
        }
    }
    pub fn observation(
        &mut self,
        epochs: Epochs,
    ) -> Result<earph_core::dop::DsdOutputObservation, EngineError> {
        #[cfg(target_os = "linux")]
        {
            let progress = self.native.progress().map_err(error)?;
            let report = self.native.report();
            let format = ObservedStreamFormat::from(report.observed.format);
            Ok(earph_core::dop::DsdOutputObservation {
                output_epochs: epochs,
                submitted_carrier_frames: progress.submitted_frames,
                estimated_consumed_carrier_frames: Some(progress.estimated_consumed_frames),
                backend_delay_carrier_frames: Some(progress.backend_delay_frames),
                consumption_basis: progress.measurement_boundary,
                backend: BackendObservation {
                    backend: OutputBackend::Alsa,
                    device_identity_sha256: None,
                    audio_storage_bytes: self.native.audio_storage_bytes().map_err(error)?,
                    client_format: format.clone(),
                    device_format: (report.requested.route == earph_alsa::Route::DirectHardware)
                        .then_some(format),
                    measurement_boundary: "alsa_client_negotiation",
                    preservation: PreservationState::Unverified,
                },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = epochs;
            Err(DSD_UNSUPPORTED)
        }
    }
    pub fn write(
        &mut self,
        block: &earph_core::dop::DopBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError> {
        #[cfg(target_os = "linux")]
        {
            self.native
                .write(block, offset, frames, earph_alsa::SignalPermission::Granted)
                .map(|progress| match progress {
                    earph_alsa::WriteProgress::Accepted { frames } => Some(frames),
                    earph_alsa::WriteProgress::WouldBlock => None,
                })
                .map_err(error)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (block, offset, frames);
            Err(DSD_UNSUPPORTED)
        }
    }
    pub fn wait(&mut self) -> Result<(), EngineError> {
        #[cfg(target_os = "linux")]
        {
            self.native.wait_ready(1).map(|_| ()).map_err(error)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(DSD_UNSUPPORTED)
        }
    }
    pub fn stop(&mut self) -> Result<(), EngineError> {
        #[cfg(target_os = "linux")]
        {
            self.native.stop().map_err(error)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(DSD_UNSUPPORTED)
        }
    }
}
const DSD_UNSUPPORTED: EngineError = EngineError::new(
    ErrorCode::CapabilityUnsupported,
    "typed DSD output is unavailable on this backend",
);

pub(crate) fn open(
    config: &OutputConfiguration,
    format: PcmFormat,
    epochs: Epochs,
) -> Result<Box<dyn Output>, EngineError> {
    #[cfg(target_os = "linux")]
    {
        if config.backend != OutputBackend::Alsa {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "selected output backend is unavailable on this host",
            ));
        }
        let output = earph_alsa::AlsaOutput::open(
            earph_alsa::OpenRequest {
                schema_version: 1,
                device: config.endpoint.clone(),
                route: match config.route {
                    OutputRoute::System => earph_alsa::Route::System,
                    OutputRoute::DirectHardware => earph_alsa::Route::DirectHardware,
                },
                format,
                period_frames: config.period_frames,
                buffer_frames: config.buffer_frames,
            },
            epochs,
        )
        .map_err(error)?;
        verified_output(output)
    }
    #[cfg(target_os = "macos")]
    {
        if config.backend != OutputBackend::CoreAudio {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "selected output backend is unavailable on this host",
            ));
        }
        verified_output(coreaudio::NativeOutput::open(config, format, epochs)?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (config, format, epochs);
        Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "local playback output is not implemented on this host",
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn verified_output(output: impl Output + 'static) -> Result<Box<dyn Output>, EngineError> {
    // Validate actual storage accounting before transferring native output ownership.
    output.audio_storage_bytes()?;
    Ok(Box::new(output))
}

#[cfg(target_os = "linux")]
fn error(error: earph_alsa::AlsaError) -> EngineError {
    use std::io::Write;
    // Native failures reach this non-callback boundary after adapter cleanup.
    if error.native_errno.is_some() {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}",
            serde_json::json!({"backend_fault": error})
        );
    }
    EngineError::new(error.code, error.message)
}
#[cfg(target_os = "linux")]
fn counters(progress: earph_alsa::OutputProgress) -> Counters {
    Counters {
        submitted: progress.submitted_frames,
        consumed: Some(progress.estimated_consumed_frames),
        restart_cursor: Some(progress.estimated_consumed_frames),
        delay: Some(progress.backend_delay_frames),
        basis: progress.measurement_boundary,
    }
}
#[cfg(target_os = "linux")]
impl Output for earph_alsa::AlsaOutput {
    fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
        earph_alsa::AlsaOutput::audio_storage_bytes(self).map_err(error)
    }
    fn is_running(&self) -> bool {
        self.state() == earph_alsa::OutputState::Running
    }
    fn observation(&self) -> Result<BackendObservation, EngineError> {
        let report = self.report();
        let format = ObservedStreamFormat::from(report.observed.format);
        Ok(BackendObservation {
            backend: OutputBackend::Alsa,
            device_identity_sha256: None,
            audio_storage_bytes: self.audio_storage_bytes().map_err(error)?,
            client_format: format.clone(),
            device_format: (report.requested.route == earph_alsa::Route::DirectHardware)
                .then_some(format),
            measurement_boundary: "alsa_client_negotiation",
            preservation: PreservationState::Unverified,
        })
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError> {
        self.write(block, offset, frames, earph_alsa::SignalPermission::Granted)
            .map(|progress| match progress {
                earph_alsa::WriteProgress::Accepted { frames } => Some(frames),
                earph_alsa::WriteProgress::WouldBlock => None,
            })
            .map_err(error)
    }
    fn progress(&mut self) -> Result<Counters, EngineError> {
        self.progress().map(counters).map_err(error)
    }
    fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
        self.suspend(epochs).map(counters).map_err(error)
    }
    fn recover(&mut self, epochs: Epochs) -> Result<bool, EngineError> {
        self.recover_underrun(epochs).map_err(error)
    }
    fn finish(&mut self) -> Result<Option<Counters>, EngineError> {
        match self
            .finish(earph_alsa::SignalPermission::Granted)
            .map_err(error)?
        {
            earph_alsa::DrainProgress::Complete => Ok(Some(Counters {
                submitted: self.submitted_frames(),
                consumed: Some(self.submitted_frames()),
                restart_cursor: Some(self.submitted_frames()),
                delay: Some(0),
                basis: "alsa_client_estimate",
            })),
            earph_alsa::DrainProgress::WouldBlock => Ok(None),
        }
    }
    fn wait(&mut self) -> Result<(), EngineError> {
        self.wait_ready(2).map(|_| ()).map_err(error)
    }
    fn stop(&mut self) -> Result<(), EngineError> {
        self.stop().map_err(error)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    #[test]
    fn output_trait_reports_actual_alsa_scratch_after_stop() {
        use earph_core::{
            audio::{ChannelLayout, PcmRepresentation},
            ids::{DeviceId, ProfileId},
            safety::{FidelityPolicy, VolumeMode},
        };
        let config = OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "null".into(),
            device_id: DeviceId::new("null").unwrap(),
            profile_id: ProfileId::new("owned-null").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        };
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
        let mut output = open(&config, format, Epochs::default()).unwrap();
        assert_eq!(output.audio_storage_bytes().unwrap(), 384);
        output.stop().unwrap();
        assert_eq!(output.audio_storage_bytes().unwrap(), 384);
    }
    #[test]
    fn unwritable_diagnostics_preserve_original_backend_error() {
        let name = "output::tests::unwritable_diagnostics_preserve_original_backend_error";
        if std::env::var("EARPH_DIAGNOSTIC_TEST_CHILD").as_deref() != Ok(name) {
            let full = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env("EARPH_DIAGNOSTIC_TEST_CHILD", name)
                .stderr(full)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stdout)
            );
            return;
        }
        let original = earph_alsa::AlsaError {
            code: ErrorCode::OutputFault,
            message: "ALSA wait failed without recovery or fallback",
            native_errno: Some(5),
        };
        let result = error(original);
        assert_eq!(result.code, ErrorCode::OutputFault);
        assert_eq!(
            result.message,
            "ALSA wait failed without recovery or fallback"
        );
    }
}
