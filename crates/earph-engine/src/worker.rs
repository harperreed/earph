// ABOUTME: Decodes local PCM and owns one native output on its worker thread.
// ABOUTME: Bounds commands, prefetches compatible joins, and drops stale work on cancellation.
use crate::{
    graph::PreparedPipeline,
    output::{self, Counters, Output},
    plan::PlanView,
    stream_render::SourceSpan,
};
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmBlock, PcmFormat},
    control::SourceKind,
    gain::GainRamp,
    graph_control::{GraphActivationAck, GraphActivationRequest},
    ids::{Epochs, OwnerId, QueueEntryId, RequestId, SessionId},
    queue::{PreparationStatus, QueueEntry},
    session::{
        BackendObservation, PlaybackObservation, PlaybackProcessingObservation, PreservationState,
        SessionSnapshot, StreamingReplacementBoundary, StreamingRuntimeObservation, TransportState,
    },
};
use earph_media::LocalFile;
mod network;
use network::{HttpSource, NetworkPreparation, Reader};
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
const INVALID_STREAM_COUNT: EngineError = EngineError::new(
    ErrorCode::InternalFault,
    "stream source count disagrees with its offered span",
);
#[derive(Clone)]
pub(crate) struct Settings {
    pub snapshot: SessionSnapshot,
    pub playing: bool,
}
#[derive(Clone)]
pub(crate) struct StopRequest {
    pub request_id: RequestId,
    pub settings: Settings,
}
impl StopRequest {
    pub fn matches_snapshot(&self, snapshot: &SessionSnapshot) -> bool {
        let captured = &self.settings.snapshot;
        captured.owner_id == snapshot.owner_id
            && captured.session_id.is_some()
            && captured.session_id == snapshot.session_id
            && captured.source == Some(SourceKind::LocalFile)
            && snapshot.source == Some(SourceKind::LocalFile)
            && captured.epochs == snapshot.epochs
            && captured.graph_epoch == snapshot.graph_epoch
            && captured.device_epoch == snapshot.device_epoch
            && snapshot.operation.as_ref() == Some(&self.request_id)
    }
    pub fn matches_source(&self, token: &SourceTerminalToken) -> bool {
        let captured = &self.settings.snapshot;
        captured.owner_id == token.owner_id
            && captured.session_id.as_ref() == Some(&token.session_id)
            && captured.epochs.session == token.origin_epochs.session
            && captured.epochs.stream >= token.origin_epochs.stream
            && captured.device_epoch == token.device_epoch
            // One cancelled activation may have installed the owner's next generation.
            && (captured.graph_epoch == token.graph_epoch
                || captured.graph_epoch.checked_add(1) == Some(token.graph_epoch))
    }
}
pub(crate) enum Action {
    StartNetwork {
        settings: Settings,
        context: Arc<earph_core::network_source::NetworkSourceContext>,
        permit: earph_network::FetchPermit,
        live: crate::network_source::LiveBinding,
        clock: std::time::Instant,
        cancel: Arc<AtomicBool>,
    },
    RetireNetwork(earph_core::network_source::NetworkClosingSource),
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
    #[cfg(all(test, target_os = "linux"))]
    PausePreparation(tests::PreparationGate),
    Settings(Settings),
    Pause(Settings),
    Seek {
        settings: Settings,
        position: u64,
    },
    Stop(StopRequest),
    AcknowledgeSourceTerminal(SourceTerminalToken),
    Shutdown,
}
impl Action {
    fn is_safety(&self) -> bool {
        matches!(
            self,
            Self::Pause(_) | Self::Stop(_) | Self::Shutdown | Self::RetireNetwork(_)
        ) || matches!(self, Self::Settings(settings) if settings.snapshot.muted || settings.snapshot.software_gain_zero)
    }
}
pub(crate) enum Event {
    Network(earph_core::network_source::NetworkEvent),
    NetworkRetired(earph_core::network_source::NetworkRetirementAck),
    NetworkGraphActivated(earph_core::network_source::NetworkGraphActivationAck),
    NetworkGraphFailed {
        request_id: RequestId,
        error: EngineError,
        actual: earph_core::network_source::NetworkExecutionFence,
    },
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
        token: SourceTerminalToken,
        observation: PlaybackObservation,
    },
    Failed {
        epochs: Epochs,
        token: Option<SourceTerminalToken>,
        error: EngineError,
        observation: Option<PlaybackObservation>,
    },
    StopFailed {
        request: Box<StopRequest>,
        token: SourceTerminalToken,
        error: EngineError,
        observation: Option<PlaybackObservation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SourceTerminalToken {
    pub(crate) owner_id: Option<OwnerId>,
    pub(crate) session_id: SessionId,
    pub(crate) origin_epochs: Epochs,
    pub(crate) graph_epoch: u64,
    pub(crate) device_epoch: u64,
    pub(crate) final_entry_id: QueueEntryId,
}
impl SourceTerminalToken {
    pub(crate) fn matches_source(&self, snapshot: &SessionSnapshot) -> bool {
        self.owner_id == snapshot.owner_id
            && Some(&self.session_id) == snapshot.session_id.as_ref()
            && self.origin_epochs.session == snapshot.epochs.session
            && self.graph_epoch == snapshot.graph_epoch
            && self.device_epoch == snapshot.device_epoch
    }
}

pub(crate) struct Worker {
    sender: SyncSender<Action>,
    pub events: Receiver<Event>,
    queued: Arc<AtomicUsize>,
    retiring: Arc<AtomicBool>,
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
        let retiring = Arc::new(AtomicBool::new(false));
        let view = Arc::new(Mutex::new(PlanView::default()));
        let progress = Arc::new(Mutex::new(None));
        let observations = progress.clone();
        let q = queued.clone();
        let v = view.clone();
        let retirement = retiring.clone();
        let join = thread::spawn(move || run(receiver, events, q, v, observations, retirement));
        Self {
            sender,
            events: event_receiver,
            queued,
            retiring,
            cancel: None,
            join: Some(join),
            view,
            progress,
        }
    }
    pub fn has_capacity_for(&self, safety: bool, actions: usize) -> bool {
        (safety || !self.retiring.load(Ordering::Acquire))
            && self
                .queued
                .load(Ordering::Acquire)
                .checked_add(actions)
                .is_some_and(|queued| queued <= if safety { COMMANDS } else { ORDINARY_COMMANDS })
    }
    pub fn cancel_active(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Release);
        }
    }
    pub fn source_cancellation(&self) -> Result<Arc<AtomicBool>, EngineError> {
        self.cancel
            .as_ref()
            .filter(|cancel| !cancel.load(Ordering::Acquire))
            .cloned()
            .ok_or_else(|| EngineError::new(ErrorCode::StaleSession, "source is unavailable"))
    }
    pub fn send(&mut self, action: Action) -> Result<(), EngineError> {
        if matches!(action, Action::RetireNetwork(_)) {
            self.retiring.store(true, Ordering::Release);
        }
        let safety = action.is_safety();
        let reserved = safety || matches!(action, Action::AcknowledgeSourceTerminal(_));
        if self.queued.load(Ordering::Acquire)
            >= if reserved {
                COMMANDS
            } else {
                ORDINARY_COMMANDS
            }
        {
            if safety {
                self.cancel_active();
            }
            return Err(BUSY);
        }
        if matches!(
            action,
            Action::Stop(_)
                | Action::Shutdown
                | Action::Start { .. }
                | Action::StartNetwork { .. }
                | Action::RetireNetwork(_)
        ) && let Some(cancel) = &self.cancel
        {
            cancel.store(true, Ordering::Release);
        }
        if let Action::Start { cancel, .. } | Action::StartNetwork { cancel, .. } = &action {
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
    entry: Option<QueueEntry>,
    file: Reader,
    packet: Option<PcmBlock>,
    offset: usize,
}
impl Track {
    fn open(
        entry: QueueEntry,
        epochs: Epochs,
        event_epochs: Epochs,
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        match LocalFile::open(Path::new(&entry.path), epochs) {
            Ok(mut file) => {
                let packet = file.next_block()?;
                item(
                    events,
                    event_epochs,
                    &entry.id,
                    PreparationStatus::Ready {
                        format: file.format(),
                        total_frames: file.total_frames(),
                    },
                )?;
                Ok(Self {
                    entry: Some(entry),
                    file: Reader::Local(file),
                    packet,
                    offset: 0,
                })
            }
            Err(error) => {
                item(
                    events,
                    event_epochs,
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
    fn local_entry(&self) -> Result<&QueueEntry, EngineError> {
        self.entry.as_ref().ok_or(FAILED)
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
struct QueueReservation {
    view: Arc<Mutex<PlanView>>,
    epochs: Epochs,
}
impl Drop for QueueReservation {
    fn drop(&mut self) {
        clear_reservation(&self.view, self.epochs);
    }
}
struct Playback {
    preparation_failure: Option<EngineError>,
    pipeline: Option<PreparedPipeline>,
    installed_graph_epoch: u64,
    phase: RenderPhase,
    pending_kind: PendingPeriodKind,
    source_stream_position: u64,
    source_epochs: Epochs,
    native_epochs: Epochs,
    replacement_boundary: Option<Arc<StreamingReplacementBoundary>>,
    network_replacement_boundary:
        Option<Arc<earph_core::network_source::NetworkReplacementBoundary>>,
    saturation_count: u64,
    graph_peak: Option<f64>,
    prequantization_peak: Option<f64>,
    backend: BackendObservation,
    last_counters: Counters,
    last_counter_epochs: Epochs,
    // Keep one irreversible native prefix across a quiet control boundary.
    control_stop_observation: Option<(Epochs, PlaybackObservation)>,
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
    // This last field retires the exact reservation after the output and source resources.
    reservation: QueueReservation,
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
        let profile_bytes = settings.snapshot.profile_storage_bytes;
        let profile_budget = EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "profile metadata exceeds source admission",
        );
        let metadata = entries
            .iter()
            .try_fold(
                size_of::<Self>() as u64 + (entries.capacity() * size_of::<QueueEntry>()) as u64,
                |n, e| n.checked_add(e.path.capacity() as u64 + e.id.as_str().len() as u64),
            )
            .ok_or(profile_budget)?;
        let base = profile_bytes.checked_add(metadata).ok_or(profile_budget)?;
        MemoryBudget::default().admit(usize::try_from(base).map_err(|_| profile_budget)?)?;
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
            #[cfg(all(test, target_os = "linux"))]
            tests::after_preparation_check();
            let track = Track::open(entry.clone(), epochs, epochs, events)?;
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
        let mut current = Some(Track::open(
            entries[selected_index].clone(),
            epochs,
            epochs,
            events,
        )?);
        Self::prepare_current(settings, &mut current, cancel, view, progress, events, base)
    }
    fn prepare_current(
        settings: Settings,
        current: &mut Option<Track>,
        cancel: Arc<AtomicBool>,
        view: Arc<Mutex<PlanView>>,
        progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
        events: &SyncSender<Event>,
        base: u64,
    ) -> Result<Self, EngineError> {
        let profile_budget = EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "source preparation exceeds memory admission",
        );
        let epochs = settings.snapshot.epochs;
        let format = current.as_ref().ok_or(FAILED)?.file.format();
        let config = settings.snapshot.output.as_ref().ok_or(FAILED)?;
        if cancel.load(Ordering::Acquire) {
            return Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "preparation was cancelled",
            ));
        }
        let mut output = output::open(config, format, epochs)?;
        let backend = output.observation()?;
        let retained = base
            .checked_add(output.audio_storage_bytes()?)
            .and_then(|n| {
                n.checked_add(format.storage_bytes(config.period_frames as u64).ok()? as u64)
            })
            .ok_or(profile_budget)?;
        MemoryBudget::default().admit(usize::try_from(retained).map_err(|_| profile_budget)?)?;
        let last_counters = output.progress()?;
        let period = PcmBlock::prepare(
            format,
            config.period_frames as usize,
            epochs,
            MemoryBudget::default(),
        )?;
        let gain = GainRamp::new(format)?;
        let mut playback = Self {
            preparation_failure: None,
            installed_graph_epoch: settings.snapshot.graph_epoch,
            pipeline: None,
            phase: RenderPhase::Feeding,
            pending_kind: PendingPeriodKind::Source,
            source_stream_position: 0,
            source_epochs: epochs,
            native_epochs: epochs,
            replacement_boundary: None,
            network_replacement_boundary: None,
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
            backend,
            last_counters,
            last_counter_epochs: epochs,
            control_stop_observation: None,
            settings,
            output,
            current: current.take().ok_or(FAILED)?,
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
            reservation: QueueReservation {
                view: view.clone(),
                epochs,
            },
            view,
            input_peak: None,
            output_peak: None,
            modified: false,
            progress,
        };
        if let Some(entry) = &playback.current.entry {
            playback.spans.push(Span {
                entry: entry.clone(),
                start: 0,
                source_start: 0,
                end: 0,
            });
        }
        playback.prefetch(events)?;
        if playback.is_network() {
            let prepared = (|| {
                let observation = playback
                    .observe_network(earph_core::network_source::NetworkLifecycle::Ready)?;
                playback.emit_network(
                    events,
                    earph_core::network_source::NetworkEventKind::Prepared { observation },
                )?;
                playback.set_gain()?;
                if !playback.settings.playing {
                    playback.network_paused(events)?;
                }
                Ok(())
            })();
            playback.preparation_failure = prepared.err();
            return Ok(playback);
        }
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
    fn cancelled_stream_source(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
            && (self.is_network()
                || self
                    .pipeline
                    .as_ref()
                    .is_some_and(|p| p.stream_render().is_some()))
    }
    fn format(&self) -> PcmFormat {
        self.current.file.format()
    }
    fn graph_latency(&self) -> Option<u64> {
        self.pipeline
            .as_ref()
            .map_or(Some(0), |p| p.preview().timing.latency_frames())
    }
    fn native_period(&self) -> &PcmBlock {
        if let Some(stream) = self.pipeline.as_ref().and_then(|p| p.stream_render()) {
            stream.period()
        } else {
            self.pipeline
                .as_ref()
                .and_then(|p| p.render())
                .map_or(&self.period, |r| r.period())
        }
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
        let incoming_stream = pipeline.stream_render().is_some();
        if self.cancelled_stream_source()
            || (incoming_stream && self.cancel.load(Ordering::Acquire))
        {
            // Stop supersedes queued activation. Preserve the old source until native close.
            drop(pipeline);
            return Ok(());
        }
        let binding = pipeline.binding();
        if binding != &activation.previous_binding
            || self.epochs() != binding.epochs
            || self.settings.snapshot.owner_id.as_ref() != Some(&binding.owner_id)
            || self.settings.snapshot.session_id.as_ref() != Some(&binding.session_id)
            || self.settings.snapshot.device_epoch != binding.device_epoch
            || !settings.snapshot.muted
            || settings.snapshot.epochs != activation.epochs
            || settings.snapshot.graph_epoch != self.installed_graph_epoch
            || pipeline.preview().source_format != self.format()
            || pipeline.preview().native_output_format != self.period.header().format
            || pipeline.preview().graph_hash != activation.prepared.preview.graph_hash
        {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "prepared graph no longer matches playback",
            ));
        }
        let installed_graph_epoch =
            settings
                .snapshot
                .graph_epoch
                .checked_add(1)
                .ok_or(EngineError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "installed graph generation exhausted",
                ))?;
        if self.is_network() {
            return self.activate_network_graph(
                settings,
                activation,
                pipeline,
                installed_graph_epoch,
                events,
            );
        }
        if incoming_stream
            || self
                .pipeline
                .as_ref()
                .is_some_and(|p| p.stream_render().is_some())
        {
            self.remember_control_observation();
        }
        let reset = pipeline.reset(activation.epochs).map_err(render_error);
        if self.cancelled_stream_source()
            || (incoming_stream && self.cancel.load(Ordering::Acquire))
        {
            drop(pipeline);
            return Ok(());
        }
        reset?;
        if self
            .pipeline
            .as_ref()
            .is_some_and(|p| p.stream_render().is_some())
        {
            // The accepted source frontier is exact. Streaming has no inverse audible cursor.
            self.reposition_with_boundary(
                settings,
                Some(self.source_position),
                events,
                false,
                true,
            )?;
        } else {
            self.reposition(settings, None, events, false)?;
        }
        if pipeline.preview().backend.as_ref() != Some(&self.backend) {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "reopened backend differs from prepared graph",
            ));
        }
        self.pipeline = Some(pipeline.activate());
        self.saturation_count = 0;
        self.graph_peak = None;
        self.prequantization_peak = None;
        self.input_peak = None;
        self.output_peak = None;
        self.set_gain()?;
        emit(
            events,
            Event::GraphActivated(GraphActivationAck {
                epochs: self.epochs(),
                request_id: activation.request_id,
                graph_hash: activation.prepared.preview.graph_hash.clone(),
                activation_frame: self.stream_position,
                observation: self.observe()?,
                source_total_frames: self.current.file.total_frames().ok_or(FAILED)?,
            }),
        )?;
        // FIFO puts this acknowledgement before later facts from the installed graph.
        self.installed_graph_epoch = installed_graph_epoch;
        Ok(())
    }
    fn failure_context(&self, requested: Option<Settings>) -> Settings {
        let mut settings = requested.unwrap_or_else(|| self.settings.clone());
        if let Some((epochs, _)) = self.cancelled_control_observation() {
            settings.snapshot.epochs = *epochs;
        }
        settings.snapshot.graph_epoch = self.installed_graph_epoch;
        settings
    }
    fn apply_settings(
        &mut self,
        settings: Settings,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        if self.cancelled_stream_source() {
            return Ok(());
        }
        if self.epochs() == settings.snapshot.epochs {
            let was_muted = self.settings.snapshot.muted;
            if !was_muted && settings.snapshot.muted {
                self.reposition(settings, None, events, true)
            } else {
                self.settings = settings;
                self.set_gain()
            }
        } else if settings.snapshot.muted
            && settings.snapshot.epochs.session == self.epochs().session
            && settings.snapshot.epochs.stream > self.epochs().stream
        {
            self.reposition(settings, None, events, true)
        } else {
            Ok(())
        }
    }
    fn set_gain(&mut self) -> Result<(), EngineError> {
        let zero = self.settings.snapshot.software_gain_zero;
        let gain = if self.settings.snapshot.muted || zero {
            None
        } else {
            Some(self.settings.snapshot.software_gain_db)
        };
        let ramp = if zero {
            0
        } else {
            self.settings.snapshot.gain_ramp_frames
        };
        self.gain.set_target(gain, ramp)?;
        if let Some(stream) = self.pipeline.as_mut().and_then(|p| p.stream_render_mut()) {
            stream.set_gain_target(gain, ramp)?;
        } else if let Some(render) = self.pipeline.as_mut().and_then(|p| p.render_mut()) {
            render.set_gain_target(gain, ramp).map_err(render_error)?;
        }
        if gain.is_none() && ramp == 0 {
            self.silence_pending_period()?;
        }
        Ok(())
    }
    fn silence_pending_period(&mut self) -> Result<(), EngineError> {
        if !self.pending {
            return Ok(());
        }
        if let Some(pipeline) = &mut self.pipeline {
            if let Some(stream) = pipeline.stream_render_mut() {
                let offset = stream.period_offset();
                return silence_period_suffix(&mut stream.period, offset);
            }
            if let Some(render) = pipeline.render_mut() {
                return silence_period_suffix(&mut render.period, self.period_offset);
            }
        }
        silence_period_suffix(&mut self.period, self.period_offset)
    }
    fn prefetch(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.is_network() {
            return Ok(());
        }
        if self.next.is_some() || self.phase != RenderPhase::Feeding {
            return Ok(());
        }
        let next = {
            let Ok(mut view) = self.view.try_lock() else {
                return Ok(());
            };
            view.active = Some(self.current.local_entry()?.id.clone());
            view.epochs = Some(self.epochs());
            self.reservation.epochs = self.epochs();
            let next = view.following(&self.current.local_entry()?.id);
            view.reserved = next.as_ref().map(|entry| entry.id.clone());
            next
        };
        if let Some(entry) = next {
            emit(
                events,
                Event::Reserved {
                    epochs: self.epochs(),
                    active: self.current.local_entry()?.id.clone(),
                    next: Some(entry.id.clone()),
                },
            )?;
            let next = Track::open(entry.clone(), self.source_epochs, self.epochs(), events)?;
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
        if let Some(stream) = self.pipeline.as_mut().and_then(|p| p.stream_render_mut()) {
            if let Some(restart) = progress.restart_cursor {
                stream.observe(progress.submitted, restart)?;
            } else if !self.output.initial_restart_cursor_pending() {
                return Err(EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "streaming requires a known backend restart boundary",
                ));
            }
        }
        self.last_counters = progress;
        self.last_counter_epochs = self.native_epochs;
        Ok(self.observation(progress))
    }
    fn failure_observation(&self) -> PlaybackObservation {
        if let Some((_, observation)) = self.cancelled_control_observation() {
            let mut observation = observation.clone();
            self.refresh_activity(&mut observation);
            return observation;
        }
        let mut counters = self.last_counters;
        counters.submitted = self.stream_position;
        counters.consumed = None;
        counters.restart_cursor = None;
        counters.delay = None;
        self.observation(counters)
    }
    fn refresh_activity(&self, observation: &mut PlaybackObservation) {
        observation.output_activity = self.output.activity();
    }
    fn cancelled_control_observation(&self) -> Option<&(Epochs, PlaybackObservation)> {
        (self.cancel.load(Ordering::Acquire) && self.stream_position == 0)
            .then_some(self.control_stop_observation.as_ref())
            .flatten()
    }
    fn remember_control_observation(&mut self) {
        if !self.is_network() && self.stream_position != 0 {
            self.control_stop_observation = Some((self.epochs(), self.failure_observation()));
        }
    }
    fn processing_observation(&self) -> Option<PlaybackProcessingObservation> {
        self.pipeline
            .as_ref()
            .map(|p| PlaybackProcessingObservation {
                streaming: p.stream_render().map(|r| {
                    let positions = r.output_positions();
                    StreamingRuntimeObservation {
                        source_epochs: self.source_epochs,
                        output_epochs: positions.epochs,
                        source_accepted_frames: r.stream().stats().source_frames,
                        generated_output_frames: positions.generated,
                        submission_frontier_frames: positions.next_submission,
                        backend_origin_frames: positions.backend_origin,
                        retained_start_frames: positions.retained_start,
                    }
                }),
                source_boundary: self.replacement_boundary.clone(),
                resident_bytes: p.resources().pipeline_bytes,
                audio_payload_bytes: if p.stream_render().is_some() {
                    None
                } else {
                    Some(p.render().map_or(0, |r| r.resources().audio_payload_bytes))
                },
                failure_node: p
                    .stream_render()
                    .and_then(|r| {
                        let stage = r.stream().failure()?.stage?;
                        let graph = r.stream().island(stage)?;
                        let node = graph
                            .plan()
                            .schedule()
                            .get(graph.failure_node_index()?)?
                            .id();
                        let earph_core::graph_control::GraphTiming::Streaming { stages } =
                            &p.preview().timing
                        else {
                            return None;
                        };
                        stages
                            .get(stage)?
                            .original_nodes
                            .contains(node)
                            .then(|| node.clone())
                    })
                    .or_else(|| {
                        p.render().and_then(|r| {
                            r.graph().failure_node_index().and_then(|index| {
                                r.graph()
                                    .plan()
                                    .schedule()
                                    .get(index)
                                    .map(|node| node.id().clone())
                            })
                        })
                    }),
                graph_hash: p.preview().graph_hash.clone(),
                timing: p.preview().timing.playback_timing(),
                saturation_count: self.saturation_count,
                graph_peak: self.graph_peak,
                prequantization_peak: self.prequantization_peak,
            })
    }
    fn observation(&self, counters: Counters) -> PlaybackObservation {
        let filter_phase = self.pipeline.as_ref().is_some_and(|p| {
            p.preview().transformations.iter().any(|t| {
                matches!(
                    t,
                    earph_core::graph_control::SignalTransformation::Kernel {
                        transformation: earph_core::graph::Transformation::Peq { .. }
                            | earph_core::graph::Transformation::Convolution { .. }
                    }
                )
            })
        });
        // Filter scheduling latency does not establish a frequency-independent source position.
        let audible = (!filter_phase)
            .then_some(counters.consumed)
            .flatten()
            .and_then(|position| {
                self.graph_latency()
                    .and_then(|latency| position.checked_sub(latency))
            })
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
            output_activity: self.output.activity(),
            processing: self.processing_observation(),
            format: self.format(),
            submitted_frames: counters.submitted,
            consumed_frames: counters.consumed,
            backend_delay_frames: counters.delay,
            consumption_basis: counters.basis,
            source_entry_id: self
                .current
                .entry
                .as_ref()
                .expect("local playback observation")
                .id
                .clone(),
            source_position_frames: if self
                .pipeline
                .as_ref()
                .is_some_and(|p| p.stream_render().is_some())
            {
                self.source_position
            } else {
                self.current
                    .packet
                    .as_ref()
                    .map_or(self.source_position, |packet| {
                        packet.header().frame_position + packet.header().frame_count
                    })
            },
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
        self.reposition_with_boundary(settings, position, events, notify, false)
    }
    fn reposition_with_boundary(
        &mut self,
        settings: Settings,
        position: Option<u64>,
        events: &SyncSender<Event>,
        notify: bool,
        replacement: bool,
    ) -> Result<(), EngineError> {
        if self.cancelled_stream_source() {
            return Ok(());
        }
        if self.is_network() && self.pipeline.is_none() {
            if position.is_some() {
                return Err(ENDED);
            }
            self.output.suspend(settings.snapshot.epochs)?;
            self.native_epochs = settings.snapshot.epochs;
            self.settings = settings;
            self.set_gain()?;
            return if notify {
                self.network_paused(events)
            } else {
                Ok(())
            };
        }
        if self
            .pipeline
            .as_ref()
            .is_some_and(|p| p.stream_render().is_some())
        {
            self.remember_control_observation();
        }
        let counters = self.output.suspend(settings.snapshot.epochs)?;
        self.native_epochs = settings.snapshot.epochs;
        self.backend = self.output.observation()?;
        if let Some(stream) = self.pipeline.as_mut().and_then(|p| p.stream_render_mut()) {
            let restart = restart_cursor(counters)?;
            if position.is_none() {
                stream.hold(counters.submitted, restart, settings.snapshot.epochs)?;
                self.settings = settings;
                self.stream_position = 0;
                self.pending = false;
                self.started = false;
                if self.phase == RenderPhase::BackendDrain {
                    self.phase = RenderPhase::GraphTail;
                }
                self.set_gain()?;
                if notify {
                    if self.is_network() {
                        return self.network_paused(events);
                    }
                    let observation = self.observe()?;
                    emit(
                        events,
                        Event::Paused {
                            epochs: self.epochs(),
                            observation,
                        },
                    )?;
                }
                return Ok(());
            }
            if replacement {
                // Validate the old backend cursor before recording discarded held output.
                stream.observe(counters.submitted, restart)?;
                let output = stream.output_positions();
                let frontier = output.backend_origin.checked_add(restart).ok_or(FAILED)?;
                self.replacement_boundary = Some(Arc::new(StreamingReplacementBoundary {
                    source_entry_id: self.current.local_entry()?.id.clone(),
                    source_position_frames: self.source_position,
                    source_epochs: self.source_epochs,
                    discarded_output_frames: output
                        .generated
                        .checked_sub(frontier)
                        .ok_or(FAILED)?,
                }));
            }
        }
        let requested = if let Some(position) = position {
            (self.current.local_entry()?.clone(), position)
        } else {
            let consumed = counters
                .restart_cursor
                .ok_or(EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "pause requires a known backend restart boundary",
                ))?
                .saturating_sub(self.graph_latency().ok_or(EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "streaming timing has no source rewind projection",
                ))?);
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
        if self.current.local_entry()?.id != requested.0.id {
            self.current = Track::open(requested.0, self.epochs(), self.epochs(), events)?;
        }
        self.current.seek(requested.1, self.epochs())?;
        self.source_epochs = self.epochs();
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
            entry: self.current.local_entry()?.clone(),
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
        if let Some(error) = self.preparation_failure.take() {
            return Err(error);
        }
        self.current.file.check()?;
        if self.cancel.load(Ordering::Acquire) {
            self.output.stop()?;
            return Ok(true);
        }
        self.prefetch(events)?;
        if !self.settings.playing
            || self.settings.snapshot.muted
            || self.is_network() && self.pipeline.is_none()
        {
            return Ok(false);
        }
        if self
            .pipeline
            .as_ref()
            .is_some_and(|p| p.stream_render().is_some())
        {
            let result = self.step_streaming(events);
            if result
                .as_ref()
                .is_err_and(|error| error.code == ErrorCode::OperationCancelled)
                && self.cancel.load(Ordering::Acquire)
            {
                // Cancellation may arrive inside a stream call, after the entry check.
                self.output.stop()?;
                return Ok(true);
            }
            return result;
        }
        if self.phase == RenderPhase::BackendDrain {
            if let Some(counters) = self.output.finish()? {
                self.last_counters = counters;
                self.phase = RenderPhase::Finished;
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
            if !self.source_packet(events)? {
                return Ok(false);
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
                if frames != 0 {
                    self.control_stop_observation = None;
                }
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
    fn stream_progress(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.is_network() {
            return self.network_progress(events);
        }
        let observation = self.observe()?;
        if let Ok(mut latest) = self.progress.try_lock() {
            *latest = Some((self.epochs(), observation));
        }
        Ok(())
    }
    fn stage_stream(&mut self) -> Result<(), EngineError> {
        if self.pending {
            return Ok(());
        }
        let render = self
            .pipeline
            .as_mut()
            .and_then(|p| p.stream_render_mut())
            .ok_or(FAILED)?;
        if let Some(report) = render.stage()? {
            self.graph_peak = Some(report.graph_peak);
            self.prequantization_peak = Some(report.prequantization_peak);
            self.output_peak = Some(report.output_peak);
            self.saturation_count = self
                .saturation_count
                .saturating_add(report.saturation_count);
            self.modified = true;
            self.pending = true;
        }
        Ok(())
    }
    fn step_streaming(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if self.phase == RenderPhase::BackendDrain {
            if let Some(counters) = self.output.finish()? {
                self.pipeline
                    .as_mut()
                    .and_then(|p| p.stream_render_mut())
                    .ok_or(FAILED)?
                    .observe(counters.submitted, restart_cursor(counters)?)?;
                self.last_counters = counters;
                self.phase = RenderPhase::Finished;
                return Ok(true);
            }
            self.output.wait()?;
            return Ok(false);
        }
        self.stage_stream()?;
        if !self.pending {
            if self.phase == RenderPhase::Feeding && !self.source_packet(events)? {
                return Ok(false);
            }
            let stream = self
                .pipeline
                .as_mut()
                .and_then(|p| p.stream_render_mut())
                .ok_or(FAILED)?;
            if self.phase == RenderPhase::GraphTail {
                let before = stream.stream().stats();
                let result = stream.finish();
                let after = stream.stream().stats();
                if after.source_frames != before.source_frames
                    || after.output_frames != before.output_frames
                {
                    self.control_stop_observation = None;
                }
                result?;
            } else {
                let packet = self.current.packet.as_ref().ok_or(FAILED)?;
                let count = (packet.header().frame_count as usize - self.current.offset).min(
                    stream
                        .stream()
                        .stage_info(0)
                        .ok_or(FAILED)?
                        .input_quantum_frames,
                );
                let before = stream.stream().stats();
                let result = stream.pump(Some(SourceSpan {
                    block: packet,
                    offset: self.current.offset,
                    frames: count,
                }));
                let consumed = usize::try_from(
                    stream
                        .stream()
                        .stats()
                        .source_frames
                        .checked_sub(before.source_frames)
                        .ok_or(INVALID_STREAM_COUNT)?,
                )
                .map_err(|_| INVALID_STREAM_COUNT)?;
                if consumed > count
                    || result
                        .as_ref()
                        .is_ok_and(|step| step.consumed_frames != consumed)
                {
                    return Err(INVALID_STREAM_COUNT);
                }
                if consumed != 0 || stream.stream().stats().output_frames != before.output_frames {
                    self.control_stop_observation = None;
                }
                let channels = self.format().channels().count();
                let reference = -(self.format().representation().integer_bounds()?.0 as f64);
                let mut peak = 0.0_f64;
                for index in
                    self.current.offset * channels..(self.current.offset + consumed) * channels
                {
                    peak = peak.max(
                        (packet.integer_sample(index).ok_or(FAILED)? as f64 / reference).abs(),
                    );
                }
                if consumed != 0 {
                    self.input_peak = Some(peak);
                }
                self.current.offset += consumed;
                self.source_position = self
                    .source_position
                    .checked_add(consumed as u64)
                    .ok_or(FAILED)?;
                self.source_stream_position = self
                    .source_stream_position
                    .checked_add(consumed as u64)
                    .ok_or(FAILED)?;
                if self.current.offset == packet.header().frame_count as usize {
                    self.current.packet = None;
                }
                result?;
            }
            self.stage_stream()?;
            if !self.pending {
                if self
                    .pipeline
                    .as_ref()
                    .and_then(|p| p.stream_render())
                    .ok_or(FAILED)?
                    .finished()
                {
                    self.phase = RenderPhase::BackendDrain;
                }
                self.stream_progress(events)?;
                return Ok(false);
            }
        }
        let render = self
            .pipeline
            .as_mut()
            .and_then(|p| p.stream_render_mut())
            .ok_or(FAILED)?;
        let offset = render.period_offset();
        let count = render.period().header().frame_count as usize - offset;
        self.current.file.check()?;
        match self.output.write(render.period(), offset, count)? {
            Some(frames) => {
                render.accepted(frames)?;
                if frames != 0 {
                    self.control_stop_observation = None;
                }
                self.stream_position = self
                    .stream_position
                    .checked_add(frames as u64)
                    .ok_or(FAILED)?;
                if !self.started && self.output.is_running() {
                    if self.is_network() {
                        self.emit_network(
                            events,
                            earph_core::network_source::NetworkEventKind::Started {
                                activation_frame: 0,
                            },
                        )?;
                    } else {
                        emit(
                            events,
                            Event::Started {
                                epochs: self.epochs(),
                            },
                        )?;
                    }
                    self.started = true;
                }
                self.pending = frames != count;
                self.stream_progress(events)?;
            }
            None => self.output.wait()?,
        }
        Ok(false)
    }
    fn source_packet(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if let Reader::Http(source) = &mut self.current.file {
            source.check()?;
            if self.current.packet.is_some() {
                return Ok(true);
            }
            match source.poll()? {
                network::SourcePoll::Pending => {
                    thread::sleep(Duration::from_millis(1));
                    return Ok(false);
                }
                network::SourcePoll::Block(block) => {
                    self.current.packet = Some(block);
                    self.current.offset = 0;
                    return Ok(true);
                }
                network::SourcePoll::End => {
                    self.phase = RenderPhase::GraphTail;
                    return Ok(false);
                }
            }
        }
        loop {
            if self.current.packet.is_some() {
                return Ok(true);
            }
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
                            entry: self.current.local_entry()?.clone(),
                            start: self.source_stream_position,
                            source_start: 0,
                            end: self.source_stream_position,
                        });
                        let next = {
                            let mut view = self.view.lock().map_err(|_| FAILED)?;
                            view.active = Some(self.current.local_entry()?.id.clone());
                            view.reserved = view
                                .following(&self.current.local_entry()?.id)
                                .map(|entry| entry.id);
                            view.reserved.clone()
                        };
                        emit(
                            events,
                            Event::Reserved {
                                epochs: self.epochs(),
                                active: self.current.local_entry()?.id.clone(),
                                next,
                            },
                        )?;
                        self.prefetch(events)?;
                    } else {
                        let Ok(view) = self.view.try_lock() else {
                            return Ok(false);
                        };
                        if view.following(&self.current.local_entry()?.id).is_some() {
                            return Ok(false);
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
                }
                Err(error) => {
                    item(
                        events,
                        self.epochs(),
                        &self.current.local_entry()?.id,
                        PreparationStatus::Failed { code: error.code },
                    )?;
                    return Err(error);
                }
            }
        }
    }
}

fn restart_cursor(counters: Counters) -> Result<u64, EngineError> {
    counters.restart_cursor.ok_or(EngineError::new(
        ErrorCode::CapabilityUnsupported,
        "streaming requires a known backend restart boundary",
    ))
}

struct SourceTerminal {
    token: SourceTerminalToken,
    control_epochs: Epochs,
    reported_epochs: Option<Epochs>,
    outcome: SourceTerminalOutcome,
}
enum SourceTerminalOutcome {
    Completed(PlaybackObservation),
    Failed {
        error: EngineError,
        observation: Option<PlaybackObservation>,
        stop: Option<Box<StopRequest>>,
    },
}
impl SourceTerminal {
    fn completed_from_playback(playback: &Playback) -> Result<Self, EngineError> {
        let snapshot = &playback.settings.snapshot;
        let observation = playback.observation(playback.last_counters);
        Ok(Self {
            token: SourceTerminalToken {
                owner_id: snapshot.owner_id.clone(),
                session_id: snapshot.session_id.clone().ok_or(FAILED)?,
                origin_epochs: playback.epochs(),
                graph_epoch: playback.installed_graph_epoch,
                device_epoch: snapshot.device_epoch,
                final_entry_id: observation.source_entry_id.clone(),
            },
            control_epochs: playback.epochs(),
            reported_epochs: None,
            outcome: SourceTerminalOutcome::Completed(observation),
        })
    }
    fn failed(
        settings: &Settings,
        error: EngineError,
        observation: Option<PlaybackObservation>,
    ) -> Result<Self, EngineError> {
        let snapshot = &settings.snapshot;
        let entry = observation
            .as_ref()
            .map(|value| &value.source_entry_id)
            .or(snapshot.active_entry_id.as_ref())
            .ok_or(FAILED)?;
        Ok(Self {
            token: SourceTerminalToken {
                owner_id: snapshot.owner_id.clone(),
                session_id: snapshot.session_id.clone().ok_or(FAILED)?,
                origin_epochs: snapshot.epochs,
                graph_epoch: snapshot.graph_epoch,
                device_epoch: snapshot.device_epoch,
                final_entry_id: entry.clone(),
            },
            control_epochs: snapshot.epochs,
            reported_epochs: None,
            outcome: SourceTerminalOutcome::Failed {
                error,
                observation,
                stop: None,
            },
        })
    }
    fn matches_source(&self, snapshot: &SessionSnapshot) -> bool {
        let matches = match &self.outcome {
            SourceTerminalOutcome::Failed {
                stop: Some(request),
                ..
            } => request.matches_snapshot(snapshot) && request.matches_source(&self.token),
            _ => self.token.matches_source(snapshot),
        };
        matches
            && matches!(
                snapshot.transport,
                TransportState::Preparing
                    | TransportState::Buffering
                    | TransportState::Playing
                    | TransportState::Paused
                    | TransportState::Reconfiguring
                    | TransportState::Draining
            )
    }
    fn report(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.reported_epochs == Some(self.control_epochs) {
            return Ok(());
        }
        let event = match &self.outcome {
            SourceTerminalOutcome::Completed(observation) => Event::Drained {
                epochs: self.control_epochs,
                token: self.token.clone(),
                observation: observation.clone(),
            },
            SourceTerminalOutcome::Failed {
                error,
                observation,
                stop: None,
            } => Event::Failed {
                epochs: self.control_epochs,
                token: Some(self.token.clone()),
                error: *error,
                observation: observation.clone(),
            },
            SourceTerminalOutcome::Failed {
                error,
                observation,
                stop: Some(request),
            } => Event::StopFailed {
                request: request.clone(),
                token: self.token.clone(),
                error: *error,
                observation: observation.clone(),
            },
        };
        emit(events, event)?;
        self.reported_epochs = Some(self.control_epochs);
        Ok(())
    }
    fn bind_stop(
        &mut self,
        request: &StopRequest,
        events: &SyncSender<Event>,
    ) -> Result<bool, EngineError> {
        if !request.matches_source(&self.token) {
            return Ok(false);
        }
        let SourceTerminalOutcome::Failed { error, stop, .. } = &mut self.outcome else {
            return Ok(false);
        };
        if error.code == ErrorCode::OperationCancelled {
            return Ok(false);
        }
        if stop.as_ref().is_some_and(|prior| {
            prior.request_id == request.request_id
                && prior.settings.snapshot == request.settings.snapshot
        }) {
            self.report(events)?;
            return Ok(true);
        }
        *stop = Some(Box::new(request.clone()));
        self.control_epochs = request.settings.snapshot.epochs;
        self.reported_epochs = None;
        self.report(events)?;
        Ok(true)
    }
    fn reconcile(
        &mut self,
        settings: &Settings,
        events: &SyncSender<Event>,
        positioning: bool,
    ) -> Result<bool, EngineError> {
        let snapshot = &settings.snapshot;
        if !self.matches_source(snapshot) {
            return Ok(false);
        }
        if self.control_epochs == snapshot.epochs {
            self.report(events)?;
            return Ok(true);
        }
        if (!snapshot.muted && !positioning)
            || self.control_epochs.stream.checked_add(1) != Some(snapshot.epochs.stream)
        {
            return Ok(false);
        }
        self.control_epochs = snapshot.epochs;
        self.report(events)?;
        Ok(true)
    }
    fn activation_terminal(
        &mut self,
        settings: &Settings,
        activation: &GraphActivationRequest,
        events: &SyncSender<Event>,
    ) -> Result<bool, EngineError> {
        if activation.previous_binding.epochs != self.control_epochs
            || self.token.owner_id.as_ref() != Some(&activation.previous_binding.owner_id)
            || activation.previous_binding.session_id != self.token.session_id
            || activation.previous_binding.device_epoch != self.token.device_epoch
            || activation.previous_binding.epochs.next_stream()? != activation.epochs
            || settings.snapshot.epochs != activation.epochs
        {
            return Ok(false);
        }
        self.reconcile(settings, events, true)
    }
}
fn retain_source_failure(
    terminal: &mut Option<SourceTerminal>,
    settings: &Settings,
    error: EngineError,
    observation: Option<PlaybackObservation>,
    events: &SyncSender<Event>,
) {
    match SourceTerminal::failed(settings, error, observation.clone()) {
        Ok(mut failed) => {
            let _ = failed.report(events);
            *terminal = Some(failed);
        }
        Err(_) => {
            let _ = emit(
                events,
                Event::Failed {
                    epochs: settings.snapshot.epochs,
                    token: None,
                    error,
                    observation,
                },
            );
        }
    }
}
type StoppedSource = (Settings, PlaybackObservation, Option<EngineError>);
fn playback_step_failure(
    current: &mut Playback,
    error: EngineError,
    stopped: &mut Option<StoppedSource>,
    terminal: &mut Option<SourceTerminal>,
    events: &SyncSender<Event>,
) {
    let mut observation = current.failure_observation();
    let settings = current.failure_context(None);
    let cancelled = current.cancel.load(Ordering::Acquire);
    let _ = current.output.stop();
    current.refresh_activity(&mut observation);
    if cancelled {
        *stopped = Some((settings.clone(), observation.clone(), Some(error)));
    }
    clear_reservation(&current.view, current.epochs());
    clear_reservation(&current.view, settings.snapshot.epochs);
    retain_source_failure(terminal, &settings, error, Some(observation), events);
}
fn action_failure(
    playback: &mut Option<Playback>,
    stopped: &mut Option<StoppedSource>,
    terminal: &mut Option<SourceTerminal>,
    mut settings: Option<Settings>,
    error: EngineError,
    before_control: Option<(Settings, PlaybackObservation)>,
    events: &SyncSender<Event>,
) {
    if error.code == ErrorCode::OperationCancelled
        && playback.as_ref().is_some_and(|current| {
            current.cancel.load(Ordering::Acquire)
                && (current.cancelled_stream_source() || before_control.is_some())
        })
    {
        let mut current = playback.take().expect("checked live source");
        let (context, mut observation) = if current.cancelled_control_observation().is_some() {
            (current.failure_context(None), current.failure_observation())
        } else {
            before_control
                .unwrap_or_else(|| (current.failure_context(None), current.failure_observation()))
        };
        let close_error = current.output.stop().err();
        current.refresh_activity(&mut observation);
        clear_reservation(&current.view, context.snapshot.epochs);
        clear_reservation(&current.view, current.epochs());
        if let Some(error) = close_error {
            retain_source_failure(terminal, &context, error, Some(observation.clone()), events);
        }
        *stopped = Some((context, observation, close_error));
        return;
    }
    let observation = playback.take().map(|mut current| {
        let mut observation = current.failure_observation();
        let _ = current.output.stop();
        current.refresh_activity(&mut observation);
        clear_reservation(&current.view, current.epochs());
        settings = Some(current.failure_context(settings.take()));
        observation
    });
    if let Some(settings) = settings {
        retain_source_failure(terminal, &settings, error, observation, events);
    }
}
fn stop_source(
    request: StopRequest,
    playback: &mut Option<Playback>,
    stopped: &mut Option<StoppedSource>,
    terminal: &mut Option<SourceTerminal>,
    view: &Mutex<PlanView>,
    events: &SyncSender<Event>,
) -> Result<(), EngineError> {
    if let Some(mut current) = playback.take() {
        let mut observation = current.failure_observation();
        let context = current.failure_context(None);
        let failure = current.output.stop().err();
        current.refresh_activity(&mut observation);
        clear_reservation(view, context.snapshot.epochs);
        clear_reservation(view, current.epochs());
        *stopped = Some((context, observation, failure));
    }
    if let Some(failed) = terminal.as_mut()
        && failed.bind_stop(&request, events)?
    {
        *stopped = None;
        return Ok(());
    }
    match stopped.take() {
        Some((settings, observation, Some(error))) => {
            let mut failed = SourceTerminal::failed(&settings, error, Some(observation))?;
            if !failed.bind_stop(&request, events)? {
                failed.report(events)?;
            }
            *terminal = Some(failed);
            Ok(())
        }
        value => {
            *terminal = None;
            emit(
                events,
                Event::Stopped {
                    epochs: request.settings.snapshot.epochs,
                    observation: value.map(|(_, observation, _)| observation),
                },
            )
        }
    }
}
fn run(
    receiver: Receiver<Action>,
    events: SyncSender<Event>,
    queued: Arc<AtomicUsize>,
    view: Arc<Mutex<PlanView>>,
    progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
    retiring: Arc<AtomicBool>,
) {
    let mut playback: Option<Playback> = None;
    let mut stopped: Option<StoppedSource> = None;
    let mut terminal: Option<SourceTerminal> = None;
    let mut network_preparation: Option<NetworkPreparation> = None;
    let mut network_retirement = None;
    let mut network_terminal_reported = false;
    let mut deferred_start = None;
    loop {
        if let Some(closing) = network_retirement.as_ref() {
            match network::retire_owned(&mut playback, &mut network_preparation, closing) {
                Ok(ack) => {
                    let _ = emit(&events, Event::NetworkRetired(ack));
                    network_retirement = None;
                    retiring.store(false, Ordering::Release);
                    network_terminal_reported = false;
                }
                Err(error) if error.code != ErrorCode::SourceBusy => {
                    let _ = emit(
                        &events,
                        Event::Network(earph_core::network_source::NetworkEvent {
                            binding: closing.context.binding.clone(),
                            source_epochs: closing.context.source_epochs,
                            output_epochs: closing.output_epochs,
                            graph_epoch: closing.graph_epoch,
                            device_epoch: closing.device_epoch,
                            monotonic_ns: 0,
                            kind: earph_core::network_source::NetworkEventKind::Failed {
                                code: error.code,
                            },
                        }),
                    );
                }
                Err(_) => {}
            }
        }
        let action = if network_retirement.is_none() && deferred_start.is_some() {
            deferred_start.take()
        } else if playback.is_some()
            || network_preparation.is_some()
            || network_retirement.is_some()
        {
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
            if matches!(action, Action::Start { .. } | Action::StartNetwork { .. })
                && network_retirement.is_some()
            {
                deferred_start = Some(action);
                continue;
            }
            queued.fetch_sub(1, Ordering::AcqRel);
            if matches!(action, Action::Start { .. } | Action::Shutdown) {
                terminal = None;
            }
            let failure_settings = match &action {
                Action::ActivateGraph { settings, .. }
                | Action::Pause(settings)
                | Action::Seek { settings, .. }
                | Action::Settings(settings) => Some(settings.clone()),
                _ => None,
            };
            // Controls submit no audio. Retain coherent facts before a partial seek or hold.
            let before_control = failure_settings.as_ref().and_then(|_| {
                let incoming_stream = matches!(
                    &action,
                    Action::ActivateGraph { pipeline, .. } if pipeline.stream_render().is_some()
                );
                playback
                    .as_ref()
                    .filter(|p| {
                        !p.is_network()
                            && (incoming_stream
                                || p.pipeline
                                    .as_ref()
                                    .is_some_and(|p| p.stream_render().is_some()))
                    })
                    .map(|p| (p.failure_context(None), p.failure_observation()))
            });
            let result = match action {
                Action::RetireNetwork(closing) => {
                    network_retirement = Some(closing);
                    Ok(())
                }
                Action::StartNetwork {
                    settings,
                    context,
                    permit,
                    live,
                    clock,
                    cancel,
                } => {
                    // Native local ownership ends before any HTTP request starts.
                    if let Some(current) = &mut playback
                        && let Err(error) = current.output.stop()
                    {
                        let _ = network::preparation_failed(&settings, &context, error, &events);
                        continue;
                    }
                    playback = None;
                    stopped = None;
                    terminal = None;
                    network_terminal_reported = false;
                    match HttpSource::start(context.clone(), permit, live, clock, cancel) {
                        Ok(source) => {
                            network_preparation = Some(NetworkPreparation { settings, source })
                        }
                        Err((error, source)) => {
                            let _ =
                                network::preparation_failed(&settings, &context, error, &events);
                            network_preparation = source
                                .owns_job()
                                .then_some(NetworkPreparation { settings, source });
                            network_terminal_reported = true;
                        }
                    }
                    Ok(())
                }
                Action::ActivateGraph {
                    settings,
                    activation,
                    pipeline,
                } => {
                    let epochs = activation.epochs;
                    let request_id = activation.request_id.clone();
                    let live = playback.is_some();
                    let network = playback.as_ref().is_some_and(|p| p.is_network());
                    let result = if let Some(current) = &mut playback {
                        current.activate_graph(settings, activation, pipeline, &events)
                    } else {
                        drop(pipeline);
                        match terminal
                            .as_mut()
                            .map(|completed| {
                                completed.activation_terminal(&settings, &activation, &events)
                            })
                            .transpose()
                        {
                            Ok(Some(true)) => Ok(()),
                            Ok(_) => Err(FAILED),
                            Err(error) => Err(error),
                        }
                    };
                    if network && let Err(error) = result {
                        if let Some(current) = &mut playback {
                            let _ = emit(
                                &events,
                                Event::NetworkGraphFailed {
                                    request_id,
                                    error,
                                    actual: current.execution_fence(),
                                },
                            );
                            let _ = current.output.stop();
                            network_terminal_reported = true;
                        }
                        continue;
                    }
                    if !live && let Err(error) = result {
                        let _ = emit(
                            &events,
                            Event::GraphFailed {
                                epochs,
                                request_id,
                                error,
                            },
                        );
                    }
                    if live { result } else { Ok(()) }
                }
                Action::AcknowledgeSourceTerminal(token) => {
                    if terminal
                        .as_ref()
                        .is_some_and(|completed| completed.token == token)
                    {
                        terminal = None;
                    }
                    Ok(())
                }
                Action::Shutdown => break,
                Action::Stop(request) => stop_source(
                    request,
                    &mut playback,
                    &mut stopped,
                    &mut terminal,
                    &view,
                    &events,
                ),
                Action::Start {
                    settings,
                    entries,
                    cancel,
                } => {
                    let epochs = settings.snapshot.epochs;
                    let attempted = settings.clone();
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
                            retain_source_failure(&mut terminal, &attempted, error, None, &events);
                            Ok(())
                        }
                    }
                }
                #[cfg(all(test, target_os = "linux"))]
                Action::PausePreparation(gate) => {
                    tests::install_preparation_gate(gate);
                    Ok(())
                }
                Action::Settings(settings) => {
                    if let Some(current) = &mut playback {
                        current.apply_settings(settings, &events)
                    } else if let Some(preparing) = &mut network_preparation {
                        preparing.settings = settings;
                        Ok(())
                    } else if let Some(completed) = terminal.as_mut() {
                        completed.reconcile(&settings, &events, false).map(|_| ())
                    } else {
                        Ok(())
                    }
                }
                Action::Pause(settings) => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, None, &events, true)
                    } else if let Some(preparing) = &mut network_preparation {
                        preparing.settings = settings;
                        Ok(())
                    } else if let Some(completed) = terminal.as_mut() {
                        completed
                            .reconcile(&settings, &events, true)
                            .and_then(|matched| if matched { Ok(()) } else { Err(ENDED) })
                    } else {
                        Err(ENDED)
                    }
                }
                Action::Seek { settings, position } => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, Some(position), &events, true)
                    } else if let Some(completed) = terminal.as_mut() {
                        completed
                            .reconcile(&settings, &events, true)
                            .and_then(|matched| if matched { Ok(()) } else { Err(ENDED) })
                    } else {
                        Err(ENDED)
                    }
                }
            };
            if let Err(error) = result {
                if let Some(current) = playback.as_mut().filter(|p| p.is_network()) {
                    let _ = current.output.stop();
                    let _ = current.emit_network(
                        &events,
                        earph_core::network_source::NetworkEventKind::Failed { code: error.code },
                    );
                    network_terminal_reported = true;
                } else {
                    action_failure(
                        &mut playback,
                        &mut stopped,
                        &mut terminal,
                        failure_settings,
                        error,
                        before_control,
                        &events,
                    );
                }
            }
        }
        if network_retirement.is_some() {
            continue;
        }
        if !network_terminal_reported && let Some(preparing) = &mut network_preparation {
            match preparing.source.inspect() {
                Ok(observed) if observed.ready.is_some() => {
                    let preparing = network_preparation.take().expect("owned preparation");
                    let settings = preparing.settings.clone();
                    let context = preparing.source.context.clone();
                    let cancel = preparing.source.cancellation();
                    let mut track = Some(Track {
                        entry: None,
                        file: Reader::Http(Box::new(preparing.source)),
                        packet: None,
                        offset: 0,
                    });
                    match Playback::prepare_current(
                        settings.clone(),
                        &mut track,
                        cancel,
                        view.clone(),
                        progress.clone(),
                        &events,
                        settings.snapshot.profile_storage_bytes,
                    ) {
                        Ok(current) => playback = Some(current),
                        Err(error) => {
                            if let Some(Track {
                                file: Reader::Http(source),
                                ..
                            }) = track
                            {
                                network_preparation = Some(NetworkPreparation {
                                    settings: settings.clone(),
                                    source: *source,
                                });
                            }
                            let _ =
                                network::preparation_failed(&settings, &context, error, &events);
                            network_terminal_reported = true;
                        }
                    }
                }
                Ok(_) => thread::sleep(Duration::from_millis(1)),
                Err(error) => {
                    if !network_terminal_reported {
                        let _ = network::preparation_failed(
                            &preparing.settings,
                            &preparing.source.context,
                            error,
                            &events,
                        );
                        network_terminal_reported = true;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
        }
        if network_terminal_reported && network_preparation.is_some() {
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(current) = &mut playback {
            if current.is_network() && network_terminal_reported {
                thread::sleep(Duration::from_millis(1));
                continue;
            }
            match current.step(&events) {
                Ok(true) => {
                    if current.is_network() {
                        if current.phase == RenderPhase::Finished {
                            let observation = current.network_observation(
                                current.last_counters,
                                earph_core::network_source::NetworkLifecycle::Completed,
                            );
                            let _ = current.emit_network(
                                &events,
                                earph_core::network_source::NetworkEventKind::Completed {
                                    observation,
                                },
                            );
                        }
                        network_terminal_reported = true;
                        continue;
                    }
                    if current.cancel.load(Ordering::Acquire) {
                        stopped = Some((
                            current.failure_context(None),
                            current.failure_observation(),
                            None,
                        ));
                    } else if current.phase == RenderPhase::Finished {
                        match SourceTerminal::completed_from_playback(current) {
                            Ok(mut completed) => {
                                let _ = completed.report(&events);
                                terminal = Some(completed);
                            }
                            Err(error) => {
                                retain_source_failure(
                                    &mut terminal,
                                    &current.settings,
                                    error,
                                    None,
                                    &events,
                                );
                            }
                        }
                    }
                    clear_reservation(&view, current.epochs());
                    clear_reservation(&view, current.failure_context(None).snapshot.epochs);
                    playback = None;
                }
                Ok(false) => {
                    if !current.settings.playing || current.settings.snapshot.muted {
                        thread::sleep(Duration::from_millis(1));
                    }
                }
                Err(error) => {
                    if current.is_network() {
                        current.last_counters.submitted = current.stream_position;
                        current.last_counter_epochs = current.native_epochs;
                        current.last_counters.consumed = None;
                        current.last_counters.restart_cursor = None;
                        current.last_counters.delay = None;
                        let _ = current.output.stop();
                        let _ = current.emit_network(
                            &events,
                            earph_core::network_source::NetworkEventKind::Failed {
                                code: error.code,
                            },
                        );
                        network_terminal_reported = true;
                        continue;
                    }
                    playback_step_failure(current, error, &mut stopped, &mut terminal, &events);
                    playback = None;
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
fn silence_period_suffix(period: &mut PcmBlock, offset: usize) -> Result<(), EngineError> {
    let frames = period.header().frame_count as usize;
    if offset > frames {
        return Err(FAILED);
    }
    let channels = period.header().format.channels().count();
    for sample in offset * channels..frames * channels {
        period.set_integer_sample(sample, 0)?;
    }
    Ok(())
}
fn render_error(error: crate::render::RenderError) -> EngineError {
    EngineError::new(error.code, error.message)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use earph_core::{control::ControlLimits, ids::ZoneId, session::Session};

    #[cfg(target_os = "linux")]
    pub(crate) struct PreparationGate {
        reached: SyncSender<()>,
        release: Receiver<()>,
    }
    #[cfg(target_os = "linux")]
    struct PreparationRelease(Option<SyncSender<()>>);
    #[cfg(target_os = "linux")]
    impl Drop for PreparationRelease {
        fn drop(&mut self) {
            // The only release sender wakes the worker even if the test unwinds.
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    #[cfg(target_os = "linux")]
    thread_local! {
        // A one-shot gate cannot pause another test's worker thread.
        static PREPARATION_GATE: std::cell::RefCell<Option<PreparationGate>> = const { std::cell::RefCell::new(None) };
    }
    #[cfg(target_os = "linux")]
    fn preparation_gate() -> (PreparationGate, Receiver<()>, PreparationRelease) {
        let (reached, receiver) = mpsc::sync_channel(1);
        let (release, wait) = mpsc::sync_channel(1);
        (
            PreparationGate {
                reached,
                release: wait,
            },
            receiver,
            PreparationRelease(Some(release)),
        )
    }
    #[cfg(target_os = "linux")]
    pub(super) fn install_preparation_gate(gate: PreparationGate) {
        PREPARATION_GATE.with(|slot| assert!(slot.borrow_mut().replace(gate).is_none()));
    }
    #[cfg(target_os = "linux")]
    pub(super) fn after_preparation_check() {
        let gate = PREPARATION_GATE.with(|slot| slot.borrow_mut().take());
        if let Some(gate) = gate {
            gate.reached.send(()).unwrap();
            gate.release
                .recv_timeout(Duration::from_secs(5))
                .expect("test must release preparation gate");
        }
    }

    fn settings(muted: bool) -> Settings {
        let mut snapshot =
            Session::new(ZoneId::new("main").unwrap(), ControlLimits::default()).snapshot();
        snapshot.muted = muted;
        Settings {
            snapshot,
            playing: true,
        }
    }
    #[test]
    fn graph_source_token_tracks_actual_start_replacement_and_cancellation() {
        let (mut worker, _receiver) = admission();
        assert_eq!(
            worker.source_cancellation().unwrap_err().code,
            ErrorCode::StaleSession
        );
        let original = Arc::new(AtomicBool::new(false));
        worker
            .send(Action::Start {
                settings: settings(true),
                entries: vec![],
                cancel: original.clone(),
            })
            .unwrap();
        assert!(Arc::ptr_eq(
            &worker.source_cancellation().unwrap(),
            &original
        ));
        let replacement = Arc::new(AtomicBool::new(false));
        worker
            .send(Action::Start {
                settings: settings(true),
                entries: vec![],
                cancel: replacement.clone(),
            })
            .unwrap();
        assert!(original.load(Ordering::Acquire));
        assert!(Arc::ptr_eq(
            &worker.source_cancellation().unwrap(),
            &replacement
        ));
        worker.cancel_active();
        assert!(replacement.load(Ordering::Acquire));
        assert_eq!(
            worker.source_cancellation().unwrap_err().code,
            ErrorCode::StaleSession
        );
    }
    fn admission() -> (Worker, Receiver<Action>) {
        let (sender, receiver) = mpsc::sync_channel(COMMANDS);
        let (_, events) = mpsc::sync_channel(1);
        (
            Worker {
                sender,
                events,
                queued: Arc::new(AtomicUsize::new(0)),
                retiring: Arc::new(AtomicBool::new(false)),
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
        naturally_drained_tracks(1)
    }
    #[cfg(target_os = "linux")]
    fn naturally_drained_tracks(tracks: usize) -> (Worker, Settings, PlaybackObservation) {
        let (worker, settings, _, observation) = drain_proof(tracks);
        (worker, settings, observation)
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn owned_silence_waves(tracks: usize) -> Vec<std::path::PathBuf> {
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/terminal-drain")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
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
        let mut sources = Vec::new();
        for index in 0..tracks {
            let source = path.join(format!("silence-{index}.wav"));
            fs::write(&source, &bytes).unwrap();
            sources.push(source);
        }
        sources
    }
    #[cfg(target_os = "linux")]
    fn local_source_fixture(tracks: usize) -> (Settings, earph_core::queue::LocalQueue) {
        use earph_core::{
            control::{OutputBackend, OutputConfiguration, OutputRoute, SourceKind},
            ids::{DeviceId, ProfileId, SessionId},
            queue::LocalQueue,
            safety::{FidelityPolicy, VolumeMode},
            session::TransportState,
        };
        let mut queue = LocalQueue::default();
        for source in owned_silence_waves(tracks) {
            queue.append(source.to_str().unwrap()).unwrap();
        }
        let entry = queue.snapshot().entries[0].id.clone();
        let mut settings = settings(false);
        settings.snapshot.epochs.session = 1;
        settings.snapshot.owner_id = Some(OwnerId::new("terminal-owner").unwrap());
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
        (settings, queue)
    }
    #[cfg(target_os = "linux")]
    fn drain_proof(tracks: usize) -> (Worker, Settings, SourceTerminalToken, PlaybackObservation) {
        use std::time::Instant;
        let (settings, queue) = local_source_fixture(tracks);
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
        let (token, observation) = loop {
            let event = worker
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("native null worker did not finish");
            if let Event::Drained {
                epochs,
                token,
                observation,
            } = event
            {
                assert_eq!(epochs, settings.snapshot.epochs);
                break (token, observation);
            }
        };
        assert_eq!(observation.submitted_frames, 65 * tracks as u64);
        (worker, settings, token, observation)
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn profile_retention_refuses_source_preparation_before_opening_output() {
        let (mut settings, queue) = local_source_fixture(1);
        settings.snapshot.profile_storage_bytes = 256 * 1024 * 1024;
        let (events, _receiver) = mpsc::sync_channel(32);
        let result = Playback::prepare(
            settings,
            queue.snapshot().entries,
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(PlanView::default())),
            Arc::new(Mutex::new(None)),
            &events,
        );
        assert_eq!(
            result.err().unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
    #[cfg(target_os = "linux")]
    fn next_drain(worker: &Worker) -> (Epochs, PlaybackObservation) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("native completion was lost")
            {
                Event::End { .. } => {}
                Event::Drained {
                    epochs,
                    observation,
                    ..
                } => return (epochs, observation),
                _ => panic!("native completion became another transition"),
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn final_join_completion_survives_a_previous_entry_snapshot() {
        let (mut worker, mut settings, final_observation) = naturally_drained_tracks(2);
        assert_ne!(
            settings.snapshot.active_entry_id.as_ref(),
            Some(&final_observation.source_entry_id)
        );
        worker.send(Action::Settings(settings.clone())).unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        settings.snapshot.active_entry_id = Some(final_observation.source_entry_id.clone());
        worker.send(Action::Settings(settings.clone())).unwrap();
        let (epochs, observation) = next_drain(&worker);
        assert_eq!(epochs, settings.snapshot.epochs);
        assert_eq!(observation, final_observation);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn completion_survives_repeated_mute_generations_until_acknowledged() {
        let (mut worker, mut settings, expected) = naturally_drained_worker();
        for generation in [1, 2, 3] {
            settings.snapshot.epochs.stream = generation;
            settings.snapshot.muted = true;
            worker.send(Action::Settings(settings.clone())).unwrap();
            settings.snapshot.muted = false;
            worker.send(Action::Settings(settings.clone())).unwrap();
        }
        for generation in [1, 2, 3] {
            let (epochs, observation) = next_drain(&worker);
            assert_eq!(epochs.stream, generation);
            assert_eq!(observation, expected);
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn exact_completion_acknowledgement_releases_only_its_retained_proof() {
        let (mut worker, mut settings, token, _) = drain_proof(1);
        let mut wrong = token.clone();
        wrong.origin_epochs.stream += 1;
        worker
            .send(Action::AcknowledgeSourceTerminal(wrong))
            .unwrap();
        let mut wrong = token.clone();
        wrong.final_entry_id = QueueEntryId::new("another-final-entry").unwrap();
        worker
            .send(Action::AcknowledgeSourceTerminal(wrong))
            .unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings.clone())).unwrap();
        next_drain(&worker);
        worker
            .send(Action::AcknowledgeSourceTerminal(token))
            .unwrap();
        settings.snapshot.muted = false;
        worker.send(Action::Settings(settings.clone())).unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings)).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn an_old_completion_acknowledgement_cannot_clear_a_replacement_stream() {
        let (mut worker, mut settings, old_token, _) = drain_proof(1);
        settings.snapshot.epochs.session += 1;
        settings.snapshot.session_id = Some(SessionId::new("replacement-session").unwrap());
        let entries = worker.view.lock().unwrap().entries.clone();
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let new_token = loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Drained { token, .. } => break token,
                Event::Failed { error, .. } => panic!("replacement source failed: {error:?}"),
                _ => {}
            }
        };
        assert_ne!(old_token, new_token);
        worker
            .send(Action::AcknowledgeSourceTerminal(old_token))
            .unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings.clone())).unwrap();
        let (epochs, _) = next_drain(&worker);
        assert_eq!(epochs, settings.snapshot.epochs);
        worker
            .send(Action::AcknowledgeSourceTerminal(new_token))
            .unwrap();
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings)).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[cfg(target_os = "linux")]
    fn prepare_activation(
        settings: &Settings,
        observation: &PlaybackObservation,
    ) -> (Settings, GraphActivationRequest, PreparedPipeline) {
        use serde_json::json;
        let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
        let graph = serde_json::from_value(json!({"schema_version":1,"nodes":[
            {"id":"in","kind":{"kind":"input"},"format":port},
            {"id":"out","kind":{"kind":"output"},"format":port}],
            "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"}))
        .unwrap();
        prepare_activation_graph(
            settings,
            observation,
            graph,
            Arc::new(AtomicBool::new(false)),
        )
    }
    #[cfg(target_os = "linux")]
    fn prepare_activation_graph(
        settings: &Settings,
        observation: &PlaybackObservation,
        graph: earph_core::graph::GraphDocument,
        source_cancel: Arc<AtomicBool>,
    ) -> (Settings, GraphActivationRequest, PreparedPipeline) {
        use earph_core::{graph::*, graph_control::*, ids::PreparedGraphId};
        let mut snapshot = settings.snapshot.clone();
        snapshot.playback = Some(observation.clone());
        let binding = GraphBinding {
            owner_id: snapshot.owner_id.clone().unwrap(),
            session_id: snapshot.session_id.clone().unwrap(),
            epochs: snapshot.epochs,
            device_epoch: snapshot.device_epoch,
            expected_revision: snapshot.revision,
        };
        let request_id = RequestId::new("activate-after-native-finish").unwrap();
        let mut service = crate::graph::GraphService::spawn(None).unwrap();
        service
            .submit(
                GraphJobRequest {
                    renderer_application: None,
                    profile_application: None,
                    request_id: request_id.clone(),
                    binding: binding.clone(),
                    intent: Arc::new(GraphIntent {
                        graph: Arc::new(graph),
                        processing: GraphProcessingRequest::Float64 {
                            quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
                        },
                    }),
                    validate_only: false,
                    deadline_monotonic_ns: u64::MAX,
                },
                snapshot,
                source_cancel,
            )
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let result = loop {
            if let Some(result) = service.try_completion() {
                break result.result.unwrap();
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        let activation = GraphActivationRequest {
            renderer_application: None,
            request_id,
            epochs: binding.epochs.next_stream().unwrap(),
            previous_binding: binding,
            prepared: GraphPrepared {
                token: PreparedGraphId::new("completed-source-graph").unwrap(),
                expires_monotonic_ns: u64::MAX,
                preview: result.preview,
            },
        };
        let mut settings = settings.clone();
        settings.snapshot.epochs = activation.epochs;
        settings.snapshot.muted = true;
        settings.snapshot.transport = TransportState::Reconfiguring;
        (settings, activation, result.pipeline.unwrap())
    }
    #[cfg(target_os = "linux")]
    fn streaming_playback() -> (Playback, SyncSender<Event>, Receiver<Event>) {
        streaming_playback_frames(65)
    }
    #[cfg(target_os = "linux")]
    fn streaming_playback_frames(frames: usize) -> (Playback, SyncSender<Event>, Receiver<Event>) {
        streaming_playback_chunk(frames, 64, "responsive")
    }
    #[cfg(target_os = "linux")]
    fn streaming_playback_chunk(
        frames: usize,
        chunk: usize,
        preset: &str,
    ) -> (Playback, SyncSender<Event>, Receiver<Event>) {
        let (mut playback, events, receiver) = prepared_nonzero_frames(frames);
        playback.settings.snapshot.software_gain_db = -48.0;
        let graph = serde_json::from_value({
            let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":2048});
            let mut middle = port.clone();
            middle["sample_rate_hz"] = serde_json::json!(44100);
            serde_json::json!({"schema_version":1,"output":"out","nodes":[
                {"id":"in","kind":{"kind":"input"},"format":port},
                {"id":"down","kind":{"kind":"src","input_rate_hz":48000,"preset":preset,"chunk_frames":chunk},"format":middle},
                {"id":"up","kind":{"kind":"src","input_rate_hz":44100,"preset":preset,"chunk_frames":chunk},"format":port},
                {"id":"out","kind":{"kind":"output"},"format":port}],
                "edges":[{"from":"in","to":"down","from_port":0,"to_port":0},{"from":"down","to":"up","from_port":0,"to_port":0},{"from":"up","to":"out","from_port":0,"to_port":0}]})
        }).unwrap();
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation_graph(
            &playback.settings,
            &observed,
            graph,
            playback.cancel.clone(),
        );
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        playback.settings.snapshot.graph_epoch = 1;
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        receiver.try_iter().for_each(drop);
        (playback, events, receiver)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_priming_accepts_source_without_claiming_native_output_or_eof() {
        let (mut playback, events, _receiver) = streaming_playback();
        assert!(!playback.step(&events).unwrap());
        let observed = playback.observe().unwrap();
        assert_eq!(observed.source_position_frames, 64);
        assert_eq!(observed.submitted_frames, 0);
        assert_eq!(playback.current.offset, 64);
        assert_eq!(playback.phase, RenderPhase::Feeding);
        assert!(observed.audible_source_position_frames.is_none());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_hold_retains_packet_epoch_and_history_then_true_seek_resets_both() {
        let (mut playback, events, _receiver) = streaming_playback();
        playback.step(&events).unwrap();
        let source_epoch = playback.current.packet.as_ref().unwrap().header().epochs;
        let before = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats();
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        playback.reposition(pause, None, &events, true).unwrap();
        assert_eq!(
            playback.current.packet.as_ref().unwrap().header().epochs,
            source_epoch
        );
        assert_eq!(playback.current.offset, 64);
        assert_eq!(
            playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .stream()
                .stats(),
            before
        );
        let mut seek = playback.settings.clone();
        seek.snapshot.epochs = seek.snapshot.epochs.next_stream().unwrap();
        playback
            .reposition(seek.clone(), Some(17), &events, true)
            .unwrap();
        assert!(playback.current.packet.is_none());
        assert_eq!(playback.source_position, 17);
        let render = playback.pipeline.as_ref().unwrap().stream_render().unwrap();
        assert_eq!(render.stream().stats().source_frames, 0);
        assert_eq!(render.output_positions().generated, 0);
        assert_eq!(render.control_epochs(), seek.snapshot.epochs);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_running_holds_preserve_frontiers_and_resume_the_original_packet_epoch() {
        let (mut playback, events, _receiver) = streaming_playback_frames(8193);
        for _ in 0..64 {
            playback.step(&events).unwrap();
            if playback.stream_position > 0 {
                break;
            }
        }
        assert!(playback.stream_position > 0);
        let source_epoch = playback.source_epochs;
        let before = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats();
        let source_position = playback.source_position;
        for _ in 0..2 {
            let mut pause = playback.settings.clone();
            pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
            pause.snapshot.muted = true;
            playback.reposition(pause, None, &events, true).unwrap();
            let render = playback.pipeline.as_ref().unwrap().stream_render().unwrap();
            assert_eq!(render.stream().stats(), before);
            assert_eq!(render.output_positions().generated, before.output_frames);
            assert_eq!(
                render.output_positions().retained_start,
                render.output_positions().backend_origin
            );
            assert_eq!(playback.source_position, source_position);
            assert_eq!(playback.source_epochs, source_epoch);
            assert_eq!(
                playback.current.packet.as_ref().unwrap().header().epochs,
                source_epoch
            );
        }
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        for _ in 0..8 {
            playback.step(&events).unwrap();
        }
        assert!(playback.source_position > source_position);
        let observation = playback.observe().unwrap();
        let stream = observation.processing.unwrap().streaming.unwrap();
        assert_eq!(stream.source_epochs, source_epoch);
        assert_eq!(stream.output_epochs, playback.epochs());
        assert!(stream.output_epochs.stream > stream.source_epochs.stream);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_replacement_rebinds_the_accepted_source_frontier_without_cancelling_source() {
        let (mut playback, events, receiver) = streaming_playback_frames(8193);
        for _ in 0..16 {
            playback.step(&events).unwrap();
        }
        let accepted = playback.source_position;
        let source_epoch = playback.source_epochs;
        let old_hash = playback
            .pipeline
            .as_ref()
            .unwrap()
            .preview()
            .graph_hash
            .clone();
        let source_cancel = playback.cancel.clone();
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        playback.reposition(pause, None, &events, true).unwrap();
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&playback.settings, &observed);
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        let observation = playback.observe().unwrap();
        let processing = observation.processing.unwrap();
        assert_ne!(processing.graph_hash, old_hash);
        let boundary = processing.source_boundary.unwrap();
        assert_eq!(boundary.source_position_frames, accepted);
        assert_eq!(boundary.source_epochs, source_epoch);
        assert!(processing.streaming.is_none());
        assert_eq!(playback.source_position, accepted);
        assert_eq!(playback.source_epochs, playback.epochs());
        assert!(playback.current.packet.is_none());
        assert!(!source_cancel.load(Ordering::Acquire));
        receiver.try_iter().for_each(drop);
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        playback.step(&events).unwrap();
        assert!(playback.source_position > accepted);
        assert!(!source_cancel.load(Ordering::Acquire));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_holds_at_source_eof_and_before_native_drain_do_not_restart_finish() {
        let (mut playback, events, _receiver) = streaming_playback();
        for _ in 0..64 {
            playback.step(&events).unwrap();
            if playback.phase == RenderPhase::GraphTail {
                break;
            }
        }
        assert_eq!(playback.phase, RenderPhase::GraphTail);
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        playback.reposition(pause, None, &events, true).unwrap();
        assert!(playback.current.packet.is_none());
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        for _ in 0..256 {
            playback.step(&events).unwrap();
            if playback.phase == RenderPhase::BackendDrain {
                break;
            }
        }
        assert_eq!(playback.phase, RenderPhase::BackendDrain);
        let before = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats();
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        playback.reposition(pause, None, &events, true).unwrap();
        assert_eq!(playback.phase, RenderPhase::GraphTail);
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        for _ in 0..16 {
            if playback.step(&events).unwrap() {
                break;
            }
        }
        assert_eq!(playback.phase, RenderPhase::Finished);
        assert_eq!(
            playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .stream()
                .stats(),
            before
        );
        assert_eq!(before.source_frames, 65);
    }
    #[cfg(target_os = "linux")]
    fn resumed_tail_playback() -> (Playback, SyncSender<Event>, Receiver<Event>) {
        let (mut playback, events, receiver) = streaming_playback();
        for _ in 0..64 {
            playback.step(&events).unwrap();
            if playback.phase == RenderPhase::GraphTail {
                break;
            }
        }
        assert_eq!(playback.phase, RenderPhase::GraphTail);
        let source = playback.source_position;
        assert_eq!(source, 65);
        let origin = playback.epochs();
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = origin.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        playback.reposition(pause, None, &events, true).unwrap();
        playback.settings.snapshot.muted = false;
        playback.settings.playing = true;
        playback.set_gain().unwrap();
        let generated = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats()
            .output_frames;
        for _ in 0..256 {
            playback.step(&events).unwrap();
            if playback.stream_position > 0
                && playback
                    .pipeline
                    .as_ref()
                    .unwrap()
                    .stream_render()
                    .unwrap()
                    .stream()
                    .stats()
                    .output_frames
                    > generated
            {
                break;
            }
        }
        assert_ne!(playback.epochs(), origin);
        assert_eq!(playback.source_position, source);
        assert!(
            playback.stream_position > 0,
            "no resumed tail reached the real output"
        );
        assert_eq!(
            playback.failure_observation().submitted_frames,
            playback.output.progress().unwrap().submitted
        );
        let view = playback.view.lock().unwrap();
        assert!(view.active.is_some());
        assert_eq!(
            view.validate_edit(&earph_core::control::Command::QueueRemove {
                id: playback.current.entry.as_ref().unwrap().id.clone(),
            })
            .unwrap_err()
            .code,
            ErrorCode::SourceBusy
        );
        drop(view);
        receiver.try_iter().for_each(drop);
        (playback, events, receiver)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stopped_resumed_graph_tail_releases_its_original_queue_reservation() {
        let (current, events, receiver) = resumed_tail_playback();
        let before = current.failure_observation();
        let view = current.view.clone();
        let entry = current.current.entry.as_ref().unwrap().id.clone();
        let mut settings = current.settings.clone();
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        current.cancel.store(true, Ordering::Release);
        let mut playback = Some(current);
        let mut stopped = None;
        let mut terminal = None;
        stop_source(
            StopRequest {
                request_id: RequestId::new("tail-stop").unwrap(),
                settings,
            },
            &mut playback,
            &mut stopped,
            &mut terminal,
            &view,
            &events,
        )
        .unwrap();
        let event = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            matches!(event, Event::Stopped { observation: Some(observation), .. }
            if observation == before)
        );
        assert!(playback.is_none());
        let view = view.lock().unwrap();
        assert!(view.active.is_none());
        assert!(view.reserved.is_none());
        view.validate_edit(&earph_core::control::Command::QueueRemove { id: entry })
            .unwrap();
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn completed_resumed_graph_tail_releases_its_original_queue_reservation() {
        let (mut current, events, _receiver) = resumed_tail_playback();
        let view = current.view.clone();
        let entry = current.current.entry.as_ref().unwrap().id.clone();
        let mut completed = false;
        for _ in 0..256 {
            if current.step(&events).unwrap() {
                completed = true;
                break;
            }
        }
        assert!(completed);
        assert_eq!(current.phase, RenderPhase::Finished);
        assert!(current.failure_observation().submitted_frames > 0);
        drop(current);
        let view = view.lock().unwrap();
        assert!(view.active.is_none());
        assert!(view.reserved.is_none());
        view.validate_edit(&earph_core::control::Command::QueueRemove { id: entry })
            .unwrap();
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_resumed_graph_tail_releases_its_original_queue_reservation() {
        let (mut current, events, receiver) = resumed_tail_playback();
        let view = current.view.clone();
        let entry = current.current.entry.as_ref().unwrap().id.clone();
        let before = current.failure_observation();
        current.output.stop().unwrap();
        let error = current.step(&events).unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputFault);
        let mut stopped = None;
        let mut terminal = None;
        playback_step_failure(&mut current, error, &mut stopped, &mut terminal, &events);
        assert!(terminal.is_some());
        let event = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            matches!(event, Event::Failed { observation: Some(observation), error: actual, .. }
            if observation.submitted_frames == before.submitted_frames && actual == error)
        );
        drop(current);
        let view = view.lock().unwrap();
        assert!(view.active.is_none());
        assert!(view.reserved.is_none());
        view.validate_edit(&earph_core::control::Command::QueueRemove { id: entry })
            .unwrap();
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn retired_resumed_graph_tail_cannot_release_a_newer_source_reservation() {
        let (current, events, _receiver) = resumed_tail_playback();
        let view = current.view.clone();
        let mut settings = current.settings.clone();
        settings.snapshot.epochs = settings.snapshot.epochs.next_session().unwrap();
        settings.snapshot.session_id = Some(SessionId::new("replacement-source").unwrap());
        settings.snapshot.muted = true;
        settings.playing = false;
        let replacement_epochs = settings.snapshot.epochs;
        let entries = view.lock().unwrap().entries.clone();
        let replacement = Playback::prepare(
            settings,
            entries,
            Arc::new(AtomicBool::new(false)),
            view.clone(),
            Arc::new(Mutex::new(None)),
            &events,
        )
        .unwrap();
        drop(current);
        {
            let view = view.lock().unwrap();
            assert_eq!(view.epochs, Some(replacement_epochs));
            assert!(
                view.active.is_some(),
                "an old source released the replacement's reservation"
            );
        }
        drop(replacement);
        assert!(view.lock().unwrap().active.is_none());
    }
    #[cfg(target_os = "linux")]
    fn staged_streaming_playback() -> (Playback, SyncSender<Event>, Receiver<Event>, usize) {
        let (mut playback, events, receiver) = streaming_playback_frames(8193);
        let mut staged = None;
        for _ in 0..256 {
            playback.step(&events).unwrap();
            let render = playback
                .pipeline
                .as_mut()
                .unwrap()
                .stream_render_mut()
                .unwrap();
            let step = render.pump(None).unwrap();
            if step.produced_frames >= 2 {
                playback.stage_stream().unwrap();
                staged = Some(playback.native_period().header().frame_count as usize);
                break;
            }
        }
        let frames = staged.expect("real SRC output never staged a splittable native period");
        (playback, events, receiver, frames)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_worker_continues_an_actual_partially_submitted_native_period() {
        let (mut playback, events, _receiver, frames) = staged_streaming_playback();
        let source_position = playback.source_position;
        let submitted = playback.stream_position;
        let render = playback
            .pipeline
            .as_mut()
            .unwrap()
            .stream_render_mut()
            .unwrap();
        let accepted = playback
            .output
            .write(render.period(), 0, 7.min(frames - 1))
            .unwrap()
            .unwrap();
        assert!(accepted > 0 && accepted < frames);
        render.accepted(accepted).unwrap();
        playback.stream_position += accepted as u64;
        assert_eq!(render.period_offset(), accepted);
        playback.step(&events).unwrap();
        assert_eq!(playback.source_position, source_position);
        assert_eq!(playback.stream_position, submitted + frames as u64);
        assert!(!playback.pending);
        let render = playback.pipeline.as_ref().unwrap().stream_render().unwrap();
        assert_eq!(render.period_offset(), 0);
        assert_eq!(
            render.output_positions().next_submission - render.output_positions().backend_origin,
            playback.stream_position
        );
        assert_eq!(
            playback.output.progress().unwrap().submitted,
            playback.stream_position
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_target_preserves_an_actual_accepted_prefix_and_silences_the_cached_suffix() {
        let (mut playback, events, receiver, _) = staged_streaming_playback();
        let (before, prefix) = (0..256)
            .find_map(|_| {
                let period = playback.native_period();
                let frames = period.header().frame_count as usize;
                let first = (0..frames).find(|frame| period.integer_sample(frame * 2) != Some(0));
                let last = (0..frames).rfind(|frame| period.integer_sample(frame * 2) != Some(0));
                if let (Some(first), Some(last)) = (first, last)
                    && first < last
                    && playback.pending
                {
                    return Some((period.clone(), first + 1));
                }
                playback.step(&events).unwrap();
                let render = playback
                    .pipeline
                    .as_mut()
                    .unwrap()
                    .stream_render_mut()
                    .unwrap();
                render.pump(None).unwrap();
                playback.stage_stream().unwrap();
                None
            })
            .expect("real SRC never produced a nonzero splittable cached period");
        let frames = before.header().frame_count as usize;
        let accepted = playback.output.write(&before, 0, prefix).unwrap().unwrap();
        assert!(accepted > 0 && accepted < frames);
        assert!((0..accepted * 2).any(|i| before.integer_sample(i) != Some(0)));
        assert!((accepted * 2..frames * 2).any(|i| before.integer_sample(i) != Some(0)));
        playback
            .pipeline
            .as_mut()
            .unwrap()
            .stream_render_mut()
            .unwrap()
            .accepted(accepted)
            .unwrap();
        playback.stream_position += accepted as u64;
        let source = playback.source_position;
        let peak = playback.output_peak;
        assert!(peak.unwrap() > 0.0);
        let positions = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .output_positions();
        receiver.try_iter().for_each(drop);
        let mut zero = playback.settings.clone();
        zero.snapshot.software_gain_zero = true;
        zero.snapshot.gain_ramp_frames = 17;
        playback.apply_settings(zero, &events).unwrap();
        assert_eq!(playback.output_peak, peak);
        assert_eq!(playback.native_period().header(), before.header());
        for i in 0..frames * 2 {
            assert_eq!(
                playback.native_period().integer_sample(i),
                if i < accepted * 2 {
                    before.integer_sample(i)
                } else {
                    Some(0)
                },
                "sample {i}"
            );
        }
        assert_eq!(playback.source_position, source);
        assert_eq!(
            playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .output_positions(),
            positions
        );
        assert!(playback.pending);
        assert!(
            receiver
                .try_iter()
                .all(|e| !matches!(e, Event::Paused { .. } | Event::Started { .. }))
        );
        playback.step(&events).unwrap();
        assert_eq!(playback.source_position, source);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_settings_from_an_old_output_epoch_keep_the_current_period_and_source() {
        let (mut playback, events, _receiver, _) = staged_streaming_playback();
        let before = playback.settings.snapshot.clone();
        let period = playback.native_period().clone();
        let source = playback.source_position;
        let mut stale = playback.settings.clone();
        stale.snapshot.software_gain_zero = true;
        stale.snapshot.epochs.stream -= 1;
        playback.apply_settings(stale, &events).unwrap();
        assert_eq!(playback.settings.snapshot, before);
        assert_eq!(playback.native_period(), &period);
        assert_eq!(playback.source_position, source);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_during_pending_graph_ack_keeps_the_matching_output_epoch_setting() {
        let (mut playback, events, _receiver) = prepared_nonzero_source();
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&playback.settings, &observed);
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        assert_eq!(playback.settings.snapshot.graph_epoch, 0);
        assert_eq!(playback.installed_graph_epoch, 1);
        let epochs = playback.epochs();
        let mut zero = playback.settings.clone();
        zero.snapshot.software_gain_zero = true;
        playback.apply_settings(zero, &events).unwrap();
        assert!(playback.settings.snapshot.software_gain_zero);
        assert_eq!(playback.epochs(), epochs);
        let mut acknowledged = playback.settings.clone();
        acknowledged.snapshot.graph_epoch = 1;
        playback.apply_settings(acknowledged, &events).unwrap();
        assert!(playback.settings.snapshot.software_gain_zero);
        assert_eq!(playback.installed_graph_epoch, 1);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_preserves_paused_replay_then_finishes_the_original_stream_tail() {
        let (mut playback, events, receiver) = resumed_tail_playback();
        let source = playback.source_position;
        let source_epochs = playback.source_epochs;
        let mut paused = playback.settings.clone();
        paused.snapshot.epochs = paused.snapshot.epochs.next_stream().unwrap();
        paused.snapshot.muted = true;
        paused.playing = false;
        playback.reposition(paused, None, &events, true).unwrap();
        let before = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .output_positions();
        let stats = playback
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats();
        let epochs = playback.epochs();
        receiver.try_iter().for_each(drop);
        let mut zero = playback.settings.clone();
        zero.snapshot.software_gain_zero = true;
        playback.apply_settings(zero, &events).unwrap();
        playback.step(&events).unwrap();
        assert_eq!(
            playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .output_positions(),
            before
        );
        assert_eq!(
            playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .stream()
                .stats(),
            stats
        );
        assert_eq!(playback.epochs(), epochs);
        assert_eq!(playback.source_position, source);
        assert!(!playback.settings.playing);
        assert!(receiver.try_iter().next().is_none());
        let mut resume = playback.settings.clone();
        resume.snapshot.muted = false;
        resume.playing = true;
        playback.apply_settings(resume, &events).unwrap();
        let mut observed_period = false;
        for _ in 0..512 {
            let submitted = playback.stream_position;
            let done = playback.step(&events).unwrap();
            let period = playback.native_period();
            if playback.stream_position > submitted {
                observed_period = true;
                assert!(
                    (0..period.header().frame_count as usize * 2)
                        .all(|i| period.integer_sample(i) == Some(0))
                );
            }
            if done {
                break;
            }
        }
        assert!(observed_period);
        assert_eq!(playback.phase, RenderPhase::Finished);
        assert_eq!(playback.source_epochs, source_epochs);
        assert_eq!(playback.source_position, source);
        assert!(playback.settings.snapshot.software_gain_zero);
        assert!(!playback.settings.snapshot.muted);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn zero_queued_during_preparation_is_the_first_submitted_gain() {
        let (playback, _, _) = prepared_nonzero_frames(257);
        let settings = playback.settings.clone();
        let entries = playback.view.lock().unwrap().entries.clone();
        drop(playback);
        let mut worker = Worker::spawn();
        worker.view.lock().unwrap().entries = entries.clone();
        let (gate, reached, release) = preparation_gate();
        worker.send(Action::PausePreparation(gate)).unwrap();
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut zero = settings.clone();
        zero.snapshot.software_gain_zero = true;
        zero.snapshot.muted = false;
        zero.snapshot.gain_ramp_frames = 37;
        worker.send(Action::Settings(zero)).unwrap();
        drop(release);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Drained {
                    epochs,
                    observation,
                    ..
                } => {
                    assert_eq!(epochs, settings.snapshot.epochs);
                    assert_eq!(observation.submitted_frames, 257);
                    assert_eq!(observation.source_position_frames, 257);
                    assert!(observation.input_peak.unwrap() > 0.0);
                    assert_eq!(observation.output_peak, Some(0.0));
                    assert!(observation.modified);
                    break;
                }
                Event::Failed { error, .. } => panic!("zero preparation failed: {error}"),
                Event::Paused { .. } => panic!("zero emitted an unrelated pause"),
                _ => {}
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn raw_and_fixed_render_zero_leave_the_accepted_prefix_and_cursors_intact() {
        for (graph, ramp) in [(false, 0), (false, 4), (true, 0), (true, 4)] {
            let (mut playback, events, receiver) = prepared_nonzero_frames(257);
            if graph {
                let observed = playback.observe().unwrap();
                let (settings, activation, pipeline) =
                    prepare_activation(&playback.settings, &observed);
                playback
                    .activate_graph(settings, activation, pipeline, &events)
                    .unwrap();
                playback.settings.snapshot.graph_epoch = 1;
            }
            playback.settings.snapshot.muted = false;
            playback.settings.snapshot.software_gain_db = 0.0;
            playback.settings.snapshot.gain_ramp_frames = 0;
            playback.set_gain().unwrap();
            assert!(playback.source_packet(&events).unwrap());
            let packet = playback.current.packet.as_ref().unwrap();
            let frames = playback
                .period
                .capacity_frames()
                .min(packet.header().frame_count as usize);
            if let Some(render) = playback.pipeline.as_mut().and_then(|p| p.render_mut()) {
                render.process(packet, 0, frames).unwrap();
            } else {
                for i in 0..frames * 2 {
                    playback
                        .period
                        .set_integer_sample(i, packet.integer_sample(i).unwrap())
                        .unwrap();
                }
                playback
                    .period
                    .set_span(playback.epochs(), playback.sequence, 0, frames as u64)
                    .unwrap();
                playback.gain.process(&mut playback.period).unwrap();
            }
            playback.pending = true;
            playback.pending_kind = PendingPeriodKind::Source;
            let before = playback.native_period().clone();
            assert!(before.integer_sample(frames * 2 - 1).unwrap() > 0);
            let accepted = playback
                .output
                .write(&before, 0, 7.min(frames - 1))
                .unwrap()
                .unwrap();
            playback.period_offset = accepted;
            playback.stream_position += accepted as u64;
            playback.source_position += accepted as u64;
            playback.source_stream_position += accepted as u64;
            let position = playback.source_position;
            let packet_position = playback.current.offset;
            let epochs = playback.epochs();
            receiver.try_iter().for_each(drop);
            playback.settings.snapshot.software_gain_zero = true;
            playback.settings.snapshot.gain_ramp_frames = 17;
            playback.set_gain().unwrap();
            for i in 0..frames * 2 {
                assert_eq!(
                    playback.native_period().integer_sample(i),
                    if i < accepted * 2 {
                        before.integer_sample(i)
                    } else {
                        Some(0)
                    }
                );
            }
            assert_eq!(playback.native_period().header(), before.header());
            assert_eq!(playback.period_offset, accepted);
            assert_eq!(playback.source_position, position);
            assert_eq!(playback.current.offset, packet_position);
            assert_eq!(playback.epochs(), epochs);
            assert!(playback.pending);
            assert!(receiver.try_iter().next().is_none());
            playback.settings.snapshot.software_gain_zero = false;
            playback.settings.snapshot.gain_ramp_frames = ramp;
            playback.set_gain().unwrap();
            assert_eq!(
                playback.native_period().integer_sample(accepted * 2),
                Some(0)
            );
            playback.step(&events).unwrap();
            assert_eq!(playback.source_position, frames as u64);
            assert_eq!(playback.current.offset, frames);
            playback.step(&events).unwrap();
            for (frame, expected) in if ramp == 0 {
                [8192, 8192, 8192, 8192, 8192]
            } else {
                [2048, 4096, 6144, 8192, 8192]
            }
            .into_iter()
            .enumerate()
            {
                assert_eq!(
                    playback.native_period().integer_sample(frame * 2),
                    Some(expected)
                );
                assert_eq!(
                    playback.native_period().integer_sample(frame * 2 + 1),
                    Some(expected)
                );
            }
            assert_eq!(playback.epochs(), epochs);
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn streaming_stop_keeps_a_native_prefix_written_before_source_cancellation() {
        let (mut playback, events, _receiver, frames) = staged_streaming_playback();
        let render = playback
            .pipeline
            .as_mut()
            .unwrap()
            .stream_render_mut()
            .unwrap();
        let accepted = playback
            .output
            .write(render.period(), 0, 7.min(frames - 1))
            .unwrap()
            .unwrap();
        assert!(accepted > 0 && accepted < frames);
        let actual_native = playback.output.progress().unwrap().submitted;
        playback.cancel.store(true, Ordering::Release);
        render
            .accepted(accepted)
            .expect("an irrevocable native write must remain acknowledged after cancellation");
        playback.stream_position += accepted as u64;
        let positions = render.output_positions();
        assert_eq!(
            positions.next_submission - positions.backend_origin,
            actual_native
        );
        assert_eq!(
            render.pump(None).unwrap_err().code,
            ErrorCode::OperationCancelled
        );
        assert!(playback.step(&events).unwrap());
        assert_eq!(
            playback.failure_observation().submitted_frames,
            actual_native
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn queued_stream_transport_cannot_erase_native_progress_after_stop_cancels_source() {
        for transport in 0..3 {
            let (mut current, events, receiver, _) = staged_streaming_playback();
            let activation = if transport == 2 {
                let observation = current.observe().unwrap();
                Some(prepare_activation(&current.settings, &observation))
            } else {
                None
            };
            let graph_hash = current
                .pipeline
                .as_ref()
                .unwrap()
                .preview()
                .graph_hash
                .clone();
            let native = current.output.progress().unwrap().submitted;
            assert!(native > 0);
            let source_epochs = current.epochs();
            let view = current.view.clone();
            let mut pause = current.settings.clone();
            pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
            pause.snapshot.muted = true;
            pause.playing = false;
            let mut stopping = pause.clone();
            stopping.snapshot.epochs = stopping.snapshot.epochs.next_stream().unwrap();
            let request = StopRequest {
                request_id: RequestId::new("cancel-queued-stream-transport").unwrap(),
                settings: stopping,
            };
            current.cancel.store(true, Ordering::Release);
            if let Some((settings, request, pipeline)) = activation {
                current
                    .activate_graph(settings, request, pipeline, &events)
                    .unwrap();
            } else {
                let seek = (transport == 1).then_some(17);
                current
                    .reposition(pause.clone(), seek, &events, true)
                    .expect(
                        "superseded transport must retain the cancelled source until native close",
                    );
            }
            assert_eq!(
                current.pipeline.as_ref().unwrap().preview().graph_hash,
                graph_hash
            );
            assert_eq!(current.epochs(), source_epochs);
            assert_eq!(current.output.progress().unwrap().submitted, native);
            current.apply_settings(pause, &events).unwrap();
            assert_eq!(current.epochs(), source_epochs);
            receiver.try_iter().for_each(drop);
            let mut playback = Some(current);
            let mut stopped = None;
            let mut terminal = None;
            stop_source(
                request,
                &mut playback,
                &mut stopped,
                &mut terminal,
                &view,
                &events,
            )
            .unwrap();
            match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
                Event::Stopped {
                    observation: Some(observation),
                    ..
                } => assert_eq!(observation.submitted_frames, native),
                _ => panic!("queued transport lost the actual stopped observation"),
            }
            assert!(playback.is_none());
        }
    }
    #[cfg(target_os = "linux")]
    fn cancelled_incoming_stream_preserves_installed_source(block: bool) {
        let (mut current, events, receiver) = prepared_nonzero_frames(8193);
        if block {
            let observed = current.observe().unwrap();
            let (settings, activation, pipeline) = prepare_activation(&current.settings, &observed);
            current
                .activate_graph(settings, activation, pipeline, &events)
                .unwrap();
            current.settings.snapshot.graph_epoch = 1;
        }
        current.settings.snapshot.muted = false;
        current.settings.snapshot.software_gain_db = -48.0;
        current.set_gain().unwrap();
        current.step(&events).unwrap();
        let before = current.failure_observation();
        let epochs = current.epochs();
        assert!(before.submitted_frames > 0);
        assert!(
            current
                .pipeline
                .as_ref()
                .is_none_or(|pipeline| pipeline.stream_render().is_none())
        );
        let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":2048});
        let mut middle = port.clone();
        middle["sample_rate_hz"] = serde_json::json!(44100);
        let graph = serde_json::from_value(serde_json::json!({"schema_version":1,"output":"out","nodes":[
            {"id":"in","kind":{"kind":"input"},"format":port},
            {"id":"down","kind":{"kind":"src","input_rate_hz":48000,"preset":"responsive","chunk_frames":64},"format":middle},
            {"id":"up","kind":{"kind":"src","input_rate_hz":44100,"preset":"responsive","chunk_frames":64},"format":port},
            {"id":"out","kind":{"kind":"output"},"format":port}],
            "edges":[{"from":"in","to":"down","from_port":0,"to_port":0},{"from":"down","to":"up","from_port":0,"to_port":0},{"from":"up","to":"out","from_port":0,"to_port":0}]})).unwrap();
        let observed = current.observe().unwrap();
        let (settings, activation, pipeline) =
            prepare_activation_graph(&current.settings, &observed, graph, current.cancel.clone());
        assert!(pipeline.stream_render().is_some());
        current.cancel.store(true, Ordering::Release);
        current
            .activate_graph(settings, activation, pipeline, &events)
            .expect("queued cancelled incoming SRC must leave the installed source for Stop");
        assert_eq!(current.epochs(), epochs);
        assert_eq!(current.failure_observation(), before);
        assert_eq!(
            current.output.progress().unwrap().submitted,
            before.submitted_frames
        );
        assert!(
            current
                .pipeline
                .as_ref()
                .is_none_or(|pipeline| pipeline.stream_render().is_none())
        );
        let view = current.view.clone();
        let mut stop = current.settings.clone();
        stop.snapshot.epochs = stop.snapshot.epochs.next_stream().unwrap();
        receiver.try_iter().for_each(drop);
        let mut playback = Some(current);
        let mut stopped = None;
        let mut terminal = None;
        stop_source(
            StopRequest {
                request_id: RequestId::new("stop-cancelled-incoming-src").unwrap(),
                settings: stop,
            },
            &mut playback,
            &mut stopped,
            &mut terminal,
            &view,
            &events,
        )
        .unwrap();
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::Stopped {
                observation: Some(observation),
                ..
            } => assert_eq!(observation, before),
            _ => panic!("Stop lost the installed native prefix"),
        }
        assert!(playback.is_none());
        assert!(terminal.is_none());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn cancelled_incoming_stream_preserves_integer_source_until_stop() {
        cancelled_incoming_stream_preserves_installed_source(false);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn cancelled_incoming_stream_preserves_block_source_until_stop() {
        cancelled_incoming_stream_preserves_installed_source(true);
    }
    #[cfg(target_os = "linux")]
    fn cancelled_successful_hold_preserves_native_prefix(notify: bool) {
        let (mut current, events, receiver, _) = staged_streaming_playback();
        let before = current.failure_observation();
        assert!(before.submitted_frames > 0);
        let view = current.view.clone();
        let mut pause = current.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        current
            .reposition(pause.clone(), None, &events, notify)
            .unwrap();
        assert_eq!(current.failure_observation().submitted_frames, 0);
        assert_eq!(current.output.progress().unwrap().submitted, 0);
        let mut stop = pause;
        stop.snapshot.epochs = stop.snapshot.epochs.next_stream().unwrap();
        current.cancel.store(true, Ordering::Release);
        receiver.try_iter().for_each(drop);
        let mut playback = Some(current);
        let mut stopped = None;
        let mut terminal = None;
        stop_source(
            StopRequest {
                request_id: RequestId::new("stop-after-successful-stream-hold").unwrap(),
                settings: stop,
            },
            &mut playback,
            &mut stopped,
            &mut terminal,
            &view,
            &events,
        )
        .unwrap();
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::Stopped {
                observation: Some(observation),
                ..
            } => assert_eq!(observation, before),
            _ => panic!("successful hold lost the pre-control native prefix"),
        }
        assert!(playback.is_none());
        assert!(terminal.is_none());
        assert!(view.lock().unwrap().active.is_none());
        assert!(view.lock().unwrap().reserved.is_none());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_after_successful_stream_hold_keeps_native_prefix_before_observe() {
        cancelled_successful_hold_preserves_native_prefix(false);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_after_successful_stream_hold_keeps_native_prefix_after_observe() {
        cancelled_successful_hold_preserves_native_prefix(true);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn failed_step_after_cancelled_hold_clears_the_retained_control_reservation() {
        let (mut current, events, receiver, _) = staged_streaming_playback();
        let before = current.failure_observation();
        let origin = current.epochs();
        assert!(before.submitted_frames > 0);
        let view = current.view.clone();
        {
            let mut view = view.lock().unwrap();
            assert_eq!(view.epochs, Some(origin));
            assert!(view.active.is_some());
            view.reserved = Some(current.current.entry.as_ref().unwrap().id.clone());
        }
        let mut pause = current.settings.clone();
        pause.snapshot.epochs = origin.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        current.reposition(pause, None, &events, true).unwrap();
        assert_ne!(current.epochs(), origin);
        assert_eq!(view.lock().unwrap().epochs, Some(origin));
        current.cancel.store(true, Ordering::Release);
        receiver.try_iter().for_each(drop);
        let mut stopped = None;
        let mut terminal = None;
        // This injects the returned error at retirement, not a fake native stop failure.
        let error = EngineError::new(ErrorCode::OutputFault, "retirement boundary test fault");
        playback_step_failure(&mut current, error, &mut stopped, &mut terminal, &events);
        let (settings, observation, failure) = stopped.unwrap();
        assert_eq!(settings.snapshot.epochs, origin);
        assert_eq!(observation, before);
        assert_eq!(failure, Some(error));
        assert!(terminal.is_some());
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            Event::Failed {
                epochs,
                error: actual,
                observation: Some(observation),
                ..
            } => {
                assert_eq!(epochs, origin);
                assert_eq!(actual, error);
                assert_eq!(observation, before);
            }
            _ => panic!("retirement lost the real held source context"),
        }
        let view = view.lock().unwrap();
        assert!(view.active.is_none());
        assert!(view.reserved.is_none());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn cancelled_new_source_progress_after_hold_cannot_reuse_the_old_control_snapshot() {
        let (mut current, events, _receiver, _) = staged_streaming_playback();
        for _ in 0..128 {
            let positions = current
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .output_positions();
            if positions.next_submission == positions.generated {
                break;
            }
            current.step(&events).unwrap();
        }
        let positions = current
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .output_positions();
        assert_eq!(positions.next_submission, positions.generated);
        assert!(current.stream_position > 0);
        let mut pause = current.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        current.reposition(pause, None, &events, true).unwrap();
        let before = current.source_position;
        current.settings.snapshot.muted = false;
        current.settings.playing = true;
        current.set_gain().unwrap();
        current
            .pipeline
            .as_mut()
            .unwrap()
            .stream_render_mut()
            .unwrap()
            .cancel_after_next_pump();
        assert!(current.step(&events).unwrap());
        let accepted = current
            .pipeline
            .as_ref()
            .unwrap()
            .stream_render()
            .unwrap()
            .stream()
            .stats()
            .source_frames;
        assert!(accepted > before);
        let observation = current.failure_observation();
        assert_eq!(observation.source_position_frames, accepted);
        assert_eq!(observation.submitted_frames, 0);
        assert_eq!(
            current.failure_context(None).snapshot.epochs,
            current.epochs()
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn in_flight_stream_controls_keep_coherent_progress_when_stop_cancels_after_suspend() {
        for seek in [false, true] {
            let (mut current, events, receiver, _) = staged_streaming_playback();
            let view = current.view.clone();
            let before_control =
                Some((current.failure_context(None), current.failure_observation()));
            let source = current.source_position;
            let native = current.output.progress().unwrap().submitted;
            assert!(native > 0);
            let mut pause = current.settings.clone();
            pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
            pause.snapshot.muted = true;
            let counters = current.output.suspend(pause.snapshot.epochs).unwrap();
            assert_eq!(counters.submitted, native);
            current.cancel.store(true, Ordering::Release);
            let error = if seek {
                // Use the same real decoder seek and field updates that precede pipeline reset.
                current.current.seek(17, pause.snapshot.epochs).unwrap();
                current.settings = pause.clone();
                current.source_epochs = pause.snapshot.epochs;
                current.source_position = 17;
                current.stream_position = 0;
                current
                    .pipeline
                    .as_mut()
                    .unwrap()
                    .reset(pause.snapshot.epochs)
                    .map_err(render_error)
                    .unwrap_err()
            } else {
                current
                    .pipeline
                    .as_mut()
                    .unwrap()
                    .stream_render_mut()
                    .unwrap()
                    .hold(
                        counters.submitted,
                        counters.restart_cursor.unwrap(),
                        pause.snapshot.epochs,
                    )
                    .unwrap_err()
            };
            assert_eq!(error.code, ErrorCode::OperationCancelled);
            let mut playback = Some(current);
            let mut stopped = None;
            let mut terminal = None;
            action_failure(
                &mut playback,
                &mut stopped,
                &mut terminal,
                Some(pause.clone()),
                error,
                before_control,
                &events,
            );
            pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
            let stop_epochs = pause.snapshot.epochs;
            let request = StopRequest {
                request_id: RequestId::new("stop-in-flight-stream-hold").unwrap(),
                settings: pause,
            };
            receiver.try_iter().for_each(drop);
            stop_source(
                request,
                &mut playback,
                &mut stopped,
                &mut terminal,
                &view,
                &events,
            )
            .unwrap();
            match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
                Event::Stopped {
                    observation: Some(observation),
                    ..
                } => {
                    assert_eq!(observation.submitted_frames, native);
                    assert_eq!(observation.source_position_frames, source);
                    let streaming = observation.processing.unwrap().streaming.unwrap();
                    assert_eq!(streaming.source_accepted_frames, source);
                    assert!(streaming.source_epochs.stream < stop_epochs.stream);
                }
                _ => panic!("in-flight cancellation lost the actual native prefix"),
            }
            assert!(
                view.lock().unwrap().active.is_none(),
                "retired source left a queue reservation under its pre-control epoch"
            );
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn cancelled_stream_pump_reconciles_the_actual_accepted_source_prefix() {
        let (mut playback, events, _receiver) = streaming_playback_chunk(8193, 1024, "quality");
        let render = playback
            .pipeline
            .as_mut()
            .unwrap()
            .stream_render_mut()
            .unwrap();
        let before = render.stream().stats().source_frames;
        render.cancel_after_next_pump();
        // DSP succeeds first; the bridge then rejects publication using the real source token.
        assert!(playback.step(&events).unwrap());
        let render = playback.pipeline.as_ref().unwrap().stream_render().unwrap();
        let accepted = render.stream().stats().source_frames;
        assert!(playback.cancel.load(Ordering::Acquire));
        assert!(
            accepted > before,
            "the actual DSP call accepted no source prefix"
        );
        assert_eq!(
            playback.source_position, accepted,
            "failed pump accepted source without advancing its real packet frontier"
        );
        assert_eq!(
            playback.failure_observation().source_position_frames,
            accepted
        );
        assert_eq!(render.output_positions().generated, 0);
        assert_eq!(playback.failure_observation().submitted_frames, 0);
    }
    #[cfg(target_os = "linux")]
    fn prepared_nonzero_source() -> (Playback, SyncSender<Event>, Receiver<Event>) {
        prepared_nonzero_frames(65)
    }
    #[cfg(target_os = "linux")]
    fn prepared_nonzero_frames(frames: usize) -> (Playback, SyncSender<Event>, Receiver<Event>) {
        let (worker, mut settings, _, _) = drain_proof(1);
        let mut entries = worker.view.lock().unwrap().entries.clone();
        drop(worker);
        let mut bytes = std::fs::read(&entries[0].path).unwrap();
        bytes.resize(44 + frames * 4, 0);
        bytes[4..8].copy_from_slice(&((36 + frames * 4) as u32).to_le_bytes());
        bytes[40..44].copy_from_slice(&((frames * 4) as u32).to_le_bytes());
        for sample in bytes[44..].as_chunks_mut::<2>().0 {
            sample.copy_from_slice(&8192_i16.to_le_bytes());
        }
        let source = std::path::Path::new(&entries[0].path).with_file_name("nonzero.wav");
        std::fs::write(&source, bytes).unwrap();
        entries[0].path = source.to_str().unwrap().into();
        settings.snapshot.muted = true;
        let view = Arc::new(Mutex::new(PlanView::default()));
        view.lock().unwrap().entries = entries.clone();
        let (events, receiver) = mpsc::sync_channel(1024);
        let playback = Playback::prepare(
            settings,
            entries,
            Arc::new(AtomicBool::new(false)),
            view,
            Arc::new(Mutex::new(None)),
            &events,
        )
        .unwrap();
        (playback, events, receiver)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_refreshes_real_paused_facts() {
        for position in [None, Some(17)] {
            let (mut playback, events, receiver) = prepared_nonzero_source();
            let actual = playback.output.observation().unwrap();
            playback.backend.measurement_boundary = "stale cached test boundary";
            let mut pause = playback.settings.clone();
            pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
            pause.snapshot.transport = TransportState::Paused;
            pause.snapshot.muted = true;
            pause.playing = false;
            receiver.try_iter().for_each(drop);
            playback
                .reposition(pause.clone(), position, &events, true)
                .unwrap();
            let observation = receiver
                .try_iter()
                .find_map(|event| match event {
                    Event::Paused {
                        epochs,
                        observation,
                    } => {
                        assert_eq!(epochs, pause.snapshot.epochs);
                        Some(observation)
                    }
                    _ => None,
                })
                .unwrap();
            assert_eq!(observation.backend.as_ref(), Some(&actual));
            assert_eq!(observation.source_position_frames, position.unwrap_or(0));
            assert_eq!(observation.submitted_frames, 0);
            assert!(playback.settings.snapshot.muted);
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_refreshes_real_stream_hold_facts() {
        let (mut playback, events, receiver) = streaming_playback();
        let actual = playback.output.observation().unwrap();
        playback.backend.measurement_boundary = "stale stream test boundary";
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.transport = TransportState::Paused;
        pause.snapshot.muted = true;
        pause.playing = false;
        receiver.try_iter().for_each(drop);
        playback.reposition(pause, None, &events, true).unwrap();
        let observation = receiver
            .try_iter()
            .find_map(|event| match event {
                Event::Paused { observation, .. } => Some(observation),
                _ => None,
            })
            .unwrap();
        assert_eq!(observation.backend.as_ref(), Some(&actual));
        assert_eq!(observation.submitted_frames, 0);
        assert!(playback.settings.snapshot.muted);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_refreshes_real_activation_facts() {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        let observed = playback.observe().unwrap();
        let actual = observed.backend.clone();
        let (settings, activation, pipeline) = prepare_activation(&playback.settings, &observed);
        let hash = pipeline.preview().graph_hash.clone();
        playback.backend.measurement_boundary = "stale activation test boundary";
        receiver.try_iter().for_each(drop);
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        let ack = receiver
            .try_iter()
            .find_map(|event| match event {
                Event::GraphActivated(ack) => Some(ack),
                _ => None,
            })
            .unwrap();
        assert_eq!(ack.observation.backend, actual);
        assert_eq!(ack.graph_hash, hash);
        assert_eq!(ack.observation.submitted_frames, 0);
        assert!(playback.settings.snapshot.muted);
    }
    #[cfg(target_os = "linux")]
    mod activity_tests {
        use super::*;
        use earph_core::session::{OutputActivity, OutputActivityBoundary, OutputActivityScope};

        // Only diagnostic samples are controlled. Decoding, output, and cleanup use real ALSA null.
        struct ControlledActivity {
            inner: Box<dyn Output>,
            sample: Arc<Mutex<OutputActivity>>,
            close_failure: bool,
            cancel_during_close: Option<Arc<AtomicBool>>,
        }
        impl Output for ControlledActivity {
            fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
                self.inner.audio_storage_bytes()
            }
            fn is_running(&self) -> bool {
                self.inner.is_running()
            }
            fn observation(&self) -> Result<BackendObservation, EngineError> {
                self.inner.observation()
            }
            fn activity(&self) -> Option<OutputActivity> {
                Some(*self.sample.lock().unwrap())
            }
            fn write(
                &mut self,
                block: &PcmBlock,
                offset: usize,
                frames: usize,
            ) -> Result<Option<usize>, EngineError> {
                self.inner.write(block, offset, frames)
            }
            fn progress(&mut self) -> Result<Counters, EngineError> {
                self.inner.progress()
            }
            fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
                let counters = self.inner.suspend(epochs)?;
                self.sample.lock().unwrap().native_open_successes += 1;
                Ok(counters)
            }
            fn finish(&mut self) -> Result<Option<Counters>, EngineError> {
                let result = self.inner.finish()?;
                if result.is_some() {
                    self.sample.lock().unwrap().close_confirmed = true;
                }
                Ok(result)
            }
            fn wait(&mut self) -> Result<(), EngineError> {
                self.inner.wait()
            }
            fn stop(&mut self) -> Result<(), EngineError> {
                self.inner.stop()?;
                if let Some(cancel) = &self.cancel_during_close {
                    cancel.store(true, Ordering::Release);
                }
                if self.close_failure {
                    return Err(EngineError::new(
                        ErrorCode::OutputFault,
                        "controlled close failure",
                    ));
                }
                self.sample.lock().unwrap().close_confirmed = true;
                Ok(())
            }
        }
        fn attach(
            mut playback: Playback,
            close_failure: bool,
            cancel_during_close: bool,
        ) -> (Playback, Arc<Mutex<OutputActivity>>) {
            let sample = Arc::new(Mutex::new(OutputActivity {
                scope: OutputActivityScope::OutputInstanceAcrossNativeReopens,
                measurement_boundary: OutputActivityBoundary::CoreAudioSdkEntryCalls,
                origin_epochs: playback.epochs(),
                native_open_successes: 1,
                sdk_write_attempts: 4,
                accepted_frames: 29,
                sdk_start_attempts: 1,
                counter_overflow: false,
                close_confirmed: false,
            }));
            playback.output = Box::new(ControlledActivity {
                inner: playback.output,
                sample: sample.clone(),
                close_failure,
                cancel_during_close: cancel_during_close.then(|| playback.cancel.clone()),
            });
            (playback, sample)
        }
        #[test]
        fn cancellation_refreshes_activity_without_replacing_the_retained_source_prefix() {
            let (current, _, _, _) = staged_streaming_playback();
            let (mut current, sample) = attach(current, false, false);
            let before = current.failure_observation();
            current.control_stop_observation = Some((current.epochs(), before.clone()));
            current.stream_position = 0;
            current.cancel.store(true, Ordering::Release);
            sample.lock().unwrap().native_open_successes = 2;
            let mut expected = before;
            expected.output_activity = Some(*sample.lock().unwrap());
            assert_eq!(current.failure_observation(), expected);
        }
        #[test]
        fn natural_completion_retains_the_closed_activity_sample() {
            let (current, events, _receiver) = streaming_playback();
            let (mut current, sample) = attach(current, false, false);
            for _ in 0..1024 {
                if current.step(&events).unwrap() {
                    break;
                }
            }
            assert_eq!(current.phase, RenderPhase::Finished);
            let terminal = SourceTerminal::completed_from_playback(&current).unwrap();
            let SourceTerminalOutcome::Completed(observation) = terminal.outcome else {
                panic!("completed source required")
            };
            assert!(observation.output_activity.unwrap().close_confirmed);
            assert_eq!(observation.output_activity, Some(*sample.lock().unwrap()));
        }
        #[test]
        fn terminal_paths_sample_activity_after_close_and_preserve_failures() {
            for close_failure in [false, true] {
                for path in ["stop", "action", "step", "cancel_step", "cancel"] {
                    let (current, events, receiver, _) = staged_streaming_playback();
                    let (current, sample) = attach(current, close_failure, false);
                    let before = current.failure_observation();
                    let settings = current.failure_context(None);
                    let view = current.view.clone();
                    let error =
                        EngineError::new(ErrorCode::InternalFault, "controlled source failure");
                    let mut stopped = None;
                    let mut terminal = None;
                    let mut playback = Some(current);
                    receiver.try_iter().for_each(drop);
                    match path {
                        "stop" => stop_source(
                            StopRequest {
                                request_id: RequestId::new("activity-stop").unwrap(),
                                settings: settings.clone(),
                            },
                            &mut playback,
                            &mut stopped,
                            &mut terminal,
                            &view,
                            &events,
                        )
                        .unwrap(),
                        "step" => playback_step_failure(
                            playback.as_mut().unwrap(),
                            error,
                            &mut stopped,
                            &mut terminal,
                            &events,
                        ),
                        "cancel_step" => {
                            playback
                                .as_mut()
                                .unwrap()
                                .cancel
                                .store(true, Ordering::Release);
                            playback_step_failure(
                                playback.as_mut().unwrap(),
                                error,
                                &mut stopped,
                                &mut terminal,
                                &events,
                            );
                        }
                        "cancel" => {
                            playback
                                .as_mut()
                                .unwrap()
                                .cancel
                                .store(true, Ordering::Release);
                            action_failure(
                                &mut playback,
                                &mut stopped,
                                &mut terminal,
                                Some(settings.clone()),
                                EngineError::new(
                                    ErrorCode::OperationCancelled,
                                    "controlled cancellation",
                                ),
                                Some((settings, before.clone())),
                                &events,
                            );
                        }
                        _ => action_failure(
                            &mut playback,
                            &mut stopped,
                            &mut terminal,
                            Some(settings),
                            error,
                            None,
                            &events,
                        ),
                    }
                    let observation = if let Some((_, observation, _)) = stopped {
                        observation
                    } else {
                        receiver.try_iter().find_map(|event| match event {
                            Event::Stopped { observation, .. } | Event::Failed { observation, .. } | Event::StopFailed { observation, .. } => observation,
                            _ => None,
                        }).unwrap_or_else(|| panic!("missing terminal observation: {path}, close_failure={close_failure}"))
                    };
                    let activity = observation.output_activity.unwrap();
                    assert_eq!(activity.close_confirmed, !close_failure, "{path}");
                    assert_eq!(activity, *sample.lock().unwrap(), "{path}");
                    let mut expected = before;
                    expected.output_activity = Some(activity);
                    assert_eq!(observation, expected, "{path}");
                }
            }
        }
        #[test]
        fn sampling_after_close_keeps_the_original_cancellation_admission_point() {
            let (current, events, receiver, _) = staged_streaming_playback();
            let (mut current, sample) = attach(current, false, true);
            let mut stopped = None;
            let mut terminal = None;
            let error = EngineError::new(ErrorCode::InternalFault, "controlled source failure");
            receiver.try_iter().for_each(drop);
            assert!(!current.cancel.load(Ordering::Acquire));
            playback_step_failure(&mut current, error, &mut stopped, &mut terminal, &events);
            assert!(current.cancel.load(Ordering::Acquire));
            assert!(sample.lock().unwrap().close_confirmed);
            assert!(
                stopped.is_none(),
                "a cancellation arriving during close changed cache admission"
            );
            let event = receiver.try_iter().next().unwrap();
            assert!(
                matches!(event, Event::Failed { error: actual, observation: Some(observation), .. }
                if actual == error && observation.output_activity.unwrap().close_confirmed)
            );
        }
    }
    // These controlled observations model a reopen boundary, not native hardware changes.
    #[cfg(target_os = "linux")]
    struct PostSuspendObservation {
        inner: Box<dyn Output>,
        failure: Option<EngineError>,
        suspended: bool,
        stopped: Arc<AtomicBool>,
    }
    #[cfg(target_os = "linux")]
    impl Output for PostSuspendObservation {
        fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
            self.inner.audio_storage_bytes()
        }
        fn is_running(&self) -> bool {
            self.inner.is_running()
        }
        fn observation(&self) -> Result<BackendObservation, EngineError> {
            let mut observation = self.inner.observation()?;
            if self.suspended {
                if let Some(error) = self.failure {
                    return Err(error);
                }
                observation.measurement_boundary = "controlled post-suspend observation";
            }
            Ok(observation)
        }
        fn write(
            &mut self,
            block: &PcmBlock,
            offset: usize,
            frames: usize,
        ) -> Result<Option<usize>, EngineError> {
            self.inner.write(block, offset, frames)
        }
        fn progress(&mut self) -> Result<Counters, EngineError> {
            self.inner.progress()
        }
        fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
            let counters = self.inner.suspend(epochs)?;
            self.suspended = true;
            Ok(counters)
        }
        fn finish(&mut self) -> Result<Option<Counters>, EngineError> {
            self.inner.finish()
        }
        fn wait(&mut self) -> Result<(), EngineError> {
            self.inner.wait()
        }
        fn stop(&mut self) -> Result<(), EngineError> {
            self.inner.stop()?;
            self.stopped.store(true, Ordering::Release);
            Ok(())
        }
    }
    #[cfg(target_os = "linux")]
    fn controlled_post_suspend_observation(
        mut playback: Playback,
        failure: Option<EngineError>,
    ) -> (Playback, Arc<AtomicBool>) {
        let stopped = Arc::new(AtomicBool::new(false));
        playback.output = Box::new(PostSuspendObservation {
            inner: playback.output,
            failure,
            suspended: false,
            stopped: stopped.clone(),
        });
        (playback, stopped)
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_refuses_changed_prepared_facts() {
        post_suspend_activation_failure(None, ErrorCode::StaleSession);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_failure_closes_without_success_ack() {
        post_suspend_activation_failure(
            Some(EngineError::new(
                ErrorCode::OutputFault,
                "controlled post-suspend observation failure",
            )),
            ErrorCode::OutputFault,
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn backend_observation_after_suspend_cancel_keeps_source_origin_and_newer_reservation() {
        let (current, events, receiver, _) = staged_streaming_playback();
        let before = current.failure_observation();
        let origin = current.settings.snapshot.epochs;
        let view = current.view.clone();
        let mut pause = current.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.muted = true;
        pause.playing = false;
        let error = EngineError::new(
            ErrorCode::OperationCancelled,
            "controlled post-suspend cancellation",
        );
        let (mut current, closed) = controlled_post_suspend_observation(current, Some(error));
        receiver.try_iter().for_each(drop);
        let error = current
            .reposition(pause.clone(), None, &events, true)
            .unwrap_err();
        assert!(
            receiver.try_iter().next().is_none(),
            "failed refresh emitted Paused"
        );
        current.cancel.store(true, Ordering::Release);
        let mut next = current.settings.clone();
        next.snapshot.epochs = next.snapshot.epochs.next_session().unwrap();
        next.snapshot.session_id = Some(SessionId::new("newer-refresh-source").unwrap());
        next.snapshot.muted = true;
        next.playing = false;
        let next_epochs = next.snapshot.epochs;
        let entries = view.lock().unwrap().entries.clone();
        let replacement = Playback::prepare(
            next,
            entries,
            Arc::new(AtomicBool::new(false)),
            view.clone(),
            Arc::new(Mutex::new(None)),
            &events,
        )
        .unwrap();
        receiver.try_iter().for_each(drop);
        let mut playback = Some(current);
        let mut stopped = None;
        let mut terminal = None;
        action_failure(
            &mut playback,
            &mut stopped,
            &mut terminal,
            Some(pause),
            error,
            None,
            &events,
        );
        let (settings, observation, failure) = stopped.unwrap();
        assert_eq!(settings.snapshot.epochs, origin);
        assert_eq!(observation, before);
        assert_eq!(failure, None);
        assert!(closed.load(Ordering::Acquire));
        assert!(terminal.is_none());
        assert!(receiver.try_iter().next().is_none());
        assert_eq!(view.lock().unwrap().epochs, Some(next_epochs));
        assert!(view.lock().unwrap().active.is_some());
        drop(replacement);
        assert!(view.lock().unwrap().active.is_none());
    }
    #[cfg(target_os = "linux")]
    fn post_suspend_activation_failure(failure: Option<EngineError>, expected: ErrorCode) {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&playback.settings, &observed);
        let preview = pipeline.preview().clone();
        let view = playback.view.clone();
        let (mut playback, closed) = controlled_post_suspend_observation(playback, failure);
        receiver.try_iter().for_each(drop);
        let error = playback
            .activate_graph(settings.clone(), activation, pipeline, &events)
            .unwrap_err();
        assert_eq!(error.code, expected);
        assert_eq!(playback.installed_graph_epoch, 0);
        let mut playback = Some(playback);
        let mut stopped = None;
        let mut terminal = None;
        action_failure(
            &mut playback,
            &mut stopped,
            &mut terminal,
            Some(settings.clone()),
            error,
            None,
            &events,
        );
        assert!(playback.is_none());
        assert!(
            closed.load(Ordering::Acquire),
            "the real output was not stopped"
        );
        assert!(view.lock().unwrap().active.is_none());
        assert!(view.lock().unwrap().reserved.is_none());
        let failures: Vec<_> = receiver.try_iter().collect();
        assert_eq!(
            failures.len(),
            1,
            "failed activation emitted another acknowledgment"
        );
        match &failures[0] {
            Event::Failed {
                epochs,
                error,
                token: Some(token),
                ..
            } => {
                assert_eq!(*epochs, settings.snapshot.epochs);
                assert_eq!(error.code, expected);
                assert_eq!(token.origin_epochs, settings.snapshot.epochs);
            }
            _ => panic!("failed refresh lost its terminal failure"),
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&preview) > 1 && std::time::Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(
            Arc::strong_count(&preview),
            1,
            "incoming graph did not retire"
        );
        assert!(terminal.is_some());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn paused_graph_replacement_clears_peaks_from_actual_nonzero_output() {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&playback.settings, &observed);
        let first_hash = pipeline.preview().graph_hash.clone();
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        playback.settings.snapshot.graph_epoch = 1;
        playback.settings.snapshot.muted = false;
        playback.set_gain().unwrap();
        assert!(!playback.step(&events).unwrap());
        assert!(playback.input_peak.unwrap() > 0.0);
        assert!(playback.output_peak.unwrap() > 0.0);
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.transport = TransportState::Paused;
        pause.snapshot.muted = true;
        pause.playing = false;
        playback.reposition(pause, None, &events, true).unwrap();
        let mut next = playback.settings.clone();
        next.snapshot.software_gain_db = -48.0;
        let observed = playback.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&next, &observed);
        let next_hash = pipeline.preview().graph_hash.clone();
        assert_ne!(first_hash, next_hash);
        receiver.try_iter().for_each(drop);
        playback
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        let acknowledgement = receiver
            .try_iter()
            .find_map(|event| match event {
                Event::GraphActivated(value) => Some(value),
                _ => None,
            })
            .unwrap();
        assert_eq!(acknowledgement.observation.submitted_frames, 0);
        assert_eq!(
            acknowledgement.observation.processing.unwrap().graph_hash,
            next_hash
        );
        assert_eq!(acknowledgement.observation.input_peak, None);
        assert_eq!(acknowledgement.observation.output_peak, None);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn native_suspend_failure_survives_queued_mute_generations_until_acknowledged() {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        playback.output.stop().unwrap();
        let mut pause = playback.settings.clone();
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.transport = TransportState::Paused;
        let error = playback
            .reposition(pause.clone(), None, &events, true)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputFault);
        assert_ne!(playback.epochs(), pause.snapshot.epochs);
        receiver.try_iter().for_each(drop);
        let mut terminal =
            SourceTerminal::failed(&pause, error, Some(playback.failure_observation())).unwrap();
        terminal.report(&events).unwrap();
        let mut mute = pause.clone();
        mute.snapshot.muted = true;
        for _ in 0..2 {
            mute.snapshot.epochs = mute.snapshot.epochs.next_stream().unwrap();
            assert!(terminal.reconcile(&mute, &events, false).unwrap());
        }
        for expected in 1..=3 {
            match receiver.try_recv().unwrap() {
                Event::Failed {
                    epochs,
                    error: failure,
                    token: Some(token),
                    observation: Some(_),
                } => {
                    assert_eq!(epochs.stream, expected);
                    assert_eq!(failure.code, ErrorCode::OutputFault);
                    assert_eq!(token.origin_epochs, pause.snapshot.epochs);
                }
                _ => panic!("suspend failure lost its source identity"),
            }
        }
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn a_queued_graph_activation_keeps_the_underlying_source_failure() {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        let settings = playback.settings.clone();
        let observed = playback.observe().unwrap();
        let (next, activation, pipeline) = prepare_activation(&settings, &observed);
        playback.output.stop().unwrap();
        let error = playback
            .reposition(settings.clone(), None, &events, true)
            .unwrap_err();
        receiver.try_iter().for_each(drop);
        let mut terminal =
            SourceTerminal::failed(&settings, error, Some(playback.failure_observation())).unwrap();
        terminal.report(&events).unwrap();
        assert!(
            terminal
                .activation_terminal(&next, &activation, &events)
                .unwrap()
        );
        let failures: Vec<_> = receiver.try_iter().collect();
        assert_eq!(failures.len(), 2);
        for (index, event) in failures.into_iter().enumerate() {
            match event {
                Event::Failed {
                    epochs,
                    error: actual,
                    token: Some(token),
                    ..
                } => {
                    assert_eq!(epochs.stream, index as u64);
                    assert_eq!(actual.code, ErrorCode::OutputFault);
                    assert_eq!(token.origin_epochs, settings.snapshot.epochs);
                }
                _ => panic!("queued graph erased the actual source fault"),
            }
        }
        drop(pipeline);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn retained_failure_is_reissued_and_its_old_ack_cannot_clear_a_new_source() {
        let (mut worker, mut settings, _, _) = drain_proof(1);
        let valid_entries = worker.view.lock().unwrap().entries.clone();
        let mut missing_entries = valid_entries.clone();
        missing_entries[0].path.push_str(".missing");
        settings.snapshot.epochs.session += 1;
        settings.snapshot.session_id = Some(SessionId::new("failed-source").unwrap());
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries: missing_entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (failure_token, failure_code) = loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Failed {
                    token: Some(token),
                    error,
                    ..
                } => break (token, error.code),
                Event::Item { .. } => {}
                _ => panic!("missing source did not retain its failure"),
            }
        };
        settings.snapshot.muted = true;
        for expected in [1, 2] {
            settings.snapshot.epochs.stream = expected;
            worker.send(Action::Settings(settings.clone())).unwrap();
            assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
                Ok(Event::Failed { epochs, error, token: Some(_), .. })
                if epochs.stream == expected && error.code == failure_code));
        }
        worker
            .send(Action::AcknowledgeSourceTerminal(failure_token.clone()))
            .unwrap();
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings.clone())).unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        settings.snapshot.epochs = Epochs {
            session: 3,
            stream: 0,
        };
        settings.snapshot.session_id = Some(SessionId::new("replacement-after-failure").unwrap());
        settings.snapshot.muted = false;
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries: valid_entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Drained { .. } => break,
                Event::Failed { error, .. } => panic!("replacement failed: {error:?}"),
                _ => {}
            }
        }
        worker
            .send(Action::AcknowledgeSourceTerminal(failure_token))
            .unwrap();
        settings.snapshot.muted = true;
        settings.snapshot.epochs.stream += 1;
        worker.send(Action::Settings(settings.clone())).unwrap();
        assert_eq!(next_drain(&worker).0, settings.snapshot.epochs);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn old_settings_cannot_relabel_a_fault_after_graph_activation() {
        let (mut playback, events, receiver) = prepared_nonzero_source();
        let observed = playback.observe().unwrap();
        let (old_settings, activation, pipeline) =
            prepare_activation(&playback.settings, &observed);
        playback
            .activate_graph(old_settings.clone(), activation, pipeline, &events)
            .unwrap();
        playback
            .apply_settings(old_settings.clone(), &events)
            .unwrap();
        assert_eq!(playback.settings.snapshot.graph_epoch, 0);
        playback.output.stop().unwrap();
        let mut pause = old_settings;
        pause.snapshot.epochs = pause.snapshot.epochs.next_stream().unwrap();
        pause.snapshot.transport = TransportState::Paused;
        let error = playback
            .reposition(pause.clone(), None, &events, true)
            .unwrap_err();
        let context = playback.failure_context(Some(pause));
        let mut terminal =
            SourceTerminal::failed(&context, error, Some(playback.failure_observation())).unwrap();
        terminal.report(&events).unwrap();
        let mut saw_activation = false;
        for event in receiver.try_iter() {
            match event {
                Event::GraphActivated(_) => saw_activation = true,
                Event::Failed {
                    token: Some(token), ..
                } => {
                    assert!(saw_activation);
                    assert_eq!(token.graph_epoch, 1);
                    return;
                }
                _ => {}
            }
        }
        panic!("native failure was not published");
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_preserves_an_unacknowledged_native_fault_for_the_same_source() {
        let name =
            "worker::tests::stop_preserves_an_unacknowledged_native_fault_for_the_same_source";
        if std::env::var("EARPH_STOP_FAULT_TEST_CHILD").as_deref() != Ok(name) {
            let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/stop-native-fault")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
            std::fs::create_dir_all(&directory).unwrap();
            let config = directory.join("alsa.conf");
            std::fs::write(&config,"pcm.null { type null }\npcm.earph_fault { type file slave.pcm \"null\" file \"/dev/full\" format \"raw\" }\n").unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--nocapture"])
                .env("EARPH_STOP_FAULT_TEST_CHILD", name)
                .env("ALSA_CONFIG_PATH", config)
                .output()
                .unwrap();
            std::fs::write(directory.join("stdout.log"), &result.stdout).unwrap();
            std::fs::write(directory.join("stderr.log"), &result.stderr).unwrap();
            assert!(
                result.status.success(),
                "{} / {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            let diagnostic = String::from_utf8_lossy(&result.stderr);
            assert!(diagnostic.contains("/dev/full write failed"));
            assert!(diagnostic.contains("\"code\":\"OUTPUT_FAULT\""));
            return;
        }
        let (mut worker, mut settings, _, _) = drain_proof(1);
        let mut entries = worker.view.lock().unwrap().entries.clone();
        let path = std::path::Path::new(&entries[0].path).with_file_name("native-fault.wav");
        let mut bytes = std::fs::read(&entries[0].path).unwrap()[..44].to_vec();
        bytes[4..8].copy_from_slice(&(36_u32 + 8193 * 4).to_le_bytes());
        bytes[40..44].copy_from_slice(&(8193_u32 * 4).to_le_bytes());
        bytes.resize(44 + 8193 * 4, 0);
        std::fs::write(&path, bytes).unwrap();
        entries[0].path = path.to_str().unwrap().into();
        settings.snapshot.epochs.session += 1;
        settings.snapshot.session_id = Some(SessionId::new("fault-before-stop").unwrap());
        settings.snapshot.output.as_mut().unwrap().endpoint = "earph_fault".into();
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries,
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let token = loop {
            match worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                Event::Failed {
                    error,
                    token: Some(token),
                    ..
                } => {
                    assert_eq!(error.code, ErrorCode::OutputFault);
                    break token;
                }
                Event::Drained { .. } => panic!("full device unexpectedly completed"),
                _ => {}
            }
        };
        for _ in 0..2 {
            settings.snapshot.muted = false;
            worker.send(Action::Settings(settings.clone())).unwrap();
            settings.snapshot.muted = true;
            settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
            worker.send(Action::Settings(settings.clone())).unwrap();
            assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
                Ok(Event::Failed {token:Some(actual),error,..}) if actual==token && error.code==ErrorCode::OutputFault));
        }
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        settings.snapshot.muted = true;
        let request_id = RequestId::new("stop-after-native-fault").unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        worker
            .send(Action::Stop(StopRequest {
                request_id,
                settings: settings.clone(),
            }))
            .unwrap();
        assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
            Ok(Event::StopFailed { error,token:actual, .. }) if error.code==ErrorCode::OutputFault && actual==token));
        let mut wrong = token.clone();
        wrong.final_entry_id = QueueEntryId::new("other-entry").unwrap();
        worker
            .send(Action::AcknowledgeSourceTerminal(wrong))
            .unwrap();
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        let request_id = RequestId::new("retry-stop-after-native-fault").unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        worker
            .send(Action::Stop(StopRequest {
                request_id,
                settings: settings.clone(),
            }))
            .unwrap();
        assert!(matches!(worker.events.recv_timeout(Duration::from_secs(5)),
            Ok(Event::StopFailed {token:actual,..}) if actual==token));
        worker
            .send(Action::AcknowledgeSourceTerminal(token))
            .unwrap();
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        let request_id = RequestId::new("stop-after-fault-ack").unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        worker
            .send(Action::Stop(StopRequest {
                request_id,
                settings,
            }))
            .unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(5)),
            Ok(Event::Stopped { .. })
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn genuine_preparation_error_survives_stop_with_its_cancellation_flag_set() {
        let (mut worker, mut settings, _, _) = drain_proof(1);
        let mut entries = worker.view.lock().unwrap().entries.clone();
        entries[0].path.push_str(".missing");
        settings.snapshot.epochs.session += 1;
        settings.snapshot.session_id = Some(SessionId::new("failed-prepare-before-stop").unwrap());
        let cancel = Arc::new(AtomicBool::new(false));
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries,
                cancel: cancel.clone(),
            })
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Event::Failed { error, .. } = worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap()
            {
                assert_eq!(error.code, ErrorCode::AssetInvalid);
                break;
            }
        }
        let request_id = RequestId::new("stop-genuine-prepare-error").unwrap();
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        worker
            .send(Action::Stop(StopRequest {
                request_id: request_id.clone(),
                settings,
            }))
            .unwrap();
        assert!(cancel.load(Ordering::Acquire));
        assert!(
            matches!(worker.events.recv_timeout(Duration::from_secs(5)).unwrap(),Event::StopFailed{request,error:EngineError{code:ErrorCode::AssetInvalid,..},..} if request.request_id==request_id)
        );
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_after_preparation_check_preserves_the_genuine_error_reported_after_cancellation() {
        let (settings, queue) = local_source_fixture(1);
        let mut snapshot = queue.snapshot();
        snapshot.entries[0].path.push_str(".missing");
        assert!(!Path::new(&snapshot.entries[0].path).exists());
        let entry = snapshot.entries[0].id.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut worker = Worker::spawn();
        let (gate, reached, release) = preparation_gate();
        worker.view.lock().unwrap().publish(snapshot.clone());
        worker.send(Action::PausePreparation(gate)).unwrap();
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries: snapshot.entries,
                cancel: cancel.clone(),
            })
            .unwrap();
        reached.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!cancel.load(Ordering::Acquire));
        let request_id = RequestId::new("stop-during-source-open").unwrap();
        let mut stopped = settings.clone();
        stopped.snapshot.epochs = stopped.snapshot.epochs.next_stream().unwrap();
        stopped.snapshot.operation = Some(request_id.clone());
        let expected = StopRequest {
            request_id,
            settings: stopped,
        };
        worker.send(Action::Stop(expected.clone())).unwrap();
        assert!(cancel.load(Ordering::Acquire));
        drop(release);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut failure_token = None;
        let mut saw_item = false;
        loop {
            let event = worker
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            assert!(cancel.load(Ordering::Acquire));
            match event {
                Event::Item { epochs, id, status } => {
                    assert_eq!(epochs, settings.snapshot.epochs);
                    assert_eq!(id, entry);
                    assert_eq!(
                        status,
                        PreparationStatus::Failed {
                            code: ErrorCode::AssetInvalid
                        }
                    );
                    saw_item = true;
                }
                Event::Failed {
                    epochs,
                    token,
                    error,
                    observation,
                } => {
                    assert!(saw_item);
                    assert_eq!(epochs, settings.snapshot.epochs);
                    assert_eq!(error.code, ErrorCode::AssetInvalid);
                    assert!(observation.is_none());
                    let token = token.unwrap();
                    assert_eq!(token.origin_epochs, settings.snapshot.epochs);
                    assert_eq!(token.final_entry_id, entry);
                    failure_token = Some(token);
                }
                Event::StopFailed {
                    request,
                    token,
                    error,
                    observation,
                } => {
                    assert_eq!(request.request_id, expected.request_id);
                    assert_eq!(request.settings.snapshot, expected.settings.snapshot);
                    assert!(request.matches_source(&token));
                    assert_eq!(error.code, ErrorCode::AssetInvalid);
                    assert!(observation.is_none());
                    assert_eq!(Some(token), failure_token);
                    break;
                }
                _ => panic!("cancelled source preparation lost its genuine failure"),
            }
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_binds_a_cancelled_native_fault_without_relabeling_the_installed_graph() {
        let (mut current, events, receiver) = prepared_nonzero_source();
        let captured = current.settings.clone();
        let observed = current.observe().unwrap();
        let (settings, activation, pipeline) = prepare_activation(&captured, &observed);
        current
            .activate_graph(settings, activation, pipeline, &events)
            .unwrap();
        current.output.stop().unwrap();
        let error = current
            .reposition(current.settings.clone(), None, &events, true)
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::OutputFault);
        let context = current.failure_context(None);
        assert_eq!(context.snapshot.graph_epoch, 1);
        let native_observation = current.failure_observation();
        let mut settings = captured;
        settings.snapshot.epochs = current.epochs().next_stream().unwrap();
        settings.snapshot.muted = true;
        let request_id = RequestId::new("stop-cancelled-native-fault").unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        let request = StopRequest {
            request_id,
            settings,
        };
        let mut playback = None;
        let mut stopped = Some((context, native_observation, Some(error)));
        let mut terminal = None;
        let view = Mutex::new(PlanView::default());
        stop_source(
            request.clone(),
            &mut playback,
            &mut stopped,
            &mut terminal,
            &view,
            &events,
        )
        .unwrap();
        let mut saw_ack = false;
        let mut saw_fault = false;
        for event in receiver.try_iter() {
            match event {
                Event::GraphActivated(_) => saw_ack = true,
                Event::StopFailed {
                    request: actual,
                    token,
                    error,
                    ..
                } => {
                    saw_fault = true;
                    assert!(saw_ack);
                    assert_eq!(actual.settings.snapshot.graph_epoch, 0);
                    assert_eq!(token.graph_epoch, 1);
                    assert_eq!(token.origin_epochs.stream, 1);
                    assert_eq!(actual.settings.snapshot.epochs.stream, 2);
                    assert_eq!(error.code, ErrorCode::OutputFault);
                }
                Event::Stopped { .. } | Event::Failed { .. } => {
                    panic!("native fault lost its exact Stop binding")
                }
                _ => {}
            }
        }
        assert!(saw_fault);
        assert!(terminal.is_some());
        stop_source(
            request,
            &mut playback,
            &mut stopped,
            &mut terminal,
            &view,
            &events,
        )
        .unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn stop_after_unacknowledged_normal_eof_keeps_its_successful_result() {
        let (mut worker, mut settings, _, _) = drain_proof(2);
        settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
        let request_id = RequestId::new("stop-after-normal-eof").unwrap();
        settings.snapshot.operation = Some(request_id.clone());
        worker
            .send(Action::Stop(StopRequest {
                request_id,
                settings: settings.clone(),
            }))
            .unwrap();
        assert!(
            matches!(worker.events.recv_timeout(Duration::from_secs(5)),Ok(Event::Stopped {epochs,..}) if epochs==settings.snapshot.epochs)
        );
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn graph_activation_after_proven_native_finish_reports_normal_completion() {
        let (mut worker, settings, token, final_observation) = drain_proof(1);
        let (settings, activation, pipeline) = prepare_activation(&settings, &final_observation);
        let epochs = activation.epochs;
        worker
            .send(Action::ActivateGraph {
                settings,
                activation,
                pipeline,
            })
            .unwrap();
        match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Drained {
                epochs: completed,
                token: retained,
                observation,
            } => {
                assert_eq!(completed, epochs);
                assert_eq!(retained, token);
                assert_eq!(observation, final_observation);
            }
            _ => panic!("normal native completion became an output fault"),
        }
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn graph_activation_without_native_completion_proof_keeps_its_failure() {
        let (completed_worker, settings, _, final_observation) = drain_proof(1);
        drop(completed_worker);
        let (settings, activation, pipeline) = prepare_activation(&settings, &final_observation);
        let request_id = activation.request_id.clone();
        let epochs = activation.epochs;
        let mut worker = Worker::spawn();
        worker
            .send(Action::Start {
                settings: settings.clone(),
                entries: vec![],
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_secs(5)),
            Ok(Event::Failed { .. })
        ));
        worker
            .send(Action::ActivateGraph {
                settings,
                activation,
                pipeline,
            })
            .unwrap();
        assert!(
            matches!(worker.events.recv_timeout(Duration::from_secs(5)), Ok(Event::GraphFailed {
            epochs: failed, request_id: failed_id, .. }) if failed == epochs && failed_id == request_id)
        );
        assert!(matches!(
            worker.events.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
    #[test]
    fn drain_acknowledgements_use_reserve_without_cancelling_unrelated_work() {
        let (mut worker, receiver) = admission();
        let cancel = Arc::new(AtomicBool::new(false));
        worker.cancel = Some(cancel.clone());
        for _ in 0..64 {
            worker.send(Action::Settings(settings(true))).unwrap();
        }
        let token = SourceTerminalToken {
            owner_id: None,
            session_id: SessionId::new("old").unwrap(),
            origin_epochs: Epochs::default(),
            graph_epoch: 0,
            device_epoch: 0,
            final_entry_id: QueueEntryId::new("old-entry").unwrap(),
        };
        assert_eq!(
            worker
                .send(Action::AcknowledgeSourceTerminal(token.clone()))
                .unwrap_err()
                .code,
            ErrorCode::SourceBusy
        );
        assert!(!cancel.load(Ordering::Acquire));
        let actions: Vec<_> = receiver.try_iter().collect();
        assert_eq!(actions.len(), 64);
        worker.queued.store(32, Ordering::Release);
        worker
            .send(Action::AcknowledgeSourceTerminal(token))
            .unwrap();
        assert!(matches!(
            receiver.try_recv(),
            Ok(Action::AcknowledgeSourceTerminal(_))
        ));
        assert!(!cancel.load(Ordering::Acquire));
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
            let (epochs, observation) = next_drain(&worker);
            assert_eq!(epochs, settings.snapshot.epochs);
            assert_eq!(observation, expected);
            worker.send(Action::Settings(settings)).unwrap();
            assert!(matches!(
                worker.events.recv_timeout(Duration::from_millis(30)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ));
        }
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn terminal_drain_cannot_cross_an_unrelated_generation() {
        for changed in ["session", "stream", "graph"] {
            let (mut worker, mut settings, _) = naturally_drained_worker();
            settings.snapshot.muted = true;
            settings.snapshot.epochs.stream += 1;
            match changed {
                "session" => settings.snapshot.epochs.session += 1,
                "stream" => settings.snapshot.epochs.stream += 1,
                "graph" => settings.snapshot.graph_epoch += 1,
                _ => unreachable!(),
            }
            worker.send(Action::Settings(settings)).unwrap();
            assert!(
                matches!(
                    worker.events.recv_timeout(Duration::from_millis(30)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ),
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
        let (_, observation) = next_drain(&worker);
        assert_eq!(observation, expected);
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn reposition_after_native_finish_reports_the_proven_completion() {
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
            assert_eq!(next_drain(&worker).0, epochs);
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
        assert!(!worker.has_capacity_for(false, 1));
        assert!(worker.send(Action::Settings(settings(false))).is_err());
        let mute = worker.send(Action::Settings(settings(true)));
        let stop = worker.send(Action::Stop(StopRequest {
            request_id: RequestId::new("reserved-stop").unwrap(),
            settings: settings(true),
        }));
        let actions: Vec<_> = receiver.try_iter().collect();
        assert!(mute.is_ok(), "ordinary commands blocked mute: {mute:?}");
        assert!(stop.is_ok(), "ordinary commands blocked stop: {stop:?}");
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(actions.len(), 34);
        for action in &actions[..32] {
            assert!(matches!(action, Action::Settings(s) if !s.snapshot.muted));
        }
        assert!(matches!(&actions[32], Action::Settings(s) if s.snapshot.muted));
        assert!(matches!(&actions[33], Action::Stop(_)));
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
        assert!(!worker.has_capacity_for(true, 1));
        let error = worker
            .send(Action::Stop(StopRequest {
                request_id: RequestId::new("overflow-stop").unwrap(),
                settings: settings(true),
            }))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::SourceBusy);
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(worker.queued.load(Ordering::Acquire), 64);
        let actions: Vec<_> = receiver.try_iter().collect();
        assert_eq!(actions.len(), 64);
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, Action::Stop(_)))
        );
    }
    #[test]
    fn exact_zero_uses_safety_reserve_and_fails_closed_on_failed_delivery() {
        for disconnected in [false, true] {
            let (mut worker, receiver) = admission();
            let cancel = Arc::new(AtomicBool::new(false));
            worker.cancel = Some(cancel.clone());
            let mut zero = settings(false);
            zero.snapshot.software_gain_zero = true;
            if disconnected {
                drop(receiver);
                assert_eq!(
                    worker.send(Action::Settings(zero)).unwrap_err().code,
                    ErrorCode::OutputFault
                );
                assert_eq!(worker.queued.load(Ordering::Acquire), 0);
            } else {
                for _ in 0..ORDINARY_COMMANDS {
                    worker.send(Action::Settings(settings(false))).unwrap();
                }
                for _ in ORDINARY_COMMANDS..COMMANDS {
                    worker
                        .send(Action::Settings(zero.clone()))
                        .expect("ordinary traffic must leave exact zero its safety reserve");
                }
                assert_eq!(
                    worker.send(Action::Settings(zero)).unwrap_err().code,
                    ErrorCode::SourceBusy
                );
                assert_eq!(worker.queued.load(Ordering::Acquire), COMMANDS);
                assert_eq!(receiver.try_iter().count(), COMMANDS);
            }
            assert!(cancel.load(Ordering::Acquire));
        }
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

    #[cfg(target_os = "linux")]
    mod initial_clock_tests {
        use super::*;
        use std::sync::atomic::AtomicU8;
        // Only timing observations are controlled. Real SRC, replay, PCM writes, and ALSA null remain active.
        struct InitialClock {
            inner: Box<dyn Output>,
            phase: Arc<AtomicU8>,
        }
        impl InitialClock {
            fn counters(&self, mut counters: Counters) -> Counters {
                if self.phase.load(Ordering::Acquire) != 2 {
                    counters.restart_cursor = None;
                    counters.consumed = None;
                }
                counters
            }
        }
        impl Output for InitialClock {
            fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
                self.inner.audio_storage_bytes()
            }
            fn is_running(&self) -> bool {
                self.inner.is_running()
            }
            fn observation(&self) -> Result<BackendObservation, EngineError> {
                self.inner.observation()
            }
            fn write(
                &mut self,
                block: &PcmBlock,
                offset: usize,
                frames: usize,
            ) -> Result<Option<usize>, EngineError> {
                self.inner.write(block, offset, frames)
            }
            fn initial_restart_cursor_pending(&self) -> bool {
                self.phase.load(Ordering::Acquire) == 0
            }
            fn progress(&mut self) -> Result<Counters, EngineError> {
                if self.phase.load(Ordering::Acquire) == 3 {
                    return Err(EngineError::new(
                        ErrorCode::OutputFault,
                        "controlled native clock fault",
                    ));
                }
                let counters = self.inner.progress()?;
                Ok(self.counters(counters))
            }
            fn suspend(&mut self, epochs: Epochs) -> Result<Counters, EngineError> {
                let counters = self.inner.suspend(epochs)?;
                Ok(self.counters(counters))
            }
            fn finish(&mut self) -> Result<Option<Counters>, EngineError> {
                let counters = self.inner.finish()?;
                Ok(counters.map(|c| self.counters(c)))
            }
            fn wait(&mut self) -> Result<(), EngineError> {
                self.inner.wait()
            }
            fn stop(&mut self) -> Result<(), EngineError> {
                self.inner.stop()
            }
        }
        fn pending() -> (Playback, SyncSender<Event>, Receiver<Event>, Arc<AtomicU8>) {
            let (mut playback, events, receiver, _) = staged_streaming_playback();
            let phase = Arc::new(AtomicU8::new(0));
            let render = playback
                .pipeline
                .as_mut()
                .unwrap()
                .stream_render_mut()
                .unwrap();
            let accepted = playback
                .output
                .write(render.period(), 0, 1)
                .unwrap()
                .unwrap();
            render.accepted(accepted).unwrap();
            playback.stream_position += accepted as u64;
            playback.output = Box::new(InitialClock {
                inner: playback.output,
                phase: phase.clone(),
            });
            (playback, events, receiver, phase)
        }
        #[test]
        fn pending_coreaudio_clock_preserves_real_stream_replay_until_a_known_frontier() {
            let (mut playback, _events, _receiver, phase) = pending();
            let before = playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .output_positions();
            let observed = playback.observe().unwrap();
            assert!(observed.consumed_frames.is_none());
            assert_eq!(
                playback
                    .pipeline
                    .as_ref()
                    .unwrap()
                    .stream_render()
                    .unwrap()
                    .output_positions(),
                before
            );
            phase.store(2, Ordering::Release);
            playback.observe().unwrap();
            let after = playback
                .pipeline
                .as_ref()
                .unwrap()
                .stream_render()
                .unwrap()
                .output_positions();
            assert_eq!(after.generated, before.generated);
            assert_eq!(after.next_submission, before.next_submission);
            assert!(after.retained_start > before.retained_start);
            phase.store(1, Ordering::Release);
            assert_eq!(
                playback.observe().unwrap_err().code,
                ErrorCode::CapabilityUnsupported
            );
            phase.store(3, Ordering::Release);
            assert_eq!(playback.observe().unwrap_err().code, ErrorCode::OutputFault);
        }
        #[test]
        fn pending_clock_does_not_authorize_pause_or_final_stream_retirement() {
            let (mut playback, events, _receiver, _phase) = pending();
            playback.phase = RenderPhase::BackendDrain;
            assert_eq!(
                playback.step_streaming(&events).unwrap_err().code,
                ErrorCode::CapabilityUnsupported
            );
            let (mut playback, events, _receiver, _phase) = pending();
            let mut settings = playback.settings.clone();
            settings.snapshot.epochs = settings.snapshot.epochs.next_stream().unwrap();
            settings.snapshot.muted = true;
            settings.playing = false;
            assert_eq!(
                playback
                    .reposition(settings, None, &events, true)
                    .unwrap_err()
                    .code,
                ErrorCode::CapabilityUnsupported
            );
        }
    }
}
