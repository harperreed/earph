// ABOUTME: Owns the in-memory session state and bounded controller history.
// ABOUTME: Applies safety and epoch rules before committing any state change.
use crate::{
    EngineError, ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
    control::*,
    graph_control::*,
    ids::{increment, *},
    queue::{LocalQueue, QueueSnapshot},
    safety::{FidelityPolicy, OutputContext, SampleDomain, VolumePolicy},
};
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::Arc};

#[path = "session_graph.rs"]
mod graph;
use graph::GraphControl;

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
#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalBoundary {
    Pause,
    Seek,
    Stop,
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
    pub device_epoch: u64,
    pub event_sequence: u64,
    pub monotonic_ns: u64,
    pub audible_position_frames: Option<u64>,
    pub decoded_position_frames: u64,
    pub operation: Option<RequestId>,
    pub error: Option<ErrorCode>,
    pub seekable: bool,
    pub shutdown_requested: bool,
    pub output: Option<OutputConfiguration>,
    pub output_consented: bool,
    pub volume_policy: VolumePolicy,
    pub gain_ramp_frames: u32,
    pub active_entry_id: Option<QueueEntryId>,
    pub reserved_next_entry_id: Option<QueueEntryId>,
    pub playback: Option<PlaybackObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybackObservation {
    pub processing: Option<PlaybackProcessingObservation>,
    pub format: PcmFormat,
    pub submitted_frames: u64,
    pub consumed_frames: Option<u64>,
    pub backend_delay_frames: Option<u64>,
    pub consumption_basis: &'static str,
    pub source_entry_id: QueueEntryId,
    pub source_position_frames: u64,
    pub audible_entry_id: Option<QueueEntryId>,
    pub audible_source_position_frames: Option<u64>,
    pub input_peak: Option<f64>,
    pub output_peak: Option<f64>,
    pub modified: bool,
    pub backend: Option<BackendObservation>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PlaybackProcessingObservation {
    pub resident_bytes: u64,
    pub audio_payload_bytes: usize,
    pub failure_node: Option<NodeId>,
    pub graph_hash: String,
    #[serde(flatten)]
    pub timing: PlaybackTiming,
    pub saturation_count: u64,
    pub graph_peak: Option<f64>,
    pub prequantization_peak: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedSampleRepresentation {
    Signed16,
    Signed24,
    Signed32,
    Float32,
    Float64,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservedStreamFormat {
    pub sample_rate_hz: f64,
    pub channels: u32,
    pub representation: ObservedSampleRepresentation,
    pub bytes_per_frame: u32,
    pub interleaved: bool,
}
impl From<PcmFormat> for ObservedStreamFormat {
    fn from(format: PcmFormat) -> Self {
        Self {
            sample_rate_hz: f64::from(format.sample_rate_hz()),
            channels: format.channels().count() as u32,
            representation: match format.representation() {
                PcmRepresentation::Signed16 => ObservedSampleRepresentation::Signed16,
                PcmRepresentation::Signed24 => ObservedSampleRepresentation::Signed24,
                PcmRepresentation::Signed32 => ObservedSampleRepresentation::Signed32,
                PcmRepresentation::Float64 => ObservedSampleRepresentation::Float64,
            },
            bytes_per_frame: (format.channels().count() * format.representation().bytes()) as u32,
            interleaved: true,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreservationState {
    Unverified,
    Modified,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackendObservation {
    pub backend: OutputBackend,
    pub client_format: ObservedStreamFormat,
    pub device_format: Option<ObservedStreamFormat>,
    pub measurement_boundary: &'static str,
    pub preservation: PreservationState,
}
#[derive(Debug, Clone)]
pub struct EngineEvent {
    pub epochs: Epochs,
    pub monotonic_ns: u64,
    pub kind: EngineEventKind,
}
#[derive(Debug, Clone)]
pub enum EngineEventKind {
    LocalPrepared {
        observation: PlaybackObservation,
    },
    LocalProgress {
        observation: PlaybackObservation,
    },
    LocalReserved {
        active: QueueEntryId,
        next: Option<QueueEntryId>,
    },
    LocalPaused {
        observation: PlaybackObservation,
    },
    LocalStopped {
        observation: Option<PlaybackObservation>,
    },
    LocalCompleted {
        observation: PlaybackObservation,
    },
    LocalFailed {
        code: ErrorCode,
    },
    Prepared,
    Started {
        activation_frame: u64,
    },
    Decoded {
        position_frames: u64,
    },
    Consumed {
        frames: u64,
    },
    EndOfInput,
    Drained,
    Discontinuity,
    OutputReplaced {
        device_id: DeviceId,
    },
    OutputFailed,
    Cancelled {
        request_id: RequestId,
    },
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
    #[serde(default)]
    pub volume_policy: VolumePolicy,
    #[serde(default)]
    pub output: Option<OutputConfiguration>,
    #[serde(default)]
    pub graph_intent: Option<Arc<GraphIntent>>,
}

#[derive(Clone)]
pub struct Session {
    state: SessionSnapshot,
    limits: ControlLimits,
    receipts: VecDeque<Arc<(CommandEnvelope, Receipt)>>,
    events: VecDeque<StateEvent>,
    playback_intent: PlaybackIntent,
    queue: LocalQueue,
    local_playback_enabled: bool,
    local_boundary: Option<LocalBoundary>,
    graph: GraphControl,
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
                device_epoch: 0,
                event_sequence: 0,
                monotonic_ns: 0,
                audible_position_frames: Some(0),
                decoded_position_frames: 0,
                operation: None,
                error: None,
                seekable: false,
                shutdown_requested: false,
                output: None,
                output_consented: false,
                volume_policy: VolumePolicy::default(),
                gain_ramp_frames: 0,
                active_entry_id: None,
                reserved_next_entry_id: None,
                playback: None,
            },
            receipts: VecDeque::with_capacity(limits.receipts),
            events: VecDeque::with_capacity(limits.events),
            limits,
            playback_intent: PlaybackIntent::Playing,
            queue: LocalQueue::default(),
            local_playback_enabled: false,
            local_boundary: None,
            graph: GraphControl::default(),
        }
    }
    pub fn new_owner(zone_id: ZoneId, owner_id: OwnerId, limits: ControlLimits) -> Self {
        let mut session = Self::new(zone_id, limits);
        session.state.owner_id = Some(owner_id);
        session.state.audible_position_frames = None;
        session
    }
    pub fn enable_local_playback(&mut self) {
        self.local_playback_enabled = true;
    }
    pub fn wants_playback(&self) -> bool {
        matches!(self.playback_intent, PlaybackIntent::Playing)
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
        // Revisions belong to one owner. A new owner can reseed an exhausted revision.
        session.state.revision = if checkpoint.revision == u64::MAX {
            0
        } else {
            checkpoint.revision
        };
        if let Some(output) = &checkpoint.output {
            output.validate()?;
        }
        session.state.output = checkpoint.output;
        if let Some(intent) = checkpoint.graph_intent {
            intent.graph.validate_structure()?;
            session.graph.configured = Some(intent);
        }
        session.state.volume_policy = checkpoint.volume_policy;
        session.state.volume_policy.disarm();
        session.state.software_gain_db =
            (-40.0_f64).min(session.state.volume_policy.software_cap_db());
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
            volume_policy: self.state.volume_policy.clone(),
            output: self.state.output.clone(),
            graph_intent: self.graph.configured.clone(),
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
                    Command::State => Some(QueryResult::State(Box::new(self.snapshot()))),
                    Command::QueueGet { offset, limit } => {
                        self.queue.page(offset, limit).ok().map(QueryResult::Queue)
                    }
                    Command::Capabilities => Some(QueryResult::Capabilities(Capabilities {
                        schema_version: 1,
                        sources: if self.local_playback_enabled {
                            vec![SourceKind::LocalFile]
                        } else if self.state.owner_id.is_some() {
                            vec![]
                        } else {
                            vec![SourceKind::Virtual]
                        },
                        physical_output: self.local_playback_enabled,
                    })),
                    Command::GraphInspect {} => {
                        Some(QueryResult::GraphInspection(self.graph_inspection()))
                    }
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
        self.local_boundary = None;
        if let Some(id) = self.state.operation.take()
            && let Some(record) = self
                .receipts
                .iter_mut()
                .find(|record| record.0.request_id == id)
        {
            let receipt = &mut Arc::make_mut(record).1;
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
        self.submit_with_graph_effects(command, self.state.monotonic_ns, expand, commit)
    }
    /// Admits graph jobs through the same staged session and durable mutation hook.
    pub fn submit_with_graph_effects(
        &mut self,
        command: CommandEnvelope,
        now_ns: u64,
        expand: impl FnOnce(&str) -> Result<Vec<String>, EngineError>,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Receipt {
        self.expire_graph_work(now_ns);
        if let Some(record) = self.live_graph_receipt(&command.request_id) {
            return self.replay_graph_record(&command, record);
        }
        if let Some(record) = self
            .receipts
            .iter()
            .find(|record| record.0.request_id == command.request_id)
        {
            return if record.0 == command {
                record.1.clone()
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
        // Do not retain structurally oversized intent, including rejected commands.
        if let Command::GraphPrepare { graph, .. } | Command::GraphValidate { graph, .. } =
            &command.payload
            && let Err(error) = graph.validate_storage()
        {
            return self.receipt(&command, ReceiptStatus::Rejected, Some(error));
        }
        let result = staged.apply_command(&command, now_ns, expand);
        let receipt = match result {
            Ok(status) => {
                let persisted = if staged.state.revision != self.state.revision
                    || matches!(
                        command.payload,
                        Command::GraphPrepare { .. } | Command::GraphValidate { .. }
                    ) {
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
        let record = Arc::new((command, receipt.clone()));
        self.retain_graph_record(record.clone());
        self.receipts.push_back(record);
        receipt
    }
    fn apply_command(
        &mut self,
        command: &CommandEnvelope,
        now_ns: u64,
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
                | Command::GraphInspect { .. }
                | Command::QueueAppend { .. }
                | Command::QueueAppendDirectory { .. }
                | Command::QueueRemove { .. }
                | Command::QueueMove { .. }
                | Command::QueueSelect { .. }
                | Command::OwnerShutdown
                | Command::OutputSelect { .. }
                | Command::OutputConsent { .. }
                | Command::VolumeSet { .. }
                | Command::VolumeCaps { .. }
                | Command::FixedArm { .. }
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
        self.validate_graph_control(&command.payload)?;
        if matches!(
            command.payload,
            Command::GraphPrepare { .. }
                | Command::GraphValidate { .. }
                | Command::GraphCommit { .. }
        ) {
            return self.apply_graph_command(command, now_ns);
        }
        self.validate_live_queue_edit(&command.payload)?;
        let mut next = self.state.clone();
        next.revision = increment(next.revision)?;
        next.event_sequence = self.next_session_event_sequence(matches!(
            command.payload,
            Command::Stop | Command::OwnerShutdown
        ))?;
        let mut intent = self.playback_intent;
        let mut cancel_previous = false;
        let status = match &command.payload {
            Command::OutputSelect { output } => {
                if !self.local_playback_enabled {
                    return Err(STATE);
                }
                if self.state.session_id.is_some()
                    && !matches!(
                        self.state.transport,
                        TransportState::Idle | TransportState::Failed | TransportState::Suspended
                    )
                {
                    return Err(EngineError::new(
                        ErrorCode::SourceBusy,
                        "stop the active session before changing the output",
                    ));
                }
                output.validate()?;
                next.output_id = Some(output.device_id.clone());
                next.device_epoch = increment(next.device_epoch)?;
                next.output = Some(output.clone());
                next.playback = None;
                next.audible_position_frames = None;
                next.decoded_position_frames = 0;
                next.output_consented = false;
                next.volume_policy.disarm();
                next.muted = true;
                ReceiptStatus::Applied
            }
            Command::OutputConsent {
                device_id,
                profile_id,
            } => {
                let output = next.output.as_ref().ok_or(STATE)?;
                if &output.device_id != device_id || &output.profile_id != profile_id {
                    return Err(STALE);
                }
                next.output_consented = true;
                ReceiptStatus::Applied
            }
            Command::FixedArm {
                device_id,
                profile_id,
            } => {
                let output = next.output.as_ref().ok_or(STATE)?;
                if &output.device_id != device_id || &output.profile_id != profile_id {
                    return Err(STALE);
                }
                if !next.output_consented {
                    return Err(EngineError::new(
                        ErrorCode::SafetyPolicyBlocked,
                        "approve the current output before arming fixed gain",
                    ));
                }
                next.volume_policy.arm_fixed(OutputContext {
                    device_id: device_id.clone(),
                    profile_id: profile_id.clone(),
                });
                ReceiptStatus::Applied
            }
            Command::VolumeCaps {
                software_millidecibels,
                hardware_millidecibels,
            } => {
                next.volume_policy = VolumePolicy::new(
                    f64::from(*software_millidecibels) / 1000.0,
                    hardware_millidecibels.map(|value| f64::from(value) / 1000.0),
                )?;
                next.software_gain_db = next
                    .software_gain_db
                    .min(next.volume_policy.software_cap_db());
                next.muted = true;
                ReceiptStatus::Applied
            }
            Command::VolumeSet {
                millidecibels,
                ramp_frames,
            } => {
                let gain = f64::from(*millidecibels) / 1000.0;
                if !(-120.0..=0.0).contains(&gain)
                    || gain > next.volume_policy.software_cap_db()
                    || *ramp_frames > 48000
                {
                    return Err(EngineError::new(
                        ErrorCode::SafetyPolicyBlocked,
                        "software gain or ramp exceeds the current cap",
                    ));
                }
                next.software_gain_db = gain;
                if let Some(active) = &self.graph.active {
                    active.validate_headroom(gain, next.volume_policy.software_cap_db())?;
                }
                next.gain_ramp_frames = *ramp_frames;
                if !next.muted {
                    Self::validate_local_unmute(&next)?;
                }
                ReceiptStatus::Applied
            }
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
                if *source == SourceKind::LocalFile {
                    if !self.local_playback_enabled {
                        return Err(STATE);
                    }
                    let output = self.state.output.as_ref().ok_or(EngineError::new(
                        ErrorCode::SafetyPolicyBlocked,
                        "select and approve an output before preparing playback",
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
                    let id = self
                        .queue
                        .selected()
                        .cloned()
                        .or_else(|| self.queue.entries().first().map(|entry| entry.id.clone()))
                        .ok_or(EngineError::new(
                            ErrorCode::AssetInvalid,
                            "select a nonempty local queue",
                        ))?;
                    self.queue.select(Some(id.clone()))?;
                    next.active_entry_id = Some(id);
                    next.reserved_next_entry_id = self
                        .queue
                        .entries()
                        .iter()
                        .position(|entry| Some(&entry.id) == next.active_entry_id.as_ref())
                        .and_then(|index| self.queue.entries().get(index + 1))
                        .map(|entry| entry.id.clone());
                    next.playback = None;
                } else if self.state.owner_id.is_some() {
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
                next.audible_position_frames = if *source == SourceKind::LocalFile {
                    None
                } else {
                    Some(0)
                };
                next.decoded_position_frames = 0;
                next.error = None;
                intent = PlaybackIntent::Playing;
                cancel_previous = true;
                ReceiptStatus::Accepted
            }
            Command::Stop => {
                next.epochs = next.epochs.next_stream()?;
                next.muted = true;
                cancel_previous = true;
                if next.source == Some(SourceKind::LocalFile) {
                    intent = PlaybackIntent::Paused;
                    next.transport = TransportState::Paused;
                    ReceiptStatus::Accepted
                } else {
                    next.transport = TransportState::Idle;
                    next.session_id = None;
                    next.source = None;
                    next.seekable = false;
                    next.active_entry_id = None;
                    next.reserved_next_entry_id = None;
                    ReceiptStatus::Applied
                }
            }
            Command::Pause
                if next.source == Some(SourceKind::LocalFile)
                    && matches!(
                        next.transport,
                        TransportState::Preparing
                            | TransportState::Buffering
                            | TransportState::Playing
                            | TransportState::Paused
                    ) =>
            {
                next.transport = TransportState::Paused;
                intent = PlaybackIntent::Paused;
                if next.playback.is_some() {
                    next.epochs = next.epochs.next_stream()?;
                    cancel_previous = true;
                    ReceiptStatus::Accepted
                } else {
                    ReceiptStatus::Applied
                }
            }
            Command::Pause if next.transport == TransportState::Playing => {
                next.transport = TransportState::Paused;
                intent = PlaybackIntent::Paused;
                ReceiptStatus::Applied
            }
            Command::Resume if next.transport == TransportState::Paused => {
                next.transport =
                    if next.source == Some(SourceKind::LocalFile) && next.playback.is_none() {
                        TransportState::Preparing
                    } else {
                        TransportState::Buffering
                    };
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
                if next.source == Some(SourceKind::LocalFile) {
                    let entry = self
                        .queue
                        .entries()
                        .iter()
                        .find(|entry| Some(&entry.id) == next.active_entry_id.as_ref())
                        .ok_or(STATE)?;
                    match entry.status {
                        crate::queue::PreparationStatus::Ready { total_frames, .. }
                            if *position_frames <= total_frames => {}
                        crate::queue::PreparationStatus::Ready { .. } => {
                            return Err(EngineError::new(
                                ErrorCode::AssetInvalid,
                                "seek exceeds the selected file's frame count",
                            ));
                        }
                        _ => return Err(STATE),
                    }
                }
                next.epochs = next.epochs.next_stream()?;
                next.transport = TransportState::Buffering;
                next.audible_position_frames = if next.source == Some(SourceKind::LocalFile) {
                    None
                } else {
                    Some(*position_frames)
                };
                next.decoded_position_frames = *position_frames;
                cancel_previous = true;
                ReceiptStatus::Accepted
            }
            Command::Mute { muted } => {
                if !muted && self.local_playback_enabled {
                    self.validate_graph_unmute(&next)?;
                    Self::validate_local_unmute(&next)?;
                }
                next.muted = *muted;
                ReceiptStatus::Applied
            }
            _ => return Err(STATE),
        };
        if matches!(
            command.payload,
            Command::QueueAppend { .. }
                | Command::QueueAppendDirectory { .. }
                | Command::QueueMove { .. }
                | Command::QueueRemove { .. }
        ) && next.source == Some(SourceKind::LocalFile)
        {
            next.reserved_next_entry_id = self
                .queue
                .entries()
                .iter()
                .position(|entry| Some(&entry.id) == next.active_entry_id.as_ref())
                .and_then(|index| self.queue.entries().get(index + 1))
                .map(|entry| entry.id.clone());
        }
        let clear_active = matches!(
            command.payload,
            Command::Start { .. }
                | Command::Stop
                | Command::OwnerShutdown
                | Command::OutputSelect { .. }
        );
        if self.graph.active.is_some()
            && next.muted
            && !self.state.muted
            && next.epochs == self.state.epochs
        {
            next.epochs = next.epochs.next_stream()?;
        }
        let invalidated =
            next.epochs != self.state.epochs || next.device_epoch != self.state.device_epoch;
        if clear_active {
            self.local_boundary = None;
        }
        self.state = next;
        if invalidated || clear_active {
            self.invalidate_graph_work(EngineError::new(
                ErrorCode::OperationCancelled,
                "graph work superseded",
            ));
        }
        if clear_active {
            self.graph.active = None;
        }
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
            self.local_boundary = match &command.payload {
                Command::Pause => Some(LocalBoundary::Pause),
                Command::Seek { .. } => Some(LocalBoundary::Seek),
                Command::Stop => Some(LocalBoundary::Stop),
                _ => None,
            };
        }
        self.playback_intent = intent;
        self.log(Some(command.request_id.clone()));
        Ok(status)
    }
    fn validate_local_unmute(state: &SessionSnapshot) -> Result<(), EngineError> {
        let blocked = EngineError::new(
            ErrorCode::SafetyPolicyBlocked,
            "select and approve the current output before unmuting",
        );
        let output = state.output.as_ref().ok_or(blocked)?;
        if !state.output_consented {
            return Err(blocked);
        }
        if output.fidelity == FidelityPolicy::StrictPreservation && state.gain_ramp_frames != 0 {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "strict unmute cannot apply a software gain ramp",
            ));
        }
        state.volume_policy.validate_unmute(
            &OutputContext {
                device_id: output.device_id.clone(),
                profile_id: output.profile_id.clone(),
            },
            output.volume_mode,
            SampleDomain::Pcm,
            output.fidelity,
            state.software_gain_db,
            None,
        )
    }
    fn validate_live_queue_edit(&self, command: &Command) -> Result<(), EngineError> {
        if self.state.source != Some(SourceKind::LocalFile)
            || self.state.session_id.is_none()
            || matches!(
                self.state.transport,
                TransportState::Idle | TransportState::Failed | TransportState::Suspended
            )
        {
            return Ok(());
        }
        let active = self.state.active_entry_id.as_ref().ok_or(STATE)?;
        let active_index = self
            .queue
            .entries()
            .iter()
            .position(|entry| &entry.id == active)
            .ok_or(STATE)?;
        let protected_index = self
            .state
            .reserved_next_entry_id
            .as_ref()
            .and_then(|id| {
                self.queue
                    .entries()
                    .iter()
                    .position(|entry| &entry.id == id)
            })
            .unwrap_or(active_index);
        let blocked = EngineError::new(
            ErrorCode::SourceBusy,
            "the active entry and reserved next entry cannot change during playback",
        );
        match command {
            Command::QueueRemove { id } | Command::QueueMove { id, .. } => {
                if self
                    .queue
                    .entries()
                    .iter()
                    .position(|entry| &entry.id == id)
                    .is_some_and(|index| index <= protected_index)
                {
                    return Err(blocked);
                }
                if matches!(command, Command::QueueMove { new_index, .. } if *new_index <= protected_index)
                {
                    return Err(blocked);
                }
            }
            Command::QueueSelect { id } if id.as_ref() != Some(active) => return Err(blocked),
            _ => {}
        }
        Ok(())
    }
    pub fn set_queue_preparation(
        &mut self,
        epochs: Epochs,
        id: &QueueEntryId,
        status: crate::queue::PreparationStatus,
    ) -> Result<(), EngineError> {
        if epochs != self.state.epochs {
            return Err(STALE);
        }
        let sequence = self.next_session_event_sequence(false)?;
        self.queue.update_status(id, status)?;
        self.state.event_sequence = sequence;
        Ok(())
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
        let proved_completion = matches!(event.kind, EngineEventKind::LocalCompleted { .. });
        let mut terminal_exhaustion = None;
        let mut next_counter = |value| match increment(value) {
            Ok(value) => Ok(value),
            Err(error) if proved_completion => {
                terminal_exhaustion = Some(error);
                Ok(value)
            }
            Err(error) => Err(error),
        };
        if !matches!(
            event.kind,
            EngineEventKind::Decoded { .. }
                | EngineEventKind::Consumed { .. }
                | EngineEventKind::LocalProgress { .. }
                | EngineEventKind::LocalReserved { .. }
        ) {
            next.revision = next_counter(next.revision)?;
        }
        next.event_sequence = if proved_completion {
            next_counter(next.event_sequence)?
        } else {
            self.next_session_event_sequence(matches!(
                event.kind,
                EngineEventKind::LocalFailed { .. }
                    | EngineEventKind::OutputFailed
                    | EngineEventKind::OutputReplaced { .. }
                    | EngineEventKind::Discontinuity
                    | EngineEventKind::Cancelled { .. }
                    | EngineEventKind::LocalStopped { .. }
                    | EngineEventKind::LocalCompleted { .. }
                    | EngineEventKind::Drained
            ))?
        };
        let mut completion = None;
        let mut graph_completion_error = None;
        match event.kind {
            EngineEventKind::LocalCompleted { observation }
                if next.source == Some(SourceKind::LocalFile)
                    && next.session_id.is_some()
                    && next.transport.accepts_source_events() =>
            {
                let ended = EngineError::new(
                    ErrorCode::CapabilityUnsupported,
                    "source ended before requested transition",
                );
                graph_completion_error = Some(ended);
                let positioning = matches!(
                    self.local_boundary,
                    Some(LocalBoundary::Pause | LocalBoundary::Seek)
                );
                completion = Some((
                    if positioning {
                        ReceiptStatus::Failed
                    } else {
                        ReceiptStatus::Applied
                    },
                    positioning.then_some(ended),
                    None,
                ));
                next.decoded_position_frames = observation.source_position_frames;
                next.audible_position_frames = observation.audible_source_position_frames;
                next.playback = Some(observation);
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                next.active_entry_id = None;
                next.reserved_next_entry_id = None;
                match next.epochs.next_stream() {
                    Ok(epochs) => next.epochs = epochs,
                    Err(error) => terminal_exhaustion = Some(error),
                }
            }
            EngineEventKind::LocalPrepared { observation }
                if next.source == Some(SourceKind::LocalFile) =>
            {
                next.audible_position_frames = observation.audible_source_position_frames;
                next.decoded_position_frames = observation.source_position_frames;
                next.playback = Some(observation);
                next.transport = if self.wants_playback() {
                    TransportState::Buffering
                } else {
                    TransportState::Paused
                };
                if !self.wants_playback() {
                    completion = Some((ReceiptStatus::Applied, None, None));
                }
            }
            EngineEventKind::LocalProgress { observation }
                if next.source == Some(SourceKind::LocalFile) =>
            {
                next.decoded_position_frames = observation.source_position_frames;
                next.audible_position_frames = observation.audible_source_position_frames;
                next.playback = Some(observation);
            }
            EngineEventKind::LocalReserved {
                active,
                next: reserved,
            } if next.source == Some(SourceKind::LocalFile) => {
                next.active_entry_id = Some(active);
                next.reserved_next_entry_id = reserved;
            }
            EngineEventKind::LocalPaused { observation }
                if next.source == Some(SourceKind::LocalFile) =>
            {
                next.active_entry_id = Some(observation.source_entry_id.clone());
                next.audible_position_frames = observation.audible_source_position_frames;
                next.decoded_position_frames = observation.source_position_frames;
                next.playback = Some(observation);
                next.transport = if self.wants_playback() {
                    TransportState::Buffering
                } else {
                    TransportState::Paused
                };
                completion = Some((ReceiptStatus::Applied, None, None));
            }
            EngineEventKind::LocalStopped { observation }
                if next.source == Some(SourceKind::LocalFile) =>
            {
                if let Some(observation) = observation {
                    next.decoded_position_frames = observation.source_position_frames;
                    next.playback = Some(observation);
                }
                next.audible_position_frames = None;
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                next.active_entry_id = None;
                next.reserved_next_entry_id = None;
                completion = Some((ReceiptStatus::Applied, None, None));
            }
            EngineEventKind::LocalFailed { code } => {
                next.device_epoch = increment(next.device_epoch)?;
                next.audible_position_frames = None;
                if let Some(playback) = &mut next.playback {
                    playback.consumed_frames = None;
                    playback.backend_delay_frames = None;
                    playback.audible_entry_id = None;
                    playback.audible_source_position_frames = None;
                }
                next.transport = TransportState::Failed;
                next.muted = true;
                next.output_consented = false;
                next.volume_policy.disarm();
                next.error = Some(code);
                next.epochs = next.epochs.next_stream()?;
                completion = Some((
                    ReceiptStatus::Failed,
                    Some(EngineError::new(
                        code,
                        "local playback failed; explicit recovery is required",
                    )),
                    None,
                ));
            }
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
                let position = next
                    .audible_position_frames
                    .and_then(|position| position.checked_add(frames))
                    .ok_or(STATE)?;
                next.audible_position_frames = Some(position);
                if position > next.decoded_position_frames {
                    return Err(STATE);
                }
            }
            EngineEventKind::EndOfInput
                if next.transport == TransportState::Playing
                    || (next.source == Some(SourceKind::LocalFile)
                        && next.transport == TransportState::Buffering) =>
            {
                next.transport = TransportState::Draining
            }
            EngineEventKind::Drained if next.transport == TransportState::Draining => {
                if next.source == Some(SourceKind::LocalFile) && next.operation.is_some() {
                    completion = Some((ReceiptStatus::Applied, None, None));
                }
                next.transport = TransportState::Idle;
                next.muted = true;
                next.session_id = None;
                next.source = None;
                next.seekable = false;
                next.active_entry_id = None;
                next.reserved_next_entry_id = None;
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
                next.device_epoch = increment(next.device_epoch)?;
                next.transport = TransportState::Suspended;
                next.muted = true;
                next.output_id = Some(device_id);
                next.epochs = next.epochs.next_stream()?;
                completion = Some((ReceiptStatus::Cancelled, Some(OUTPUT), None));
            }
            EngineEventKind::OutputFailed => {
                next.device_epoch = increment(next.device_epoch)?;
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
        if let Some(error) = terminal_exhaustion {
            next.error = Some(error.code);
            next.shutdown_requested = true;
            graph_completion_error = Some(error);
        }
        let publish_event = next.event_sequence != self.state.event_sequence;
        let request_id = self.state.operation.clone();
        let invalidated =
            next.epochs != self.state.epochs || next.device_epoch != self.state.device_epoch;
        let clear_active = matches!(
            next.transport,
            TransportState::Idle | TransportState::Failed | TransportState::Suspended
        );
        self.state = next;
        if invalidated || clear_active {
            let error = if let Some(error) = graph_completion_error {
                error
            } else if clear_active && self.state.error.is_some() {
                OUTPUT
            } else {
                EngineError::new(
                    ErrorCode::OperationCancelled,
                    "graph stream generation changed",
                )
            };
            self.invalidate_graph_work(error);
        }
        if clear_active {
            self.graph.active = None;
            self.local_boundary = None;
        }
        if let Some((status, error, activation)) = completion {
            self.finish_pending(status, error, activation);
        }
        if publish_event {
            self.log(request_id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_non_graph_requests_keep_the_existing_replay_contract() {
        let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
        let mut command = CommandEnvelope {
            schema_version: 1,
            owner_id: None,
            request_id: RequestId::new("invalid").unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            session_id: None,
            expected_revision: Some(0),
            payload: Command::QueueAppend {
                path: String::new(),
            },
        };
        assert_eq!(
            session.submit(command.clone()).status,
            ReceiptStatus::Rejected
        );
        command.payload = Command::Mute { muted: true };
        assert_eq!(session.submit(command).status, ReceiptStatus::Rejected);
        assert_eq!(session.state.revision, 0);
    }
    #[test]
    fn staging_shares_immutable_receipt_history() {
        let mut session = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
        session.submit(CommandEnvelope {
            schema_version: 1,
            owner_id: None,
            request_id: RequestId::new("history").unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            session_id: None,
            expected_revision: None,
            payload: Command::State,
        });
        let staged = session.clone();
        assert!(std::ptr::eq(
            &session.receipts.front().unwrap().0,
            &staged.receipts.front().unwrap().0,
        ));
    }
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
