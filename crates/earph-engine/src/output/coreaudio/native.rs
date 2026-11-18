// ABOUTME: Owns the worker's explicit Core Audio stream and bounded prebuffer.
// ABOUTME: Restarts only from callback boundaries and drains through the SDK clock boundary.
use super::*;
use earph_coreaudio::{CoreAudioOutput, OutputState, SignalPermission, WriteProgress};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static RESTART_REQUIRED: AtomicBool = AtomicBool::new(false);
fn error(error: earph_coreaudio::CoreAudioError) -> EngineError {
    if error.code == ErrorCode::OutputFault && error.os_status.is_some() {
        RESTART_REQUIRED.store(true, Ordering::Release);
    }
    EngineError::new(error.code, error.message)
}
fn report_fault(
    operation: &'static str,
    error: Option<earph_coreaudio::CoreAudioError>,
    progress: Option<earph_coreaudio::OutputProgress>,
) {
    // Report on the worker thread, outside the callback's allocation and I/O restrictions.
    let _ = write_fault_diagnostic(&mut std::io::stderr().lock(), operation, error, progress);
}
fn open_error(operation: &'static str, failure: earph_coreaudio::CoreAudioError) -> EngineError {
    report_fault(operation, Some(failure), None);
    error(failure)
}
fn counters(
    operation: &'static str,
    progress: earph_coreaudio::OutputProgress,
) -> Result<Counters, EngineError> {
    if progress.fault {
        report_fault(operation, None, Some(progress));
        return Err(EngineError::new(
            ErrorCode::OutputFault,
            "Core Audio output faulted; explicit recovery is required",
        ));
    }
    Ok(Counters {
        submitted: progress.submitted_frames,
        consumed: None,
        restart_cursor: Some(progress.callback_delivered_frames),
        delay: None,
        basis: "core_audio_ioproc_delivery_not_audible_position",
    })
}
pub(in crate::output) struct NativeOutput {
    native: CoreAudioOutput,
    request: OutputRequest,
    drain_deadline: Option<Instant>,
    complete: Option<Counters>,
}
impl NativeOutput {
    pub(in crate::output) fn open(
        config: &OutputConfiguration,
        format: PcmFormat,
        epochs: Epochs,
    ) -> Result<Self, EngineError> {
        if RESTART_REQUIRED.load(Ordering::Acquire) {
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "native output recovery requires an owner process restart",
            ));
        }
        let request = request(config, format)?;
        let native = CoreAudioOutput::open(request.clone(), epochs)
            .map_err(|failure| open_error("open", failure))?;
        Ok(Self {
            native,
            request,
            drain_deadline: None,
            complete: None,
        })
    }
    fn native_error(
        &self,
        operation: &'static str,
        failure: earph_coreaudio::CoreAudioError,
    ) -> EngineError {
        report_fault(operation, Some(failure), Some(self.native.progress()));
        error(failure)
    }
    fn start_ready(&mut self, eof: bool) -> Result<(), EngineError> {
        if self.native.state() == OutputState::Prepared
            && self.native.progress().submitted_frames > 0
            && (eof
                || self.native.progress().queued_frames
                    >= u64::from(self.request.buffer_frames) * 2)
        {
            self.native
                .start(SignalPermission::Granted)
                .map_err(|failure| self.native_error("start", failure))?;
        }
        Ok(())
    }
}
impl Output for NativeOutput {
    fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
        self.native.audio_storage_bytes().map_err(error)
    }
    fn is_running(&self) -> bool {
        self.native.state() == OutputState::Running
    }
    fn observation(&self) -> Result<BackendObservation, EngineError> {
        let report = self.native.report();
        let stream = &report.observed.output_streams[0];
        Ok(BackendObservation {
            backend: OutputBackend::CoreAudio,
            audio_storage_bytes: self.native.audio_storage_bytes().map_err(error)?,
            client_format: observed(&stream.virtual_format),
            device_format: Some(observed(&stream.physical)),
            measurement_boundary: "core_audio_observed_stream_formats",
            preservation: if report.requested.route == PlaybackRoute::Float32Compatibility {
                PreservationState::Modified
            } else {
                PreservationState::Unverified
            },
        })
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError> {
        let result = self
            .native
            .write(block, offset, frames)
            .map_err(|failure| self.native_error("write", failure))?;
        self.start_ready(false)?;
        match result {
            WriteProgress::Accepted { frames } => Ok(Some(frames)),
            WriteProgress::WouldBlock => Ok(None),
        }
    }
    fn progress(&mut self) -> Result<Counters, EngineError> {
        self.complete
            .map_or_else(|| counters("progress", self.native.progress()), Ok)
    }
    fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
        if self.native.state() == OutputState::Running {
            self.native
                .pause()
                .map_err(|failure| self.native_error("suspend_pause", failure))?;
        }
        let progress = counters("suspend_progress", self.native.progress())?;
        self.native
            .close()
            .map_err(|failure| self.native_error("suspend_close", failure))?;
        self.native = CoreAudioOutput::open(self.request.clone(), epochs)
            .map_err(|failure| open_error("suspend_reopen", failure))?;
        self.drain_deadline = None;
        self.complete = None;
        Ok(progress)
    }
    fn finish(&mut self) -> Result<Option<Counters>, EngineError> {
        if let Some(complete) = self.complete {
            return Ok(Some(complete));
        }
        let deadline = *self
            .drain_deadline
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(5));
        self.native
            .finish()
            .map_err(|failure| self.native_error("finish", failure))?;
        self.start_ready(true)?;
        let progress = counters("finish_progress", self.native.progress())?;
        if progress.submitted == 0
            || self
                .native
                .scheduled_output_complete()
                .map_err(|failure| self.native_error("finish_presentation", failure))?
                == Some(true)
        {
            self.native
                .close()
                .map_err(|failure| self.native_error("finish_close", failure))?;
            self.complete = Some(progress);
            return Ok(Some(progress));
        }
        if Instant::now() >= deadline {
            report_fault("finish_deadline", None, Some(self.native.progress()));
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "Core Audio scheduled handoff remained pending or unknown",
            ));
        }
        Ok(None)
    }
    fn wait(&mut self) -> Result<(), EngineError> {
        std::thread::sleep(Duration::from_millis(1));
        Ok(())
    }
    fn stop(&mut self) -> Result<(), EngineError> {
        self.native
            .close()
            .map_err(|failure| self.native_error("stop", failure))
    }
}
impl Drop for NativeOutput {
    fn drop(&mut self) {
        if let Err(failure) = self.native.close() {
            let _ = self.native_error("drop_close", failure);
        }
    }
}
