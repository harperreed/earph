// ABOUTME: Defines one versioned command and receipt contract for all controllers.
// ABOUTME: Rejects unsupported features before the session can accept work.
use crate::{
    EngineError, ErrorCode,
    graph::{GraphDocument, GraphProcessingRequest},
    graph_control::*,
    ids::*,
    profile_control::*,
    profiles::{ProfileOutputSelector, ProfileResolution},
    safety::{FidelityPolicy, VolumeMode},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// Two digest widths bound retained capacity while allowing malformed owner rejections.
pub const GRAPH_HASH_STORAGE_BYTES: usize = 128;

#[derive(Debug, Clone, Copy)]
pub struct ControlLimits {
    pub(crate) message_bytes: usize,
    pub(crate) receipts: usize,
    pub(crate) events: usize,
}
impl Default for ControlLimits {
    fn default() -> Self {
        Self {
            message_bytes: 1048576,
            receipts: 1024,
            events: 1024,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Virtual,
    LocalFile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputBackend {
    Alsa,
    CoreAudio,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputRoute {
    System,
    DirectHardware,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputConfiguration {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub endpoint: String,
    pub device_id: DeviceId,
    pub profile_id: ProfileId,
    pub period_frames: u32,
    pub buffer_frames: u32,
    pub volume_mode: VolumeMode,
    pub fidelity: FidelityPolicy,
}
impl OutputConfiguration {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.endpoint.is_empty()
            || self.endpoint.len() > 256
            || self.endpoint.trim() != self.endpoint
            || self.endpoint.chars().any(char::is_control)
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "select one explicit output endpoint",
            ));
        }
        if !(1..=8192).contains(&self.period_frames)
            || self.buffer_frames < self.period_frames.saturating_mul(2)
            || self.buffer_frames > 65536
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "invalid output period or buffer bound",
            ));
        }
        if self.backend == OutputBackend::Alsa
            && self.route == OutputRoute::DirectHardware
            && (!self.endpoint.starts_with("hw:") || self.endpoint.len() == 3)
        {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "direct ALSA output requires an explicit hw: endpoint",
            ));
        }
        if self.volume_mode == VolumeMode::Hardware {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "hardware volume set and readback are not implemented",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "capabilities.get")]
    Capabilities,
    #[serde(rename = "state.get")]
    State,
    #[serde(rename = "queue.get")]
    QueueGet { offset: usize, limit: usize },
    #[serde(rename = "graph.inspect")]
    GraphInspect {},
    #[serde(rename = "profiles.list")]
    ProfilesList { offset: usize, limit: usize },
    #[serde(rename = "profiles.get")]
    ProfilesGet { id: ProfileId },
    #[serde(rename = "profiles.resolve")]
    ProfilesResolve {
        manual_profile_id: Option<ProfileId>,
        output: ProfileOutputSelector,
    },
    #[serde(rename = "profiles.create")]
    ProfilesCreate {
        id: ProfileId,
        draft: Arc<ProfileDraft>,
        graph: Arc<GraphIntent>,
    },
    #[serde(rename = "profiles.update")]
    ProfilesUpdate {
        id: ProfileId,
        expected_profile_revision: u64,
        draft: Arc<ProfileDraft>,
        graph: Arc<GraphIntent>,
    },
    #[serde(rename = "graph.validate")]
    GraphValidate {
        graph: Arc<GraphDocument>,
        processing: GraphProcessingRequest,
    },
    #[serde(rename = "graph.prepare")]
    GraphPrepare {
        graph: Arc<GraphDocument>,
        processing: GraphProcessingRequest,
    },
    #[serde(rename = "graph.commit")]
    GraphCommit {
        token: PreparedGraphId,
        graph_hash: String,
    },
    #[serde(rename = "queue.append")]
    QueueAppend { path: String },
    #[serde(rename = "queue.append_directory")]
    QueueAppendDirectory { path: String },
    #[serde(rename = "queue.remove")]
    QueueRemove { id: QueueEntryId },
    #[serde(rename = "queue.reorder")]
    QueueMove { id: QueueEntryId, new_index: usize },
    #[serde(rename = "queue.select")]
    QueueSelect { id: Option<QueueEntryId> },
    #[serde(rename = "owner.shutdown")]
    OwnerShutdown,
    #[serde(rename = "output.select")]
    OutputSelect { output: OutputConfiguration },
    #[serde(rename = "output.consent")]
    OutputConsent {
        device_id: DeviceId,
        profile_id: ProfileId,
    },
    #[serde(rename = "volume.set")]
    VolumeSet {
        millidecibels: i32,
        ramp_frames: u32,
    },
    #[serde(rename = "volume.caps.set")]
    VolumeCaps {
        software_millidecibels: i32,
        hardware_millidecibels: Option<i32>,
    },
    #[serde(rename = "volume.arm_fixed")]
    FixedArm {
        device_id: DeviceId,
        profile_id: ProfileId,
    },
    #[serde(rename = "sessions.start")]
    Start {
        source: SourceKind,
        device_id: DeviceId,
        takeover: bool,
        seekable: bool,
    },
    #[serde(rename = "sessions.stop")]
    Stop,
    #[serde(rename = "sessions.pause")]
    Pause,
    #[serde(rename = "sessions.resume")]
    Resume,
    #[serde(rename = "sessions.seek")]
    Seek { position_frames: u64 },
    #[serde(rename = "mute.set")]
    Mute { muted: bool },
    #[serde(other)]
    Unsupported,
}
impl Command {
    pub fn is_profile_save(&self) -> bool {
        matches!(
            self,
            Self::ProfilesCreate { .. } | Self::ProfilesUpdate { .. }
        )
    }
    pub fn is_profile(&self) -> bool {
        self.is_profile_save()
            || matches!(
                self,
                Self::ProfilesList { .. } | Self::ProfilesGet { .. } | Self::ProfilesResolve { .. }
            )
    }
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Capabilities
                | Self::State
                | Self::QueueGet { .. }
                | Self::GraphInspect { .. }
                | Self::ProfilesList { .. }
                | Self::ProfilesGet { .. }
                | Self::ProfilesResolve { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub request_id: RequestId,
    pub zone_id: ZoneId,
    #[serde(default)]
    pub owner_id: Option<OwnerId>,
    pub session_id: Option<SessionId>,
    pub expected_revision: Option<u64>,
    pub payload: Command,
}
impl CommandEnvelope {
    pub(crate) fn profile_capacity_bytes(&self) -> Result<usize, EngineError> {
        if !self.payload.is_profile() {
            return Ok(0);
        }
        let ids = [
            self.request_id.capacity_bytes(),
            self.zone_id.capacity_bytes(),
            self.owner_id.as_ref().map_or(0, OwnerId::capacity_bytes),
            self.session_id
                .as_ref()
                .map_or(0, SessionId::capacity_bytes),
        ];
        if ids.iter().any(|n| *n > 128) {
            return Err(BUDGET);
        }
        let mut bytes = size_of::<Self>() + 128 + ids.iter().sum::<usize>();
        let id = match &self.payload {
            Command::ProfilesCreate { id, draft, graph }
            | Command::ProfilesUpdate {
                id, draft, graph, ..
            } => {
                draft.validate_storage()?;
                validate_graph_storage(&graph.graph)?;
                bytes = bytes
                    .checked_add(draft.retained_capacity_bytes()?)
                    .and_then(|n| n.checked_add(graph.graph.retained_capacity_bytes().ok()?))
                    .and_then(|n| n.checked_add(size_of::<GraphIntent>()))
                    .ok_or(BUDGET)?;
                Some(id)
            }
            Command::ProfilesGet { id } => Some(id),
            Command::ProfilesResolve {
                manual_profile_id,
                output,
            } => {
                if output.device_id.capacity_bytes() > 128
                    || output
                        .manual_setup
                        .as_ref()
                        .is_some_and(|s| s.capacity() > 128)
                {
                    return Err(BUDGET);
                }
                bytes += output.device_id.capacity_bytes()
                    + output.manual_setup.as_ref().map_or(0, String::capacity);
                manual_profile_id.as_ref()
            }
            _ => None,
        };
        if let Some(id) = id {
            if id.capacity_bytes() > 128 {
                return Err(BUDGET);
            }
            bytes += id.capacity_bytes();
        }
        Ok(bytes)
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        self.validate_storage()?;
        if let Command::GraphCommit { graph_hash, .. } = &self.payload
            && !valid_graph_hash(graph_hash)
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "invalid graph resolution hash",
            ));
        }
        if let Command::GraphValidate { graph, .. } | Command::GraphPrepare { graph, .. } =
            &self.payload
        {
            graph.validate_structure()?;
        }
        Ok(())
    }
    // Bounded semantic failures must reach the owner's retained receipt history.
    fn validate_storage(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 || matches!(self.payload, Command::Unsupported) {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported command version or method",
            ));
        }
        if let Command::QueueAppend { path } | Command::QueueAppendDirectory { path } =
            &self.payload
        {
            crate::queue::validate_path(path)?;
        }
        if let Command::OutputSelect { output } = &self.payload {
            output.validate()?;
        }
        self.validate_retained_storage()
    }
    pub(crate) fn validate_retained_storage(&self) -> Result<(), EngineError> {
        if self.payload.is_profile() {
            self.profile_capacity_bytes()?;
        }
        match &self.payload {
            Command::GraphValidate { graph, .. } | Command::GraphPrepare { graph, .. } => {
                graph.validate_storage()
            }
            Command::GraphCommit { graph_hash, .. }
                if graph_hash.capacity() > GRAPH_HASH_STORAGE_BYTES =>
            {
                Err(EngineError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "graph digest exceeds storage limit",
                ))
            }
            Command::GraphCommit { token, graph_hash }
                if token
                    .capacity_bytes()
                    .checked_add(graph_hash.capacity())
                    .is_none_or(|bytes| bytes > crate::graph::MAX_GRAPH_INTENT_BYTES) =>
            {
                Err(EngineError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "graph commit identities exceed storage limit",
                ))
            }
            _ => Ok(()),
        }
    }
}
pub fn decode_command(
    bytes: &[u8],
    limits: &ControlLimits,
) -> Result<CommandEnvelope, EngineError> {
    if bytes.len() > limits.message_bytes {
        return Err(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "command exceeds byte limit",
        ));
    }
    if bytes.len() > GRAPH_COMMAND_BYTES {
        #[derive(Deserialize)]
        struct Method<'a> {
            #[serde(borrow)]
            method: std::borrow::Cow<'a, str>,
        }
        #[derive(Deserialize)]
        struct Header<'a> {
            #[serde(borrow)]
            payload: Method<'a>,
        }
        // Scan ignored values without retaining the graph or its parameters.
        if let Ok(header) = serde_json::from_slice::<Header<'_>>(bytes)
            && matches!(
                header.payload.method.as_ref(),
                "graph.prepare" | "graph.validate"
            )
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "graph command exceeds byte limit",
            ));
        }
    }
    let command: CommandEnvelope = serde_json::from_slice(bytes)
        .map_err(|_| EngineError::new(ErrorCode::AssetInvalid, "invalid command document"))?;
    command.validate_storage()?;
    if matches!(
        command.payload,
        Command::GraphValidate { .. } | Command::GraphPrepare { .. }
    ) && bytes.len() > GRAPH_COMMAND_BYTES
    {
        return Err(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "graph command exceeds byte limit",
        ));
    }
    Ok(command)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Accepted,
    Applied,
    Rejected,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum QueryResult {
    State(Box<crate::session::SessionSnapshot>),
    Capabilities(Capabilities),
    Queue(crate::queue::QueuePage),
    GraphPreview(Arc<GraphPreview>),
    GraphPrepared(GraphPrepared),
    GraphInspection(GraphInspection),
    GraphDiagnostic(GraphDiagnostic),
    ProfileSaved(Arc<ProfileRecord>),
    Profile(Arc<ProfileRecord>),
    Profiles(ProfilePage),
    ProfileResolution(ProfileResolution),
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capabilities {
    pub schema_version: u32,
    pub sources: Vec<SourceKind>,
    pub physical_output: bool,
    pub profiles: ProfileCapabilities,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Receipt {
    pub request_id: RequestId,
    pub zone_id: ZoneId,
    pub session_id: Option<SessionId>,
    pub status: ReceiptStatus,
    pub error: Option<EngineError>,
    pub revision: u64,
    pub epochs: Epochs,
    pub activation_frame: Option<u64>,
    pub result: Option<QueryResult>,
}
