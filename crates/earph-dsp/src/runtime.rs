// ABOUTME: Executes prepared f64 graph plans with bounded reusable audio state.
// ABOUTME: Preserves stream state and exposes explicit lifecycle and meter observations.
use crate::{CompiledNode, CompiledPlan, GraphError, NodeId, NodeKind, PreservationDecision};
use earph_core::{ErrorCode, ids::Epochs};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphLifecycle {
    Prepared,
    Running,
    Draining,
    Finished,
    Failed,
    Cancelled,
}

/// Aggregate absolute sample peak and scalar sample count, including valid tail frames.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct MeterObservation {
    pub peak_abs: f64,
    pub sample_count: u64,
}

pub struct PreparedGraph {
    plan: CompiledPlan,
    epochs: Epochs,
    lifecycle: GraphLifecycle,
    nodes: Vec<NodeState>,
    remaining_tail: u64,
    failure_node_index: Option<usize>,
}

struct NodeState {
    output: Vec<f64>,
    delay: DelayState,
    inputs: Vec<DelayState>,
    meter: MeterObservation,
}

struct DelayState {
    samples: Vec<f64>,
    cursor: usize,
}
impl DelayState {
    fn prepare(frames: u64, channels: usize) -> Result<Self, GraphError> {
        let samples = usize::try_from(frames)
            .ok()
            .and_then(|frames| frames.checked_mul(channels))
            .ok_or_else(resource_error)?;
        Ok(Self {
            samples: audio_buffer(samples)?,
            cursor: 0,
        })
    }
    fn tick(&mut self, input: f64) -> f64 {
        if self.samples.is_empty() {
            return input;
        }
        let output = self.samples[self.cursor];
        self.samples[self.cursor] = input;
        self.cursor += 1;
        if self.cursor == self.samples.len() {
            self.cursor = 0;
        }
        output
    }
    fn reset(&mut self) {
        self.samples.fill(0.0);
        self.cursor = 0;
    }
}

impl PreparedGraph {
    /// Allocate all audio buffers and delay state outside the processing thread.
    pub fn prepare(plan: CompiledPlan, epochs: Epochs) -> Result<Self, GraphError> {
        if plan.preservation() == PreservationDecision::RetainInteger {
            return Err(GraphError::new(
                ErrorCode::CapabilityUnsupported,
                "integer preservation requires the integer payload path",
            ));
        }
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(plan.schedule().len())
            .map_err(|_| resource_error())?;
        for node in plan.schedule() {
            let channels = node.format().channels.count();
            let delay_frames = match node.kind() {
                NodeKind::Delay { delay_frames } => *delay_frames,
                _ => 0,
            };
            let mut inputs = Vec::new();
            inputs
                .try_reserve_exact(node.inputs().len())
                .map_err(|_| resource_error())?;
            for input in node.inputs() {
                let input_channels = plan.schedule()[input.source_index]
                    .format()
                    .channels
                    .count();
                inputs.push(DelayState::prepare(
                    input.compensation_frames,
                    input_channels,
                )?);
            }
            nodes.push(NodeState {
                output: audio_buffer(plan.context().quantum_frames * channels)?,
                delay: DelayState::prepare(delay_frames, channels)?,
                inputs,
                meter: MeterObservation::default(),
            });
        }
        Ok(Self {
            plan,
            epochs,
            lifecycle: GraphLifecycle::Prepared,
            nodes,
            remaining_tail: 0,
            failure_node_index: None,
        })
    }

    pub fn plan(&self) -> &CompiledPlan {
        &self.plan
    }
    pub fn epochs(&self) -> Epochs {
        self.epochs
    }
    pub fn lifecycle(&self) -> GraphLifecycle {
        self.lifecycle
    }

    /// Resolve a failed kernel through `plan().schedule()[index].id()` without allocating.
    pub fn failure_node_index(&self) -> Option<usize> {
        self.failure_node_index
    }

    pub fn meter(&self, id: &NodeId) -> Option<MeterObservation> {
        self.plan
            .schedule()
            .iter()
            .position(|node| node.id() == id && matches!(node.kind(), NodeKind::Meter))
            .map(|index| self.nodes[index].meter)
    }

    /// Process one exact interleaved span. Invalid geometry leaves stream state unchanged.
    pub fn process(&mut self, input: &[f64], output: &mut [f64]) -> Result<usize, GraphError> {
        self.check_active()?;
        if !matches!(
            self.lifecycle,
            GraphLifecycle::Prepared | GraphLifecycle::Running
        ) {
            return Err(geometry_error("graph no longer accepts input"));
        }
        let input_channels = self.plan.context().input_format.channels().count();
        let frames = self.span_frames(input.len(), input_channels)?;
        let output_channels = self.plan.context().output_format.channels().count();
        if output.len() != frames * output_channels {
            return Err(geometry_error("output span must match input frames"));
        }
        if input.iter().any(|sample| !sample.is_finite()) {
            return Err(self.fail(self.plan.input_index(), nonfinite_error()));
        }
        if frames == 0 {
            return Ok(0);
        }
        self.execute(Some(input), output, frames)?;
        self.lifecycle = GraphLifecycle::Running;
        Ok(frames)
    }

    /// Drain a valid output prefix once. The unused suffix remains untouched.
    pub fn finish(&mut self, output: &mut [f64]) -> Result<usize, GraphError> {
        self.check_active()?;
        let channels = self.plan.context().output_format.channels().count();
        let capacity = self.span_frames(output.len(), channels)?;
        let remaining = match self.lifecycle {
            GraphLifecycle::Prepared | GraphLifecycle::Finished => 0,
            GraphLifecycle::Running => self.plan.tail_frames(),
            GraphLifecycle::Draining => self.remaining_tail,
            GraphLifecycle::Failed | GraphLifecycle::Cancelled => {
                unreachable!("checked terminal state")
            }
        };
        if remaining == 0 {
            self.lifecycle = GraphLifecycle::Finished;
            return Ok(0);
        }
        if capacity == 0 {
            return Err(geometry_error(
                "tail output requires positive frame capacity",
            ));
        }
        let frames = remaining.min(capacity as u64) as usize;
        self.execute(None, &mut output[..frames * channels], frames)?;
        self.remaining_tail = remaining - frames as u64;
        self.lifecycle = if self.remaining_tail == 0 {
            GraphLifecycle::Finished
        } else {
            GraphLifecycle::Draining
        };
        Ok(frames)
    }

    /// Start an explicitly newer stream epoch, clearing existing state in place.
    pub fn reset(&mut self, epochs: Epochs) -> Result<(), GraphError> {
        if self.lifecycle == GraphLifecycle::Cancelled {
            return Err(cancelled_error());
        }
        if (epochs.session, epochs.stream) <= (self.epochs.session, self.epochs.stream) {
            return Err(GraphError::new(
                ErrorCode::StaleSession,
                "reset requires a newer stream or session epoch",
            ));
        }
        for node in &mut self.nodes {
            node.output.fill(0.0);
            node.delay.reset();
            for input in &mut node.inputs {
                input.reset();
            }
            node.meter = MeterObservation::default();
        }
        self.epochs = epochs;
        self.remaining_tail = 0;
        self.failure_node_index = None;
        self.lifecycle = GraphLifecycle::Prepared;
        Ok(())
    }

    /// Stop permanently while retaining resources for disposal outside processing.
    pub fn cancel(&mut self) {
        self.lifecycle = GraphLifecycle::Cancelled;
    }

    fn check_active(&self) -> Result<(), GraphError> {
        match self.lifecycle {
            GraphLifecycle::Cancelled => Err(cancelled_error()),
            GraphLifecycle::Failed => Err(GraphError::new(
                ErrorCode::OutputFault,
                "graph processing failed",
            )),
            _ => Ok(()),
        }
    }
    fn span_frames(&self, samples: usize, channels: usize) -> Result<usize, GraphError> {
        if !samples.is_multiple_of(channels)
            || samples / channels > self.plan.context().quantum_frames
        {
            return Err(geometry_error(
                "span must contain whole frames within the quantum",
            ));
        }
        Ok(samples / channels)
    }
    fn fail(&mut self, index: usize, error: GraphError) -> GraphError {
        self.failure_node_index = Some(index);
        self.lifecycle = GraphLifecycle::Failed;
        error
    }
    fn execute(
        &mut self,
        input: Option<&[f64]>,
        output: &mut [f64],
        frames: usize,
    ) -> Result<(), GraphError> {
        for (index, node) in self.plan.schedule().iter().enumerate() {
            let (previous, current) = self.nodes.split_at_mut(index);
            if let Err(error) = execute_node(node, previous, &mut current[0], input, frames) {
                return Err(self.fail(index, error));
            }
        }
        output.copy_from_slice(&self.nodes[self.plan.output_index()].output[..output.len()]);
        Ok(())
    }
}

fn execute_node(
    node: &CompiledNode,
    previous: &[NodeState],
    state: &mut NodeState,
    input: Option<&[f64]>,
    frames: usize,
) -> Result<(), GraphError> {
    let channels = node.format().channels.count();
    let samples = frames * channels;
    match node.kind() {
        NodeKind::Input => {
            if let Some(input) = input {
                state.output[..samples].copy_from_slice(input);
            } else {
                state.output[..samples].fill(0.0);
            }
        }
        NodeKind::Matrix {
            input_channels,
            coefficients,
        } => {
            let input_channels = input_channels.count();
            let source = &previous[node.inputs()[0].source_index].output;
            for frame in 0..frames {
                let left = state.inputs[0].tick(source[frame * input_channels]);
                let right = if input_channels == 2 {
                    state.inputs[0].tick(source[frame * input_channels + 1])
                } else {
                    0.0
                };
                for channel in 0..channels {
                    let row = channel * input_channels;
                    let mut value = coefficients[row] * left;
                    if input_channels == 2 {
                        value += coefficients[row + 1] * right;
                    }
                    if !value.is_finite() {
                        return Err(nonfinite_error());
                    }
                    state.output[frame * channels + channel] = value;
                }
            }
        }
        NodeKind::Sum => {
            for sample in 0..samples {
                let mut value = 0.0;
                for (input, delay) in node.inputs().iter().zip(&mut state.inputs) {
                    value += delay.tick(previous[input.source_index].output[sample]);
                }
                if !value.is_finite() {
                    return Err(nonfinite_error());
                }
                state.output[sample] = value;
            }
        }
        kind @ (NodeKind::Output
        | NodeKind::Bypass
        | NodeKind::Gain { .. }
        | NodeKind::Delay { .. }
        | NodeKind::Meter) => {
            let source = &previous[node.inputs()[0].source_index].output;
            for (sample, source) in source.iter().take(samples).enumerate() {
                let value = state.inputs[0].tick(*source);
                let value = match kind {
                    NodeKind::Gain { gain_linear } => value * gain_linear,
                    NodeKind::Delay { .. } => state.delay.tick(value),
                    _ => value,
                };
                if !value.is_finite() {
                    return Err(nonfinite_error());
                }
                state.output[sample] = value;
                if matches!(kind, NodeKind::Meter) {
                    state.meter.peak_abs = state.meter.peak_abs.max(value.abs());
                }
            }
            if matches!(kind, NodeKind::Meter) {
                state.meter.sample_count = state
                    .meter
                    .sample_count
                    .checked_add(samples as u64)
                    .ok_or_else(resource_error)?;
            }
        }
    }
    Ok(())
}

fn audio_buffer(samples: usize) -> Result<Vec<f64>, GraphError> {
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(samples)
        .map_err(|_| resource_error())?;
    buffer.resize(samples, 0.0);
    Ok(buffer)
}
fn resource_error() -> GraphError {
    GraphError::new(
        ErrorCode::ResourceBudgetExceeded,
        "graph state allocation or counter exceeds its bounds",
    )
}
fn geometry_error(message: &'static str) -> GraphError {
    GraphError::new(ErrorCode::GraphInvalid, message)
}
fn nonfinite_error() -> GraphError {
    GraphError::new(
        ErrorCode::OutputFault,
        "graph input or kernel output is non-finite",
    )
}
fn cancelled_error() -> GraphError {
    GraphError::new(
        ErrorCode::OperationCancelled,
        "graph processing was cancelled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompileContext, GraphBudget, GraphDocument, ProcessingPolicy, compile};
    use earph_core::audio::{ChannelLayout, PcmFormat, PcmRepresentation};

    #[test]
    fn prepared_audio_storage_matches_the_admitted_budget_including_compensation() {
        let document: GraphDocument = serde_json::from_str(r#"{
            "schema_version": 1,
            "nodes": [
                {"id":"in","kind":{"kind":"input"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":8}},
                {"id":"delay","kind":{"kind":"delay","delay_frames":3},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":8}},
                {"id":"sum","kind":{"kind":"sum"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":8}},
                {"id":"out","kind":{"kind":"output"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":8}}
            ],
            "edges": [
                {"from":"in","to":"delay","from_port":0,"to_port":0},
                {"from":"in","to":"sum","from_port":0,"to_port":0},
                {"from":"delay","to":"sum","from_port":0,"to_port":1},
                {"from":"sum","to":"out","from_port":0,"to_port":0}
            ], "output":"out"
        }"#).unwrap();
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
        let context = CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 8,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget {
                max_buffer_bytes: 608,
                ..GraphBudget::default()
            },
        };
        let plan = compile(&document, &context).unwrap();
        let mut graph = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
        let output_bytes: usize = graph.nodes.iter().map(|node| node.output.len() * 8).sum();
        let delay_bytes: usize = graph
            .nodes
            .iter()
            .map(|node| {
                (node.delay.samples.len()
                    + node
                        .inputs
                        .iter()
                        .map(|input| input.samples.len())
                        .sum::<usize>())
                    * 8
            })
            .sum();
        assert_eq!(output_bytes, 512);
        assert_eq!(delay_bytes, 96);
        assert_eq!(graph.plan().resources().buffer_bytes, 608);
        let mut output = [99.0; 8];
        graph
            .process(&[1.0, 10.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], &mut output)
            .unwrap();
        assert_eq!(output, [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 20.0]);
    }

    #[test]
    fn delay_ring_keeps_interleaved_channels_separate_and_clears_in_place() {
        let mut delay = DelayState::prepare(2, 2).unwrap();
        let captured: Vec<_> = [1.0, 10.0, 2.0, 20.0, 3.0, 30.0]
            .into_iter()
            .map(|sample| delay.tick(sample))
            .collect();
        assert_eq!(captured, [0.0, 0.0, 0.0, 0.0, 1.0, 10.0]);
        delay.reset();
        assert_eq!(delay.tick(9.0), 0.0);
        assert_eq!(delay.tick(90.0), 0.0);
        assert_eq!(delay.tick(8.0), 0.0);
        assert_eq!(delay.tick(80.0), 0.0);
        assert_eq!(delay.tick(0.0), 9.0);
        assert_eq!(delay.tick(0.0), 90.0);
    }
}
