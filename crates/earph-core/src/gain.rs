// ABOUTME: Applies explicit bounded software attenuation to integer PCM.
// ABOUTME: Uses one ramp value per frame and reports measured sample peaks.
use crate::{
    EngineError, ErrorCode,
    audio::{PcmBlock, PcmFormat},
};

#[derive(Debug, Clone)]
pub struct GainRamp {
    format: PcmFormat,
    current: f64,
    target: f64,
    step: f64,
    remaining: u32,
}
#[derive(Debug, Clone, Copy)]
pub struct GainReport {
    pub input_peak: f64,
    pub output_peak: f64,
    pub modified: bool,
}

impl GainRamp {
    pub fn new(format: PcmFormat) -> Result<Self, EngineError> {
        format.representation().integer_bounds()?;
        Ok(Self {
            format,
            current: 0.0,
            target: 0.0,
            step: 0.0,
            remaining: 0,
        })
    }
    pub fn set_target(
        &mut self,
        gain_db: Option<f64>,
        ramp_frames: u32,
    ) -> Result<(), EngineError> {
        if ramp_frames > self.format.sample_rate_hz()
            || gain_db.is_some_and(|db| !db.is_finite() || !(-120.0..=0.0).contains(&db))
        {
            return Err(EngineError::new(
                ErrorCode::SafetyPolicyBlocked,
                "software gain or ramp exceeds its bounds",
            ));
        }
        self.target = gain_db.map_or(0.0, |db| 10.0_f64.powf(db / 20.0));
        self.remaining = ramp_frames;
        if ramp_frames == 0 {
            self.current = self.target;
            self.step = 0.0;
        } else {
            self.step = (self.target - self.current) / f64::from(ramp_frames);
        }
        Ok(())
    }
    pub fn process(&mut self, block: &mut PcmBlock) -> Result<GainReport, EngineError> {
        if block.header().format != self.format {
            return Err(EngineError::new(
                ErrorCode::FormatUnsupported,
                "gain block differs from prepared format",
            ));
        }
        let (minimum, maximum) = self.format.representation().integer_bounds()?;
        let reference = -(minimum as f64);
        let channels = self.format.channels().count();
        let mut report = GainReport {
            input_peak: 0.0,
            output_peak: 0.0,
            modified: false,
        };
        for frame in 0..block.header().frame_count as usize {
            if self.remaining != 0 {
                self.current += self.step;
                self.remaining -= 1;
                if self.remaining == 0 {
                    self.current = self.target;
                }
            }
            let gain = self.current.clamp(0.0, 1.0);
            report.modified |= gain != 1.0;
            for channel in 0..channels {
                let index = frame * channels + channel;
                let sample = block.integer_sample(index).ok_or(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "gain block contains a missing sample",
                ))?;
                let output = if gain == 1.0 {
                    sample
                } else {
                    ((sample as f64 * gain).round() as i64).clamp(minimum, maximum)
                };
                report.input_peak = report.input_peak.max((sample as f64).abs() / reference);
                report.output_peak = report.output_peak.max((output as f64).abs() / reference);
                if output != sample {
                    block.set_integer_sample(index, output)?;
                }
            }
        }
        Ok(report)
    }
}
