// ABOUTME: Owns the worker's explicit Core Audio stream and bounded prebuffer.
// ABOUTME: Uses route-specific restart evidence and drains through the native completion boundary.
use super::activity::ActivityTracker;
use super::*;
use earph_coreaudio::{CoreAudioOutput, OutputState, SignalPermission, WriteProgress};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static RESTART_REQUIRED: AtomicBool = AtomicBool::new(false);
static DIAGNOSTICS: FaultReporter = FaultReporter::new();
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
    if let Some(sender) = DIAGNOSTICS.get() {
        // Bounded, best-effort diagnostics never delay native close or terminal publication.
        // The original native error and first-fault latch remain authoritative if this queue fills.
        let _ = sender.try_send(FaultDiagnostic {
            operation,
            error,
            progress,
        });
    }
}
fn open_error(operation: &'static str, failure: earph_coreaudio::CoreAudioError) -> EngineError {
    report_fault(operation, Some(failure), None);
    error(failure)
}
fn counters(
    restart_cursor: Option<u64>,
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
        restart_cursor,
        delay: None,
        basis: if progress.queue_acquired_frames.is_some() {
            "audio_queue_sample_time_not_audible_position"
        } else {
            "core_audio_ioproc_delivery_not_audible_position"
        },
    })
}
pub(in crate::output) struct NativeOutput {
    native: CoreAudioOutput,
    request: OutputRequest,
    drain_deadline: Option<Instant>,
    complete: Option<Counters>,
    activity: ActivityTracker,
    #[cfg(test)]
    start_permission: Option<SignalPermission>,
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
        if DIAGNOSTICS
            .get_or_start(|| spawn_fault_reporter(std::io::stderr()))
            .is_err()
        {
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "native fault diagnostics could not start",
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
            activity: ActivityTracker::opened(epochs),
            #[cfg(test)]
            start_permission: None,
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
    fn counters(&self, operation: &'static str) -> Result<Counters, EngineError> {
        let progress = self.native.progress();
        let restart = self
            .native
            .restart_cursor_frames()
            .map_err(|failure| self.native_error(operation, failure))?;
        counters(restart, operation, progress)
    }
    fn start_ready(&mut self, eof: bool) -> Result<(), EngineError> {
        if self.native.state() == OutputState::Prepared
            && self.native.progress().submitted_frames > 0
            && (eof
                || self.native.progress().submitted_frames
                    >= u64::from(self.request.buffer_frames) * 2)
        {
            #[cfg(test)]
            let permission = self.start_permission.unwrap_or(SignalPermission::Granted);
            #[cfg(not(test))]
            let permission = SignalPermission::Granted;
            self.activity
                .start(|| self.native.start(permission))
                .map_err(|failure| self.native_error("start", failure))?;
        }
        Ok(())
    }
}
impl Output for NativeOutput {
    fn activity(&self) -> Option<earph_core::session::OutputActivity> {
        Some(self.activity.snapshot())
    }
    fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
        self.native.audio_storage_bytes().map_err(error)
    }
    fn is_running(&self) -> bool {
        self.native.state() == OutputState::Running && !self.native.initial_restart_cursor_pending()
    }
    fn observation(&self) -> Result<BackendObservation, EngineError> {
        backend_observation(
            self.native.report(),
            self.native.audio_storage_bytes().map_err(error)?,
        )
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError> {
        // Start only an already acknowledged prefix. A start error must not hide this write's acceptance.
        self.start_ready(false)?;
        let result = self
            .activity
            .write(|| self.native.write(block, offset, frames))
            .map_err(|failure| self.native_error("write", failure))?;
        match result {
            WriteProgress::Accepted { frames } => Ok(Some(frames)),
            WriteProgress::WouldBlock => Ok(None),
        }
    }
    fn initial_restart_cursor_pending(&self) -> bool {
        self.native.initial_restart_cursor_pending()
    }
    fn progress(&mut self) -> Result<Counters, EngineError> {
        self.complete.map_or_else(|| self.counters("progress"), Ok)
    }
    fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
        if self.native.state() == OutputState::Running {
            self.native
                .pause()
                .map_err(|failure| self.native_error("suspend_pause", failure))?;
        }
        let progress = self.counters("suspend_progress")?;
        self.native
            .close()
            .map_err(|failure| self.native_error("suspend_close", failure))?;
        self.activity.closed();
        self.native = CoreAudioOutput::open(self.request.clone(), epochs)
            .map_err(|failure| open_error("suspend_reopen", failure))?;
        self.activity.reopened();
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
        let progress = self.counters("finish_progress")?;
        if progress.submitted == 0
            || self
                .native
                .scheduled_output_complete()
                .map_err(|failure| self.native_error("finish_presentation", failure))?
                == Some(true)
        {
            // Completion may first be observed above. Read its proven final prefix before disposal.
            let progress = self.counters("finish_complete")?;
            self.native
                .close()
                .map_err(|failure| self.native_error("finish_close", failure))?;
            self.activity.closed();
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
            .map_err(|failure| self.native_error("stop", failure))?;
        self.activity.closed();
        Ok(())
    }
}

impl Drop for NativeOutput {
    fn drop(&mut self) {
        if let Err(failure) = self.native.close() {
            let _ = self.native_error("drop_close", failure);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        audio::{ChannelLayout, MemoryBudget, PcmRepresentation},
        ids::{DeviceId, ProfileId},
        safety::{FidelityPolicy, VolumeMode},
    };

    #[test]
    #[ignore = "requires a real interleaved float32 output; run the explicit Mac native gate"]
    fn denied_prebuffer_start_never_hides_an_accepted_native_prefix() {
        let device = earph_coreaudio::devices()
            .unwrap()
            .devices
            .into_iter()
            .find(|device| {
                device.alive
                    && device.output_streams.len() == 1
                    && device.output_streams[0].virtual_format.format_flags & !64 == 9
                    && device.output_streams[0].virtual_format.bits_per_channel == 32
                    && [1, 2].contains(&device.output_streams[0].virtual_format.channels_per_frame)
            })
            .expect("silent native test requires an interleaved float32 mono/stereo output");
        let before = earph_coreaudio::inspect(&device.uid).unwrap();
        let observed = &device.output_streams[0].virtual_format;
        assert_eq!(observed.sample_rate_hz.fract(), 0.0);
        let format = PcmFormat::new(
            observed.sample_rate_hz as u32,
            if observed.channels_per_frame == 1 {
                ChannelLayout::Mono
            } else {
                ChannelLayout::Stereo
            },
            PcmRepresentation::Signed24,
        )
        .unwrap();
        let config = OutputConfiguration {
            backend: OutputBackend::CoreAudio,
            route: OutputRoute::System,
            endpoint: device.uid.clone(),
            device_id: DeviceId::new("silent-start-test").unwrap(),
            profile_id: ProfileId::new("silent-start-test").unwrap(),
            period_frames: device.buffer_frames,
            buffer_frames: device.buffer_frames.checked_mul(4).unwrap(),
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        };
        let mut output = NativeOutput::open(&config, format, Epochs::default()).unwrap();
        output.start_permission = Some(SignalPermission::Denied);
        let frames = device.buffer_frames as usize;
        let mut block =
            PcmBlock::prepare(format, frames, Epochs::default(), MemoryBudget::default()).unwrap();
        for sequence in 0..2 {
            block
                .set_span(
                    Epochs::default(),
                    sequence,
                    sequence * frames as u64,
                    frames as u64,
                )
                .unwrap();
            assert_eq!(output.write(&block, 0, frames).unwrap(), Some(frames));
        }
        let accepted = output.native.progress().submitted_frames;
        assert_eq!(accepted, frames as u64 * 2);
        let activity = output.activity().unwrap();
        assert_eq!(activity.sdk_write_attempts, 2);
        assert_eq!(activity.accepted_frames, accepted);
        assert_eq!(activity.sdk_start_attempts, 0);
        block
            .set_span(Epochs::default(), 2, accepted, frames as u64)
            .unwrap();
        assert_eq!(
            output.write(&block, 0, frames).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        let progress = output.native.progress();
        assert_eq!(progress.submitted_frames, accepted);
        assert_eq!(progress.callback_delivered_frames, 0);
        assert!(progress.queue_acquired_frames.is_some());
        assert_eq!(output.native.state(), OutputState::Prepared);
        let activity = output.activity().unwrap();
        assert_eq!(activity.sdk_write_attempts, 2);
        assert_eq!(activity.accepted_frames, accepted);
        assert_eq!(activity.sdk_start_attempts, 1);
        output.stop().unwrap();
        assert!(output.activity().unwrap().close_confirmed);
        drop(output);
        let after = earph_coreaudio::inspect(&device.uid).unwrap();
        assert_eq!(before.nominal_rate_hz, after.nominal_rate_hz);
        assert_eq!(before.buffer_frames, after.buffer_frames);
        assert_eq!(before.hog_owner_pid, after.hog_owner_pid);
        assert_eq!(
            serde_json::to_value(before.output_streams).unwrap(),
            serde_json::to_value(after.output_streams).unwrap()
        );
    }
}
