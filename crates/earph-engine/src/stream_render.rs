// ABOUTME: Bridges variable-rate prepared streams into bounded native integer periods.
// ABOUTME: Preserves pre-volume history across holds and separates source acceptance from output writes.
use crate::{output_replay::OutputReplay, render::quantize};
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation},
    gain::GainRamp,
    ids::Epochs,
};
use earph_dsp::stream::{
    PreparedStream, StreamError, StreamErrorKind, StreamLifecycle, StreamPlan, StreamStep,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Copy)]
pub struct StreamRenderConfig {
    pub input_format: PcmFormat,
    pub native_format: PcmFormat,
    pub period_frames: usize,
    pub queue_frames: usize,
    pub epochs: Epochs,
    pub budget_bytes: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamRenderResources {
    /// Conservative requested component bytes; excludes backend storage and allocator/RSS overhead.
    pub resident_bytes: u64,
    pub preparation_peak_bytes: u64,
    pub bridge_bytes: u64,
    pub replay_frames: usize,
}
#[derive(Debug, Clone, Copy)]
pub struct SourceSpan<'a> {
    pub block: &'a PcmBlock,
    pub offset: usize,
    pub frames: usize,
}
#[derive(Debug, Clone, Copy)]
pub struct StreamPeriodReport {
    pub frames: usize,
    pub saturation_count: u64,
    pub graph_peak: f64,
    pub prequantization_peak: f64,
    pub output_peak: f64,
}
pub struct PreparedStreamRender {
    stream: PreparedStream,
    replay: OutputReplay,
    input: Vec<f64>,
    output: Vec<f64>,
    input_format: PcmFormat,
    // Holds keep decoder packets valid. Only a true seek or replacement changes this epoch.
    source_epochs: Epochs,
    control_epochs: Epochs,
    period: PcmBlock,
    prepared_period: PcmBlock,
    gain: GainRamp,
    staged_frames: usize,
    staged_offset: usize,
    staged_report: Option<StreamPeriodReport>,
    sequence: u64,
    finishing: bool,
    failure: Option<EngineError>,
    cancel: Arc<AtomicBool>,
    resources: StreamRenderResources,
}
impl PreparedStreamRender {
    pub fn resource_requirements(
        plan: &StreamPlan,
        config: StreamRenderConfig,
    ) -> Result<StreamRenderResources, EngineError> {
        config.input_format.representation().integer_bounds()?;
        config.native_format.representation().integer_bounds()?;
        if !geometry(config.input_format, plan.input_format())
            || !geometry(config.native_format, plan.output_format())
            || config.period_frames == 0
            || config.period_frames > 8192
            || config.queue_frames < config.period_frames
        {
            return Err(format_error());
        }
        let first = plan.stage_info(0).ok_or_else(format_error)?;
        let input_frames = first.input_quantum_frames;
        let replay_frames = config
            .queue_frames
            .checked_add(config.period_frames)
            .ok_or_else(budget_error)?;
        let input = plan.input_format().storage_bytes(input_frames as u64)? as u64;
        let output = plan
            .output_format()
            .storage_bytes(config.period_frames as u64)? as u64;
        let replay = plan.output_format().storage_bytes(replay_frames as u64)? as u64;
        let native = config
            .native_format
            .storage_bytes(config.period_frames as u64)? as u64;
        let native = native.checked_mul(2).ok_or_else(budget_error)?;
        let bridge_bytes = [input, output, replay, native]
            .into_iter()
            .try_fold(size_of::<Self>() as u64, |n, v| {
                n.checked_add(v).ok_or_else(budget_error)
            })?;
        let resources = plan.resources();
        let resident_bytes = resources
            .resident_bytes
            .checked_add(bridge_bytes)
            .ok_or_else(budget_error)?;
        let preparation_peak_bytes = resources
            .preparation_peak_bytes
            .checked_add(bridge_bytes)
            .ok_or_else(budget_error)?;
        if preparation_peak_bytes > config.budget_bytes {
            return Err(budget_error());
        }
        Ok(StreamRenderResources {
            resident_bytes,
            preparation_peak_bytes,
            bridge_bytes,
            replay_frames,
        })
    }
    pub fn prepare(
        plan: StreamPlan,
        config: StreamRenderConfig,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, EngineError> {
        let resources = Self::resource_requirements(&plan, config)?;
        if cancel.load(Ordering::Acquire) {
            return Err(cancel_error());
        }
        let first = plan.stage_info(0).ok_or_else(format_error)?;
        let input = buffer(
            first
                .input_quantum_frames
                .checked_mul(config.input_format.channels().count())
                .ok_or_else(budget_error)?,
        )?;
        let output = buffer(
            config
                .period_frames
                .checked_mul(config.native_format.channels().count())
                .ok_or_else(budget_error)?,
        )?;
        let replay = OutputReplay::prepare(
            config.native_format.channels().count(),
            resources.replay_frames,
            config.epochs,
            config
                .native_format
                .channels()
                .count()
                .checked_mul(resources.replay_frames)
                .and_then(|n| n.checked_mul(8))
                .ok_or_else(budget_error)?,
        )?;
        if replay.resident_bytes()
            != resources.replay_frames * config.native_format.channels().count() * 8
        {
            return Err(budget_error());
        }
        let period = PcmBlock::prepare(
            config.native_format,
            config.period_frames,
            config.epochs,
            MemoryBudget::default(),
        )?;
        let prepared_period = PcmBlock::prepare(
            config.native_format,
            config.period_frames,
            config.epochs,
            MemoryBudget::default(),
        )?;
        let gain = GainRamp::new(config.native_format)?;
        let stream =
            PreparedStream::prepare(plan, config.epochs, cancel.clone()).map_err(stream_error)?;
        Ok(Self {
            stream,
            replay,
            input,
            output,
            input_format: config.input_format,
            source_epochs: config.epochs,
            control_epochs: config.epochs,
            period,
            prepared_period,
            gain,
            staged_frames: 0,
            staged_offset: 0,
            staged_report: None,
            sequence: 0,
            finishing: false,
            failure: None,
            cancel,
            resources,
        })
    }
    pub fn resources(&self) -> StreamRenderResources {
        self.resources
    }
    pub fn stream(&self) -> &PreparedStream {
        &self.stream
    }
    pub fn control_epochs(&self) -> Epochs {
        self.control_epochs
    }
    pub fn period(&self) -> &PcmBlock {
        &self.period
    }
    pub fn period_offset(&self) -> usize {
        self.staged_offset
    }
    pub fn set_gain_target(
        &mut self,
        gain_db: Option<f64>,
        ramp_frames: u32,
    ) -> Result<(), EngineError> {
        self.check_live()?;
        self.gain.set_target(gain_db, ramp_frames)
    }
    pub fn pump(&mut self, source: Option<SourceSpan<'_>>) -> Result<StreamStep, EngineError> {
        self.check_live()?;
        let mut samples = 0;
        if let Some(source) = source {
            let header = source.block.header();
            if header.epochs != self.source_epochs {
                return Err(EngineError::new(
                    ErrorCode::StaleSession,
                    "stream render source epoch changed",
                ));
            }
            if header.format != self.input_format {
                return Err(format_error());
            }
            let channels = self.input_format.channels().count();
            let end = source
                .offset
                .checked_add(source.frames)
                .ok_or_else(span_error)?;
            samples = source.frames.checked_mul(channels).ok_or_else(span_error)?;
            if end as u64 > header.frame_count || samples > self.input.len() {
                return Err(span_error());
            }
            let reference = -(self.input_format.representation().integer_bounds()?.0 as f64);
            for (index, target) in self.input[..samples].iter_mut().enumerate() {
                *target = source
                    .block
                    .integer_sample(source.offset * channels + index)
                    .ok_or_else(span_error)? as f64
                    / reference;
            }
        }
        let output_frames = self
            .replay
            .available_frames()
            .min(self.period.capacity_frames());
        if output_frames == 0 {
            return Ok(StreamStep {
                need_output: true,
                ..StreamStep::default()
            });
        }
        let channels = self.period.header().format.channels().count();
        let step = self
            .stream
            .pump(
                &self.input[..samples],
                &mut self.output[..output_frames * channels],
            )
            .map_err(|e| self.retain_stream_error(e))?;
        self.replay
            .append(&self.output[..step.produced_frames * channels])
            .map_err(|e| self.retain_error(e))?;
        Ok(step)
    }
    pub fn finish(&mut self) -> Result<StreamStep, EngineError> {
        self.check_live()?;
        if !self.finishing {
            self.stream
                .begin_finish()
                .map_err(|e| self.retain_stream_error(e))?;
            self.finishing = true;
        }
        let output_frames = self
            .replay
            .available_frames()
            .min(self.period.capacity_frames());
        if output_frames == 0 {
            return Ok(StreamStep {
                need_output: true,
                ..StreamStep::default()
            });
        }
        let channels = self.period.header().format.channels().count();
        let step = self
            .stream
            .finish(&mut self.output[..output_frames * channels])
            .map_err(|e| self.retain_stream_error(e))?;
        self.replay
            .append(&self.output[..step.produced_frames * channels])
            .map_err(|e| self.retain_error(e))?;
        Ok(step)
    }
    pub fn stage(&mut self) -> Result<Option<StreamPeriodReport>, EngineError> {
        self.check_live()?;
        if let Some(report) = self.staged_report {
            return Ok(Some(report));
        }
        let frames = self.replay.copy_pending(&mut self.output)?;
        if frames == 0 {
            return Ok(None);
        }
        let prepared = self.prepare_period(frames)?;
        self.publish_period(prepared)
    }
    fn prepare_period(&mut self, frames: usize) -> Result<PreparedPeriod, EngineError> {
        let sequence = self.sequence.checked_add(1).ok_or_else(budget_error)?;
        let channels = self.period.header().format.channels().count();
        let mut next_gain = self.gain.clone();
        let gain = next_gain.process_f64(&mut self.output[..frames * channels], frames)?;
        let report = quantize(&self.output[..frames * channels], &mut self.prepared_period)?;
        let state = self.replay.state();
        self.prepared_period.set_span(
            self.control_epochs,
            self.sequence,
            state.next_submission - state.backend_origin,
            frames as u64,
        )?;
        let report = StreamPeriodReport {
            frames,
            saturation_count: report.saturation_count,
            graph_peak: gain.input_peak,
            prequantization_peak: gain.output_peak,
            output_peak: report.output_peak,
        };
        Ok(PreparedPeriod {
            gain: next_gain,
            report,
            sequence,
        })
    }
    fn publish_period(
        &mut self,
        prepared: PreparedPeriod,
    ) -> Result<Option<StreamPeriodReport>, EngineError> {
        self.check_live()?;
        std::mem::swap(&mut self.period, &mut self.prepared_period);
        self.gain = prepared.gain;
        self.sequence = prepared.sequence;
        self.staged_frames = prepared.report.frames;
        self.staged_offset = 0;
        let report = prepared.report;
        self.staged_report = Some(report);
        Ok(Some(report))
    }
    pub fn accepted(&mut self, frames: usize) -> Result<(), EngineError> {
        self.check_live()?;
        if frames > self.staged_frames - self.staged_offset {
            return Err(span_error());
        }
        self.replay.submitted(frames)?;
        self.staged_offset += frames;
        if self.staged_offset == self.staged_frames {
            self.clear_staged();
        }
        Ok(())
    }
    pub fn observe(&mut self, submitted: u64, restart: u64) -> Result<(), EngineError> {
        self.check_live()?;
        self.replay
            .observe(submitted, restart)
            .map_err(|e| self.retain_error(e))
    }
    pub fn hold(
        &mut self,
        submitted: u64,
        restart: u64,
        epochs: Epochs,
    ) -> Result<(), EngineError> {
        self.check_live()?;
        if let Err(error) = self.replay.hold(submitted, restart, epochs) {
            return Err(if error.code == ErrorCode::OutputFault {
                self.retain_error(error)
            } else {
                error
            });
        }
        self.control_epochs = epochs;
        self.clear_staged();
        self.gain.set_target(None, 0)?;
        self.sequence = 0;
        Ok(())
    }
    pub fn reset(&mut self, epochs: Epochs) -> Result<(), EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancel_error());
        }
        if (epochs.session, epochs.stream)
            <= (self.control_epochs.session, self.control_epochs.stream)
        {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "stream render reset requires newer epochs",
            ));
        }
        self.stream.reset(epochs).map_err(stream_error)?;
        self.replay.reset(epochs)?;
        self.source_epochs = epochs;
        self.control_epochs = epochs;
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.clear_staged();
        self.sequence = 0;
        self.finishing = false;
        self.failure = None;
        self.gain.set_target(None, 0)
    }
    pub fn finished(&self) -> bool {
        self.stream.lifecycle() == StreamLifecycle::Finished
            && self.replay.pending_frames() == 0
            && self.staged_report.is_none()
    }
    /// Cancel the actual shared source. Retire unused candidates by off-thread drop instead.
    pub fn cancel(&mut self) {
        self.stream.cancel();
        self.replay.cancel();
    }
    fn clear_staged(&mut self) {
        self.staged_frames = 0;
        self.staged_offset = 0;
        self.staged_report = None;
    }
    fn check_live(&self) -> Result<(), EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(cancel_error());
        }
        self.failure.map_or(Ok(()), Err)
    }
    fn retain_error(&mut self, error: EngineError) -> EngineError {
        self.failure = Some(error);
        error
    }
    fn retain_stream_error(&mut self, error: StreamError) -> EngineError {
        let value = stream_error(error);
        if matches!(
            self.stream.lifecycle(),
            StreamLifecycle::Failed | StreamLifecycle::Cancelled
        ) {
            self.failure = Some(value);
        }
        value
    }
}
struct PreparedPeriod {
    gain: GainRamp,
    report: StreamPeriodReport,
    sequence: u64,
}
fn geometry(a: PcmFormat, b: PcmFormat) -> bool {
    a.sample_rate_hz() == b.sample_rate_hz()
        && a.channels() == b.channels()
        && b.representation() == PcmRepresentation::Float64
}
fn buffer(count: usize) -> Result<Vec<f64>, EngineError> {
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| budget_error())?;
    if v.capacity() != count {
        return Err(budget_error());
    }
    v.resize(count, 0.0);
    Ok(v)
}
fn stream_error(error: StreamError) -> EngineError {
    let code = match error.kind {
        StreamErrorKind::Budget | StreamErrorKind::Counter => ErrorCode::ResourceBudgetExceeded,
        StreamErrorKind::Cancelled => ErrorCode::OperationCancelled,
        StreamErrorKind::StaleEpoch => ErrorCode::StaleSession,
        StreamErrorKind::Graph(code) => code,
        StreamErrorKind::Scheduler => ErrorCode::InternalFault,
        _ => ErrorCode::GraphInvalid,
    };
    EngineError::new(code, "prepared stream render failed")
}
fn budget_error() -> EngineError {
    EngineError::new(
        ErrorCode::ResourceBudgetExceeded,
        "stream render exceeds its admitted resources",
    )
}
fn format_error() -> EngineError {
    EngineError::new(
        ErrorCode::FormatUnsupported,
        "stream render formats or native queue geometry do not match",
    )
}
fn span_error() -> EngineError {
    EngineError::new(
        ErrorCode::AssetInvalid,
        "invalid stream render source or native span",
    )
}
fn cancel_error() -> EngineError {
    EngineError::new(ErrorCode::OperationCancelled, "stream render was cancelled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
    use earph_dsp::{
        resample::SrcDesign,
        stream::{StreamBudget, StreamStagePlan},
    };

    #[test]
    fn cancellation_between_quantization_and_publication_preserves_the_last_period_and_gain() {
        let design = SrcDesign::analyze(
            48000,
            48000,
            ChannelLayout::Mono,
            64,
            SrcPreset::Responsive,
            16 << 20,
        )
        .unwrap();
        let plan = StreamPlan::new(
            vec![StreamStagePlan::Src(design)],
            &[1.0],
            StreamBudget::default(),
        )
        .unwrap();
        let format =
            PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut render = PreparedStreamRender::prepare(
            plan,
            StreamRenderConfig {
                input_format: format,
                native_format: format,
                period_frames: 2,
                queue_frames: 4,
                epochs: Epochs::default(),
                budget_bytes: 64 << 20,
            },
            cancel.clone(),
        )
        .unwrap();
        render.set_gain_target(Some(0.0), 0).unwrap();
        render.replay.append(&[0.125, 0.25]).unwrap();
        render.stage().unwrap().unwrap();
        render.accepted(2).unwrap();
        render.observe(2, 2).unwrap();
        let header = render.period().header();
        assert_eq!(render.period().integer_sample(0), Some(4096));
        render.set_gain_target(Some(-12.0), 2).unwrap();
        let gain = render.gain.clone();
        render.replay.append(&[0.5, 0.75]).unwrap();
        let frames = render.replay.copy_pending(&mut render.output).unwrap();
        let prepared = render.prepare_period(frames).unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            render.publish_period(prepared).unwrap_err().code,
            ErrorCode::OperationCancelled
        );
        assert_eq!(render.period().header(), header);
        assert_eq!(render.period().integer_sample(0), Some(4096));
        assert_eq!(render.period().integer_sample(1), Some(8192));
        let (mut actual, mut expected) = ([1.0; 2], [1.0; 2]);
        render.gain.clone().process_f64(&mut actual, 2).unwrap();
        gain.clone().process_f64(&mut expected, 2).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(render.sequence, 1);
        assert!(render.staged_report.is_none());
    }
}
