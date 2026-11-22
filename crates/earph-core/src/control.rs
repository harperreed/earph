// ABOUTME: Defines one versioned command and receipt contract for all controllers.
// ABOUTME: Rejects unsupported features before the session can accept work.
use crate::{
    EngineError, ErrorCode,
    graph::{GraphDocument, GraphProcessingRequest},
    graph_control::*,
    ids::*,
    profile_control::*,
    profiles::{ProfileOutputSelector, ProfileResolution},
    safety::{FidelityPolicy, VolumeMode, VolumePolicy},
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
    HttpFlac,
    Sendspin,
}

/// Captures the authority and safety facts behind one absolute software-volume gesture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeObservation {
    pub source: Option<SourceKind>,
    pub active_entry_id: Option<QueueEntryId>,
    pub epochs: Epochs,
    pub device_epoch: u64,
    pub graph_epoch: u64,
    pub output: OutputConfiguration,
    pub output_consented: bool,
    pub muted: bool,
    pub software_gain_zero: bool,
    pub volume_policy: VolumePolicy,
    pub local_volume_policy: VolumePolicy,
}
// VolumePolicy admits only finite caps, so this observation's equality remains reflexive.
impl Eq for VolumeObservation {}

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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", deny_unknown_fields)]
pub enum Command {
    #[serde(rename = "capabilities.get")]
    Capabilities,
    #[serde(rename = "state.get")]
    State,
    #[serde(rename = "signal_path.get")]
    SignalPathGet {},
    #[serde(rename = "metrics.get")]
    MetricsGet {},
    #[serde(rename = "events.get")]
    EventsGet {
        #[serde(deserialize_with = "crate::serde_helpers::nullable")]
        cursor: Option<crate::diagnostics::EventCursor>,
    },
    #[serde(rename = "diagnostics.preview")]
    DiagnosticsPreview {},
    #[serde(rename = "diagnostics.export")]
    DiagnosticsExport {
        directory: String,
        expected_sha256: crate::profiles::ProfileObjectHash,
    },
    #[serde(rename = "diagnostics.inspect")]
    DiagnosticsInspect {
        directory: String,
        expected_sha256: crate::profiles::ProfileObjectHash,
    },
    #[serde(rename = "diagnostics.cancel")]
    DiagnosticsCancel { request_id: RequestId },
    #[serde(rename = "devices.list")]
    DevicesList { backend: OutputBackend },
    #[serde(rename = "devices.inspect")]
    DevicesInspect {
        backend: OutputBackend,
        endpoint: String,
    },
    #[serde(rename = "queue.get")]
    QueueGet { offset: usize, limit: usize },
    #[serde(rename = "sources.get")]
    SourcesGet {},
    #[serde(rename = "renderer.get")]
    RendererGet {},
    #[serde(rename = "renderer.grants.replace")]
    RendererGrantsReplace {
        expected_generation: u64,
        grants: Arc<crate::renderer::RendererGrantDocument>,
        lifetime_ms: u64,
    },
    #[serde(rename = "renderer.grants.revoke")]
    RendererGrantsRevoke {
        expected_generation: u64,
        controller_id: RendererControllerId,
    },
    #[serde(rename = "renderer.callbacks.replace")]
    RendererCallbackGrantsReplace {
        controller_id: RendererControllerId,
        expected_controller_generation: u64,
        expected_generation: u64,
        lifetime_ms: u64,
        grants: Arc<crate::renderer_events::RendererCallbackGrantDocument>,
    },
    #[serde(rename = "renderer.callbacks.revoke")]
    RendererCallbackGrantsRevoke {
        controller_id: RendererControllerId,
        expected_controller_generation: u64,
        expected_generation: u64,
    },
    #[serde(rename = "renderer.subscription")]
    RendererSubscription {
        controller_id: RendererControllerId,
        grant_generation: u64,
        callback_generation: u64,
        action: crate::renderer_events::RendererSubscriptionAction,
    },
    #[serde(rename = "renderer.request")]
    RendererRequest {
        controller_id: RendererControllerId,
        grant_generation: u64,
        action: crate::renderer::RendererAction,
    },
    #[serde(rename = "sources.stage_uri")]
    SourceStageUri { uri: String, metadata: String },
    #[serde(rename = "sources.network.get")]
    NetworkGet {},
    #[serde(rename = "sources.grants.replace")]
    NetworkGrantsReplace {
        expected_generation: u64,
        grants: Arc<crate::network_source::NetworkGrantDocument>,
        lifetime_ms: u64,
    },
    #[serde(rename = "sources.grants.revoke")]
    NetworkGrantsRevoke { expected_generation: u64 },
    #[serde(rename = "sources.start_http_flac")]
    NetworkStartHttpFlac {
        media_id: StagedMediaId,
        staging_generation: u64,
        grant_generation: u64,
        device_id: DeviceId,
        takeover: bool,
    },
    /// Starts the player connection to the server from the owner's launch configuration.
    #[serde(rename = "sources.sendspin.start")]
    SendspinStart { device_id: DeviceId },
    #[serde(rename = "sources.sendspin.get")]
    SendspinGet {},
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
    #[serde(rename = "profiles.prepare")]
    ProfilesPrepare {
        id: ProfileId,
        expected_profile_revision: u64,
        profile_sha256: crate::profiles::ProfileObjectHash,
    },
    #[serde(rename = "profiles.apply")]
    ProfilesApply {
        id: ProfileId,
        expected_profile_revision: u64,
        profile_sha256: crate::profiles::ProfileObjectHash,
        token: PreparedGraphId,
        graph_hash: String,
    },
    #[serde(rename = "profiles.bind")]
    ProfilesBind {
        id: ProfileId,
        expected_profile_revision: u64,
        profile_sha256: crate::profiles::ProfileObjectHash,
        expected_device_epoch: u64,
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
    #[serde(rename = "profiles.import_correction")]
    ProfilesImport {
        id: ProfileId,
        expected_profile_revision: Option<u64>,
        draft: Arc<ProfileDraft>,
        input: Arc<ProfileCorrectionInput>,
    },
    #[serde(rename = "profiles.package_inspect")]
    ProfilesPackageInspect { directory: String },
    #[serde(rename = "profiles.package_preview")]
    ProfilesPackagePreview { profile: ProfileCatalogReference },
    #[serde(rename = "profiles.import_package")]
    ProfilesImportPackage {
        directory: String,
        expected_manifest_sha256: crate::profiles::ProfileObjectHash,
        profile: ProfileCatalogReference,
    },
    #[serde(rename = "profiles.export")]
    ProfilesExport {
        directory: String,
        profile: ProfileCatalogReference,
        expected_manifest_sha256: crate::profiles::ProfileObjectHash,
        export_review: PackageExportReview,
        content_review: PackageContentReview,
        redistribution: PackageRedistribution,
    },
    #[serde(rename = "assets.import_fir")]
    FirImport {
        path: String,
        purpose: crate::dsp_nodes::FirPurpose,
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
    #[serde(rename = "volume.set_guarded")]
    VolumeSetGuarded {
        observed: Box<VolumeObservation>,
        observed_revision: u64,
        millidecibels: i32,
        ramp_frames: u32,
    },
    #[serde(rename = "volume.silence")]
    VolumeSilence,
    #[serde(rename = "volume.factory_defaults")]
    VolumeFactoryDefaults,
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
    #[serde(rename = "transport.seek_dsd")]
    SeekDsd { position_bits: u64 },
    #[serde(rename = "mute.set")]
    Mute { muted: bool },
    #[serde(other)]
    Unsupported,
}
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("kind", &std::mem::discriminant(self))
            .finish_non_exhaustive()
    }
}

impl Command {
    pub fn is_renderer(&self) -> bool {
        matches!(
            self,
            Self::RendererGet {}
                | Self::RendererGrantsReplace { .. }
                | Self::RendererGrantsRevoke { .. }
                | Self::RendererRequest { .. }
                | Self::RendererCallbackGrantsReplace { .. }
                | Self::RendererCallbackGrantsRevoke { .. }
                | Self::RendererSubscription { .. }
        )
    }

    pub fn is_network(&self) -> bool {
        matches!(
            self,
            Self::NetworkGet {}
                | Self::NetworkGrantsReplace { .. }
                | Self::NetworkGrantsRevoke { .. }
                | Self::NetworkStartHttpFlac { .. }
        )
    }

    pub fn is_package(&self) -> bool {
        matches!(
            self,
            Self::ProfilesPackageInspect { .. }
                | Self::ProfilesPackagePreview { .. }
                | Self::ProfilesImportPackage { .. }
                | Self::ProfilesExport { .. }
        )
    }
    pub fn prepares_graph(&self) -> bool {
        matches!(
            self,
            Self::GraphPrepare { .. } | Self::GraphValidate { .. } | Self::ProfilesPrepare { .. }
        )
    }
    pub fn activates_graph(&self) -> bool {
        matches!(self, Self::GraphCommit { .. } | Self::ProfilesApply { .. })
    }
    pub fn is_profile_save(&self) -> bool {
        matches!(
            self,
            Self::ProfilesCreate { .. }
                | Self::ProfilesUpdate { .. }
                | Self::ProfilesImport { .. }
                | Self::FirImport { .. }
                | Self::ProfilesPackageInspect { .. }
                | Self::ProfilesPackagePreview { .. }
                | Self::ProfilesImportPackage { .. }
                | Self::ProfilesExport { .. }
        )
    }
    pub fn is_profile(&self) -> bool {
        self.is_profile_save()
            || matches!(
                self,
                Self::ProfilesList { .. }
                    | Self::ProfilesGet { .. }
                    | Self::ProfilesResolve { .. }
                    | Self::ProfilesPrepare { .. }
                    | Self::ProfilesApply { .. }
                    | Self::ProfilesBind { .. }
            )
    }
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::Capabilities
                | Self::DevicesList { .. }
                | Self::DevicesInspect { .. }
                | Self::State
                | Self::SignalPathGet {}
                | Self::MetricsGet {}
                | Self::EventsGet { .. }
                | Self::DiagnosticsPreview {}
                | Self::DiagnosticsInspect { .. }
                | Self::QueueGet { .. }
                | Self::SourcesGet { .. }
                | Self::RendererGet {}
                | Self::NetworkGet {}
                | Self::SendspinGet {}
                | Self::GraphInspect { .. }
                | Self::ProfilesList { .. }
                | Self::ProfilesGet { .. }
                | Self::ProfilesResolve { .. }
        )
    }
    pub fn is_diagnostic(&self) -> bool {
        matches!(
            self,
            Self::SignalPathGet {}
                | Self::MetricsGet {}
                | Self::EventsGet { .. }
                | Self::DiagnosticsPreview {}
        )
    }
    pub fn is_diagnostic_artifact(&self) -> bool {
        matches!(
            self,
            Self::DiagnosticsExport { .. }
                | Self::DiagnosticsInspect { .. }
                | Self::DiagnosticsCancel { .. }
        )
    }
    pub fn starts_diagnostic_work(&self) -> bool {
        matches!(
            self,
            Self::DiagnosticsExport { .. } | Self::DiagnosticsInspect { .. }
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
            Command::ProfilesPackageInspect { directory } => {
                validate_package_path(directory)?;
                bytes = bytes.checked_add(directory.capacity()).ok_or(BUDGET)?;
                None
            }
            Command::ProfilesPackagePreview { profile } => {
                validate_catalog(std::slice::from_ref(profile), 1)?;
                Some(&profile.id)
            }
            Command::ProfilesImportPackage {
                directory, profile, ..
            }
            | Command::ProfilesExport {
                directory, profile, ..
            } => {
                validate_package_path(directory)?;
                validate_catalog(std::slice::from_ref(profile), 1)?;
                bytes = bytes.checked_add(directory.capacity()).ok_or(BUDGET)?;
                Some(&profile.id)
            }
            Command::ProfilesImport {
                id, draft, input, ..
            } => {
                draft.validate_storage()?;
                let input_bytes = input.retained_capacity_bytes()?;
                bytes = bytes
                    .checked_add(draft.retained_capacity_bytes()?)
                    .and_then(|n| n.checked_add(input_bytes))
                    .ok_or(BUDGET)?;
                Some(id)
            }
            Command::FirImport { path, .. } => {
                crate::queue::validate_path(path)?;
                if path.capacity() > 1024 {
                    return Err(BUDGET);
                }
                bytes += path.capacity();
                None
            }
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
            Command::ProfilesGet { id }
            | Command::ProfilesPrepare { id, .. }
            | Command::ProfilesBind { id, .. } => Some(id),
            Command::ProfilesApply {
                id,
                token,
                graph_hash,
                ..
            } => {
                if token.capacity_bytes() > 128 || graph_hash.capacity() > GRAPH_HASH_STORAGE_BYTES
                {
                    return Err(BUDGET);
                }
                bytes = bytes
                    .checked_add(token.capacity_bytes())
                    .and_then(|n| n.checked_add(graph_hash.capacity()))
                    .ok_or(BUDGET)?;
                Some(id)
            }
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
        if let Command::GraphCommit { graph_hash, .. } | Command::ProfilesApply { graph_hash, .. } =
            &self.payload
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
        if let Command::DevicesInspect { endpoint, .. } = &self.payload {
            crate::devices::validate_endpoint(endpoint)?;
        }
        self.validate_retained_storage()
    }
    pub(crate) fn validate_retained_storage(&self) -> Result<(), EngineError> {
        if self.payload.is_network() || self.payload.is_renderer() {
            self.network_capacity_bytes()?;
        }

        if matches!(
            self.payload,
            Command::SourceStageUri { .. }
                | Command::SourcesGet {}
                | Command::SignalPathGet {}
                | Command::MetricsGet {}
                | Command::EventsGet { .. }
                | Command::DiagnosticsPreview {}
                | Command::DiagnosticsExport { .. }
                | Command::DiagnosticsInspect { .. }
                | Command::DiagnosticsCancel { .. }
                | Command::SendspinStart { .. }
                | Command::SendspinGet {}
        ) && [
            self.request_id.capacity_bytes(),
            self.zone_id.capacity_bytes(),
            self.owner_id.as_ref().map_or(0, OwnerId::capacity_bytes),
            self.session_id
                .as_ref()
                .map_or(0, SessionId::capacity_bytes),
        ]
        .into_iter()
        .any(|bytes| bytes > 128)
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "request identity exceeds storage limit",
            ));
        }
        if self.payload.is_profile() {
            self.profile_capacity_bytes()?;
        }
        if let Command::EventsGet {
            cursor: Some(cursor),
        } = &self.payload
            && cursor.owner_id.capacity_bytes() > 128
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "event cursor exceeds storage limit",
            ));
        }
        match &self.payload {
            Command::DiagnosticsExport { directory, .. }
            | Command::DiagnosticsInspect { directory, .. } => {
                if directory.capacity() > 1024 {
                    return Err(BUDGET);
                }
                Ok(())
            }
            Command::DiagnosticsCancel { request_id } if request_id.capacity_bytes() > 128 => {
                Err(BUDGET)
            }
            Command::SourceStageUri { uri, metadata } => {
                crate::source::validate_storage(uri, metadata)
            }
            Command::DevicesInspect { endpoint, .. } if endpoint.capacity() > 4096 => {
                Err(crate::devices::DEVICE_BUDGET)
            }
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
    SignalPath(Arc<crate::diagnostics::SignalPathRead>),
    Metrics(Arc<crate::diagnostics::MetricsRead>),
    Events(Arc<crate::diagnostics::EventsRead>),
    DiagnosticPreview(Arc<crate::diagnostics::PreviewRead>),
    DiagnosticArtifact(Arc<crate::diagnostics::DiagnosticArtifactReceipt>),
    DiagnosticCancel(Arc<crate::diagnostics::DiagnosticCancelReceipt>),
    Sources(crate::source::SourceStagingSnapshot),
    Network(crate::network_source::NetworkSnapshot),
    Sendspin(crate::sendspin_control::SendspinSnapshot),
    Renderer(crate::renderer::RendererSnapshot),
    Devices(Arc<crate::devices::DeviceQueryResult>),
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
    ProfileBound(crate::profiles::ProfileAssociation),
    ProfilePackage(Arc<ProfilePackageResult>),
    ProfilePackageIssue(ProfilePackageIssue),
    FirAsset(crate::dsp_nodes::FirAssetReference),
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Capabilities {
    pub diagnostics: crate::diagnostics::DiagnosticCapabilities,
    pub devices: Arc<crate::devices::DeviceCapabilities>,
    pub schema_version: u32,
    pub sources: Vec<SourceKind>,
    pub physical_output: bool,
    pub profiles: ProfileCapabilities,
    pub output_activity: OutputActivityCapabilities,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputActivityCapabilities {
    pub backends: Vec<OutputBackend>,
    pub scope: crate::session::OutputActivityScope,
    pub measurement_boundary: crate::session::OutputActivityBoundary,
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
