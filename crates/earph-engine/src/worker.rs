// ABOUTME: Decodes local PCM and owns one native output on its worker thread.
// ABOUTME: Bounds commands, prefetches compatible joins, and drops stale work on cancellation.
use crate::{
    output::{self, Counters, Output},
    plan::PlanView,
};
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmBlock, PcmFormat},
    gain::GainRamp,
    ids::{Epochs, QueueEntryId},
    queue::{PreparationStatus, QueueEntry},
    session::{BackendObservation, PlaybackObservation, PreservationState, SessionSnapshot},
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
#[derive(Clone)]
pub(crate) struct Settings {
    pub snapshot: SessionSnapshot,
    pub playing: bool,
}
pub(crate) enum Action {
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
struct Playback {
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
    end: bool,
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
            end: false,
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
    fn set_gain(&mut self) -> Result<(), EngineError> {
        let gain = if self.settings.snapshot.muted {
            None
        } else {
            Some(self.settings.snapshot.software_gain_db)
        };
        let ramp = self.settings.snapshot.gain_ramp_frames;
        self.gain.set_target(gain, ramp)
    }
    fn prefetch(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.next.is_some() || self.end {
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
        let audible = counters.consumed.and_then(|position| {
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
    ) -> Result<(), EngineError> {
        let counters = self.output.suspend(settings.snapshot.epochs)?;
        let requested = if let Some(position) = position {
            (self.current.entry.clone(), position)
        } else {
            let consumed = counters.restart_cursor.ok_or(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "pause requires a known backend restart boundary",
            ))?;
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
        self.settings = settings;
        if self.current.entry.id != requested.0.id {
            self.current = Track::open(requested.0, self.epochs(), events)?;
        }
        self.current.seek(requested.1, self.epochs())?;
        self.next = None;
        self.pending = false;
        self.period_offset = 0;
        self.stream_position = 0;
        self.source_position = requested.1;
        self.sequence = 0;
        self.started = false;
        self.end = false;
        self.spans.clear();
        self.spans.push(Span {
            entry: self.current.entry.clone(),
            start: 0,
            source_start: requested.1,
            end: 0,
        });
        self.gain = GainRamp::new(self.format())?;
        self.set_gain()?;
        self.prefetch(events)?;
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
        if self.end {
            if let Some(counters) = self.output.finish()? {
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
                                    start: self.stream_position,
                                    source_start: 0,
                                    end: self.stream_position,
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
                            self.end = true;
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
            self.period_offset = 0;
            self.pending = true;
        }
        let count = self.period.header().frame_count as usize - self.period_offset;
        match self.output.write(&self.period, self.period_offset, count)? {
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
                self.source_position += frames as u64;
                self.spans.last_mut().ok_or(FAILED)?.end = self.stream_position;
                if self.period_offset == self.period.header().frame_count as usize {
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

fn run(
    receiver: Receiver<Action>,
    events: SyncSender<Event>,
    queued: Arc<AtomicUsize>,
    view: Arc<Mutex<PlanView>>,
    progress: Arc<Mutex<Option<(Epochs, PlaybackObservation)>>>,
) {
    let mut playback: Option<Playback> = None;
    let mut stopped: Option<(PlaybackObservation, Option<EngineError>)> = None;
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
            let failure_epochs = match &action {
                Action::Pause(settings)
                | Action::Seek { settings, .. }
                | Action::Settings(settings) => Some(settings.snapshot.epochs),
                _ => None,
            };
            let result = match action {
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
                                current.reposition(settings, None, &events)
                            } else {
                                current.settings = settings;
                                current.set_gain()
                            }
                        } else {
                            Ok(())
                        }
                    } else {
                        Ok(())
                    }
                }
                Action::Pause(settings) => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, None, &events)
                    } else {
                        Ok(())
                    }
                }
                Action::Seek { settings, position } => {
                    if let Some(current) = &mut playback {
                        current.reposition(settings, Some(position), &events)
                    } else {
                        Ok(())
                    }
                }
            };
            if let Err(error) = result
                && let Some(mut current) = playback.take()
            {
                let observation = current.failure_observation();
                let _ = current.output.stop();
                clear_reservation(&view, current.epochs());
                let _ = emit(
                    &events,
                    Event::Failed {
                        epochs: failure_epochs.unwrap_or(current.epochs()),
                        error,
                        observation: Some(observation),
                    },
                );
            }
        }
        if let Some(current) = &mut playback {
            match current.step(&events) {
                Ok(true) => {
                    if current.cancel.load(Ordering::Acquire) {
                        stopped = Some((current.failure_observation(), None));
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
