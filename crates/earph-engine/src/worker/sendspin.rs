// ABOUTME: Plays one Sendspin player connection into the worker's native output.
// ABOUTME: Writes received PCM contiguously; timing stays unscheduled and unqualified.
use super::*;
use earph_core::{
    audio::{ChannelLayout, PcmRepresentation},
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

/// The declared player state and the timeline must use one constant output delay.
/// No scheduler exists yet, so the player reports no delay, lead time, or buffer need.
const OUTPUT_DELAY_MS: u32 = 0;
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
fn closed_event(
    epochs: Epochs,
    submitted_frames: u64,
    error: Option<EngineError>,
) -> SendspinSourceEvent {
    SendspinSourceEvent {
        epochs,
        observation: SendspinSourceObservation::new(
            SendspinTransportPhase::Closed,
            submitted_frames,
        ),
        error: error.map(|error| error.code),
    }
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
    report_closed(shared, closed_event(epochs, 0, Some(error)), events);
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
    server_now: Option<ServerPosition>,
}
impl Reducer {
    fn new(binding: PlayerBinding) -> Self {
        Self {
            binding,
            timeline: PlayerTimeline::new(binding, TimelineLimits::default()),
            declared: None,
            available: None,
            server_now: None,
        }
    }
    fn activated(&mut self, participating: bool) -> Result<(), EngineError> {
        self.timeline
            .set_participation(participating, participating)
            .map_err(player_error)
    }
    fn player(
        &mut self,
        event: PlayerEvent<'_>,
        accept_audio: bool,
    ) -> Result<TimelineAction, EngineError> {
        self.timeline
            .apply(event, self.binding, accept_audio, None)
            .map_err(player_error)
    }
    /// The timeline rejects a regressing clock, so a later mapping revision cannot step back.
    fn advance(&mut self, server_us: i64) -> Result<(), EngineError> {
        let now = ServerPosition::new(server_us, 0).map_err(player_error)?;
        let now = self.server_now.map_or(now, |last| last.max(now));
        self.timeline
            .advance_server_time(now, OUTPUT_DELAY_MS)
            .map_err(player_error)?;
        self.server_now = Some(now);
        Ok(())
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
    offset: usize,
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
    held: Option<Held>,
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
        let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16)?;
        let reducer = Reducer::new(binding);
        // Every fallible step precedes the source, whose drop reports a clean close.
        let mut gain = GainRamp::new(format)?;
        let (target, ramp) = gain_target(&settings.snapshot);
        gain.set_target(target, ramp)?;
        let state = PlayerState::new(OUTPUT_DELAY_MS, 0, 0).map_err(player_error)?;
        let output = output::open(config, format, epochs)?;
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
            held: None,
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
        let event = closed_event(
            self.settings.snapshot.epochs,
            self.submitted_frames,
            error.or(stopped.err()),
        );
        self.published = event.observation;
        report_closed(&self.shared, event, &self.events);
        stopped
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
        if let Some(mapping) = observed.clock.and_then(|clock| clock.usable_mapping) {
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
        let busy = self.write()?;
        self.publish(observed.player)?;
        if !busy {
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
                self.reducer.player(event, lease.player_available())?;
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
    /// Drops unplayed audio and reprepares the output for another stream.
    fn halt(&mut self) -> Result<(), EngineError> {
        self.pending = None;
        self.release()?;
        self.output
            .suspend(self.settings.snapshot.epochs)
            .map(|_| ())
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
    /// Moves the next span of received samples through gain into the period.
    fn fill(&mut self) -> Result<bool, EngineError> {
        if self.held.as_ref().is_some_and(|held| {
            held.offset == held.samples.len() || !self.reducer.timeline.is_current(&held.lease)
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
            });
        }
        let held = self.held.as_mut().ok_or(FAILED)?;
        let scalars = (held.samples.len() - held.offset).min(self.period.capacity_frames() * 2);
        let span = &held.samples[held.offset..held.offset + scalars];
        for (index, sample) in span.iter().enumerate() {
            self.period.set_integer_sample(index, i64::from(*sample))?;
        }
        held.offset += scalars;
        let frames = scalars / 2;
        self.period.set_span(
            self.settings.snapshot.epochs,
            self.sequence,
            self.submitted_frames,
            frames as u64,
        )?;
        self.sequence += 1;
        self.gain.process(&mut self.period)?;
        self.pending = Some((0, frames));
        Ok(true)
    }
    /// Writes at most one span. Returns false when no audio waits.
    fn write(&mut self) -> Result<bool, EngineError> {
        if self.pending.is_none() && !self.fill()? {
            return Ok(false);
        }
        let (offset, end) = self.pending.ok_or(FAILED)?;
        match self.output.write(&self.period, offset, end - offset)? {
            Some(frames) => {
                self.submitted_frames += frames as u64;
                self.pending = (offset + frames < end).then_some((offset + frames, end));
            }
            None => self.output.wait()?,
        }
        Ok(true)
    }
    fn publish(&mut self, player: Option<PlayerObservation>) -> Result<(), EngineError> {
        let observation =
            SendspinSourceObservation::new(self.reducer.phase(player), self.submitted_frames);
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
            reducer.player(start(1), true).unwrap(),
            TimelineAction::Discarded,
            "an inactive player role discards lifecycle events"
        );
        reducer.activated(true).unwrap();
        assert_eq!(
            reducer.player(start(2), true).unwrap(),
            TimelineAction::Started
        );
        assert_eq!(
            reducer.phase(player(1, true, true)),
            SendspinTransportPhase::Streaming
        );
        let payload = [1, 0, 2, 0, 3, 0, 4, 0];
        assert_eq!(
            reducer.player(audio(&payload, 3), true).unwrap(),
            TimelineAction::Queued
        );
        let mut chunk = reducer.timeline.take_chunk().unwrap();
        assert_eq!(chunk.take_samples().unwrap(), [1, 2, 3, 4]);
        reducer.timeline.release_chunk(chunk).unwrap();
        reducer.player(audio(&payload, 4), true).unwrap_err();
        let later = PlayerAudioFrame {
            server_timestamp_us: 3000,
            send_ahead_us: 0,
            payload: &payload,
        };
        let later = PlayerEvent::Audio {
            frame: later,
            arrival: stamp(5),
        };
        assert_eq!(reducer.player(later, true).unwrap(), TimelineAction::Queued);
        let end = PlayerEvent::End { arrival: stamp(6) };
        assert_eq!(reducer.player(end, true).unwrap(), TimelineAction::Ended);
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
        reducer.player(start(1), true).unwrap();
        let payload = [1, 0, 2, 0];
        assert_eq!(
            reducer.player(audio(&payload, 2), false).unwrap(),
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
        reducer.advance(4000).unwrap();
        assert_eq!(
            reducer.server_now,
            Some(ServerPosition::new(5000, 0).unwrap())
        );
        reducer.advance(6000).unwrap();
        assert_eq!(
            reducer.server_now,
            Some(ServerPosition::new(6000, 0).unwrap())
        );
    }
}
