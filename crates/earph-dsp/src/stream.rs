// ABOUTME: Schedules serial SRC boundaries and bounded same-rate graph islands.
// ABOUTME: Separates external source acceptance, internal padding, and emitted output.
use crate::resample::{PreparedSrc, SrcDesign, SrcError, SrcMetadata, SrcStats};
use crate::{CompiledPlan, GraphLifecycle, PreparedGraph, PreservationDecision};
use earph_core::{
    ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
    ids::Epochs,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const MAX_STAGES: usize = 128;
const MAX_SWEEPS: usize = 32;
// Same-rate metadata has bounded 128-node/256-edge geometry, separate from audio storage.
const ISLAND_METADATA_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamErrorKind {
    Geometry,
    Budget,
    Adjacency,
    PeakContract,
    UnsupportedTail,
    Lifecycle,
    StaleEpoch,
    Cancelled,
    NonFinite,
    InputPeak,
    Counter,
    Scheduler,
    Graph(ErrorCode),
    Src(SrcError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamError {
    pub kind: StreamErrorKind,
    pub stage: Option<usize>,
}
impl StreamError {
    fn new(kind: StreamErrorKind) -> Self {
        Self { kind, stage: None }
    }
    fn at(mut self, stage: usize) -> Self {
        self.stage = Some(stage);
        self
    }
}
type Result<T> = std::result::Result<T, StreamError>;
fn checked_add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b)
        .ok_or(StreamError::new(StreamErrorKind::Counter))
}
fn audio_bytes(frames: usize, format: PcmFormat) -> Result<u64> {
    (frames as u64)
        .checked_mul(format.channels().count() as u64)
        .and_then(|n| n.checked_mul(8))
        .filter(|n| *n <= isize::MAX as u64)
        .ok_or(StreamError::new(StreamErrorKind::Budget))
}

#[derive(Debug, Clone, Copy)]
pub struct StreamBudget {
    pub max_stages: usize,
    pub max_resident_bytes: u64,
    pub max_preparation_bytes: u64,
    pub max_sweeps: usize,
}
impl Default for StreamBudget {
    fn default() -> Self {
        Self {
            max_stages: MAX_STAGES,
            max_resident_bytes: 256 << 20,
            max_preparation_bytes: 256 << 20,
            max_sweeps: 4,
        }
    }
}

pub enum StreamStagePlan {
    SameRate(Box<CompiledPlan>),
    Src(SrcDesign),
}
#[derive(Debug, Clone, Copy)]
pub struct StreamStageInfo {
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    pub input_quantum_frames: usize,
    pub output_quantum_frames: usize,
    pub input_peak_bounds: [f64; 2],
    pub output_peak_bounds: [f64; 2],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamResources {
    /// Conservative requested bytes, including bounded metadata reserves; excludes allocator/RSS overhead.
    pub resident_bytes: u64,
    /// Includes all resident stages and the largest sequential construction overlap.
    pub preparation_peak_bytes: u64,
    pub fifo_bytes: u64,
    pub staging_bytes: u64,
    pub kernel_bytes: u64,
    pub metadata_bytes: u64,
}
pub struct StreamPlan {
    stages: Vec<StreamStagePlan>,
    infos: Vec<StreamStageInfo>,
    input_bounds: [f64; 2],
    resources: StreamResources,
    max_sweeps: usize,
}
impl StreamPlan {
    /// Validate already-resolved stage certificates before allocating processing storage.
    pub fn new(
        stages: Vec<StreamStagePlan>,
        input_bounds: &[f64],
        budget: StreamBudget,
    ) -> Result<Self> {
        let count = stages.len();
        if count == 0
            || count > budget.max_stages.min(MAX_STAGES)
            || stages.capacity() > MAX_STAGES
            || budget.max_sweeps == 0
            || budget.max_sweeps > MAX_SWEEPS
        {
            return Err(StreamError::new(StreamErrorKind::Budget));
        }
        let first_format = stage_formats(&stages[0])?.0;
        if input_bounds.len() != first_format.channels().count()
            || input_bounds.iter().any(|n| !n.is_finite() || *n < 0.0)
        {
            return Err(StreamError::new(StreamErrorKind::PeakContract));
        }
        let mut initial = [0.0; 2];
        initial[..input_bounds.len()].copy_from_slice(input_bounds);
        let mut previous_format = first_format;
        let mut bounds = initial;
        let mut fifo_bytes = 0;
        let mut staging_bytes = 0;
        let mut kernel_bytes = 0;
        let mut metadata_bytes = (3 * size_of::<usize>()
            + size_of::<PreparedStream>()
            + count * (size_of::<StageState>() + size_of::<StreamStageInfo>())
            + (count + 1) * size_of::<FrameFifo>()) as u64;
        let mut construction_extra = 0;
        let mut previous_quantum = 0;
        for (index, stage) in stages.iter().enumerate() {
            let info = stage_info(stage, bounds).map_err(|e| e.at(index))?;
            if info.input_format != previous_format {
                return Err(StreamError::new(StreamErrorKind::Adjacency).at(index));
            }
            let fifo_frames = info.input_quantum_frames.max(previous_quantum);
            fifo_bytes = checked_add(fifo_bytes, audio_bytes(fifo_frames, info.input_format)?)?;
            staging_bytes = checked_add(
                staging_bytes,
                checked_add(
                    audio_bytes(info.input_quantum_frames, info.input_format)?,
                    audio_bytes(info.output_quantum_frames, info.output_format)?,
                )?,
            )?;
            match stage {
                StreamStagePlan::SameRate(plan) => {
                    if plan.bounded_iir_truncation() && index + 1 != count {
                        return Err(StreamError::new(StreamErrorKind::UnsupportedTail).at(index));
                    }
                    kernel_bytes = checked_add(kernel_bytes, plan.resources().buffer_bytes)?;
                    metadata_bytes = checked_add(metadata_bytes, ISLAND_METADATA_BYTES)?;
                    construction_extra = construction_extra.max(plan.preparation_scratch_bytes());
                }
                StreamStagePlan::Src(design) => {
                    let r = design.resources();
                    kernel_bytes = checked_add(kernel_bytes, r.resident_bytes)?;
                    construction_extra =
                        construction_extra.max(r.preparation_peak_bytes - r.resident_bytes);
                }
            }
            bounds = info.output_peak_bounds;
            previous_format = info.output_format;
            previous_quantum = info.output_quantum_frames;
        }
        fifo_bytes = checked_add(fifo_bytes, audio_bytes(previous_quantum, previous_format)?)?;
        staging_bytes = checked_add(
            staging_bytes,
            audio_bytes(previous_quantum, previous_format)?,
        )?;
        let resident_bytes = checked_add(
            checked_add(fifo_bytes, staging_bytes)?,
            checked_add(kernel_bytes, metadata_bytes)?,
        )?;
        // The descriptor allocation remains live while runtime stages instantiate sequentially.
        let descriptors = (stages.capacity() * size_of::<StreamStagePlan>()) as u64;
        let preparation_peak_bytes = checked_add(
            resident_bytes,
            checked_add(construction_extra, descriptors)?,
        )?;
        if resident_bytes > budget.max_resident_bytes
            || preparation_peak_bytes > budget.max_preparation_bytes
        {
            return Err(StreamError::new(StreamErrorKind::Budget));
        }
        let mut infos = Vec::new();
        infos
            .try_reserve_exact(count)
            .map_err(|_| StreamError::new(StreamErrorKind::Budget))?;
        bounds = initial;
        for (index, stage) in stages.iter().enumerate() {
            let info = stage_info(stage, bounds).map_err(|e| e.at(index))?;
            bounds = info.output_peak_bounds;
            infos.push(info);
        }
        if infos.capacity() != count {
            return Err(StreamError::new(StreamErrorKind::Budget));
        }
        Ok(Self {
            stages,
            infos,
            input_bounds: initial,
            resources: StreamResources {
                resident_bytes,
                preparation_peak_bytes,
                fifo_bytes,
                staging_bytes,
                kernel_bytes,
                metadata_bytes,
            },
            max_sweeps: budget.max_sweeps,
        })
    }
    pub fn resources(&self) -> StreamResources {
        self.resources
    }
    pub fn stage_info(&self, index: usize) -> Option<StreamStageInfo> {
        self.infos.get(index).copied()
    }
    pub fn input_format(&self) -> PcmFormat {
        self.infos[0].input_format
    }
    pub fn output_format(&self) -> PcmFormat {
        self.infos.last().unwrap().output_format
    }
    pub fn output_peak_bounds(&self) -> [f64; 2] {
        self.infos.last().unwrap().output_peak_bounds
    }
}
fn stage_formats(stage: &StreamStagePlan) -> Result<(PcmFormat, PcmFormat)> {
    match stage {
        StreamStagePlan::SameRate(plan) => {
            Ok((plan.context().input_format, plan.context().output_format))
        }
        StreamStagePlan::Src(design) => {
            let input = PcmFormat::new(
                design.input_rate(),
                design.channels(),
                PcmRepresentation::Float64,
            )
            .map_err(|_| StreamError::new(StreamErrorKind::Geometry))?;
            let output = PcmFormat::new(
                design.output_rate(),
                design.channels(),
                PcmRepresentation::Float64,
            )
            .map_err(|_| StreamError::new(StreamErrorKind::Geometry))?;
            Ok((input, output))
        }
    }
}
fn stage_info(stage: &StreamStagePlan, upstream: [f64; 2]) -> Result<StreamStageInfo> {
    let (input_format, output_format) = stage_formats(stage)?;
    if input_format.representation() != PcmRepresentation::Float64
        || output_format.representation() != PcmRepresentation::Float64
    {
        return Err(StreamError::new(StreamErrorKind::Geometry));
    }
    let (input_quantum_frames, output_quantum_frames, input_peak_bounds, output_peak_bounds) =
        match stage {
            StreamStagePlan::SameRate(plan) => {
                if plan.preservation() == PreservationDecision::RetainInteger
                    || input_format.sample_rate_hz() != output_format.sample_rate_hz()
                    || !plan.enforces_input_peak_bounds()
                {
                    return Err(StreamError::new(StreamErrorKind::PeakContract));
                }
                let mut input = [0.0; 2];
                let mut output = [0.0; 2];
                input[..input_format.channels().count()].copy_from_slice(plan.input_peak_bounds());
                output[..output_format.channels().count()]
                    .copy_from_slice(plan.output_peak_bounds());
                if input[..input_format.channels().count()]
                    .iter()
                    .zip(upstream)
                    .any(|(limit, bound)| *limit < bound)
                {
                    return Err(StreamError::new(StreamErrorKind::PeakContract));
                }
                (
                    plan.context().quantum_frames,
                    plan.context().quantum_frames,
                    input,
                    output,
                )
            }
            StreamStagePlan::Src(design) => {
                let mut output = [0.0; 2];
                for channel in 0..input_format.channels().count() {
                    output[channel] = design
                        .peak_bound(upstream[channel])
                        .map_err(|e| StreamError::new(StreamErrorKind::Src(e)))?;
                }
                (
                    design.chunk_frames(),
                    design.max_output_frames(),
                    upstream,
                    output,
                )
            }
        };
    if input_quantum_frames == 0
        || output_quantum_frames == 0
        || input_peak_bounds
            .iter()
            .chain(&output_peak_bounds)
            .any(|x| !x.is_finite() || *x < 0.0)
    {
        return Err(StreamError::new(StreamErrorKind::PeakContract));
    }
    Ok(StreamStageInfo {
        input_format,
        output_format,
        input_quantum_frames,
        output_quantum_frames,
        input_peak_bounds,
        output_peak_bounds,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamLifecycle {
    Running,
    Finishing,
    Finished,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStep {
    pub consumed_frames: usize,
    pub produced_frames: usize,
    pub work_units: usize,
    pub need_input: bool,
    pub need_output: bool,
    pub terminal: bool,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub source_frames: u64,
    pub output_frames: u64,
    pub work_units: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageStats {
    pub input_frames: u64,
    pub output_frames: u64,
    pub work_units: u64,
}

struct FrameFifo {
    samples: Vec<f64>,
    channels: usize,
    head: usize,
    len: usize,
}
impl FrameFifo {
    fn new(frames: usize, format: PcmFormat) -> Result<Self> {
        Ok(Self {
            samples: buffer(audio_bytes(frames, format)? as usize / 8)?,
            channels: format.channels().count(),
            head: 0,
            len: 0,
        })
    }
    fn capacity(&self) -> usize {
        self.samples.len() / self.channels
    }
    fn free(&self) -> usize {
        self.capacity() - self.len
    }
    fn push(&mut self, input: &[f64], frames: usize) {
        for frame in 0..frames {
            let slot = (self.head + self.len + frame) % self.capacity();
            self.samples[slot * self.channels..(slot + 1) * self.channels]
                .copy_from_slice(&input[frame * self.channels..(frame + 1) * self.channels]);
        }
        self.len += frames;
    }
    fn peek(&self, output: &mut [f64], frames: usize) {
        for frame in 0..frames {
            let slot = (self.head + frame) % self.capacity();
            output[frame * self.channels..(frame + 1) * self.channels]
                .copy_from_slice(&self.samples[slot * self.channels..(slot + 1) * self.channels]);
        }
    }
    fn discard(&mut self, frames: usize) {
        self.head = (self.head + frames) % self.capacity();
        self.len -= frames;
    }
    fn clear(&mut self) {
        self.samples.fill(0.0);
        self.head = 0;
        self.len = 0;
    }
}
enum Kernel {
    SameRate(Box<PreparedGraph>),
    Src(Box<PreparedSrc>),
}
struct StageState {
    info: StreamStageInfo,
    kernel: Kernel,
    input: Vec<f64>,
    output: Vec<f64>,
    finishing: bool,
    terminal: bool,
    stats: StageStats,
    src_stats: Option<SrcStats>,
}
pub struct PreparedStream {
    stages: Vec<StageState>,
    fifos: Vec<FrameFifo>,
    publish: Vec<f64>,
    input_bounds: [f64; 2],
    epochs: Epochs,
    lifecycle: StreamLifecycle,
    stats: StreamStats,
    resources: StreamResources,
    max_sweeps: usize,
    failure: Option<StreamError>,
    cancel: Arc<AtomicBool>,
}
impl PreparedStream {
    pub fn prepare(plan: StreamPlan, epochs: Epochs, cancel: Arc<AtomicBool>) -> Result<Self> {
        let count = plan.stages.len();
        let input_format = plan.input_format();
        let output_format = plan.output_format();
        let mut fifos = Vec::new();
        fifos
            .try_reserve_exact(count + 1)
            .map_err(|_| StreamError::new(StreamErrorKind::Budget))?;
        for (index, info) in plan.infos.iter().enumerate() {
            let frames = info.input_quantum_frames.max(if index == 0 {
                0
            } else {
                plan.infos[index - 1].output_quantum_frames
            });
            fifos.push(FrameFifo::new(frames, info.input_format)?);
        }
        let last_quantum = plan.infos.last().unwrap().output_quantum_frames;
        fifos.push(FrameFifo::new(last_quantum, output_format)?);
        let publish = buffer(audio_bytes(last_quantum, output_format)? as usize / 8)?;
        let mut stages = Vec::new();
        stages
            .try_reserve_exact(count)
            .map_err(|_| StreamError::new(StreamErrorKind::Budget))?;
        for (index, (stage, info)) in plan.stages.into_iter().zip(plan.infos).enumerate() {
            let input =
                buffer(audio_bytes(info.input_quantum_frames, info.input_format)? as usize / 8)?;
            let output =
                buffer(audio_bytes(info.output_quantum_frames, info.output_format)? as usize / 8)?;
            let kernel = match stage {
                StreamStagePlan::SameRate(plan) => {
                    let graph = PreparedGraph::prepare(*plan, epochs)
                        .map_err(|e| StreamError::new(StreamErrorKind::Graph(e.code)).at(index))?;
                    if graph.resident_audio_bytes().map(|n| n as u64)
                        != Some(graph.plan().resources().buffer_bytes)
                    {
                        return Err(StreamError::new(StreamErrorKind::Budget).at(index));
                    }
                    Kernel::SameRate(Box::new(graph))
                }
                StreamStagePlan::Src(design) => Kernel::Src(Box::new(
                    design
                        .instantiate(design.resources().preparation_peak_bytes)
                        .map_err(|e| StreamError::new(StreamErrorKind::Src(e)).at(index))?,
                )),
            };
            let src_stats = match &kernel {
                Kernel::Src(k) => Some(k.stats()),
                Kernel::SameRate(_) => None,
            };
            stages.push(StageState {
                info,
                kernel,
                input,
                output,
                finishing: false,
                terminal: false,
                stats: StageStats::default(),
                src_stats,
            });
        }
        if stages.capacity() != count || fifos.capacity() != count + 1 {
            return Err(StreamError::new(StreamErrorKind::Budget));
        }
        debug_assert_eq!(fifos[0].channels, input_format.channels().count());
        Ok(Self {
            stages,
            fifos,
            publish,
            input_bounds: plan.input_bounds,
            epochs,
            lifecycle: StreamLifecycle::Running,
            stats: StreamStats::default(),
            resources: plan.resources,
            max_sweeps: plan.max_sweeps,
            failure: None,
            cancel,
        })
    }
    pub fn resources(&self) -> StreamResources {
        self.resources
    }
    pub fn input_format(&self) -> PcmFormat {
        self.stages[0].info.input_format
    }
    pub fn output_format(&self) -> PcmFormat {
        self.stages.last().unwrap().info.output_format
    }
    pub fn epochs(&self) -> Epochs {
        self.epochs
    }
    pub fn lifecycle(&self) -> StreamLifecycle {
        self.lifecycle
    }
    pub fn stats(&self) -> StreamStats {
        self.stats
    }
    pub fn stage_stats(&self, index: usize) -> Option<StageStats> {
        self.stages.get(index).map(|s| s.stats)
    }
    pub fn src_stats(&self, index: usize) -> Option<SrcStats> {
        self.stages.get(index).and_then(|s| s.src_stats)
    }
    pub fn src_metadata(&self, index: usize) -> Option<SrcMetadata> {
        match &self.stages.get(index)?.kernel {
            Kernel::Src(src) => Some(src.metadata()),
            Kernel::SameRate(_) => None,
        }
    }
    pub fn stage_info(&self, index: usize) -> Option<StreamStageInfo> {
        self.stages.get(index).map(|s| s.info)
    }
    pub fn island(&self, index: usize) -> Option<&PreparedGraph> {
        match &self.stages.get(index)?.kernel {
            Kernel::SameRate(graph) => Some(graph),
            Kernel::Src(_) => None,
        }
    }
    pub fn failure(&self) -> Option<StreamError> {
        self.failure
    }
    pub fn begin_finish(&mut self) -> Result<()> {
        self.check_live()?;
        if self.lifecycle == StreamLifecycle::Running {
            self.lifecycle = StreamLifecycle::Finishing;
        }
        Ok(())
    }
    pub fn pump(&mut self, input: &[f64], output: &mut [f64]) -> Result<StreamStep> {
        self.check_live()?;
        if self.lifecycle != StreamLifecycle::Running {
            return Err(StreamError::new(StreamErrorKind::Lifecycle));
        }
        self.run(input, output)
    }
    pub fn finish(&mut self, output: &mut [f64]) -> Result<StreamStep> {
        self.check_live()?;
        if !output
            .len()
            .is_multiple_of(self.output_format().channels().count())
        {
            return Err(StreamError::new(StreamErrorKind::Geometry));
        }
        if self.lifecycle == StreamLifecycle::Running {
            return Err(StreamError::new(StreamErrorKind::Lifecycle));
        }
        if self.lifecycle == StreamLifecycle::Finished {
            return Ok(StreamStep {
                terminal: true,
                ..StreamStep::default()
            });
        }
        self.run(&[], output)
    }
    fn check_live(&self) -> Result<()> {
        if let Some(error) = self.failure {
            return Err(error);
        }
        if self.lifecycle == StreamLifecycle::Cancelled {
            return Err(StreamError::new(StreamErrorKind::Lifecycle));
        }
        Ok(())
    }
    fn run(&mut self, input: &[f64], output: &mut [f64]) -> Result<StreamStep> {
        if self.cancel.load(Ordering::Acquire) {
            self.cancel();
            return Err(StreamError::new(StreamErrorKind::Cancelled));
        }
        let in_channels = self.input_format().channels().count();
        let out_channels = self.output_format().channels().count();
        if !input.len().is_multiple_of(in_channels) || !output.len().is_multiple_of(out_channels) {
            return Err(StreamError::new(StreamErrorKind::Geometry));
        }
        let capacity = (output.len() / out_channels).min(self.publish.len() / out_channels);
        match self.run_inner(input, capacity) {
            Ok(step) => {
                if self.cancel.load(Ordering::Acquire) {
                    self.cancel();
                    return Err(StreamError::new(StreamErrorKind::Cancelled));
                }
                output[..step.produced_frames * out_channels]
                    .copy_from_slice(&self.publish[..step.produced_frames * out_channels]);
                Ok(step)
            }
            Err(error) => {
                if error.kind == StreamErrorKind::Cancelled {
                    self.cancel();
                } else {
                    self.lifecycle = StreamLifecycle::Failed;
                    self.failure = Some(error);
                }
                Err(error)
            }
        }
    }
    fn run_inner(&mut self, input: &[f64], capacity: usize) -> Result<StreamStep> {
        let channels = self.input_format().channels().count();
        let accepted = (input.len() / channels).min(self.fifos[0].free());
        for (index, value) in input[..accepted * channels].iter().enumerate() {
            if !value.is_finite() {
                return Err(StreamError::new(StreamErrorKind::NonFinite));
            }
            if value.abs() > self.input_bounds[index % channels] {
                return Err(StreamError::new(StreamErrorKind::InputPeak));
            }
        }
        let source_frames = checked_add(self.stats.source_frames, accepted as u64)?;
        self.fifos[0].push(input, accepted);
        self.stats.source_frames = source_frames;
        let mut produced = self.publish_output(0, capacity);
        let mut work = 0;
        for _ in 0..self.max_sweeps {
            let mut changed = false;
            for index in (0..self.stages.len()).rev() {
                if self.cancel.load(Ordering::Acquire) {
                    return Err(StreamError::new(StreamErrorKind::Cancelled));
                }
                let closed = if index == 0 {
                    self.lifecycle == StreamLifecycle::Finishing
                } else {
                    self.stages[index - 1].terminal
                };
                let (called, progressed) = self.advance_stage(index, closed)?;
                work += usize::from(called);
                changed |= progressed;
            }
            let emitted = self.publish_output(produced, capacity);
            produced += emitted;
            changed |= emitted != 0;
            if !changed {
                break;
            }
        }
        self.stats.output_frames = checked_add(self.stats.output_frames, produced as u64)?;
        self.stats.work_units = checked_add(self.stats.work_units, work as u64)?;
        if self.lifecycle == StreamLifecycle::Finishing
            && self.stages.iter().all(|s| s.terminal)
            && self.fifos.iter().all(|f| f.len == 0)
        {
            self.lifecycle = StreamLifecycle::Finished;
        }
        Ok(StreamStep {
            consumed_frames: accepted,
            produced_frames: produced,
            work_units: work,
            need_input: self.lifecycle == StreamLifecycle::Running && self.fifos[0].free() > 0,
            need_output: self.fifos.last().unwrap().len > 0,
            terminal: self.lifecycle == StreamLifecycle::Finished,
        })
    }
    fn publish_output(&mut self, produced: usize, capacity: usize) -> usize {
        let fifo = self.fifos.last_mut().unwrap();
        let frames = fifo.len.min(capacity - produced);
        fifo.peek(&mut self.publish[produced * fifo.channels..], frames);
        fifo.discard(frames);
        frames
    }
    fn advance_stage(&mut self, index: usize, upstream_closed: bool) -> Result<(bool, bool)> {
        let (before, after) = self.fifos.split_at_mut(index + 1);
        let input_fifo = &mut before[index];
        let output_fifo = &mut after[0];
        let stage = &mut self.stages[index];
        if stage.terminal || output_fifo.free() == 0 {
            return Ok((false, false));
        }
        let input_frames = input_fifo.len.min(stage.info.input_quantum_frames);
        let output_frames = output_fifo.free().min(stage.info.output_quantum_frames);
        let input_channels = stage.info.input_format.channels().count();
        let output_channels = stage.info.output_format.channels().count();
        input_fifo.peek(&mut stage.input, input_frames);
        let next_work = checked_add(stage.stats.work_units, 1)?;
        let was_finishing = stage.finishing;
        let (consumed, produced, internal, terminal) = match &mut stage.kernel {
            Kernel::SameRate(graph) => {
                if input_frames > 0 {
                    let frames = input_frames.min(output_frames);
                    graph
                        .process(
                            &stage.input[..frames * input_channels],
                            &mut stage.output[..frames * output_channels],
                        )
                        .map_err(|e| StreamError::new(StreamErrorKind::Graph(e.code)).at(index))?;
                    (frames, frames, true, false)
                } else if upstream_closed {
                    stage.finishing = true;
                    let frames = graph
                        .finish(&mut stage.output[..output_frames * output_channels])
                        .map_err(|e| StreamError::new(StreamErrorKind::Graph(e.code)).at(index))?;
                    (
                        0,
                        frames,
                        true,
                        graph.lifecycle() == GraphLifecycle::Finished,
                    )
                } else {
                    return Ok((false, false));
                }
            }
            Kernel::Src(src) => {
                let old = src.stats();
                let status = src.status();
                let step = if stage.finishing {
                    src.finish(&mut stage.output[..output_frames * output_channels])
                } else if input_frames == 0 && upstream_closed {
                    src.begin_finish()
                        .map_err(|e| StreamError::new(StreamErrorKind::Src(e)).at(index))?;
                    stage.finishing = true;
                    src.finish(&mut stage.output[..output_frames * output_channels])
                } else if input_frames > 0
                    || status.queued_output_frames > 0
                    || status.staged_input_frames == stage.info.input_quantum_frames
                {
                    src.process(
                        &stage.input[..input_frames * input_channels],
                        &mut stage.output[..output_frames * output_channels],
                    )
                } else {
                    return Ok((false, false));
                }
                .map_err(|e| StreamError::new(StreamErrorKind::Src(e)).at(index))?;
                let now = src.stats();
                stage.src_stats = Some(now);
                (
                    step.consumed_frames,
                    step.produced_frames,
                    now.library_calls != old.library_calls,
                    step.terminal,
                )
            }
        };
        if consumed > input_frames || produced > output_frames {
            return Err(StreamError::new(StreamErrorKind::Scheduler).at(index));
        }
        for (offset, value) in stage.output[..produced * output_channels]
            .iter()
            .enumerate()
        {
            if !value.is_finite()
                || value.abs() > stage.info.output_peak_bounds[offset % output_channels]
            {
                return Err(StreamError::new(StreamErrorKind::PeakContract).at(index));
            }
        }
        stage.stats.input_frames = checked_add(stage.stats.input_frames, consumed as u64)?;
        stage.stats.output_frames = checked_add(stage.stats.output_frames, produced as u64)?;
        stage.stats.work_units = next_work;
        stage.terminal = terminal;
        input_fifo.discard(consumed);
        output_fifo.push(&stage.output, produced);
        let progressed = consumed > 0
            || produced > 0
            || internal
            || terminal
            || stage.finishing != was_finishing;
        if !progressed {
            return Err(StreamError::new(StreamErrorKind::Scheduler).at(index));
        }
        Ok((true, progressed))
    }
    /// Conservative final output still possible after input EOF, including buffered valid audio and padding.
    pub fn remaining_output_bound(&self) -> Result<u64> {
        if matches!(
            self.lifecycle,
            StreamLifecycle::Finished | StreamLifecycle::Failed | StreamLifecycle::Cancelled
        ) {
            return Ok(0);
        }
        let mut remaining = self.fifos[0].len as u64;
        for (index, stage) in self.stages.iter().enumerate() {
            remaining = match &stage.kernel {
                Kernel::SameRate(graph) => {
                    let tail = if !stage.terminal && (stage.stats.input_frames > 0 || remaining > 0)
                    {
                        graph.plan().tail_frames()
                    } else {
                        0
                    };
                    checked_add(remaining, tail)?
                }
                Kernel::Src(src) => {
                    let status = src.status();
                    let metadata = src.metadata();
                    let calls = if stage.finishing {
                        status.remaining_finish_calls as u64
                    } else if remaining == 0 && src.stats().real_input_frames == 0 {
                        0
                    } else {
                        checked_add(
                            checked_add(remaining, status.staged_input_frames as u64)?,
                            u64::from(src.stats().guard_frames == 0),
                        )?
                        .div_ceil(metadata.chunk_frames as u64)
                        .checked_add(metadata.drain_zero_calls as u64)
                        .ok_or(StreamError::new(StreamErrorKind::Counter))?
                    };
                    checked_add(
                        calls
                            .checked_mul(metadata.max_output_frames as u64)
                            .ok_or(StreamError::new(StreamErrorKind::Counter))?,
                        status.queued_output_frames as u64,
                    )?
                }
            };
            remaining = checked_add(remaining, self.fifos[index + 1].len as u64)?;
        }
        Ok(remaining)
    }
    pub fn reset(&mut self, epochs: Epochs) -> Result<()> {
        if self.lifecycle == StreamLifecycle::Cancelled {
            return Err(StreamError::new(StreamErrorKind::Lifecycle));
        }
        if (epochs.session, epochs.stream) <= (self.epochs.session, self.epochs.stream) {
            return Err(StreamError::new(StreamErrorKind::StaleEpoch));
        }
        for (index, stage) in self.stages.iter_mut().enumerate() {
            match &mut stage.kernel {
                Kernel::SameRate(graph) => graph
                    .reset(epochs)
                    .map_err(|e| StreamError::new(StreamErrorKind::Graph(e.code)).at(index))?,
                Kernel::Src(src) => {
                    src.reset();
                    stage.src_stats = Some(src.stats());
                }
            }
            stage.input.fill(0.0);
            stage.output.fill(0.0);
            stage.stats = StageStats::default();
            stage.finishing = false;
            stage.terminal = false;
        }
        for fifo in &mut self.fifos {
            fifo.clear();
        }
        self.publish.fill(0.0);
        self.epochs = epochs;
        self.lifecycle = StreamLifecycle::Running;
        self.stats = StreamStats::default();
        self.failure = None;
        Ok(())
    }
    /// Permanently cancel while retaining every allocation for off-thread retirement.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
        for stage in &mut self.stages {
            match &mut stage.kernel {
                Kernel::SameRate(graph) => graph.cancel(),
                Kernel::Src(src) => src.cancel(),
            }
        }
        self.lifecycle = StreamLifecycle::Cancelled;
    }
}
fn buffer(samples: usize) -> Result<Vec<f64>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(samples)
        .map_err(|_| StreamError::new(StreamErrorKind::Budget))?;
    if result.capacity() != samples {
        return Err(StreamError::new(StreamErrorKind::Budget));
    }
    result.resize(samples, 0.0);
    Ok(result)
}
