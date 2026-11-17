// ABOUTME: Captures integer PCM under a deterministic output clock.
// ABOUTME: Enforces generation, ordering, and capacity before accepting frames.
use earph_core::{EngineError, ErrorCode, audio::*, ids::Epochs};

const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid output frame range");
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "virtual output capacity exceeded",
);

#[derive(Debug, Default)]
pub struct VirtualClock {
    frames: u64,
    sample_rate_hz: Option<u32>,
}
impl VirtualClock {
    pub fn advance_frames(&mut self, frames: u64, sample_rate_hz: u32) -> Result<(), EngineError> {
        if sample_rate_hz == 0
            || self
                .sample_rate_hz
                .is_some_and(|rate| rate != sample_rate_hz)
        {
            return Err(EngineError::new(
                ErrorCode::FormatUnsupported,
                "virtual clock rate changed without replacement",
            ));
        }
        let next = self.frames.checked_add(frames).ok_or(BUDGET)?;
        self.frames = next;
        self.sample_rate_hz = Some(sample_rate_hz);
        Ok(())
    }
    pub fn elapsed_ns(&self) -> u128 {
        self.sample_rate_hz.map_or(0, |rate| {
            u128::from(self.frames) * 1000000000 / u128::from(rate)
        })
    }
    pub fn frames(&self) -> u64 {
        self.frames
    }
}

pub struct VirtualOutput {
    format: PcmFormat,
    epochs: Epochs,
    capture: Vec<i32>,
    capacity_samples: usize,
    position_frames: u64,
    clock: VirtualClock,
    write_limit: usize,
    failed: bool,
}
impl VirtualOutput {
    pub fn prepare(
        format: PcmFormat,
        capacity_frames: usize,
        epochs: Epochs,
    ) -> Result<Self, EngineError> {
        format.representation().integer_bounds()?;
        let samples = format.scalar_count(capacity_frames as u64)?;
        MemoryBudget::default().admit(samples.checked_mul(4).ok_or(BUDGET)?)?;
        Ok(Self {
            format,
            epochs,
            capture: Vec::with_capacity(samples),
            capacity_samples: samples,
            position_frames: 0,
            clock: VirtualClock::default(),
            write_limit: usize::MAX,
            failed: false,
        })
    }
    pub fn set_write_limit(&mut self, frames: usize) {
        self.write_limit = frames;
    }
    pub fn fail(&mut self) {
        self.failed = true;
    }
    pub fn invalidate(&mut self, epochs: Epochs, frame_position: u64) -> Result<(), EngineError> {
        if epochs == self.epochs {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "replacement must invalidate old epochs",
            ));
        }
        self.epochs = epochs;
        self.position_frames = frame_position;
        Ok(())
    }
    pub fn write(&mut self, block: &PcmBlock, offset_frames: usize) -> Result<usize, EngineError> {
        if self.failed {
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "virtual backend failed",
            ));
        }
        let header = block.header();
        if header.epochs != self.epochs {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "stale audio block",
            ));
        }
        if header.format != self.format {
            return Err(EngineError::new(
                ErrorCode::FormatUnsupported,
                "output format mismatch",
            ));
        }
        let count = usize::try_from(header.frame_count).map_err(|_| INVALID)?;
        if offset_frames > count
            || header.frame_position.checked_add(offset_frames as u64) != Some(self.position_frames)
        {
            return Err(INVALID);
        }
        let frames = (count - offset_frames).min(self.write_limit);
        let samples = self.format.scalar_count(frames as u64)?;
        let end = self.capture.len().checked_add(samples).ok_or(BUDGET)?;
        if end > self.capacity_samples {
            return Err(BUDGET);
        }
        let position = self
            .position_frames
            .checked_add(frames as u64)
            .ok_or(INVALID)?;
        self.clock
            .advance_frames(frames as u64, self.format.sample_rate_hz())?;
        let start = offset_frames * self.format.channels().count();
        for index in start..start + samples {
            self.capture
                .push(block.integer_sample(index).ok_or(INVALID)? as i32);
        }
        self.position_frames = position;
        Ok(frames)
    }
    pub fn captured_samples(&self) -> &[i32] {
        &self.capture
    }
    pub fn clock(&self) -> &VirtualClock {
        &self.clock
    }
    pub fn into_capture(self) -> Vec<i32> {
        self.capture
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn clock_overflow_preserves_previous_position() {
        let mut clock = super::VirtualClock::default();
        clock.advance_frames(u64::MAX, 48000).unwrap();
        let before = clock.elapsed_ns();
        assert!(clock.advance_frames(1, 48000).is_err());
        assert_eq!(clock.elapsed_ns(), before);
    }
}
