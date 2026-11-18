// ABOUTME: Decodes local PCM and owns one native output on its worker thread.
// ABOUTME: Bounds commands, prefetches compatible joins, and drops stale work on cancellation.
use crate::{
    graph::PreparedPipeline,
    output::{self, Counters, Output},
    plan::PlanView,
};
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmBlock, PcmFormat},
    gain::GainRamp,
    graph_control::{GraphActivationAck, GraphActivationRequest},
    ids::{Epochs, QueueEntryId, RequestId},
    queue::{PreparationStatus, QueueEntry},
    session::{
        BackendObservation, PlaybackObservation, PlaybackProcessingObservation, PreservationState,
        SessionSnapshot, TransportState,
    },
};
use earph_media::LocalFile;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const ORDINARY_COMMANDS: usize = 32;
pub(crate) const COMMANDS: usize = ORDINARY_COMMANDS + 32;
const FAILED: EngineError =
    EngineError::new(ErrorCode::OutputFault, "playback worker is unavailable");
const BUSY: EngineError = EngineError::new(ErrorCode::SourceBusy, "playback control queue is full");
const ENDED: EngineError = EngineError::new(
    ErrorCode::CapabilityUnsupported,
    "source ended before reposition",
);
#[derive(Clone)]
pub(crate) struct Settings {
    pub snapshot: SessionSnapshot,
    pub playing: bool,
}
pub(crate) enum Action {
    ActivateGraph {
        settings: Settings,
        activation: GraphActivationRequest,
        pipeline: PreparedPipeline,
    },
    Start {
        settings: Settings,
        entries: Vec<QueueEntry>,
        cancel: Arc<AtomicBool>,
    },
    Settings(Settings),
    Pause(Settings),
    Seek {
        settings: Settings,
        position: u64,
    },
    Stop {
        epochs: Epochs,
    },
    Shutdown,
}
impl Action {
    fn is_safety(&self) -> bool {
        matches!(self, Self::Pause(_) | Self::Stop { .. } | Self::Shutdown)
            || matches!(self, Self::Settings(settings) if settings.snapshot.muted)
    }
}
pub(crate) enum Event {
    GraphActivated(GraphActivationAck),
    GraphFailed {
        epochs: Epochs,
        request_id: RequestId,
        error: EngineError,
    },
    Item {
        epochs: Epochs,
        id: QueueEntryId,
        status: PreparationStatus,
    },
    Ready {
        epochs: Epochs,
        observation: PlaybackObservation,
    },
    Started {
        epochs: Epochs,
    },
    Reserved {
        epochs: Epochs,
        active: QueueEntryId,
        next: Option<QueueEntryId>,
    },
    Paused {
        epochs: Epochs,
        observation: PlaybackObservation,
    },
    Stopped {
        epochs: Epochs,
        observation: Option<PlaybackObservation>,
    },
    End {
        epochs: Epochs,
    },
    Drained {
        epochs: Epochs,
        observation: PlaybackObservation,
    },
    Failed {
        epochs: Epochs,
        error: EngineError,
        observation: Option<PlaybackObservation>,
    },
}

pub(crate) struct Worker {
    sender: SyncSender<Action>,
    pub events: Receiver<Event>,
    queued: Arc<AtomicUsize>,
    cancel: Option<Arc<AtomicBool>>,
    join: Option<JoinHandle<()>>,
    pub view: Arc<Mutex<PlanView>>,
    pub progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
}
impl Worker {
    pub fn spawn() -> Self {
        let (sender, receiver) = mpsc::sync_channel(COMMANDS);
        let (events, event_receiver) = mpsc::sync_channel(1024);
        let queued = Arc::new(AtomicUsize::new(0));
        let view = Arc::new(Mutex::new(PlanView::default()));
        let progress = Arc::new(Mutex::new(None));
        let observations = progress.clone();
        let q = queued.clone();
        let v = view.clone();
        let join = thread::spawn(move || run(receiver, events, q, v, observations));
        Self {
            sender,
            events: event_receiver,
            queued,
            cancel: None,
            join: Some(join),
            view,
            progress,
        }
    }
    pub fn has_capacity(&self, safety: bool) -> bool {
        self.queued.load(Ordering::Acquire) < if safety { COMMANDS } else { ORDINARY_COMMANDS }
    }
    pub fn cancel_active(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
    }
    pub fn send(&mut self, action: Action) -> Result<(), EngineError> {
        let safety = action.is_safety();
        if !self.has_capacity(safety) {
            if safety {
                self.cancel_active();
            }
            return Err(BUSY);
        }
        if matches!(
            action,
            Action::Stop { .. } | Action::Shutdown | Action::Start { .. }
        ) && let Some(cancel) = &self.cancel
        {
            cancel.store(true, Ordering::Release);
        }
        if let Action::Start { cancel, .. } = &action {
            self.cancel = Some(cancel.clone());
        }
        self.queued.fetch_add(1, Ordering::AcqRel);
        if self.sender.try_send(action).is_err() {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            if safety {
                self.cancel_active();
            }
            return Err(FAILED);
        }
        Ok(())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
        let (_, replacement) = mpsc::sync_channel(1);
        drop(std::mem::replace(&mut self.events, replacement));
        self.queued.fetch_add(1, Ordering::AcqRel);
        let _ = self.sender.send(Action::Shutdown);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn emit(sender: &SyncSender<Event>, event: Event) -> Result<(), EngineError> {
    sender.send(event).map_err(|_| FAILED)
}
fn item(
    sender: &SyncSender<Event>,
    epochs: Epochs,
    id: &QueueEntryId,
    status: PreparationStatus,
) -> Result<(), EngineError> {
    emit(
        sender,
        Event::Item {
            epochs,
            id: id.clone(),
            status,
        },
    )
}

struct Track {
    entry: QueueEntry,
    file: LocalFile,
    packet: Option<PcmBlock>,
    offset: usize,
}
impl Track {
    fn open(
        entry: QueueEntry,
        epochs: Epochs,
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        match LocalFile::open(Path::new(&entry.path), epochs) {
            Ok(mut file) => {
                let packet = file.next_block()?;
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::Ready {
                        format: file.format(),
                        total_frames: file.total_frames(),
                    },
                )?;
                Ok(Self {
                    entry,
                    file,
                    packet,
                    offset: 0,
                })
            }
            Err(error) => {
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::Failed { code: error.code },
                )?;
                Err(error)
            }
        }
    }
    fn seek(&mut self, position: u64, epochs: Epochs) -> Result<(), EngineError> {
        self.file.seek(position, epochs)?;
        self.packet = None;
        self.offset = 0;
        Ok(())
    }
}
#[derive(Clone)]
struct Span {
    entry: QueueEntry,
    start: u64,
    source_start: u64,
    end: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderPhase {
    Feeding,
    GraphTail,
    BackendDrain,
    Finished,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPeriodKind {
    Source,
    Tail,
}
struct Playback {
    pipeline: Option<PreparedPipeline>,
    phase: RenderPhase,
    pending_kind: PendingPeriodKind,
    source_stream_position: u64,
    saturation_count: u64,
    graph_peak: Option<f64>,
    prequantization_peak: Option<f64>,
    backend: BackendObservation,
    last_counters: Counters,
    settings: Settings,
    output: Box<dyn Output>,
    current: Track,
    next: Option<Track>,
    period: PcmBlock,
    period_offset: usize,
    pending: bool,
    gain: GainRamp,
    stream_position: u64,
    source_position: u64,
    spans: Vec<Span>,
    sequence: u64,
    started: bool,
    cancel: Arc<AtomicBool>,
    view: Arc<Mutex<PlanView>>,
    input_peak: Option<f64>,
    output_peak: Option<f64>,
    modified: bool,
    progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
}
impl Playback {
    fn prepare(
        settings: Settings,
        entries: Vec<QueueEntry>,
        cancel: Arc<AtomicBool>,
        view: Arc<Mutex<PlanView>>,
        progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        let epochs = settings.snapshot.epochs;
        let selected = settings.snapshot.active_entry_id.as_ref().ok_or(FAILED)?;
        let selected_index = entries
            .iter()
            .position(|entry| &entry.id == selected)
            .ok_or(FAILED)?;
        let mut expected = None;
        for entry in &entries[selected_index..] {
            if cancel.load(Ordering::Acquire) {
                return Err(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "preparation was cancelled",
                ));
            }
            let track = Track::open(entry.clone(), epochs, events)?;
            if expected.is_some_and(|format| format != track.file.format()) {
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::Failed {
                        code: ErrorCode::FormatUnsupported,
                    },
                )?;
                return Err(EngineError::new(
                    ErrorCode::FormatUnsupported,
                    "queue transition requires an unsupported format change",
                ));
            }
            expected = Some(track.file.format());
        }
        let current = Track::open(entries[selected_index].clone(), epochs, events)?;
        let format = current.file.format();
        let config = settings.snapshot.output.as_ref().ok_or(FAILED)?;
        if cancel.load(Ordering::Acquire) {
            return Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "preparation was cancelled",
            ));
        }
        let mut output = output::open(config, format, epochs)?;
        let backend = output.observation();
        let last_counters = output.progress()?;
        let period = PcmBlock::prepare(
            format,
            config.period_frames as usize,
            epochs,
            MemoryBudget::default(),
        )?;
        let gain = GainRamp::new(format)?;
        let mut playback = Self {
            pipeline: None,
            phase: RenderPhase::Feeding,
            pending_kind: PendingPeriodKind::Source,
            source_stream_position: 0,
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
            backend,
            last_counters,
            settings,
            output,
            current,
            next: None,
            period,
            period_offset: 0,
            pending: false,
            gain,
            stream_position: 0,
            source_position: 0,
            spans: Vec::with_capacity(256),
            sequence: 0,
            started: false,
            cancel,
            view,
            input_peak: None,
            output_peak: None,
            modified: false,
            progress,
        };
        playback.spans.push(Span {
            entry: playback.current.entry.clone(),
            start: 0,
            source_start: 0,
            end: 0,
        });
        playback.prefetch(events)?;
        let observation = playback.observe()?;
        emit(
            events,
            Event::Ready {
                epochs,
                observation,
            },
        )?;
        playback.set_gain()?;
        Ok(playback)
    }
    fn epochs(&self) -> Epochs {
        self.settings.snapshot.epochs
    }
    fn format(&self) -> PcmFormat {
        self.current.file.format()
    }
    fn graph_latency(&self) -> u64 {
        self.pipeline
            .as_ref()
            .map_or(0, |p| p.preview().latency_frames)
    }
    fn native_period(&self) -> &PcmBlock {
        self.pipeline
            .as_ref()
            .and_then(|p| p.render())
            .map_or(&self.period, |r| r.period())
    }
    fn record_render(&mut self, report: crate::render::RenderReport) {
        self.input_peak = Some(report.input_peak);
        self.graph_peak = Some(report.graph_peak);
        self.prequantization_peak = Some(report.prequantization_peak);
        self.output_peak = Some(report.output_peak);
        self.saturation_count = self
            .saturation_count
            .saturating_add(report.saturation_count);
        self.modified = true;
    }
    fn activate_graph(
        &mut self,
        settings: Settings,
        activation: GraphActivationRequest,
        mut pipeline: PreparedPipeline,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        let binding = pipeline.binding();
        if binding != &activation.previous_binding
            || self.epochs() != binding.epochs
            || self.settings.snapshot.owner_id.as_ref() != Some(&binding.owner_id)
            || self.settings.snapshot.session_id.as_ref() != Some(&binding.session_id)
            || self.settings.snapshot.device_epoch != binding.device_epoch
            || !settings.snapshot.muted
            || settings.snapshot.epochs != activation.epochs
            || pipeline.preview().source_format != self.format()
            || pipeline.preview().native_output_format != self.period.header().format
            || pipeline.preview().graph_hash != activation.prepared.preview.graph_hash
        {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "prepared graph no longer matches playback",
            ));
        }
        pipeline.reset(activation.epochs).map_err(render_error)?;
        self.reposition(settings, None, events, false)?;
        self.pipeline = Some(pipeline.activate());
        self.saturation_count = 0;
        self.graph_peak = None;
        self.prequantization_peak = None;
        self.set_gain()?;
        emit(
            events,
            Event::GraphActivated(GraphActivationAck {
                epochs: self.epochs(),
                request_id: activation.request_id,
                graph_hash: activation.prepared.preview.graph_hash.clone(),
                activation_frame: self.stream_position,
                observation: self.observe()?,
            }),
        )
    }
    fn set_gain(&mut self) -> Result<(), EngineError> {
        let gain = if self.settings.snapshot.muted {
            None
        } else {
            Some(self.settings.snapshot.software_gain_db)
        };
        let ramp = self.settings.snapshot.gain_ramp_frames;
        self.gain.set_target(gain, ramp).and_then(|()| {
            self.pipeline
                .as_mut()
                .and_then(|p| p.render_mut())
                .map_or(Ok(()), |r| {
                    r.set_gain_target(gain, ramp).map_err(render_error)
                })
        })
    }
    fn prefetch(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.next.is_some() || self.phase != RenderPhase::Feeding {
            return Ok(());
        }
        let next = {
            let Ok(mut view) = self.view.try_lock() else {
                return Ok(());
            };
            view.active = Some(self.current.entry.id.clone());
            view.epochs = Some(self.epochs());
            let next = view.following(&self.current.entry.id);
            view.reserved = next.as_ref().map(|entry| entry.id.clone());
            next
        };
        if let Some(entry) = next {
            emit(
                events,
                Event::Reserved {
                    epochs: self.epochs(),
                    active: self.current.entry.id.clone(),
                    next: Some(entry.id.clone()),
                },
            )?;
            let next = Track::open(entry.clone(), self.epochs(), events)?;
            if next.file.format() != self.format() {
                item(
                    events,
                    self.epochs(),
                    &entry.id,
                    PreparationStatus::Failed {
                        code: ErrorCode::FormatUnsupported,
                    },
                )?;
                return Err(EngineError::new(
                    ErrorCode::FormatUnsupported,
                    "prefetched track changes the exact PCM format",
                ));
            }
            self.next = Some(next);
        }
        Ok(())
    }
    fn observe(&mut self) -> Result<PlaybackObservation, EngineError> {
        let progress = self.output.progress()?;
        self.last_counters = progress;
        Ok(self.observation(progress))
    }
    fn failure_observation(&self) -> PlaybackObservation {
        let mut counters = self.last_counters;
        counters.submitted = self.stream_position;
        counters.consumed = None;
        counters.restart_cursor = None;
        counters.delay = None;
        self.observation(counters)
    }
    fn observation(&self, counters: Counters) -> PlaybackObservation {
        let audible = counters
            .consumed
            .and_then(|position| position.checked_sub(self.graph_latency()))
            .and_then(|position| {
                self.spans
                    .iter()
                    .rev()
                    .find(|span| position >= span.start)
                    .map(|span| {
                        (
                            span.entry.id.clone(),
                            span.source_start + position.min(span.end).saturating_sub(span.start),
                        )
                    })
            });
        PlaybackObservation {
            processing: self
                .pipeline
                .as_ref()
                .map(|p| PlaybackProcessingObservation {
                    resident_bytes: p.resources().pipeline_bytes,
                    audio_payload_bytes: p
                        .render()
                        .map_or(0, |r| r.resources().audio_payload_bytes),
                    failure_node: p.render().and_then(|r| {
                        r.graph().failure_node_index().and_then(|index| {
                            r.graph()
                                .plan()
                                .schedule()
                                .get(index)
                                .map(|node| node.id().clone())
                        })
                    }),
                    graph_hash: p.preview().graph_hash.clone(),
                    latency_frames: p.preview().latency_frames,
                    tail_frames: p.preview().tail_frames,
                    saturation_count: self.saturation_count,
                    graph_peak: self.graph_peak,
                    prequantization_peak: self.prequantization_peak,
                }),
            format: self.format(),
            submitted_frames: counters.submitted,
            consumed_frames: counters.consumed,
            backend_delay_frames: counters.delay,
            consumption_basis: counters.basis,
            source_entry_id: self.current.entry.id.clone(),
            source_position_frames: self
                .current
                .packet
                .as_ref()
                .map_or(self.source_position, |packet| {
                    packet.header().frame_position + packet.header().frame_count
                }),
            audible_entry_id: audible.as_ref().map(|(id, _)| id.clone()),
            audible_source_position_frames: audible.map(|(_, position)| position),
            input_peak: self.input_peak,
            output_peak: self.output_peak,
            modified: self.modified || self.backend.preservation == PreservationState::Modified,
            backend: Some(self.backend.clone()),
        }
    }
    fn reposition(
        &mut self,
        settings: Settings,
        position: Option<u64>,
        events: &SyncSender<Event>,
        notify: bool,
    ) -> Result<(), EngineError> {
        let counters = self.output.suspend(settings.snapshot.epochs)?;
        let requested = if let Some(position) = position {
            (self.current.entry.clone(), position)
        } else {
            let consumed = counters
                .restart_cursor
                .ok_or(EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "pause requires a known backend restart boundary",
                ))?
                .saturating_sub(self.graph_latency());
            let span = self
                .spans
                .iter()
                .rev()
                .find(|span| consumed >= span.start)
                .ok_or(FAILED)?;
            (
                span.entry.clone(),
                span.source_start + consumed.min(span.end).saturating_sub(span.start),
            )
        };
        let reset_graph = settings.snapshot.epochs != self.epochs();
        self.settings = settings;
        if self.current.entry.id != requested.0.id {
            self.current = Track::open(requested.0, self.epochs(), events)?;
        }
        self.current.seek(requested.1, self.epochs())?;
        self.next = None;
        self.pending = false;
        self.period_offset = 0;
        self.stream_position = 0;
        self.source_stream_position = 0;
        self.source_position = requested.1;
        self.sequence = 0;
        self.started = false;
        self.phase = RenderPhase::Feeding;
        self.pending_kind = PendingPeriodKind::Source;
        self.spans.clear();
        self.spans.push(Span {
            entry: self.current.entry.clone(),
            start: 0,
            source_start: requested.1,
            end: 0,
        });
        self.gain = GainRamp::new(self.format())?;
        if reset_graph {
            let epochs = self.epochs();
            if let Some(pipeline) = &mut self.pipeline {
                pipeline.reset(epochs).map_err(render_error)?;
            }
        }
        self.set_gain()?;
        self.prefetch(events)?;
        if !notify {
            return Ok(());
        }
        let observation = self.observe()?;
        emit(
            events,
            Event::Paused {
                epochs: self.epochs(),
                observation,
            },
        )
    }
    fn step(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            self.output.stop()?;
            return Ok(true);
        }
        self.prefetch(events)?;
        if !self.settings.playing || self.settings.snapshot.muted {
            return Ok(false);
        }
        if self.phase == RenderPhase::BackendDrain {
            if let Some(counters) = self.output.finish()? {
                self.last_counters = counters;
                self.phase = RenderPhase::Finished;
                emit(
                    events,
                    Event::Drained {
                        epochs: self.epochs(),
                        observation: self.observation(counters),
                    },
                )?;
                return Ok(true);
            }
            self.output.wait()?;
            return Ok(false);
        }
        if !self.pending && self.phase == RenderPhase::GraphTail {
            let report = self
                .pipeline
                .as_mut()
                .and_then(|p| p.render_mut())
                .map(|r| r.finish())
                .transpose()
                .map_err(render_error)?;
            match report {
                Some(report) if report.valid_frames != 0 => {
                    self.record_render(report);
                    self.pending_kind = PendingPeriodKind::Tail;
                    self.pending = true;
                    self.period_offset = 0;
                }
                _ => {
                    self.phase = RenderPhase::BackendDrain;
                    return Ok(false);
                }
            }
        }
        if !self.pending {
            loop {
                if self.current.packet.is_none() {
                    match self.current.file.next_block() {
                        Ok(Some(packet)) => {
                            self.current.packet = Some(packet);
                            self.current.offset = 0;
                        }
                        Ok(None) => {
                            self.prefetch(events)?;
                            if let Some(next) = self.next.take() {
                                self.current = next;
                                self.source_position = 0;
                                self.spans.push(Span {
                                    entry: self.current.entry.clone(),
                                    start: self.source_stream_position,
                                    source_start: 0,
                                    end: self.source_stream_position,
                                });
                                let next_id = {
                                    let mut view = self.view.lock().map_err(|_| FAILED)?;
                                    view.active = Some(self.current.entry.id.clone());
                                    view.reserved = view
                                        .following(&self.current.entry.id)
                                        .map(|entry| entry.id);
                                    view.reserved.clone()
                                };
                                emit(
                                    events,
                                    Event::Reserved {
                                        epochs: self.epochs(),
                                        active: self.current.entry.id.clone(),
                                        next: next_id,
                                    },
                                )?;
                                self.prefetch(events)?;
                                continue;
                            }
                            {
                                let Ok(view) = self.view.try_lock() else {
                                    return Ok(false);
                                };
                                if view.following(&self.current.entry.id).is_some() {
                                    return Ok(false);
                                }
                            }
                            self.phase = RenderPhase::GraphTail;
                            emit(
                                events,
                                Event::End {
                                    epochs: self.epochs(),
                                },
                            )?;
                            return Ok(false);
                        }
                        Err(error) => {
                            item(
                                events,
                                self.epochs(),
                                &self.current.entry.id,
                                PreparationStatus::Failed { code: error.code },
                            )?;
                            return Err(error);
                        }
                    }
                }
                break;
            }
            let packet = self.current.packet.as_ref().ok_or(FAILED)?;
            let count = (packet.header().frame_count as usize - self.current.offset)
                .min(self.period.capacity_frames());
            if let Some(render) = self.pipeline.as_mut().and_then(|p| p.render_mut()) {
                let report = render
                    .process(packet, self.current.offset, count)
                    .map_err(render_error)?;
                self.record_render(report);
            } else {
                let channels = self.format().channels().count();
                for scalar in 0..count * channels {
                    self.period.set_integer_sample(
                        scalar,
                        packet
                            .integer_sample(self.current.offset * channels + scalar)
                            .ok_or(FAILED)?,
                    )?;
                }
                self.period.set_span(
                    self.epochs(),
                    self.sequence,
                    self.stream_position,
                    count as u64,
                )?;
                let report = self.gain.process(&mut self.period)?;
                self.input_peak = Some(report.input_peak);
                self.output_peak = Some(report.output_peak);
                self.modified |= report.modified;
            }
            self.pending_kind = PendingPeriodKind::Source;
            self.period_offset = 0;
            self.pending = true;
        }
        let period_frames = self.native_period().header().frame_count as usize;
        let count = period_frames - self.period_offset;
        let period = self
            .pipeline
            .as_ref()
            .and_then(|p| p.render())
            .map_or(&self.period, |r| r.period());
        match self.output.write(period, self.period_offset, count)? {
            Some(frames) => {
                if !self.started && self.output.is_running() {
                    emit(
                        events,
                        Event::Started {
                            epochs: self.epochs(),
                        },
                    )?;
                    self.started = true;
                }
                self.period_offset += frames;
                self.stream_position += frames as u64;
                if self.pending_kind == PendingPeriodKind::Source {
                    self.source_position += frames as u64;
                    self.source_stream_position += frames as u64;
                    self.spans.last_mut().ok_or(FAILED)?.end = self.source_stream_position;
                }
                if self.period_offset == period_frames {
                    if self.pending_kind == PendingPeriodKind::Source {
                        self.current.offset += self.period_offset;
                        if self.current.offset
                            == self
                                .current
                                .packet
                                .as_ref()
                                .ok_or(FAILED)?
                                .header()
                                .frame_count as usize
                        {
                            self.current.packet = None;
                        }
                    }
                    self.pending = false;
                    self.sequence += 1;
                }
                let observation = self.observe()?;
                if let Ok(mut latest) = self.progress.try_lock() {
                    *latest = Some((self.epochs(), observation));
                }
            }
            None => self.output.wait()?,
        }
        Ok(false)
    }
}

struct CompletedDrain {
    epochs: Epochs,
    graph_epoch: u64,
    session_id: Option<earph_core::ids::SessionId>,
    observation: PlaybackObservation,
}
impl CompletedDrain {
    fn reconcile(
        self,
        settings: &Settings,
        events: &SyncSender<Event>,
    ) -> Result<Option<Self>, EngineError> {
        let snapshot = &settings.snapshot;
        if self.graph_epoch != snapshot.graph_epoch
            || self.session_id != snapshot.session_id
            || snapshot.active_entry_id.as_ref() != Some(&self.observation.source_entry_id)
            || !matches!(
                snapshot.transport,
                TransportState::Buffering | TransportState::Playing | TransportState::Draining
            )
        {
            return Ok(None);
        }
        if self.epochs == snapshot.epochs {
            return Ok(Some(self));
        }
        if !snapshot.muted
            || self.epochs.session != snapshot.epochs.session
            || self.epochs.stream.checked_add(1) != Some(snapshot.epochs.stream)
        {
            return Ok(None);
        }
        // End may also be stale when mute crossed the final native finish call.
        emit(
            events,
            Event::End {
                epochs: snapshot.epochs,
            },
        )?;
        emit(
            events,
            Event::Drained {
                epochs: snapshot.epochs,
                observation: self.observation,
            },
        )?;
        Ok(None)
    }
}
fn run(
    receiver: Receiver<Action>,
    events: SyncSender<Event>,
    queued: Arc<AtomicUsize>,
    view: Arc<Mutex<PlanView>>,
    progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
) {
    let mut playback: Option<Playback> = None;
    let mut stopped: Option<(PlaybackObservation, Option<EngineError>)> = None;
    let mut drained: Option<CompletedDrain> = None;
    loop {
        let action = if playback.is_some() {
            match receiver.try_recv() {
                Ok(action) => Some(action),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => break,
            }
        } else {
            match receiver.recv() {
                Ok(action) => Some(action),
                Err(_) => break,
            }
        };
        if let Some(action) = action {
            queued.fetch_sub(1, Ordering::AcqRel);
            if !matches!(action, Action::Settings(_)) {
                drained = None;
            }
            let failure_epochs = match &action {
                Action::ActivateGraph { settings, .. }
                | Action::Pause(settings)
                | Action::Seek { settings, .. }
                | Action::Settings(settings) => Some(settings.snapshot.epochs),
                _ => None,
            };
            let result = match action {
                Action::ActivateGraph {
                    settings,
                    activation,
                    pipeline,
                } => {
                    let epochs = activation.epochs;
                    let request_id = activation.request_id.clone();
                    let result = if let Some(current) = &mut playback {
                        current.activate_graph(settings, activation, pipeline, &events)
                    } else {
                        drop(pipeline);
                        Err(FAILED)
                    };
                    if let Err(error) = result {
                        let _ = emit(
                            &events,
                            Event::GraphFailed {
                                epochs,
                                request_id,
                                error,
                            },
                        );
                    }
                    result
                }
                Action::Shutdown => break,
                Action::Stop { epochs } => {
                    if let Some(mut current) = playback.take() {
                        let observation = current.failure_observation();
                        let failure = current.output.stop().err();
                        clear_reservation(&view, current.epochs());
                        stopped = Some((observation, failure));
                    }
                    match stopped.take() {
                        Some((observation, Some(error))) => emit(
                            &events,
                            Event::Failed {
                                epochs,
                                error,
                                observation: Some(observation),
                            },
                        ),
                        value => emit(
                            &events,
                            Event::Stopped {
                                epochs,
                                observation: value.map(|(observation, _)| observation),
                            },
                        ),
                    }
                }
                Action::Start {
                    settings,
                    entries,
                    cancel,
                } => {
                    let epochs = settings.snapshot.epochs;
                    stopped = None;
                    playback = None;
                    match Playback::prepare(
                        settings,
                        entries,
                        cancel,
                        view.clone(),
                        progress.clone(),
                        &events,
                    ) {
                        Ok(value) => {
                            playback = Some(value);
                            Ok(())
                        }
                        Err(error) => {
                            clear_reservation(&view, epochs);
                            let _ = emit(
                                &events,
                                Event::Failed {
                                    epochs,
                                    error,
                                    observation: None,
                                },
                            );
                            Ok(())
                        }
                    }
                }
                Action::Settings(settings) => {
                    if let Some(current) = &mut playback {
                        if current.epochs() == settings.snapshot.epochs {
                            let was_muted = current.settings.snapshot.muted;
                            if !was_muted && settings.snapshot.muted {
                                current.reposition(settings, None, &events, true)
                            } else {
                                current.settings = settings;
                                current.set_gain()
                            }
                        } else if settings.snapshot.muted
                            && settings.snapshot.epochs.session == current.epochs().session
                            && settings.snapshot.epochs.stream > current.epochs().stream
                        {
                            current.reposition(settings, None, &events, true)
                        } else {
                            Ok(())
                        }
                    } else if let Some(completed) = drained.take() {
                        match completed.reconcile(&settings, &events) {
                            Ok(retained) => {
                                drained = retained;
                                Ok(())
                            }
                            Err(error) => Err(error),
                        }
                    } else {
                        Ok(())
                    }
                }
                Action::Pause(settings) => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, None, &events, true)
                    } else {
                        Err(ENDED)
                    }
                }
                Action::Seek { settings, position } => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, Some(position), &events, true)
                    } else {
                        Err(ENDED)
                    }
                }
            };
            if let Err(error) = result {
                let mut epochs = failure_epochs;
                let observation = playback.take().map(|mut current| {
                    let observation = current.failure_observation();
                    let _ = current.output.stop();
                    clear_reservation(&view, current.epochs());
                    epochs = Some(failure_epochs.unwrap_or(current.epochs()));
                    observation
                });
                if let Some(epochs) = epochs {
                    let _ = emit(
                        &events,
                        Event::Failed {
                            epochs,
                            error,
                            observation,
                        },
                    );
                }
            }
        }
        if let Some(current) = &mut playback {
            match current.step(&events) {
                Ok(true) => {
                    if current.cancel.load(Ordering::Acquire) {
                        stopped = Some((current.failure_observation(), None));
                    } else if current.phase == RenderPhase::Finished {
                        drained = Some(CompletedDrain {
                            epochs: current.epochs(),
                            graph_epoch: current.settings.snapshot.graph_epoch,
                            session_id: current.settings.snapshot.session_id.clone(),
                            observation: current.observation(current.last_counters),
                        });
                    }
                    clear_reservation(&view, current.epochs());
                    playback = None;
                }
                Ok(false) => {
                    if !current.settings.playing || current.settings.snapshot.muted {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(error) => {
                    let epochs = current.epochs();
                    let observation = current.failure_observation();
                    if current.cancel.load(Ordering::Acquire) {
                        stopped = Some((observation.clone(), Some(error)));
                    }
                    let _ = current.output.stop();
                    clear_reservation(&view, epochs);
                    playback = None;
                    let _ = emit(
                        &events,
                        Event::Failed {
                            epochs,
                            error,
                            observation: Some(observation),
                        },
                    );
                }
            }
        }
    }
}

fn clear_reservation(view: &Mutex<PlanView>, epochs: Epochs) {
    if let Ok(mut view) = view.lock()
        && view.epochs == Some(epochs)
    {
        view.active = None;
        view.reserved = None;
    }
}
fn render_error(error: crate::render::RenderError) -> EngineError {
    EngineError::new(error.code, error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{control::ControlLimits, ids::ZoneId, session::Session};

    fn settings(muted: bool) -> Settings {
        let mut snapshot =
            Session::new(ZoneId::new("main").unwrap(), ControlLimits::default()).snapshot();
        snapshot.muted = muted;
        Settings {
            snapshot,
            playing: true,
        }
    }
    fn admission() -> (Worker, Receiver<Action>) {
        let (sender, receiver) = mpsc::sync_channel(COMMANDS);
        let (_, events) = mpsc::sync_channel(1);
        (
            Worker {
                sender,
                events,
                queued: Arc::new(AtomicUsize::new(0)),
                cancel: None,
                join: None,
                view: Arc::new(Mutex::new(PlanView::default())),
                progress: Arc::new(Mutex::new(None)),
            },
            receiver,
        )
    }
    #[cfg(target_os = "linux")]
    fn naturally_drained_worker() -> (Worker, Settings, PlaybackObservation) {
        use earph_core::{
            control::{OutputBackend, OutputConfiguration, OutputRoute, SourceKind},
            ids::{DeviceId, ProfileId, SessionId},
            queue::LocalQueue,
            safety::{FidelityPolicy, VolumeMode},
            session::TransportState,
        };
        use std::{
            fs,
            time::{Instant, SystemTime, UNIX_EPOCH},
        };
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/terminal-drain")
            .join(format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&path).unwrap();
        let source = path.join("silence.wav");
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(296_u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(2_u16.to_le_bytes());
        bytes.extend(48000_u32.to_le_bytes());
        bytes.extend(192000_u32.to_le_bytes());
        bytes.extend(4_u16.to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(260_u32.to_le_bytes());
        bytes.extend([0_u8; 260]);
        fs::write(&source, bytes).unwrap();
        let mut queue = LocalQueue::default();
        let entry = queue.append(source.to_str().unwrap()).unwrap();
        let mut settings = settings(false);
        settings.snapshot.epochs.session = 1;
        settings.snapshot.session_id = Some(SessionId::new("terminal-session").unwrap());
        settings.snapshot.source = Some(SourceKind::LocalFile);
        settings.snapshot.transport = TransportState::Buffering;
        settings.snapshot.active_entry_id = Some(entry);
        settings.snapshot.output_consented = true;
        settings.snapshot.output = Some(OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "null".into(),
            device_id: DeviceId::new("null").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        });
        let mut worker = Worker::spawn();
        worker.view.lock().unwrap().publish(queue.snapshot());
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries: queue.snapshot().entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let observation = loop {
            let event = worker
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("native null worker did not finish");
            if let Event::Drained {
                epochs,
                observation,
            } = event
            {
                assert_eq!(epochs, settings.snapshot.epochs);
                break observation;
            }
        };
        assert_eq!(observation.submitted_frames, 65);
        (worker, settings, observation)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn terminal_drain_survives_a_matching_mute_before_or_after_end_observation() {
        use earph_core::session::TransportState;
        for transport in [
            TransportState::Buffering,
            TransportState::Playing,
            TransportState::Draining,
        ] {
            let (mut worker, mut settings, expected) = naturally_drained_worker();
            settings.snapshot.muted = true;
            settings.snapshot.epochs.stream += 1;
            settings.snapshot.transport = transport;
            worker.send(Action::Settings(settings.clone())).unwrap();
            assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
                Ok(Event::End { epochs }) if epochs == settings.snapshot.epochs));
            match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
                Event::Drained {
                    epochs,
                    observation,
                } => {
                    assert_eq!(epochs, settings.snapshot.epochs);
                    assert_eq!(observation, expected);
                }
                _ => panic!("terminal handoff omitted its final counters"),
            }
            worker.send(Action::Settings(settings)).unwrap();
            assert!(
                worker
                    .events
                    .recv_timeout(Duration::from_millis(30))
                    .is_err()
            );
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn terminal_drain_cannot_cross_an_unrelated_action_or_generation() {
        for changed in ["seek", "session", "stream", "graph", "source"] {
            let (mut worker, mut settings, _) = naturally_drained_worker();
            settings.snapshot.muted = true;
            settings.snapshot.epochs.stream += 1;
            match changed {
                "seek" => worker
                    .send(Action::Seek {
                        settings: settings.clone(),
                        position: 0,
                    })
                    .unwrap(),
                "session" => settings.snapshot.epochs.session += 1,
                "stream" => settings.snapshot.epochs.stream += 1,
                "graph" => settings.snapshot.graph_epoch += 1,
                "source" => settings.snapshot.active_entry_id = None,
                _ => unreachable!(),
            }
            if changed == "seek" {
                assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
                    Ok(Event::Failed { epochs, error, .. })
                    if epochs == settings.snapshot.epochs && error.code == ErrorCode::CapabilityUnsupported));
            }
            worker.send(Action::Settings(settings)).unwrap();
            assert!(
                worker
                    .events
                    .recv_timeout(Duration::from_millis(30))
                    .is_err(),
                "{changed}"
            );
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn same_stream_volume_settings_preserve_the_terminal_mute_handoff() {
        let (mut worker, mut settings, expected) = naturally_drained_worker();
        settings.snapshot.software_gain_db = -48.0;
        worker.send(Action::Settings(settings.clone())).unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings.clone())).unwrap();
        assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
            Ok(Event::End { epochs }) if epochs == settings.snapshot.epochs));
        match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Drained { observation, .. } => assert_eq!(observation, expected),
            _ => panic!("same-stream settings lost the terminal result"),
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn reposition_after_native_finish_reports_a_current_generation_failure() {
        for seek in [false, true] {
            let (mut worker, mut settings, _) = naturally_drained_worker();
            settings.snapshot.muted = true;
            settings.snapshot.epochs.stream += 1;
            let epochs = settings.snapshot.epochs;
            let action = if seek {
                Action::Seek {
                    settings,
                    position: 0,
                }
            } else {
                Action::Pause(settings)
            };
            worker.send(action).unwrap();
            match worker
                .events
                .recv_timeout(Duration::from_secs(5))
                .expect("reposition receipt would remain pending")
            {
                Event::Failed {
                    epochs: failed,
                    error,
                    ..
                } => {
                    assert_eq!(failed, epochs);
                    assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
                    assert_eq!(error.message, "source ended before reposition");
                }
                _ => panic!("reposition fabricated a live source after native finish"),
            }
        }
    }
    #[test]
    fn ordinary_saturation_preserves_mute_and_stop_admission_in_fifo_order() {
        let (mut worker, receiver) = admission();
        let cancel = Arc::new(AtomicBool::new(false));
        worker.cancel = Some(cancel.clone());
        for _ in 0..32 {
            worker.send(Action::Settings(settings(false))).unwrap();
        }
        assert!(!worker.has_capacity(false));
        assert!(worker.send(Action::Settings(settings(false))).is_err());
        let mute = worker.send(Action::Settings(settings(true)));
        let stop = worker.send(Action::Stop {
            epochs: Epochs::default(),
        });
        let actions: Vec<_> = receiver.try_iter().collect();
        assert!(mute.is_ok(), "ordinary commands blocked mute: {mute:?}");
        assert!(stop.is_ok(), "ordinary commands blocked stop: {stop:?}");
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(actions.len(), 34);
        for action in &actions[..32] {
            assert!(matches!(action, Action::Settings(s) if !s.snapshot.muted));
        }
        assert!(matches!(&actions[32], Action::Settings(s) if s.snapshot.muted));
        assert!(matches!(&actions[33], Action::Stop { .. }));
    }
    #[test]
    fn exhausted_safety_reserve_cancels_without_claiming_delivery() {
        let (mut worker, receiver) = admission();
        let cancel = Arc::new(AtomicBool::new(false));
        worker.cancel = Some(cancel.clone());
        for _ in 0..32 {
            worker.send(Action::Settings(settings(false))).unwrap();
        }
        for _ in 0..32 {
            worker.send(Action::Settings(settings(true))).unwrap();
        }
        assert_eq!(worker.queued.load(Ordering::Acquire), 64);
        assert!(!worker.has_capacity(true));
        let error = worker
            .send(Action::Stop {
                epochs: Epochs::default(),
            })
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SourceBusy);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(worker.queued.load(Ordering::Acquire), 64);
        let actions: Vec<_> = receiver.try_iter().collect();
        assert_eq!(actions.len(), 64);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Stop { .. }))
        );
    }
    #[test]
    fn disconnected_safety_delivery_cancels_without_claiming_delivery() {
        let (mut worker, receiver) = admission();
        let cancel = Arc::new(AtomicBool::new(false));
        worker.cancel = Some(cancel.clone());
        drop(receiver);
        let error = worker.send(Action::Settings(settings(true))).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputFault);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(worker.queued.load(Ordering::Acquire), 0);
    }
}
