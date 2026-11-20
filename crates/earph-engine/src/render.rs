// ABOUTME: Bridges prepared f64 graph processing into bounded native integer periods.
// ABOUTME: Applies shared safety gain before one explicit saturating quantization.
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation},
    gain::GainRamp,
    ids::Epochs,
};
use earph_dsp::{CompiledPlan, GraphError, GraphLifecycle, PreparedGraph};

/// Fixed processing diagnostics retain a schedule index without cloning node identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderError {
    pub code: ErrorCode,
    pub message: &'static str,
    pub failure_node_index: Option<usize>,
}
impl From<EngineError> for RenderError {
    fn from(error: EngineError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            failure_node_index: None,
        }
    }
}

/// Audio payload capacities exclude plan metadata, allocator overhead, and backend storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderResources {
    pub kernel_audio_bytes: usize,
    pub host_input_bytes: usize,
    pub host_output_bytes: usize,
    /// The native block has fixed storage length; its Vec capacity is not exposed by Core.
    pub native_period_bytes: usize,
    pub audio_payload_bytes: usize,
}
impl RenderResources {
    /// Admit the complete audio payload before preparing graph or bridge buffers.
    pub fn required(plan: &CompiledPlan, native: PcmFormat) -> Result<Self, RenderError> {
        let context = plan.context();
        let quantum = u64::try_from(context.quantum_frames).map_err(|_| resource_error())?;
        let kernel_audio_bytes =
            usize::try_from(plan.resources().buffer_bytes).map_err(|_| resource_error())?;
        let host_input_bytes = context.input_format.storage_bytes(quantum)?;
        let host_output_bytes = context.output_format.storage_bytes(quantum)?;
        let native_period_bytes = native.storage_bytes(quantum)?;
        let audio_payload_bytes = kernel_audio_bytes
            .checked_add(host_input_bytes)
            .and_then(|n| n.checked_add(host_output_bytes))
            .and_then(|n| n.checked_add(native_period_bytes))
            .ok_or_else(resource_error)?;
        Ok(Self {
            kernel_audio_bytes,
            host_input_bytes,
            host_output_bytes,
            native_period_bytes,
            audio_payload_bytes,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderReport {
    pub valid_frames: usize,
    /// Generated tail frames never consume source input.
    pub source_frames: usize,
    pub saturation_count: u64,
    pub input_peak: f64,
    pub graph_peak: f64,
    pub prequantization_peak: f64,
    pub output_peak: f64,
    pub latency_frames: u64,
    pub tail_frames: u64,
    pub tail_finished: bool,
}

pub struct PreparedRender {
    graph: PreparedGraph,
    input_format: PcmFormat,
    input: Vec<f64>,
    output: Vec<f64>,
    // The serial worker may silence its unaccepted suffix without regenerating graph output.
    pub(crate) period: PcmBlock,
    gain: GainRamp,
    resources: RenderResources,
    sequence: u64,
    output_frames: u64,
    failure: Option<RenderError>,
}
impl PreparedRender {
    /// Prepare on the non-processing owner before handing this pipeline to playback.
    pub fn prepare(
        plan: CompiledPlan,
        input_format: PcmFormat,
        native_format: PcmFormat,
        epochs: Epochs,
        budget: MemoryBudget,
    ) -> Result<Self, RenderError> {
        input_format.representation().integer_bounds()?;
        native_format.representation().integer_bounds()?;
        let context = plan.context();
        if context.input_format.representation() != PcmRepresentation::Float64
            || context.output_format.representation() != PcmRepresentation::Float64
            || !matches_geometry(input_format, context.input_format)
            || !matches_geometry(native_format, context.output_format)
            || input_format.sample_rate_hz() != native_format.sample_rate_hz()
        {
            return Err(format_error());
        }
        let mut resources = RenderResources::required(&plan, native_format)?;
        budget.admit(resources.audio_payload_bytes)?;
        let graph = PreparedGraph::prepare(plan, epochs).map_err(graph_error)?;
        let input = buffer(resources.host_input_bytes / size_of::<f64>())?;
        let output = buffer(resources.host_output_bytes / size_of::<f64>())?;
        let period = PcmBlock::prepare(native_format, context.quantum_frames, epochs, budget)?;
        let gain = GainRamp::new(native_format)?;
        let actual_kernel = graph.resident_audio_bytes().ok_or_else(resource_error)?;
        let actual_input = input
            .capacity()
            .checked_mul(size_of::<f64>())
            .ok_or_else(resource_error)?;
        let actual_output = output
            .capacity()
            .checked_mul(size_of::<f64>())
            .ok_or_else(resource_error)?;
        // Reject allocator capacity growth beyond the admitted payload.
        if actual_kernel != resources.kernel_audio_bytes
            || actual_input != resources.host_input_bytes
            || actual_output != resources.host_output_bytes
        {
            return Err(resource_error());
        }
        resources.native_period_bytes =
            native_format.storage_bytes(period.capacity_frames() as u64)?;
        Ok(Self {
            graph,
            input_format,
            input,
            output,
            period,
            gain,
            resources,
            sequence: 0,
            output_frames: 0,
            failure: None,
        })
    }

    /// The worker must finish submitting the previous period before processing another span.
    pub fn process(
        &mut self,
        input: &PcmBlock,
        offset_frames: usize,
        frames: usize,
    ) -> Result<RenderReport, RenderError> {
        self.check_failure()?;
        let header = input.header();
        if header.epochs != self.graph.epochs() {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "render input differs from the prepared stream epoch",
            )
            .into());
        }
        let end = offset_frames
            .checked_add(frames)
            .ok_or_else(geometry_error)?;
        if header.format != self.input_format {
            return Err(format_error());
        }
        if frames == 0 || frames > self.period.capacity_frames() || end as u64 > header.frame_count
        {
            return Err(geometry_error());
        }
        self.check_counters(frames)?;
        let channels = self.input_format.channels().count();
        let count = frames.checked_mul(channels).ok_or_else(geometry_error)?;
        let start = offset_frames
            .checked_mul(channels)
            .ok_or_else(geometry_error)?;
        let reference = -(self.input_format.representation().integer_bounds()?.0 as f64);
        let mut input_peak = 0.0_f64;
        for (index, target) in self.input[..count].iter_mut().enumerate() {
            *target = input
                .integer_sample(start + index)
                .ok_or_else(geometry_error)? as f64
                / reference;
            input_peak = input_peak.max(target.abs());
        }
        let output_count = frames * self.period.header().format.channels().count();
        if let Err(error) = self
            .graph
            .process(&self.input[..count], &mut self.output[..output_count])
        {
            return Err(self.retain_graph_error(error));
        }
        self.render_period(frames, frames, input_peak)
    }

    /// Drain only a valid tail prefix. Repeated completion produces zero frames.
    pub fn finish(&mut self) -> Result<RenderReport, RenderError> {
        self.check_failure()?;
        self.check_counters(self.period.capacity_frames())?;
        let frames = self
            .graph
            .finish(&mut self.output)
            .map_err(|error| self.retain_graph_error(error))?;
        if frames == 0 {
            return Ok(self.report(0, 0));
        }
        self.render_period(frames, 0, 0.0)
    }

    /// Use the same bounded gain envelope as integer playback; admission belongs to Session.
    pub fn set_gain_target(
        &mut self,
        gain_db: Option<f64>,
        ramp_frames: u32,
    ) -> Result<(), RenderError> {
        self.gain
            .set_target(gain_db, ramp_frames)
            .map_err(Into::into)
    }
    /// Expose the immutable period for partial native writes without reprocessing.
    pub fn period(&self) -> &PcmBlock {
        &self.period
    }
    pub fn graph(&self) -> &PreparedGraph {
        &self.graph
    }
    pub fn resources(&self) -> RenderResources {
        self.resources
    }

    /// Clear graph and envelope state only at an explicitly newer stream epoch.
    pub fn reset(&mut self, epochs: Epochs) -> Result<(), RenderError> {
        self.graph.reset(epochs).map_err(graph_error)?;
        self.input.fill(0.0);
        self.output.fill(0.0);
        self.gain.set_target(None, 0)?;
        self.sequence = 0;
        self.output_frames = 0;
        self.failure = None;
        Ok(())
    }
    /// Retain owned storage until retirement on the non-processing owner.
    pub fn cancel(&mut self) {
        self.graph.cancel();
    }

    fn render_period(
        &mut self,
        frames: usize,
        source_frames: usize,
        input_peak: f64,
    ) -> Result<RenderReport, RenderError> {
        let format = self.period.header().format;
        let count = frames * format.channels().count();
        let gain = self.gain.process_f64(&mut self.output[..count], frames)?;
        let quantized = quantize(&self.output[..count], &mut self.period)?;
        self.period.set_span(
            self.graph.epochs(),
            self.sequence,
            self.output_frames,
            frames as u64,
        )?;
        self.sequence += 1;
        self.output_frames += frames as u64;
        Ok(RenderReport {
            saturation_count: quantized.saturation_count,
            input_peak,
            graph_peak: gain.input_peak,
            prequantization_peak: gain.output_peak,
            output_peak: quantized.output_peak,
            ..self.report(frames, source_frames)
        })
    }
    fn report(&self, valid_frames: usize, source_frames: usize) -> RenderReport {
        RenderReport {
            valid_frames,
            source_frames,
            saturation_count: 0,
            input_peak: 0.0,
            graph_peak: 0.0,
            prequantization_peak: 0.0,
            output_peak: 0.0,
            latency_frames: self.graph.plan().latency_frames(),
            tail_frames: self.graph.plan().tail_frames(),
            tail_finished: self.graph.lifecycle() == GraphLifecycle::Finished,
        }
    }
    fn check_counters(&self, frames: usize) -> Result<(), RenderError> {
        if self.sequence.checked_add(1).is_none()
            || self.output_frames.checked_add(frames as u64).is_none()
        {
            return Err(resource_error());
        }
        Ok(())
    }
    fn check_failure(&self) -> Result<(), RenderError> {
        self.failure.map_or(Ok(()), Err)
    }
    fn retain_graph_error(&mut self, error: GraphError) -> RenderError {
        let mut error = graph_error(error);
        error.failure_node_index = self.graph.failure_node_index();
        if self.graph.lifecycle() == GraphLifecycle::Failed {
            self.failure = Some(error);
        }
        error
    }
}
pub(crate) struct QuantizationReport {
    pub saturation_count: u64,
    pub output_peak: f64,
}
pub(crate) fn quantize(
    output: &[f64],
    period: &mut PcmBlock,
) -> Result<QuantizationReport, EngineError> {
    let format = period.header().format;
    let channels = format.channels().count();
    if output.is_empty()
        || !output.len().is_multiple_of(channels)
        || output.len() / channels > period.capacity_frames()
        || output.iter().any(|sample| !sample.is_finite())
    {
        return Err(EngineError::new(
            ErrorCode::AssetInvalid,
            "invalid quantization span",
        ));
    }
    let (minimum, maximum) = format.representation().integer_bounds()?;
    let reference = -(minimum as f64);
    let mut report = QuantizationReport {
        saturation_count: 0,
        output_peak: 0.0,
    };
    // One quantization: round ties away, saturate, and apply no dither.
    for (index, sample) in output.iter().enumerate() {
        let rounded = (*sample * reference).round();
        report.saturation_count += u64::from(rounded < minimum as f64 || rounded > maximum as f64);
        let quantized = rounded.clamp(minimum as f64, maximum as f64) as i64;
        report.output_peak = report.output_peak.max((quantized as f64).abs() / reference);
        period.set_integer_sample(index, quantized)?;
    }
    Ok(report)
}
fn matches_geometry(a: PcmFormat, b: PcmFormat) -> bool {
    a.sample_rate_hz() == b.sample_rate_hz() && a.channels() == b.channels()
}
fn buffer(count: usize) -> Result<Vec<f64>, RenderError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(count)
        .map_err(|_| resource_error())?;
    buffer.resize(count, 0.0);
    Ok(buffer)
}
fn graph_error(error: GraphError) -> RenderError {
    RenderError {
        code: error.code,
        message: error.message,
        failure_node_index: None,
    }
}
fn resource_error() -> RenderError {
    EngineError::new(
        ErrorCode::ResourceBudgetExceeded,
        "render payload or counters exceed admission",
    )
    .into()
}
fn geometry_error() -> RenderError {
    EngineError::new(ErrorCode::AssetInvalid, "invalid render input span").into()
}
fn format_error() -> RenderError {
    EngineError::new(
        ErrorCode::FormatUnsupported,
        "render formats differ from the prepared graph",
    )
    .into()
}
