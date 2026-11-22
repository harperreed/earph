// ABOUTME: Admits the Sendspin player session and reduces its fenced source events.
// ABOUTME: Lets the server move transport inside the user's session and nothing else.
use super::*;
use crate::{
    sendspin_control::{ActiveConnection, SendspinConnection, SendspinSnapshot},
    sendspin_source::{SendspinSourceEvent, SendspinSourceObservation, SendspinTransportPhase},
};

const BUSY: EngineError = EngineError::new(
    ErrorCode::SourceBusy,
    "stop the active session and wait for Sendspin to disconnect",
);

impl Session {
    /// Records that this owner was launched with a Sendspin server configuration.
    pub fn enable_sendspin(&mut self) {
        self.sendspin.configured = true;
    }
    pub fn sendspin_snapshot(&self) -> SendspinSnapshot {
        self.sendspin.snapshot()
    }
    /// Returns the connection that the current session owns.
    pub fn sendspin_connection(&self) -> Option<SendspinConnection> {
        self.sendspin
            .connection
            .as_ref()
            .filter(|connection| connection.open)
            .map(|connection| connection.fence)
    }
    /// Returns the generation of a released connection that the Host must still retire.
    pub fn retiring_sendspin_connection(&self) -> Option<u64> {
        self.sendspin
            .connection
            .as_ref()
            .filter(|connection| !connection.open)
            .map(|connection| connection.fence.generation)
    }
    pub(super) fn sendspin_prepared(&self) -> bool {
        self.sendspin
            .connection
            .as_ref()
            .is_some_and(|connection| connection.open && connection.prepared)
    }
    /// The worker closes this source on a pause, so Core refuses every positioning control.
    pub(super) fn validate_sendspin_control(&self, command: &Command) -> Result<(), EngineError> {
        match command {
            Command::Pause | Command::Resume | Command::Seek { .. } | Command::SeekDsd { .. } => {
                Err(EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "Sendspin cannot pause, resume, or seek; stop the session instead",
                ))
            }
            Command::Start { .. }
            | Command::NetworkStartHttpFlac { .. }
            | Command::RendererRequest {
                action: crate::renderer::RendererAction::Play { .. },
                ..
            } => Err(BUSY),
            command if command.prepares_graph() || command.activates_graph() => {
                Err(EngineError::new(
                    ErrorCode::FidelityPolicyConflict,
                    "Sendspin cannot apply a PCM pipeline or profile",
                ))
            }
            _ => Ok(()),
        }
    }
    pub(super) fn apply_sendspin_start(
        &mut self,
        command: &CommandEnvelope,
        device_id: &DeviceId,
    ) -> Result<ReceiptStatus, EngineError> {
        if !self.sendspin.configured
            || !self.local_playback_enabled
            || self.state.shutdown_requested
        {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "this owner has no Sendspin server configuration",
            ));
        }
        Self::validate_software_zero(&self.state)?;
        let output = self.state.output.as_ref().ok_or(EngineError::new(
            ErrorCode::SafetyPolicyBlocked,
            "select and approve an output before starting Sendspin",
        ))?;
        if !self.state.output_consented {
            return Err(EngineError::new(
                ErrorCode::SafetyPolicyBlocked,
                "approve the selected output before opening it",
            ));
        }
        if &output.device_id != device_id {
            return Err(STALE);
        }
        // This slice has no takeover. A retiring connection still owns its workers.
        if self.state.session_id.is_some()
            || self.sendspin.connection.is_some()
            || self.artifact_worker_busy()
        {
            return Err(BUSY);
        }
        // A configured pipeline would block unmute for the whole session.
        if self.has_pcm_processing() {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "Sendspin cannot run with a PCM pipeline",
            ));
        }
        let session_id = command.session_id.clone().ok_or(STALE)?;
        let generation = increment(self.sendspin.generation)?;
        let mut next = self.state.clone();
        next.revision = increment(next.revision)?;
        next.event_sequence = self.next_session_event_sequence(false)?;
        next.epochs = next.epochs.next_session()?;
        next.session_id = Some(session_id.clone());
        next.source = Some(SourceKind::Sendspin);
        next.output_id = Some(device_id.clone());
        next.seekable = false;
        next.transport = TransportState::Preparing;
        next.muted = true;
        next.audible_position_frames = None;
        next.decoded_position_frames = 0;
        next.error = None;
        next.active_entry_id = None;
        next.reserved_next_entry_id = None;
        next.observation = Default::default();
        self.state = next;
        self.sendspin.generation = generation;
        self.sendspin.error = None;
        self.sendspin.connection = Some(ActiveConnection {
            fence: SendspinConnection {
                generation,
                epochs: self.state.epochs,
            },
            session_id,
            open: true,
            prepared: false,
            observation: None,
        });
        self.local_boundary = None;
        self.local_admission = None;
        self.cancel_profile_work();
        self.invalidate_graph_work(EngineError::new(
            ErrorCode::OperationCancelled,
            "graph work superseded",
        ));
        self.finish_pending(
            ReceiptStatus::Cancelled,
            Some(EngineError::new(
                ErrorCode::OperationCancelled,
                "operation superseded",
            )),
            None,
        );
        self.state.operation = Some(command.request_id.clone());
        self.playback_intent = PlaybackIntent::Playing;
        self.log(Some(command.request_id.clone()));
        Ok(ReceiptStatus::Accepted)
    }
    /// Ends session ownership after a stop or an owner shutdown. The Host retires the workers.
    pub(super) fn release_sendspin(&mut self) {
        let Some(connection) = self
            .sendspin
            .connection
            .as_mut()
            .filter(|connection| connection.open)
        else {
            return;
        };
        connection.open = false;
        if let Some(observation) = &mut connection.observation {
            observation.phase = SendspinTransportPhase::Closed;
            self.state.observation = (*observation).into();
        }
        // A shutdown keeps other sessions. This source loses its connection now.
        if self.state.source == Some(SourceKind::Sendspin) {
            self.state.transport = TransportState::Idle;
            self.state.session_id = None;
            self.state.source = None;
            self.state.seekable = false;
        }
    }
    /// Applies one event from the playback worker's Sendspin source.
    /// The generation and epochs must name the connection that the current session owns.
    pub fn advance_sendspin(
        &mut self,
        generation: u64,
        event: SendspinSourceEvent,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        let (fence, was_prepared) = self
            .sendspin
            .connection
            .as_ref()
            .filter(|connection| connection.open)
            .map(|connection| (connection.fence, connection.prepared))
            .ok_or(STALE)?;
        if fence.generation != generation
            || fence.epochs != event.epochs
            || self.state.source != Some(SourceKind::Sendspin)
            || self.state.epochs.session != event.epochs.session
            || now_ns < self.state.monotonic_ns
        {
            return Err(STALE);
        }
        let observation = event.observation;
        let mut next = self.state.clone();
        next.monotonic_ns = now_ns;
        next.observation = observation.into();
        if observation.phase != SendspinTransportPhase::Closed {
            // The source refused a control and still runs.
            if event.error.is_some() {
                return Ok(());
            }
            let available = matches!(
                observation.phase,
                SendspinTransportPhase::AwaitingStream | SendspinTransportPhase::Streaming
            );
            let prepared = was_prepared || available;
            next.transport = match observation.phase {
                SendspinTransportPhase::Streaming => TransportState::Playing,
                _ if prepared => TransportState::Buffering,
                _ => TransportState::Preparing,
            };
            let changed = next.transport != self.state.transport || prepared != was_prepared;
            if changed {
                next.revision = increment(next.revision)?;
                next.event_sequence = self.next_session_event_sequence(false)?;
            }
            self.state = next;
            self.adopt_sendspin(observation, true, prepared);
            if prepared && !was_prepared {
                self.finish_pending(ReceiptStatus::Applied, None, None);
            }
            if changed {
                self.log(None);
            }
            return Ok(());
        }
        // A terminal event must settle even when a counter is exhausted.
        next.revision = next.revision.saturating_add(1);
        next.event_sequence = next.event_sequence.saturating_add(1);
        next.epochs.stream = next.epochs.stream.saturating_add(1);
        next.muted = true;
        next.session_id = None;
        next.source = None;
        next.seekable = false;
        let (status, error) = match event.error {
            Some(code) => {
                next.transport = TransportState::Failed;
                next.error = Some(code);
                if code == ErrorCode::OutputFault {
                    next.output_consented = false;
                    next.volume_policy.disarm();
                }
                (
                    ReceiptStatus::Failed,
                    EngineError::new(
                        code,
                        "Sendspin source failed; explicit recovery is required",
                    ),
                )
            }
            None => {
                next.transport = TransportState::Idle;
                (
                    ReceiptStatus::Cancelled,
                    EngineError::new(ErrorCode::OperationCancelled, "Sendspin source closed"),
                )
            }
        };
        self.state = next;
        self.sendspin.error = event.error;
        self.adopt_sendspin(observation, false, was_prepared);
        self.finish_pending(status, Some(error), None);
        self.log(None);
        Ok(())
    }
    fn adopt_sendspin(
        &mut self,
        observation: SendspinSourceObservation,
        open: bool,
        prepared: bool,
    ) {
        if let Some(connection) = &mut self.sendspin.connection {
            connection.observation = Some(observation);
            connection.open = open;
            connection.prepared = prepared;
        }
    }
    /// Adopts frame progress without a revision. Phase changes arrive only as events.
    pub fn observe_sendspin(
        &mut self,
        generation: u64,
        observation: SendspinSourceObservation,
    ) -> Result<(), EngineError> {
        let connection = self
            .sendspin
            .connection
            .as_mut()
            .filter(|connection| connection.open && connection.fence.generation == generation)
            .ok_or(STALE)?;
        if connection.observation.map(|current| current.phase) != Some(observation.phase) {
            return Err(STALE);
        }
        connection.observation = Some(observation);
        self.state.observation = observation.into();
        Ok(())
    }
    /// The Host calls this after it joined the Network worker and the credential supervisor.
    pub fn complete_sendspin_retirement(
        &mut self,
        generation: u64,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if self.retiring_sendspin_connection() != Some(generation)
            || now_ns < self.state.monotonic_ns
        {
            return Err(STALE);
        }
        self.sendspin.connection = None;
        self.state.monotonic_ns = now_ns;
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.event_sequence = self.state.event_sequence.saturating_add(1);
        self.log(None);
        Ok(())
    }
}
