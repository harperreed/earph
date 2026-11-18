// ABOUTME: Enforces approval, epochs, bounded storage, and output lifecycle.
// ABOUTME: Reports submitted frames separately from backend consumption estimates.
use crate::{native, *};
use earph_core::{ErrorCode, audio::PcmBlock, ids::Epochs};

pub struct AlsaOutput {
    pcm: Option<native::Pcm>,
    report: OpenReport,
    epochs: Epochs,
    state: OutputState,
    scratch: Vec<u8>,
    submitted_frames: u64,
    next_position: Option<u64>,
}
impl AlsaOutput {
    pub fn open(request: OpenRequest, epochs: Epochs) -> Result<Self, AlsaError> {
        request.validate()?;
        let bytes = request
            .format
            .payload_bytes(request.period_frames.into())
            .map_err(|_| {
                AlsaError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "PCM period exceeds addressable storage",
                )
            })?;
        let scratch = vec![0; bytes];
        let pcm = native::Pcm::open(&request)?;
        let observed = pcm.configure(&request)?;
        let identity = pcm.identity()?;
        let report = OpenReport {
            schema_version: 1,
            pcm_type: pcm.type_name(),
            identity,
            observed,
            alsa_version: native::library_version(),
            evidence_level: "opened",
            downstream_transformations: if request.route == Route::DirectHardware {
                "none_in_alsa_hw"
            } else {
                "unknown"
            },
            payload_preservation: "unverified",
            requested: request,
        };
        Ok(Self {
            pcm: Some(pcm),
            report,
            epochs,
            state: OutputState::Prepared,
            scratch,
            submitted_frames: 0,
            next_position: None,
        })
    }
    pub fn report(&self) -> &OpenReport {
        &self.report
    }
    pub fn state(&self) -> OutputState {
        self.state
    }
    pub fn submitted_frames(&self) -> u64 {
        self.submitted_frames
    }
    /// Actual Rust-owned audio capacity, excluding metadata, allocator overhead, and ALSA storage.
    pub fn audio_storage_bytes(&self) -> Result<u64, AlsaError> {
        u64::try_from(self.scratch.capacity()).map_err(|_| {
            AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "audio storage count exceeds u64",
            )
        })
    }

    pub fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
        permission: SignalPermission,
    ) -> Result<WriteProgress, AlsaError> {
        if !matches!(self.state, OutputState::Prepared | OutputState::Running) {
            return Err(AlsaError::new(
                ErrorCode::OutputFault,
                "output is stopped or failed; open a new stream explicitly",
            ));
        }
        if permission != SignalPermission::Granted {
            return Err(AlsaError::new(
                ErrorCode::SafetyPolicyBlocked,
                "signal emission requires explicit approval for the selected route",
            ));
        }
        let header = block.header();
        if header.epochs != self.epochs {
            return Err(AlsaError::new(
                ErrorCode::StaleSession,
                "PCM belongs to a different output epoch",
            ));
        }
        if header.format != self.report.requested.format {
            return Err(AlsaError::new(
                ErrorCode::FormatUnsupported,
                "PCM format differs from the opened stream",
            ));
        }
        if header.frame_count > u64::from(self.report.requested.period_frames) {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "PCM block exceeds the prepared period",
            ));
        }
        if frames == 0
            || offset
                .checked_add(frames)
                .is_none_or(|end| end as u64 > header.frame_count)
        {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "invalid PCM write span",
            ));
        }
        let position = header
            .frame_position
            .checked_add(offset as u64)
            .ok_or(AlsaError::new(
                ErrorCode::AssetInvalid,
                "PCM frame position overflow",
            ))?;
        if self
            .next_position
            .is_some_and(|expected| expected != position)
        {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "PCM span is not contiguous with accepted frames",
            ));
        }
        self.submitted_frames
            .checked_add(frames as u64)
            .ok_or(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "submitted frame counter exhausted",
            ))?;
        block
            .pack_le(&mut self.scratch)
            .map_err(|_| AlsaError::new(ErrorCode::AssetInvalid, "cannot pack PCM block"))?;
        let frame_bytes = header
            .format
            .payload_bytes(1)
            .map_err(|_| AlsaError::new(ErrorCode::InternalFault, "invalid frame size"))?;
        let start = offset * frame_bytes;
        let end = start + frames * frame_bytes;
        let result = self.pcm.as_ref().expect("active stream owns PCM").write(
            &self.scratch[start..end],
            frames,
            frame_bytes,
        );
        match result {
            Ok(WriteProgress::Accepted { frames }) => {
                self.submitted_frames += frames as u64;
                self.next_position = Some(position + frames as u64);
                self.state = OutputState::Running;
                Ok(WriteProgress::Accepted { frames })
            }
            Ok(WriteProgress::WouldBlock) => Ok(WriteProgress::WouldBlock),
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }

    fn fail(&mut self) {
        self.state = OutputState::Failed;
        self.pcm.take();
    }
    pub fn progress(&mut self) -> Result<OutputProgress, AlsaError> {
        let Some(pcm) = &self.pcm else {
            return Err(AlsaError::new(
                ErrorCode::OutputFault,
                "output has no active backend clock",
            ));
        };
        match pcm.delay() {
            Ok(delay) if delay <= self.submitted_frames => Ok(OutputProgress {
                state: self.state,
                submitted_frames: self.submitted_frames,
                backend_delay_frames: delay,
                estimated_consumed_frames: self.submitted_frames - delay,
                measurement_boundary: "alsa_client_estimate",
            }),
            result => {
                let error = result.err().unwrap_or(AlsaError::new(
                    ErrorCode::OutputFault,
                    "ALSA delay exceeds submitted frames",
                ));
                self.fail();
                Err(error)
            }
        }
    }
    pub fn stop(&mut self) -> Result<(), AlsaError> {
        let result = self.pcm.as_ref().map_or(Ok(()), native::Pcm::stop);
        if result.is_err() || self.state == OutputState::Failed {
            self.state = OutputState::Failed;
        } else {
            self.state = OutputState::Stopped;
        }
        self.pcm.take();
        result
    }
    /// Drops pending frames and reprepares the same endpoint for an explicit new epoch.
    pub fn suspend(&mut self, epochs: Epochs) -> Result<OutputProgress, AlsaError> {
        let progress = self.progress()?;
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| {
                pcm.stop()?;
                pcm.prepare()
            });
        if let Err(error) = result {
            self.fail();
            return Err(error);
        }
        self.epochs = epochs;
        self.state = OutputState::Prepared;
        self.submitted_frames = 0;
        self.next_position = None;
        Ok(progress)
    }
    pub fn finish(&mut self, permission: SignalPermission) -> Result<DrainProgress, AlsaError> {
        if permission != SignalPermission::Granted {
            return Err(AlsaError::new(
                ErrorCode::SafetyPolicyBlocked,
                "draining requires current signal approval",
            ));
        }
        if self.state == OutputState::Stopped {
            return Ok(DrainProgress::Complete);
        }
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| {
                if self.state == OutputState::Draining {
                    pcm.drain_progress()
                } else {
                    pcm.drain()
                }
            });
        match result {
            Ok(DrainProgress::Complete) => {
                self.state = OutputState::Stopped;
                self.pcm.take();
                Ok(DrainProgress::Complete)
            }
            Ok(progress) => {
                self.state = OutputState::Draining;
                Ok(progress)
            }
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }
    pub fn wait_ready(&mut self, timeout_ms: u32) -> Result<bool, AlsaError> {
        if timeout_ms == 0 || timeout_ms > 20 {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "ALSA wait must be bounded to 1..20 milliseconds",
            ));
        }
        if self.state == OutputState::Draining {
            // Completed hardware drain can wake poll with POLLERR in the valid SETUP state.
            // Observe completion without restarting the hardware drain and its silence padding.
            std::thread::sleep(std::time::Duration::from_millis(u64::from(timeout_ms)));
            return Ok(false);
        }
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| pcm.wait_ready(timeout_ms));
        if result.is_err() {
            self.fail();
        }
        result
    }
}
