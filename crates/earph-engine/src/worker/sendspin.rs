// ABOUTME: Plays one Sendspin player connection into the worker's native output.
// ABOUTME: Anchors each stream to its server timestamps and reports the measured, unqualified timing error.
use super::*;
use crate::sendspin_schedule::{
    ChunkFacts, Correction, Decision, MonotonicClamp, Qualification, Scheduler,
};
use earph_core::{
    audio::{ChannelLayout, PcmRepresentation},
    control::OutputBackend,
    sendspin_source::*,
};
use earph_network::sendspin::{
    DriverError, MonotonicDomain, PlayerObservation, SendspinHandle, SendspinObserver,
};
use earph_sendspin::{
    player::{
        MAX_DECODED_FRAMES, PcmChunk, PlayerBinding, PlayerError, PlayerEvent, PlayerState,
        PlayerTimeline, ServerPosition, TimelineAction, TimelineLimits,
    },
    session::SessionEvent,
    timing::{TimingError, UsableMapping},
};

/// The declared player state, the timeline, and the scheduler use one constant external output delay.
/// No measurement of the path after the backend exists, so this delay is zero.
const OUTPUT_DELAY_MS: u32 = 0;
/// The server must send each chunk this far ahead of its timestamp. Admitted, not measured.
const REQUIRED_LEAD_TIME_MS: u64 = 150;
/// The server should keep this much audio queued at the player. Admitted, not measured.
const MIN_BUFFER_MS: u32 = 200;
const SAMPLE_RATE_HZ: u32 = 48000;
/// A scheduling fault means a timestamp or counter left the checked integer range.
const SCHEDULE: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "Sendspin schedule arithmetic exceeds its range",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "Sendspin source preparation exceeds memory admission",
);

fn player_error(error: PlayerError) -> EngineError {
    match error {
        PlayerError::LocalBudget => EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "Sendspin audio exceeds the player timeline budget",
        ),
        PlayerError::BindingMismatch => EngineError::new(
            ErrorCode::StaleSession,
            "Sendspin event belongs to another connection",
        ),
        _ => EngineError::new(ErrorCode::AssetInvalid, "Sendspin player lifecycle failed"),
    }
}
fn transport_error(error: DriverError) -> EngineError {
    EngineError::new(
        match error {
            DriverError::Expired | DriverError::Authentication | DriverError::Admission => {
                ErrorCode::AuthDenied
            }
            DriverError::Budget => ErrorCode::ResourceBudgetExceeded,
            _ => ErrorCode::AssetInvalid,
        },
        "Sendspin player connection failed",
    )
}
/// Returns the gain target and ramp length that the session's mute and volume facts require.
fn gain_target(snapshot: &SessionSnapshot) -> (Option<f64>, u32) {
    let zero = snapshot.software_gain_zero;
    let gain = (!snapshot.muted && !zero).then_some(snapshot.software_gain_db);
    (gain, if zero { 0 } else { snapshot.gain_ramp_frames })
}
/// Reports a start that opened no output.
pub(super) fn refuse_start(
    epochs: Epochs,
    shared: &Mutex<SendspinSourceObservation>,
    error: EngineError,
    events: &SyncSender<Event>,
) {
    let event = SendspinSourceEvent {
        epochs,
        observation: SendspinSourceObservation::new(SendspinTransportPhase::Closed, 0),
        error: Some(error.code),
    };
    report_closed(shared, event, events);
}
/// Records the closed source for readers of the shared observation and of worker events.
fn report_closed(
    shared: &Mutex<SendspinSourceObservation>,
    event: SendspinSourceEvent,
    events: &SyncSender<Event>,
) {
    if let Ok(mut shared) = shared.lock() {
        *shared = event.observation;
    }
    // A departed event reader needs no report.
    let _ = events.send(Event::Sendspin(event));
}

/// Reduces ordered transport events into timeline state and the availability declaration.
/// It owns no transport and no output, so tests drive it with plain player events.
struct Reducer {
    binding: PlayerBinding,
    timeline: PlayerTimeline,
    /// The activation generation whose available state the transport accepted for sending.
    declared: Option<u64>,
    /// The activation generation whose available state reached the socket.
    available: Option<u64>,
    server_now: MonotonicClamp,
}
impl Reducer {
    fn new(binding: PlayerBinding) -> Self {
        Self {
            binding,
            timeline: PlayerTimeline::new(binding, TimelineLimits::default()),
            declared: None,
            available: None,
            server_now: MonotonicClamp::default(),
        }
    }
    fn activated(&mut self, participating: bool) -> Result<(), EngineError> {
        self.timeline
            .set_participation(participating, participating)
            .map_err(player_error)
    }
    /// A partial clear cannot retract audio before `written`, which already left for the output.
    fn player(
        &mut self,
        event: PlayerEvent<'_>,
        accept_audio: bool,
        written: Option<ServerPosition>,
    ) -> Result<TimelineAction, EngineError> {
        self.timeline
            .apply(event, self.binding, accept_audio, written)
            .map_err(player_error)
    }
    /// The timeline rejects a regressing clock, so a later mapping revision cannot step back.
    fn advance(&mut self, server_us: i64) -> Result<(), EngineError> {
        let now = ServerPosition::new(self.server_now.clamp(server_us), 0).map_err(player_error)?;
        self.timeline
            .advance_server_time(now, OUTPUT_DELAY_MS)
            .map_err(player_error)
    }
    /// Returns the activation generation that still needs an available declaration.
    fn undeclared(&self, player: Option<PlayerObservation>) -> Option<u64> {
        player
            .filter(|player| player.participating)
            .map(|player| player.activation_generation)
            .filter(|generation| self.declared != Some(*generation))
    }
    /// A closing transport revokes participation before it reports its end.
    /// Only a new activation generation returns an available player to `Preparing`.
    fn phase(&mut self, player: Option<PlayerObservation>) -> SendspinTransportPhase {
        let generation = player.map(|player| player.activation_generation);
        if player
            .is_some_and(|player| player.participating && player.state_sent && player.available)
        {
            self.available = generation;
        }
        if self.timeline.stream_active() {
            SendspinTransportPhase::Streaming
        } else if generation.is_some() && self.available == generation {
            SendspinTransportPhase::AwaitingStream
        } else {
            SendspinTransportPhase::Preparing
        }
    }
}

/// One timeline chunk whose samples moved here. The lease returns after the last sample.
struct Held {
    lease: PcmChunk,
    samples: Vec<i16>,
    /// The scalar index of the first sample that has not left for the output period.
    offset: usize,
    /// Whether the scheduler decided to write the samples from `offset` on.
    scheduled: bool,
}

pub(super) struct SendspinPlayback {
    settings: Settings,
    handle: SendspinHandle,
    observer: SendspinObserver,
    clock: MonotonicDomain,
    /// The worker handle sets this flag to end the source without a queued action.
    cancel: Arc<AtomicBool>,
    state: PlayerState,
    reducer: Reducer,
    output: Box<dyn Output>,
    period: PcmBlock,
    /// The frame range of `period` that the output has not accepted yet.
    pending: Option<(usize, usize)>,
    /// Whether `period` holds scheduled silence. Otherwise it holds samples of the held chunk.
    period_silent: bool,
    held: Option<Held>,
    scheduler: Scheduler,
    /// Where the last scheduled chunk ends on the server timeline.
    previous_end: Option<ServerPosition>,
    /// Scheduled silence frames that precede the held chunk's samples.
    silence: u64,
    /// Whether the output accepted frames since it was last prepared.
    /// ALSA fails a delay query on an output that holds none.
    primed: bool,
    /// Whether the backend reports a delay. Then an unprimed output has a known delay of zero.
    reports_delay: bool,
    delay_basis: Option<&'static str>,
    /// Output underruns that this source recovered from.
    underruns: u64,
    gain: GainRamp,
    sequence: u64,
    submitted_frames: u64,
    shared: Arc<Mutex<SendspinSourceObservation>>,
    published: SendspinSourceObservation,
    events: SyncSender<Event>,
}
impl SendspinPlayback {
    /// Opens the output for the single advertised format. A refusal cancels the connection.
    pub(super) fn start(
        settings: Settings,
        handle: SendspinHandle,
        clock: MonotonicDomain,
        binding: PlayerBinding,
        shared: Arc<Mutex<SendspinSourceObservation>>,
        cancel: Arc<AtomicBool>,
        events: &SyncSender<Event>,
    ) -> Option<Self> {
        let epochs = settings.snapshot.epochs;
        let prepared = Self::prepare(
            settings,
            handle,
            clock,
            binding,
            (shared.clone(), cancel),
            events,
        );
        match prepared {
            Ok(source) => Some(source),
            Err(error) => {
                refuse_start(epochs, &shared, error, events);
                None
            }
        }
    }
    fn prepare(
        settings: Settings,
        handle: SendspinHandle,
        clock: MonotonicDomain,
        binding: PlayerBinding,
        (shared, cancel): (Arc<Mutex<SendspinSourceObservation>>, Arc<AtomicBool>),
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        let epochs = settings.snapshot.epochs;
        let config = settings.snapshot.output.as_ref().ok_or(FAILED)?;
        let format = PcmFormat::new(
            SAMPLE_RATE_HZ,
            ChannelLayout::Stereo,
            PcmRepresentation::Signed16,
        )?;
        let reducer = Reducer::new(binding);
        // Every fallible step precedes the source, whose drop reports a clean close.
        let mut gain = GainRamp::new(format)?;
        let (target, ramp) = gain_target(&settings.snapshot);
        gain.set_target(target, ramp)?;
        let state = PlayerState::new(OUTPUT_DELAY_MS, REQUIRED_LEAD_TIME_MS, MIN_BUFFER_MS)
            .map_err(player_error)?;
        let scheduler = Scheduler::new(SAMPLE_RATE_HZ).map_err(|_| SCHEDULE)?;
        let output = output::open(config, format, epochs)?;
        #[cfg(all(test, target_os = "linux"))]
        let output = super::sendspin_tests::underrun_seam(output);
        let retained = settings
            .snapshot
            .profile_storage_bytes
            .checked_add(output.audio_storage_bytes()?)
            .and_then(|n| {
                n.checked_add(format.storage_bytes(MAX_DECODED_FRAMES as u64).ok()? as u64)
            })
            .and_then(|n| n.checked_add(reducer.timeline.metadata_capacity_bytes() as u64))
            .ok_or(BUDGET)?;
        MemoryBudget::default().admit(usize::try_from(retained).map_err(|_| BUDGET)?)?;
        let period = PcmBlock::prepare(
            format,
            config.period_frames as usize,
            epochs,
            MemoryBudget::default(),
        )?;
        let published = SendspinSourceObservation::new(SendspinTransportPhase::Preparing, 0);
        if let Ok(mut shared) = shared.lock() {
            *shared = published;
        }
        let event = SendspinSourceEvent {
            epochs,
            observation: published,
            error: None,
        };
        emit(events, Event::Sendspin(event))?;
        Ok(Self {
            observer: handle.observer(),
            handle,
            clock,
            cancel,
            state,
            reducer,
            output,
            period,
            pending: None,
            period_silent: false,
            held: None,
            scheduler,
            previous_end: None,
            silence: 0,
            primed: false,
            // ALSA reports a client delay estimate. Core Audio reports none.
            reports_delay: config.backend == OutputBackend::Alsa,
            delay_basis: None,
            underruns: 0,
            gain,
            sequence: 0,
            submitted_frames: 0,
            shared,
            published,
            events: events.clone(),
            settings,
        })
    }
    fn event(&self, error: Option<ErrorCode>) -> SendspinSourceEvent {
        SendspinSourceEvent {
            epochs: self.settings.snapshot.epochs,
            observation: self.published,
            error,
        }
    }
    fn set_gain(&mut self) -> Result<(), EngineError> {
        let (gain, ramp) = gain_target(&self.settings.snapshot);
        self.gain.set_target(gain, ramp)?;
        if gain.is_none()
            && ramp == 0
            && let Some((offset, _)) = self.pending
        {
            silence_period_suffix(&mut self.period, offset)?;
        }
        Ok(())
    }
    /// Mute always applies. Other settings apply only within this source's session epoch.
    pub(super) fn apply_settings(&mut self, settings: Settings) -> Result<(), EngineError> {
        if settings.snapshot.epochs.session == self.settings.snapshot.epochs.session {
            let epochs = self.settings.snapshot.epochs;
            self.settings = settings;
            // The opened output keeps its epochs until this source closes.
            self.settings.snapshot.epochs = epochs;
        } else if settings.snapshot.muted {
            self.settings.snapshot.muted = true;
        }
        self.set_gain()
    }
    /// Refuses controls that this source cannot honor. Returns every other action unchanged.
    pub(super) fn refuse(&self, action: Action) -> Option<Action> {
        let event = match action {
            Action::ActivateGraph { activation, .. } => Event::GraphFailed {
                epochs: activation.epochs,
                request_id: activation.request_id,
                error: EngineError::new(
                    ErrorCode::FidelityPolicyConflict,
                    "Sendspin cannot accept a PCM pipeline",
                ),
            },
            Action::Seek { .. } | Action::SeekDsd { .. } => {
                Event::Sendspin(self.event(Some(ErrorCode::CapabilityUnsupported)))
            }
            action => return Some(action),
        };
        let _ = self.events.send(event);
        None
    }
    /// Stops the output, cancels the connection, and reports the closed source once.
    pub(super) fn close(&mut self, error: Option<EngineError>) -> Result<(), EngineError> {
        if self.published.phase == SendspinTransportPhase::Closed {
            return Ok(());
        }
        self.handle.cancel();
        let stopped = self.output.stop();
        // A closed source holds no anchor. Its cumulative timing facts stay readable.
        self.scheduler.reset();
        self.published = self.observed(SendspinTransportPhase::Closed);
        let event = self.event(error.or(stopped.err()).map(|error| error.code));
        report_closed(&self.shared, event, &self.events);
        stopped
    }
    fn observed(&self, phase: SendspinTransportPhase) -> SendspinSourceObservation {
        let facts = self.scheduler.snapshot();
        SendspinSourceObservation {
            phase,
            submitted_frames: self.submitted_frames,
            timing: SendspinTiming {
                qualification: match facts.qualification {
                    Qualification::Unqualified => SendspinTimingQualification::Unqualified,
                },
                correction: match facts.correction {
                    Correction::None => SendspinTimingCorrection::None,
                },
                anchored: facts.anchored,
                anchors: facts.anchors,
                underruns: self.underruns,
                start_error_us: facts.start_error_us,
                last_error_us: facts.last_error_us,
                max_abs_error_us: facts.max_abs_error_us,
                inserted_silence_frames: facts.inserted_silence_frames,
                dropped_frames: facts.dropped_frames,
                mapping_revision: facts.mapping_revision,
                backend_delay_known: facts.delay_known,
                backend_delay_basis: self.delay_basis,
            },
        }
    }
    /// Runs one bounded tick. Returns true when the connection ended without a fault.
    pub(super) fn step(&mut self) -> Result<bool, EngineError> {
        // A closed source reports nothing more. A set flag ends the source like a stop.
        if self.published.phase == SendspinTransportPhase::Closed
            || self.cancel.load(Ordering::Acquire)
        {
            return Ok(true);
        }
        let observed = self.observer.observation();
        match observed.terminal {
            None => {}
            Some(DriverError::PeerClosed | DriverError::Cancelled) => return Ok(true),
            Some(error) => return Err(transport_error(error)),
        }
        // The event queue has no backpressure, so every tick drains it before any wait.
        self.drain()?;
        let mapping = observed.clock.and_then(|clock| clock.usable_mapping);
        if let Some(mapping) = mapping {
            self.advance(mapping)?;
            if let Some(generation) = self.reducer.undeclared(observed.player)
                && self
                    .handle
                    .try_player_state(generation, mapping, self.state, true)
                    .is_ok()
            {
                self.reducer.declared = Some(generation);
            }
        }
        let busy = match self.write(mapping) {
            Ok(busy) => busy,
            // An underrun fails an output call. The recovered output has audio to schedule.
            Err(error) => self.recover(error).map(|()| true)?,
        };
        self.publish(observed.player)?;
        if !busy {
            // This sleep also bounds a scheduled wait, so the next tick drains events on time.
            // It is shorter than the anchor pad window, which absorbs the late wake as silence.
            thread::sleep(Duration::from_millis(1));
        }
        Ok(false)
    }
    fn drain(&mut self) -> Result<(), EngineError> {
        while let Some(lease) = self.observer.next_event() {
            let streaming = self.reducer.timeline.stream_active();
            match lease.event() {
                SessionEvent::Activated(notice) => self.reducer.activated(
                    notice.roles == ["player@v1"] && notice.activities == ["playback"],
                )?,
                // The transport returned an unsent player state. Declare it again.
                SessionEvent::Unsent(_) => self.reducer.declared = None,
                _ => {}
            }
            if let Some(event) = lease.player_event() {
                let full_clear = matches!(
                    event,
                    PlayerEvent::Clear {
                        from_timestamp_us: None,
                        ..
                    }
                );
                let written = self.written_until()?;
                let available = lease.player_available();
                match self.reducer.player(event, available, written)? {
                    TimelineAction::Started => self.unanchor(),
                    // A full clear also drops the audio that the output still holds.
                    TimelineAction::Cleared(_) if full_clear => self.halt()?,
                    TimelineAction::Cleared(_) => self.retract(written)?,
                    _ => {}
                }
            }
            // A stream end or a lost player role stops the stream. Neither drains audio.
            if streaming && !self.reducer.timeline.stream_active() {
                self.halt()?;
            }
        }
        Ok(())
    }
    /// Keeps the timeline's server clock current, or its wire budget never drains.
    fn advance(&mut self, mapping: UsableMapping) -> Result<(), EngineError> {
        let now = self.clock.sample().map_err(transport_error)?.microseconds();
        match mapping.map_client_time(now, now, self.reducer.binding.clock()) {
            Ok(server) => self.reducer.advance(server.time_us),
            Err(TimingError::BindingMismatch) => Err(player_error(PlayerError::BindingMismatch)),
            // The transport ends a connection whose estimate expired; skip this tick.
            Err(_) => Ok(()),
        }
    }
    /// Returns where the audio that left the timeline for the output ends on the server timeline.
    fn written_until(&self) -> Result<Option<ServerPosition>, EngineError> {
        match &self.held {
            Some(held) if held.scheduled => held
                .lease
                .position()
                .add_frames(held.offset / 2)
                .map(Some)
                .map_err(player_error),
            _ => Ok(self.previous_end),
        }
    }
    /// Drops the held chunk when a partial clear discarded it.
    /// The replacement audio then continues from `written`, so the anchor stays.
    /// Scheduled silence still owed lies before `written`, so it stays owed.
    fn retract(&mut self, written: Option<ServerPosition>) -> Result<(), EngineError> {
        if self
            .held
            .as_ref()
            .is_some_and(|held| !self.reducer.timeline.is_current(&held.lease))
        {
            self.release()?;
            self.previous_end = written;
        }
        Ok(())
    }
    /// Drops unplayed audio and reprepares the output for other audio.
    fn halt(&mut self) -> Result<(), EngineError> {
        self.pending = None;
        self.release()?;
        self.unanchor();
        if !self.primed {
            // A prepared output that accepted no frame holds nothing to drop.
            return Ok(());
        }
        match self.output.suspend(self.settings.snapshot.epochs) {
            Ok(_) => {
                self.primed = false;
                Ok(())
            }
            // A starved output underran before this halt. Its recovery prepares it again.
            Err(error) => self.recover(error),
        }
    }
    /// Reprepares the output after an underrun and makes the unwritten audio anchor again.
    /// Every other failure returns unchanged. Gain and session settings stay as they are.
    fn recover(&mut self, error: EngineError) -> Result<(), EngineError> {
        // Only an output that accepted frames can underrun. This bound also ends a repeating fault.
        if !self.primed || !self.output.recover(self.settings.snapshot.epochs)? {
            return Err(error);
        }
        self.primed = false;
        self.underruns += 1;
        let unwritten = self.pending.take().map_or(0, |(offset, end)| end - offset);
        if let Some(held) = self.held.as_mut() {
            if !self.period_silent {
                // The held chunk takes back the samples that the output never accepted.
                held.offset -= unwritten * 2;
            }
            // A chunk without samples left stays scheduled, so the scheduler releases it.
            held.scheduled = held.offset == held.samples.len();
        }
        self.unanchor();
        Ok(())
    }
    /// Makes the next chunk anchor to its own timestamp.
    fn unanchor(&mut self) {
        self.scheduler.reset();
        self.previous_end = None;
        self.silence = 0;
    }
    fn release(&mut self) -> Result<(), EngineError> {
        let Some(held) = self.held.take() else {
            return Ok(());
        };
        // The lease returns only after its sample storage is gone.
        drop(held.samples);
        self.reducer
            .timeline
            .release_chunk(held.lease)
            .map_err(player_error)
    }
    /// Returns the frames that the backend still holds, or `None` when it reports no delay.
    fn backend_delay(&mut self) -> Result<Option<u64>, EngineError> {
        if !self.primed {
            return Ok(self.reports_delay.then_some(0));
        }
        let counters = self.output.progress()?;
        if counters.delay.is_some() {
            self.delay_basis = Some(counters.basis);
        }
        Ok(counters.delay)
    }
    /// Asks the scheduler what to do with the unscheduled head chunk.
    /// Returns `None` while no usable clock mapping places the chunk.
    fn decide(&mut self, mapping: Option<UsableMapping>) -> Result<Option<Decision>, EngineError> {
        let (Some(mapping), Some(held)) = (mapping, self.held.as_ref()) else {
            return Ok(None);
        };
        // After an underrun the held chunk continues from its first unwritten frame.
        let position = held
            .lease
            .position()
            .add_frames(held.offset / 2)
            .map_err(player_error)?;
        let frames = (held.samples.len() - held.offset) as u64 / 2;
        let now_us = self.clock.sample().map_err(transport_error)?.microseconds();
        let target = match mapping.port_target(
            position.microseconds_floor(),
            OUTPUT_DELAY_MS,
            now_us,
            self.reducer.binding.clock(),
        ) {
            Ok(target) => target,
            Err(TimingError::BindingMismatch) => {
                return Err(player_error(PlayerError::BindingMismatch));
            }
            // The transport ends a connection whose estimate expired; hold the chunk this tick.
            Err(_) => return Ok(None),
        };
        // Whole frames between the previous chunk and this one. An overlap counts as no gap.
        let gap_ticks = self
            .previous_end
            .map_or(0, |end| position.ticks() - end.ticks());
        let facts = ChunkFacts {
            target_us: target.time_us,
            mapping_revision: target.mapping_revision,
            frames,
            now_us,
            delay_frames: self.backend_delay()?,
            gap_frames: u64::try_from(gap_ticks / ServerPosition::TICKS_PER_FRAME).unwrap_or(0),
        };
        self.scheduler.decide(facts).map(Some).map_err(|_| SCHEDULE)
    }
    /// Takes and schedules chunks until one has samples or silence to write.
    /// Returns false while the head chunk waits for its target or no chunk is queued.
    fn schedule(&mut self, mapping: Option<UsableMapping>) -> Result<bool, EngineError> {
        loop {
            if self.held.as_ref().is_some_and(|held| {
                (held.scheduled && held.offset == held.samples.len())
                    || !self.reducer.timeline.is_current(&held.lease)
            }) {
                self.release()?;
            }
            if self.held.is_none() {
                let Some(mut lease) = self.reducer.timeline.take_chunk() else {
                    return Ok(false);
                };
                let samples = lease.take_samples().ok_or(FAILED)?;
                self.held = Some(Held {
                    lease,
                    samples,
                    offset: 0,
                    scheduled: false,
                });
            }
            if self.held.as_ref().is_some_and(|held| held.scheduled) {
                return Ok(true);
            }
            match self.decide(mapping)? {
                None | Some(Decision::Wait { .. }) => return Ok(false),
                Some(Decision::DropChunk) => self.release()?,
                Some(Decision::Write {
                    prepend_silence_frames,
                    skip_frames,
                }) => {
                    let held = self.held.as_mut().ok_or(FAILED)?;
                    held.offset += usize::try_from(skip_frames).map_err(|_| SCHEDULE)? * 2;
                    held.scheduled = true;
                    self.previous_end = Some(held.lease.end_position());
                    self.silence = prepend_silence_frames;
                    if self.silence > 0 {
                        return Ok(true);
                    }
                }
            }
        }
    }
    /// Moves the next span of scheduled silence or received samples through gain into the period.
    fn fill(&mut self, mapping: Option<UsableMapping>) -> Result<bool, EngineError> {
        if self.silence == 0 && !self.schedule(mapping)? {
            return Ok(false);
        }
        let capacity = self.period.capacity_frames();
        self.period_silent = self.silence > 0;
        let frames = if self.period_silent {
            let frames = usize::try_from(self.silence).map_or(capacity, |left| left.min(capacity));
            for index in 0..frames * 2 {
                self.period.set_integer_sample(index, 0)?;
            }
            self.silence -= frames as u64;
            frames
        } else {
            let held = self.held.as_mut().ok_or(FAILED)?;
            let scalars = (held.samples.len() - held.offset).min(capacity * 2);
            let span = &held.samples[held.offset..held.offset + scalars];
            for (index, sample) in span.iter().enumerate() {
                self.period.set_integer_sample(index, i64::from(*sample))?;
            }
            held.offset += scalars;
            scalars / 2
        };
        self.period.set_span(
            self.settings.snapshot.epochs,
            self.sequence,
            self.submitted_frames,
            frames as u64,
        )?;
        self.sequence += 1;
        // Silence passes the gain too, so a ramp advances with output time.
        self.gain.process(&mut self.period)?;
        self.pending = Some((0, frames));
        Ok(true)
    }
    /// Writes at most one span. Returns false when no audio is due.
    fn write(&mut self, mapping: Option<UsableMapping>) -> Result<bool, EngineError> {
        if self.pending.is_none() && !self.fill(mapping)? {
            return Ok(false);
        }
        let (offset, end) = self.pending.ok_or(FAILED)?;
        match self.output.write(&self.period, offset, end - offset)? {
            Some(frames) => {
                self.submitted_frames += frames as u64;
                self.primed = true;
                self.pending = (offset + frames < end).then_some((offset + frames, end));
            }
            None => self.output.wait()?,
        }
        Ok(true)
    }
    fn publish(&mut self, player: Option<PlayerObservation>) -> Result<(), EngineError> {
        let phase = self.reducer.phase(player);
        let observation = self.observed(phase);
        if observation == self.published {
            return Ok(());
        }
        let changed = observation.phase != self.published.phase;
        self.published = observation;
        if let Ok(mut shared) = self.shared.lock() {
            *shared = observation;
        }
        if changed {
            emit(&self.events, Event::Sendspin(self.event(None)))?;
        }
        Ok(())
    }
}
/// Every exit path, including worker shutdown, reports the closed source.
impl Drop for SendspinPlayback {
    fn drop(&mut self) {
        let _ = self.close(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_sendspin::{
        PlayerAudioFrame, message::ReceiveStamp, player::StreamFormat, timing::Binding,
    };

    fn reducer() -> Reducer {
        Reducer::new(PlayerBinding::new(Binding::new([3; 32], 1, 1), 1, 1))
    }
    fn stamp(microseconds: i64) -> ReceiveStamp {
        ReceiveStamp::new(microseconds).unwrap()
    }
    fn start(arrival: i64) -> PlayerEvent<'static> {
        PlayerEvent::Start {
            server_transmitted_us: 1000,
            format: StreamFormat::PCM,
            arrival: stamp(arrival),
        }
    }
    fn audio(payload: &[u8], arrival: i64) -> PlayerEvent<'_> {
        PlayerEvent::Audio {
            frame: PlayerAudioFrame {
                server_timestamp_us: 2000,
                send_ahead_us: 0,
                payload,
            },
            arrival: stamp(arrival),
        }
    }
    fn player(generation: u64, participating: bool, sent: bool) -> Option<PlayerObservation> {
        Some(PlayerObservation {
            activation_generation: generation,
            participating,
            state_sent: sent,
            available: sent,
        })
    }

    #[test]
    fn activation_start_audio_and_end_drive_the_timeline_in_order() {
        let mut reducer = reducer();
        assert_eq!(
            reducer.player(start(1), true, None).unwrap(),
            TimelineAction::Discarded,
            "an inactive player role discards lifecycle events"
        );
        reducer.activated(true).unwrap();
        assert_eq!(
            reducer.player(start(2), true, None).unwrap(),
            TimelineAction::Started
        );
        assert_eq!(
            reducer.phase(player(1, true, true)),
            SendspinTransportPhase::Streaming
        );
        let payload = [1, 0, 2, 0, 3, 0, 4, 0];
        assert_eq!(
            reducer.player(audio(&payload, 3), true, None).unwrap(),
            TimelineAction::Queued
        );
        let mut chunk = reducer.timeline.take_chunk().unwrap();
        assert_eq!(chunk.take_samples().unwrap(), [1, 2, 3, 4]);
        reducer.timeline.release_chunk(chunk).unwrap();
        reducer.player(audio(&payload, 4), true, None).unwrap_err();
        let later = PlayerAudioFrame {
            server_timestamp_us: 3000,
            send_ahead_us: 0,
            payload: &payload,
        };
        let later = PlayerEvent::Audio {
            frame: later,
            arrival: stamp(5),
        };
        assert_eq!(
            reducer.player(later, true, None).unwrap(),
            TimelineAction::Queued
        );
        let end = PlayerEvent::End { arrival: stamp(6) };
        assert_eq!(
            reducer.player(end, true, None).unwrap(),
            TimelineAction::Ended
        );
        assert!(
            reducer.timeline.take_chunk().is_none(),
            "a stream end discards queued audio"
        );
        assert_eq!(
            reducer.phase(player(1, true, true)),
            SendspinTransportPhase::AwaitingStream
        );
    }
    #[test]
    fn unavailable_delivery_and_deactivation_keep_audio_from_the_output() {
        let mut reducer = reducer();
        reducer.activated(true).unwrap();
        reducer.player(start(1), true, None).unwrap();
        let payload = [1, 0, 2, 0];
        assert_eq!(
            reducer.player(audio(&payload, 2), false, None).unwrap(),
            TimelineAction::Discarded
        );
        assert!(reducer.timeline.take_chunk().is_none());
        reducer.activated(false).unwrap();
        assert!(!reducer.timeline.stream_active());
        assert_eq!(
            reducer.phase(player(2, false, false)),
            SendspinTransportPhase::Preparing
        );
    }
    #[test]
    fn availability_is_declared_once_for_each_participating_activation() {
        let mut reducer = reducer();
        assert_eq!(reducer.undeclared(None), None);
        assert_eq!(reducer.undeclared(player(1, false, false)), None);
        assert_eq!(reducer.undeclared(player(1, true, false)), Some(1));
        assert_eq!(
            reducer.phase(player(1, true, false)),
            SendspinTransportPhase::Preparing,
            "an accepted declaration is not yet a sent state"
        );
        reducer.declared = Some(1);
        assert_eq!(reducer.undeclared(player(1, true, true)), None);
        assert_eq!(
            reducer.phase(player(1, true, true)),
            SendspinTransportPhase::AwaitingStream
        );
        assert_eq!(
            reducer.phase(player(1, false, false)),
            SendspinTransportPhase::AwaitingStream,
            "a closing transport revokes participation without a new activation"
        );
        assert_eq!(
            reducer.undeclared(player(2, true, false)),
            Some(2),
            "a new activation generation needs a fresh declaration"
        );
        assert_eq!(
            reducer.phase(player(2, true, false)),
            SendspinTransportPhase::Preparing
        );
    }
    #[test]
    fn server_time_never_regresses_across_mapping_revisions() {
        let mut reducer = reducer();
        reducer.advance(5000).unwrap();
        // The timeline refuses a lower time, so this success proves the clamp.
        reducer.advance(4000).unwrap();
        assert_eq!(reducer.server_now.clamp(0), 5000);
        reducer.advance(6000).unwrap();
        assert_eq!(reducer.server_now.clamp(0), 6000);
    }
}
