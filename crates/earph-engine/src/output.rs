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
pub(crate) trait Output {
    fn is_running(&self) -> bool;
    fn observation(&self) -> BackendObservation;
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError>;
    fn progress(&mut self) -> Result<Counters, EngineError>;
    fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError>;
    fn finish(&mut self) -> Result<Option<Counters>, EngineError>;
    fn wait(&mut self) -> Result<(), EngineError>;
    fn stop(&mut self) -> Result<(), EngineError>;
}

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
        Ok(Box::new(output))
    }
    #[cfg(target_os = "macos")]
    {
        if config.backend != OutputBackend::CoreAudio {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "selected output backend is unavailable on this host",
            ));
        }
        Ok(Box::new(coreaudio::NativeOutput::open(
            config, format, epochs,
        )?))
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
    fn is_running(&self) -> bool {
        self.state() == earph_alsa::OutputState::Running
    }
    fn observation(&self) -> BackendObservation {
        let report = self.report();
        let format = ObservedStreamFormat::from(report.observed.format);
        BackendObservation {
            backend: OutputBackend::Alsa,
            client_format: format.clone(),
            device_format: (report.requested.route == earph_alsa::Route::DirectHardware)
                .then_some(format),
            measurement_boundary: "alsa_client_negotiation",
            preservation: PreservationState::Unverified,
        }
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
