// ABOUTME: Retains bounded pre-volume output across native backend counter resets.
// ABOUTME: Rejects restart cursors outside actual retained history without changing the ledger.
use earph_core::{EngineError, ErrorCode, ids::Epochs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayState {
    pub epochs: Epochs,
    pub retained_start: u64,
    pub generated: u64,
    pub next_submission: u64,
    pub backend_origin: u64,
}

pub struct OutputReplay {
    samples: Vec<f64>,
    channels: usize,
    capacity_frames: usize,
    state: ReplayState,
    cancelled: bool,
}

impl OutputReplay {
    /// Reserve all history outside processing. The budget measures audio payload bytes.
    pub fn prepare(
        channels: usize,
        capacity_frames: usize,
        epochs: Epochs,
        budget_bytes: usize,
    ) -> Result<Self, EngineError> {
        if !(1..=2).contains(&channels) || capacity_frames == 0 {
            return Err(span_error());
        }
        let count = capacity_frames
            .checked_mul(channels)
            .ok_or_else(budget_error)?;
        let bytes = count
            .checked_mul(size_of::<f64>())
            .ok_or_else(budget_error)?;
        if bytes > budget_bytes {
            return Err(budget_error());
        }
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(count)
            .map_err(|_| budget_error())?;
        if samples
            .capacity()
            .checked_mul(size_of::<f64>())
            .is_none_or(|n| n > budget_bytes)
        {
            return Err(budget_error());
        }
        samples.resize(count, 0.0);
        Ok(Self {
            samples,
            channels,
            capacity_frames,
            state: ReplayState {
                epochs,
                retained_start: 0,
                generated: 0,
                next_submission: 0,
                backend_origin: 0,
            },
            cancelled: false,
        })
    }
    pub fn state(&self) -> ReplayState {
        self.state
    }
    pub fn resident_bytes(&self) -> usize {
        self.samples.capacity() * size_of::<f64>()
    }
    pub fn available_frames(&self) -> usize {
        self.capacity_frames - (self.state.generated - self.state.retained_start) as usize
    }
    pub fn pending_frames(&self) -> usize {
        (self.state.generated - self.state.next_submission) as usize
    }
    /// Append actual final-rate samples. Unreleased history is never overwritten.
    pub fn append(&mut self, samples: &[f64]) -> Result<(), EngineError> {
        self.check_live()?;
        if !samples.len().is_multiple_of(self.channels) || samples.iter().any(|x| !x.is_finite()) {
            return Err(span_error());
        }
        let frames = samples.len() / self.channels;
        if frames > self.available_frames() {
            return Err(budget_error());
        }
        let end = self
            .state
            .generated
            .checked_add(frames as u64)
            .ok_or_else(budget_error)?;
        for (frame, source) in samples.chunks_exact(self.channels).enumerate() {
            let slot =
                ((self.state.generated + frame as u64) % self.capacity_frames as u64) as usize;
            self.samples[slot * self.channels..(slot + 1) * self.channels].copy_from_slice(source);
        }
        self.state.generated = end;
        Ok(())
    }
    /// Copy a prefix without advancing submission or changing the caller's unused suffix.
    pub fn copy_pending(&self, output: &mut [f64]) -> Result<usize, EngineError> {
        self.check_live()?;
        if !output.len().is_multiple_of(self.channels) {
            return Err(span_error());
        }
        let frames = (output.len() / self.channels).min(self.pending_frames());
        for (frame, target) in output[..frames * self.channels]
            .chunks_exact_mut(self.channels)
            .enumerate()
        {
            let slot = ((self.state.next_submission + frame as u64) % self.capacity_frames as u64)
                as usize;
            target.copy_from_slice(&self.samples[slot * self.channels..(slot + 1) * self.channels]);
        }
        Ok(frames)
    }
    /// Advance only after the backend accepts this many output frames.
    pub fn submitted(&mut self, frames: usize) -> Result<(), EngineError> {
        self.check_live()?;
        if frames > self.pending_frames() {
            return Err(span_error());
        }
        self.state.next_submission = self
            .state
            .next_submission
            .checked_add(frames as u64)
            .ok_or_else(budget_error)?;
        Ok(())
    }
    /// Release only the prefix proven outside the backend's restart interval.
    pub fn observe(&mut self, submitted: u64, restart: u64) -> Result<(), EngineError> {
        self.check_live()?;
        self.state.retained_start = self.restart_position(submitted, restart)?;
        Ok(())
    }
    /// Rebind a held output while preserving generated samples and the DSP frontier.
    pub fn hold(
        &mut self,
        submitted: u64,
        restart: u64,
        epochs: Epochs,
    ) -> Result<(), EngineError> {
        self.check_live()?;
        if epochs.session != self.state.epochs.session || epochs.stream <= self.state.epochs.stream
        {
            return Err(stale_error());
        }
        let position = self.restart_position(submitted, restart)?;
        self.state.retained_start = position;
        self.state.next_submission = position;
        self.state.backend_origin = position;
        self.state.epochs = epochs;
        Ok(())
    }
    /// True source repositioning clears all output history at a newer epoch.
    pub fn reset(&mut self, epochs: Epochs) -> Result<(), EngineError> {
        self.check_live()?;
        if (epochs.session, epochs.stream) <= (self.state.epochs.session, self.state.epochs.stream)
        {
            return Err(stale_error());
        }
        self.samples.fill(0.0);
        self.state = ReplayState {
            epochs,
            retained_start: 0,
            generated: 0,
            next_submission: 0,
            backend_origin: 0,
        };
        Ok(())
    }
    /// Retain storage for off-thread retirement without cancelling another source's token.
    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
    fn check_live(&self) -> Result<(), EngineError> {
        if self.cancelled {
            Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "output replay was cancelled",
            ))
        } else {
            Ok(())
        }
    }
    fn restart_position(&self, submitted: u64, restart: u64) -> Result<u64, EngineError> {
        let position = self
            .state
            .backend_origin
            .checked_add(restart)
            .ok_or_else(output_error)?;
        if submitted != self.state.next_submission - self.state.backend_origin
            || restart > submitted
            || position < self.state.retained_start
            || position > self.state.next_submission
        {
            return Err(output_error());
        }
        Ok(position)
    }
}

fn span_error() -> EngineError {
    EngineError::new(ErrorCode::AssetInvalid, "invalid output replay span")
}
fn budget_error() -> EngineError {
    EngineError::new(
        ErrorCode::ResourceBudgetExceeded,
        "output replay exceeds its admitted storage or counters",
    )
}
fn stale_error() -> EngineError {
    EngineError::new(
        ErrorCode::StaleSession,
        "output replay requires a newer bound epoch",
    )
}
fn output_error() -> EngineError {
    EngineError::new(
        ErrorCode::OutputFault,
        "native restart cursor differs from retained output history",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_output_counter_never_wraps_or_overwrites_retained_samples() {
        let epochs = Epochs {
            session: 1,
            stream: 0,
        };
        let mut replay = OutputReplay::prepare(1, 4, epochs, 32).unwrap();
        replay.state = ReplayState {
            epochs,
            retained_start: u64::MAX - 2,
            generated: u64::MAX - 2,
            next_submission: u64::MAX - 2,
            backend_origin: u64::MAX - 2,
        };
        replay.append(&[0.25, 0.5]).unwrap();
        let before = replay.state();
        assert_eq!(
            replay.append(&[0.75]).unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(replay.state(), before);
        let mut output = [0.0; 2];
        assert_eq!(replay.copy_pending(&mut output).unwrap(), 2);
        assert_eq!(output, [0.25, 0.5]);
        replay.submitted(2).unwrap();
        replay
            .hold(
                2,
                2,
                Epochs {
                    session: 1,
                    stream: 1,
                },
            )
            .unwrap();
        assert_eq!(replay.pending_frames(), 0);
        assert_eq!(
            replay.append(&[0.75]).unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
}
