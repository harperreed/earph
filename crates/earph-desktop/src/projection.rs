// ABOUTME: Projects the version-one owner reply without inventing missing observations.
// ABOUTME: Retains exact reply bytes and requires nullable fields to be present.
use earph_core::{
    ErrorCode,
    control::{OutputBackend, OutputConfiguration, SourceKind},
    dsp_nodes::FirAssetReference,
    ids::*,
    profile_control::ProfileCatalogReference,
    profiles::ProfileAssociation,
    queue::QueueEntry,
};
use serde::{Deserialize, Deserializer};
use std::sync::Arc;
mod devices;
mod graph;
mod packages;
mod playback;
mod profiles;
pub use devices::*;
pub use graph::*;
pub use packages::*;
pub use playback::*;
pub use profiles::*;

fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumePolicyView {
    pub software_cap_db: f64,
    #[serde(deserialize_with = "nullable")]
    pub hardware_cap_db: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplyStatus {
    Accepted,
    Applied,
    Rejected,
    Cancelled,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorDetail {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportView {
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateView {
    pub profile_worker_busy: bool,
    pub profile_storage_bytes: u64,
    pub zone_id: ZoneId,
    #[serde(deserialize_with = "nullable")]
    pub owner_id: Option<OwnerId>,
    #[serde(deserialize_with = "nullable")]
    pub session_id: Option<SessionId>,
    #[serde(deserialize_with = "nullable")]
    pub source: Option<SourceKind>,
    #[serde(deserialize_with = "nullable")]
    pub output_id: Option<DeviceId>,
    pub transport: TransportView,
    pub muted: bool,
    pub software_gain_db: f64,
    pub epochs: Epochs,
    pub revision: u64,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub event_sequence: u64,
    pub monotonic_ns: u64,
    #[serde(deserialize_with = "nullable")]
    pub audible_position_frames: Option<u64>,
    pub decoded_position_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub operation: Option<RequestId>,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<ErrorCode>,
    pub seekable: bool,
    pub shutdown_requested: bool,
    #[serde(deserialize_with = "nullable")]
    pub output: Option<OutputConfiguration>,
    pub output_consented: bool,
    pub volume_policy: VolumePolicyView,
    pub local_volume_policy: VolumePolicyView,
    #[serde(deserialize_with = "nullable")]
    pub configured_profile: Option<ProfileCatalogReference>,
    pub gain_ramp_frames: u32,
    #[serde(deserialize_with = "nullable")]
    pub active_entry_id: Option<QueueEntryId>,
    #[serde(deserialize_with = "nullable")]
    pub reserved_next_entry_id: Option<QueueEntryId>,
    #[serde(deserialize_with = "nullable")]
    pub playback: Option<PlaybackObservationView>,
    #[serde(default, deserialize_with = "present")]
    pub network_playback: Option<NetworkPlaybackObservationView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCapabilitiesView {
    pub save: bool,
    pub manual_resolution: bool,
    pub exact_associations: bool,
    pub exact_association_backends: Vec<OutputBackend>,
    pub apply: bool,
    pub application: ProfileApplicationCapabilitiesView,
    pub correction_import: bool,
    pub packages: bool,
    pub package: ProfilePackageCapabilitiesView,
    pub max_profiles: usize,
    pub max_page: usize,
    pub max_profile_bytes: usize,
    pub max_graph_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesView {
    pub diagnostics: Arc<earph_core::diagnostics::DiagnosticCapabilities>,
    pub schema_version: u32,
    pub sources: Vec<SourceKind>,
    pub physical_output: bool,
    // Share immutable capability metadata without inflating every retained receipt variant.
    pub profiles: Arc<ProfileCapabilitiesView>,
    pub output_activity: OutputActivityCapabilitiesView,
    pub devices: Arc<DeviceCapabilitiesView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueView {
    pub entries: Vec<QueueEntry>,
    #[serde(deserialize_with = "nullable")]
    pub selected: Option<QueueEntryId>,
    pub offset: usize,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ResultView {
    SignalPath(Arc<earph_core::diagnostics::SignalPathRead>),
    Metrics(Arc<earph_core::diagnostics::MetricsRead>),
    Events(Arc<earph_core::diagnostics::EventsRead>),
    DiagnosticPreview(Arc<earph_core::diagnostics::PreviewRead>),
    DiagnosticArtifact(Arc<earph_core::diagnostics::DiagnosticArtifactReceipt>),
    DiagnosticCancel(Arc<earph_core::diagnostics::DiagnosticCancelReceipt>),
    Sources(Arc<earph_core::source::SourceStagingSnapshot>),
    Network(Arc<earph_core::network_source::NetworkSnapshot>),
    Renderer(Arc<earph_core::renderer::RendererSnapshot>),
    State(Arc<StateView>),
    Capabilities(CapabilitiesView),
    Queue(QueueView),
    Devices(Arc<DeviceQueryResultView>),
    GraphPreview(Arc<GraphPreviewView>),
    GraphPrepared(GraphPreparedView),
    GraphInspection(Arc<GraphInspectionView>),
    GraphDiagnostic(Arc<GraphDiagnosticView>),
    ProfileSaved(Arc<ProfileRecordView>),
    Profile(Arc<ProfileRecordView>),
    Profiles(Arc<ProfilePageView>),
    ProfileResolution(ProfileResolutionView),
    ProfileBound(Arc<ProfileAssociation>),
    ProfilePackage(Arc<ProfilePackageResultView>),
    ProfilePackageIssue(Arc<ProfilePackageIssueView>),
    FirAsset(Arc<FirAssetReference>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptView {
    pub request_id: RequestId,
    pub zone_id: ZoneId,
    #[serde(deserialize_with = "nullable")]
    pub session_id: Option<SessionId>,
    pub status: ReplyStatus,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<ErrorDetail>,
    pub revision: u64,
    pub epochs: Epochs,
    #[serde(deserialize_with = "nullable")]
    pub activation_frame: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub result: Option<ResultView>,
    #[serde(skip)]
    raw: Arc<[u8]>,
}
impl ReceiptView {
    pub fn state(&self) -> Option<&StateView> {
        match self.result.as_ref() {
            Some(ResultView::State(state)) => Some(state),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnerErrorView {
    pub error: ErrorDetail,
    raw: Arc<[u8]>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Receipt(ReceiptView),
    OwnerError(OwnerErrorView),
}
impl Reply {
    pub fn bytes(&self) -> &[u8] {
        match self {
            Self::Receipt(value) => &value.raw,
            Self::OwnerError(value) => &value.raw,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionError {
    pub detail: &'static str,
    pub bytes: Arc<[u8]>,
}

pub fn decode_reply(bytes: Vec<u8>) -> Result<Reply, ProjectionError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Envelope {
        #[serde(default, deserialize_with = "present")]
        receipt: Option<ReceiptView>,
        #[serde(default, deserialize_with = "present")]
        error: Option<ErrorDetail>,
    }
    let raw: Arc<[u8]> = bytes.into();
    let invalid = |detail| ProjectionError {
        detail,
        bytes: raw.clone(),
    };
    if raw.is_empty() || raw.len() > earph_engine::control_frame_limit() {
        return Err(invalid("reply exceeds the shared frame bound"));
    }
    let envelope: Envelope = serde_json::from_slice(&raw)
        .map_err(|_| invalid("reply does not match the version-one projection"))?;
    match (envelope.receipt, envelope.error) {
        (Some(mut receipt), None) => {
            if let Some(result) = &receipt.result {
                let valid = match result {
                    ResultView::Sources(_) | ResultView::Network(_) | ResultView::Renderer(_) => {
                        receipt.status == ReplyStatus::Applied
                    }
                    ResultView::SignalPath(value) => diagnostic_matches(value, &receipt),
                    ResultView::Metrics(value) => diagnostic_matches(value, &receipt),
                    ResultView::Events(value) => diagnostic_matches(value, &receipt),
                    ResultView::DiagnosticPreview(value) => diagnostic_matches(value, &receipt),
                    ResultView::DiagnosticArtifact(value) => {
                        value.validate_for_status(receipt.status.into()).is_ok()
                            && value.request_id == receipt.request_id
                            && diagnostic_binding_matches(&value.binding, &receipt)
                    }
                    ResultView::DiagnosticCancel(value) => {
                        receipt.status == ReplyStatus::Applied
                            && value.validate().is_ok()
                            && diagnostic_binding_matches(&value.binding, &receipt)
                    }
                    ResultView::Capabilities(value) => {
                        value.schema_version == 1
                            && value.profiles.package.schema_version == 1
                            && value.diagnostics.as_ref()
                                == &earph_core::diagnostics::DiagnosticCapabilities::default()
                    }
                    ResultView::State(value) => {
                        receipt.status == ReplyStatus::Applied
                            && value.zone_id == receipt.zone_id
                            && value.revision == receipt.revision
                            && value.epochs == receipt.epochs
                            && value.network_playback.as_ref().is_none_or(|p| p.valid())
                    }
                    ResultView::ProfileSaved(value) | ResultView::Profile(value) => {
                        value.profile.schema_version == 1 && value.graph.graph.schema_version == 1
                    }
                    ResultView::GraphInspection(value) => value
                        .configured
                        .as_ref()
                        .is_none_or(|intent| intent.graph.schema_version == 1),
                    ResultView::ProfilePackage(value) => value.manifest.schema_version == 1,
                    _ => true,
                };
                if !valid {
                    return Err(invalid("unsupported result schema or body"));
                }
            }
            receipt.raw = raw;
            Ok(Reply::Receipt(receipt))
        }
        (None, Some(error)) => Ok(Reply::OwnerError(OwnerErrorView { error, raw })),
        _ => Err(invalid(
            "reply must contain exactly one receipt or owner error",
        )),
    }
}

fn diagnostic_matches<T: earph_core::diagnostics::DiagnosticReport>(
    value: &earph_core::diagnostics::DiagnosticRead<T>,
    receipt: &ReceiptView,
) -> bool {
    receipt.status == ReplyStatus::Applied
        && value.validate().is_ok()
        && diagnostic_binding_matches(&value.binding, receipt)
}

fn diagnostic_binding_matches(
    binding: &earph_core::diagnostics::DiagnosticBinding,
    receipt: &ReceiptView,
) -> bool {
    binding.zone_id == receipt.zone_id
        && binding.revision == receipt.revision
        && binding.epochs == receipt.epochs
}

impl From<ReplyStatus> for earph_core::control::ReceiptStatus {
    fn from(status: ReplyStatus) -> Self {
        match status {
            ReplyStatus::Accepted => Self::Accepted,
            ReplyStatus::Applied => Self::Applied,
            ReplyStatus::Rejected => Self::Rejected,
            ReplyStatus::Cancelled => Self::Cancelled,
            ReplyStatus::Failed => Self::Failed,
        }
    }
}
