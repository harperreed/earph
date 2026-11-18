// ABOUTME: Owns the in-memory session state and bounded controller history.
// ABOUTME: Applies safety and epoch rules before committing any state change.
use crate::{
    EngineError, ErrorCode,
    control::*,
    ids::{increment, *},
    queue::{LocalQueue, QueueSnapshot},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

const STALE: EngineError =
    EngineError::new(ErrorCode::StaleSession, "session or zone target changed");
const STATE: EngineError = EngineError::new(
    ErrorCode::CapabilityUnsupported,
    "operation unavailable in current state",
);
const OUTPUT: EngineError = EngineError::new(
    ErrorCode::OutputFault,
    "output stopped; explicit recovery required",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportState {
    Idle,
    Preparing,
    Buffering,
    Playing,
    Paused,
    Reconfiguring,
    Draining,
    Suspended,
    Failed,
}
impl TransportState {
    fn accepts_source_events(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Buffering
                | Self::Playing
                | Self::Paused
                | Self::Reconfiguring
                | Self::Draining
        )
    }
}

#[derive(Clone, Copy)]
enum PlaybackIntent {
    Playing,
    Paused,
}
impl PlaybackIntent {
    fn transport(self) -> TransportState {
        match self {
            Self::Playing => TransportState::Playing,
            Self::Paused => TransportState::Paused,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SessionSnapshot {
    pub zone_id: ZoneId,
    pub owner_id: Option<OwnerId>,
    pub session_id: Option<SessionId>,
    pub source: Option<SourceKind>,
    pub output_id: Option<DeviceId>,
    pub transport: TransportState,
    pub muted: bool,
    pub software_gain_db: f64,
    pub epochs: Epochs,
    pub revision: u64,
    pub graph_epoch: u64,
    pub event_sequence: u64,
    pub monotonic_ns: u64,
    pub audible_position_frames: u64,
    pub decoded_position_frames: u64,
    pub operation: Option<RequestId>,
    pub error: Option<ErrorCode>,
    pub seekable: bool,
    pub shutdown_requested: bool,
}
#[derive(Debug, Clone)]
pub struct EngineEvent {
    pub epochs: Epochs,
    pub monotonic_ns: u64,
    pub kind: EngineEventKind,
}
#[derive(Debug, Clone)]
pub enum EngineEventKind {
    Prepared,
    Started { activation_frame: u64 },
    Decoded { position_frames: u64 },
    Consumed { frames: u64 },
    EndOfInput,
    Drained,
    Discontinuity,
    OutputReplaced { device_id: DeviceId },
    OutputFailed,
    Cancelled { request_id: RequestId },
}
#[derive(Debug, Clone, Serialize)]
pub struct StateEvent {
    pub sequence: u64,
    pub revision: u64,
    pub epochs: Epochs,
    pub monotonic_ns: u64,
    pub request_id: Option<RequestId>,
    pub transport: TransportState,
    pub muted: bool,
    pub error: Option<ErrorCode>,
}
#[derive(Debug, Serialize)]
pub struct EventBatch {
    pub gap: bool,
    pub events: Vec<StateEvent>,
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCheckpoint {
    pub schema_version: u32,
    pub revision: u64,
    pub queue: QueueSnapshot,
}

#[derive(Clone)]
pub struct Session {
    state: SessionSnapshot,
    limits: ControlLimits,
    receipts: VecDeque<(CommandEnvelope, Receipt)>,
    events: VecDeque<StateEvent>,
    playback_intent: PlaybackIntent,
    queue: LocalQueue,
}
impl Session {
    pub fn new(zone_id: ZoneId, limits: ControlLimits) -> Self {
        Self {
            state: SessionSnapshot {
                zone_id,
                owner_id: None,
                session_id: None,
                source: None,
                output_id: None,
                transport: TransportState::Idle,
                muted: true,
                software_gain_db: -40.0,
                epochs: Epochs::default(),
                revision: 0,
                graph_epoch: 0,
                event_sequence: 0,
                monotonic_ns: 0,
                audible_position_frames: 0,
                decoded_position_frames: 0,
                operation: None,
                error: None,
                seekable: false,
                shutdown_requested: false,
            },
            receipts: VecDeque::with_capacity(limits.receipts),
            events: VecDeque::with_capacity(limits.events),
            limits,
            playback_intent: PlaybackIntent::Playing,
            queue: LocalQueue::default(),
        }
    }
    pub fn new_owner(zone_id: ZoneId, owner_id: OwnerId, limits: ControlLimits) -> Self {
        let mut session = Self::new(zone_id, limits);
        session.state.owner_id = Some(owner_id);
        session
    }
    pub fn restore_owner(
        zone_id: ZoneId,
        owner_id: OwnerId,
        limits: ControlLimits,
        checkpoint: SessionCheckpoint,
    ) -> Result<Self, EngineError> {
        if checkpoint.schema_version != 1 {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported checkpoint version",
            ));
        }
        let mut session = Self::new_owner(zone_id, owner_id, limits);
        session.queue = LocalQueue::restore(checkpoint.queue)?;
        session.state.revision = checkpoint.revision;
        session.state.transport = TransportState::Paused;
        session.playback_intent = PlaybackIntent::Paused;
        Ok(session)
    }
    pub fn queue_snapshot(&self) -> QueueSnapshot {
        self.queue.snapshot()
    }
    pub fn checkpoint(&self) -> SessionCheckpoint {
        SessionCheckpoint {
            schema_version: 1,
            revision: self.state.revision,
            queue: self.queue.snapshot(),
        }
    }
    pub fn shutdown_requested(&self) -> bool {
        self.state.shutdown_requested
    }
    pub fn snapshot(&self) -> SessionSnapshot {
        self.state.clone()
    }
    pub fn events_since(&self, cursor: u64) -> EventBatch {
        let oldest = self
            .events
            .front()
            .map_or(self.state.event_sequence.saturating_add(1), |e| e.sequence);
        EventBatch {
            gap: cursor > self.state.event_sequence || cursor.saturating_add(1) < oldest,
            events: self
                .events
                .iter()
                .filter(|e| e.sequence > cursor)
                .cloned()
                .collect(),
            snapshot: self.snapshot(),
        }
    }
    fn receipt(
        &self,
        c: &CommandEnvelope,
        status: ReceiptStatus,
        error: Option<EngineError>,
    ) -> Receipt {
        Receipt {
            request_id: c.request_id.clone(),
            zone_id: c.zone_id.clone(),
            session_id: c.session_id.clone(),
            status,
            error,
            revision: self.state.revision,
            epochs: self.state.epochs,
            activation_frame: None,
            result: if status == ReceiptStatus::Applied {
                match c.payload {
                    Command::State => Some(QueryResult::State(self.snapshot())),
                    Command::QueueGet { offset, limit } => {
                        self.queue.page(offset, limit).ok().map(QueryResult::Queue)
                    }
                    Command::Capabilities => Some(QueryResult::Capabilities(Capabilities {
                        schema_version: 1,
                        sources: if self.state.owner_id.is_some() {
                            vec![]
                        } else {
                            vec![SourceKind::Virtual]
                        },
                        physical_output: false,
                    })),
                    _ => None,
                }
            } else {
                None
            },
        }
    }
    fn log(&mut self, request_id: Option<RequestId>) {
        if self.events.len() == self.limits.events {
            self.events.pop_front();
        }
        self.events.push_back(StateEvent {
            sequence: self.state.event_sequence,
            revision: self.state.revision,
            epochs: self.state.epochs,
            monotonic_ns: self.state.monotonic_ns,
            request_id,
            transport: self.state.transport,
            muted: self.state.muted,
            error: self.state.error,
        });
    }
    fn finish_pending(
        &mut self,
        status: ReceiptStatus,
        error: Option<EngineError>,
        activation_frame: Option<u64>,
    ) {
        if let Some(id) = self.state.operation.take()
            && let Some((_, receipt)) = self.receipts.iter_mut().find(|(c, _)| c.request_id == id)
        {
            receipt.status = status;
            receipt.error = error;
            receipt.activation_frame = activation_frame;
            receipt.revision = self.state.revision;
            receipt.epochs = self.state.epochs;
        }
    }
    pub fn submit(&mut self, command: CommandEnvelope) -> Receipt {
        self.submit_with_commit(command, |_| Ok(()))
    }
    /// Publishes a staged mutation only after the durable commit succeeds.
    pub fn submit_with_commit(
        &mut self,
        command: CommandEnvelope,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Receipt {
        self.submit_with_queue_expansion(command, |_| Err(STATE), commit)
    }
    /// Resolves a directory only after target, revision, and retry validation.
    pub fn submit_with_queue_expansion(
        &mut self,
        command: CommandEnvelope,
        expand: impl FnOnce(&str) -> Result<Vec<String>, EngineError>,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Receipt {
        if let Some((prior, receipt)) = self
            .receipts
            .iter()
            .find(|(c, _)| c.request_id == command.request_id)
        {
            return if prior == &command {
                receipt.clone()
            } else {
                self.receipt(
                    &command,
                    ReceiptStatus::Rejected,
                    Some(EngineError::new(
                        ErrorCode::AssetInvalid,
                        "request ID reused for different content",
                    )),
                )
            };
        }
        let mut staged = self.clone();
        let result = staged.apply_command(&command, expand);
        let receipt = match result {
            Ok(status) => {
                let persisted = if staged.state.revision != self.state.revision {
                    commit(&staged)
                } else {
                    Ok(())
                };
                match persisted {
                    Ok(()) => {
                        *self = staged;
                        self.receipt(&command, status, None)
                    }
                    Err(error) => self.receipt(&command, ReceiptStatus::Failed, Some(error)),
                }
            }
            Err(error) => self.receipt(&command, ReceiptStatus::Rejected, Some(error)),
        };
        if self.receipts.len() == self.limits.receipts {
            self.receipts.pop_front();
        }
        self.receipts.push_back((command, receipt.clone()));
        receipt
    }
    fn apply_command(
        &mut self,
        command: &CommandEnvelope,
        expand: impl FnOnce(&str) -> Result<Vec<String>, EngineError>,
    ) -> Result<ReceiptStatus, EngineError> {
        command.validate()?;
        if command.zone_id != self.state.zone_id {
            return Err(STALE);
        }
        if !command.payload.is_query()
            && self.state.owner_id.is_some()
            && command.owner_id != self.state.owner_id
        {
            return Err(STALE);
        }
        if matches!(command.payload, Command::Mute { .. })
            && command.session_id.is_some()
            && command.session_id != self.state.session_id
        {
            return Err(STALE);
        }
        if !matches!(
            command.payload,
            Command::Start { .. }
                | Command::Mute { .. }
                | Command::State
                | Command::Capabilities
                | Command::QueueGet { .. }
                | Command::QueueAppend { .. }
                | Command::QueueAppendDirectory { .. }
                | Command::QueueRemove { .. }
                | Command::QueueMove { .. }
                | Command::QueueSelect { .. }
                | Command::OwnerShutdown
        ) && (command.session_id.is_none() || command.session_id != self.state.session_id)
        {
            return Err(STALE);
        }
        if command.payload.is_query() {
            if let Command::QueueGet { offset, limit } = command.payload {
                self.queue.page(offset, limit)?;
            }
            return Ok(ReceiptStatus::Applied);
        }
        if !matches!(command.payload, Command::Mute { muted: true })
            && command.expected_revision != Some(self.state.revision)
        {
            return Err(EngineError::new(
                ErrorCode::StaleRevision,
                "refresh expected revision",
            ));
        }
        let mut next = self.state.clone();
        next.revision = increment(next.revision)?;
        next.event_sequence = increment(next.event_sequence)?;
        let mut intent = self.playback_intent;
        let mut cancel_previous = false;
        let status = match &command.payload {
            Command::QueueAppendDirectory { path } => {
                let paths = expand(path)?;
                if paths.len() > 256 || self.queue.entries().len().saturating_add(paths.len()) > 256
                {
                    return Err(EngineError::new(
                        ErrorCode::ResourceBudgetExceeded,
                        "directory exceeds queue capacity",
                    ));
                }
                for path in paths {
                    self.queue.append(path)?;
                }
                ReceiptStatus::Applied
            }
            Command::QueueAppend { path } => {
                self.queue.append(path.clone())?;
                ReceiptStatus::Applied
            }
            Command::QueueRemove { id } => {
                self.queue.remove(id)?;
                ReceiptStatus::Applied
            }
            Command::QueueMove { id, new_index } => {
                self.queue.move_entry(id, *new_index)?;
                ReceiptStatus::Applied
            }
            Command::QueueSelect { id } => {
                self.queue.select(id.clone())?;
                ReceiptStatus::Applied
            }
            Command::OwnerShutdown if next.owner_id.is_some() => {
                next.shutdown_requested = true;
                next.muted = true;
                next.transport = TransportState::Paused;
                next.epochs = next.epochs.next_stream()?;
                cancel_previous = true;
                ReceiptStatus::Applied
            }
            Command::Start {
                source,
                device_id,
                takeover,
                seekable,
            } => {
                if self.state.owner_id.is_some() {
                    return Err(STATE);
                }
                if self.state.session_id.is_some() && !takeover {
                    return Err(EngineError::new(
                        ErrorCode::SourceBusy,
                        "explicit takeover required",
                    ));
                }
                next.session_id = Some(command.session_id.clone().ok_or(STALE)?);
                next.source = Some(*source);
                next.output_id = Some(device_id.clone());
                next.seekable = *seekable;
                next.epochs = next.epochs.next_session()?;
                next.transport = TransportState::Preparing;
                next.muted = true;
                next.audible_position_frames = 0;
                next.decoded_position_frames = 0;
                next.error = None;
                intent = PlaybackIntent::Playing;
                cancel_previous = true;
                ReceiptStatus::Accepted
            }
            Command::Stop => {
                next.epochs = next.epochs.next_stream()?;
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                cancel_previous = true;
                ReceiptStatus::Applied
            }
            Command::Pause if next.transport == TransportState::Playing => {
                next.transport = TransportState::Paused;
                intent = PlaybackIntent::Paused;
                ReceiptStatus::Applied
            }
            Command::Resume if next.transport == TransportState::Paused => {
                next.transport = TransportState::Buffering;
                intent = PlaybackIntent::Playing;
                ReceiptStatus::Accepted
            }
            Command::Seek { position_frames }
                if next.seekable
                    && matches!(
                        next.transport,
                        TransportState::Playing
                            | TransportState::Paused
                            | TransportState::Buffering
                    ) =>
            {
                next.epochs = next.epochs.next_stream()?;
                next.transport = TransportState::Buffering;
                next.audible_position_frames = *position_frames;
                next.decoded_position_frames = *position_frames;
                cancel_previous = true;
                ReceiptStatus::Accepted
            }
            Command::Mute { muted } => {
                next.muted = *muted;
                ReceiptStatus::Applied
            }
            _ => return Err(STATE),
        };
        self.state = next;
        if cancel_previous {
            self.finish_pending(
                ReceiptStatus::Cancelled,
                Some(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "operation superseded",
                )),
                None,
            );
        }
        if status == ReceiptStatus::Accepted {
            self.state.operation = Some(command.request_id.clone());
        }
        self.playback_intent = intent;
        self.log(Some(command.request_id.clone()));
        Ok(status)
    }
    pub fn advance(&mut self, event: EngineEvent) -> Result<(), EngineError> {
        if event.epochs != self.state.epochs {
            return Err(STALE);
        }
        if event.monotonic_ns < self.state.monotonic_ns {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "non-monotonic event time",
            ));
        }
        let mut next = self.state.clone();
        next.monotonic_ns = event.monotonic_ns;
        if !matches!(
            event.kind,
            EngineEventKind::Decoded { .. } | EngineEventKind::Consumed { .. }
        ) {
            next.revision = increment(next.revision)?;
        }
        next.event_sequence = increment(next.event_sequence)?;
        let mut completion = None;
        match event.kind {
            EngineEventKind::Prepared
                if matches!(
                    next.transport,
                    TransportState::Preparing | TransportState::Reconfiguring
                ) =>
            {
                next.transport = TransportState::Buffering
            }
            EngineEventKind::Started { activation_frame }
                if next.transport == TransportState::Buffering =>
            {
                next.transport = self.playback_intent.transport();
                completion = Some((ReceiptStatus::Applied, None, Some(activation_frame)));
            }
            EngineEventKind::Decoded { position_frames }
                if next.session_id.is_some()
                    && next.transport.accepts_source_events()
                    && position_frames >= next.decoded_position_frames =>
            {
                next.decoded_position_frames = position_frames
            }
            EngineEventKind::Consumed { frames }
                if matches!(
                    next.transport,
                    TransportState::Playing | TransportState::Draining
                ) =>
            {
                next.audible_position_frames = next
                    .audible_position_frames
                    .checked_add(frames)
                    .ok_or(STATE)?;
                if next.audible_position_frames > next.decoded_position_frames {
                    return Err(STATE);
                }
            }
            EngineEventKind::EndOfInput if next.transport == TransportState::Playing => {
                next.transport = TransportState::Draining
            }
            EngineEventKind::Drained if next.transport == TransportState::Draining => {
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                next.epochs = next.epochs.next_stream()?;
            }
            EngineEventKind::Discontinuity
                if next.session_id.is_some() && next.transport.accepts_source_events() =>
            {
                next.transport = TransportState::Reconfiguring;
                next.muted = true;
                next.epochs = next.epochs.next_stream()?;
                completion = Some((
                    ReceiptStatus::Cancelled,
                    Some(EngineError::new(
                        ErrorCode::OperationCancelled,
                        "stream discontinuity",
                    )),
                    None,
                ));
            }
            EngineEventKind::OutputReplaced { device_id } => {
                next.transport = TransportState::Suspended;
                next.muted = true;
                next.output_id = Some(device_id);
                next.epochs = next.epochs.next_stream()?;
                completion = Some((ReceiptStatus::Cancelled, Some(OUTPUT), None));
            }
            EngineEventKind::OutputFailed => {
                next.transport = TransportState::Failed;
                next.muted = true;
                next.error = Some(ErrorCode::OutputFault);
                next.epochs = next.epochs.next_stream()?;
                completion = Some((ReceiptStatus::Failed, Some(OUTPUT), None));
            }
            EngineEventKind::Cancelled { request_id }
                if next.operation.as_ref() == Some(&request_id) =>
            {
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                next.epochs = next.epochs.next_stream()?;
                completion = Some((
                    ReceiptStatus::Cancelled,
                    Some(EngineError::new(
                        ErrorCode::OperationCancelled,
                        "operation cancelled",
                    )),
                    None,
                ));
            }
            _ => return Err(STATE),
        }
        let request_id = self.state.operation.clone();
        self.state = next;
        if let Some((status, error, activation)) = completion {
            self.finish_pending(status, error, activation);
        }
        self.log(request_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_revisions_do_not_partially_change_mute() {
        let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
        session.state.revision = u64::MAX;
        let before = session.snapshot();
        let receipt = session.submit(CommandEnvelope {
            schema_version: 1,
            owner_id: None,
            request_id: RequestId::new("mute").unwrap(),
            zone_id: before.zone_id.clone(),
            session_id: None,
            expected_revision: Some(before.revision),
            payload: Command::Mute { muted: false },
        });
        assert_eq!(
            receipt.error.unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(session.snapshot(), before);
    }
}
