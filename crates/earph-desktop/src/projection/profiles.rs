// ABOUTME: Decodes saved profile intent and application bindings without runtime authorization.
// ABOUTME: Requires nullable policy fields and preserves owner operation and resolution outcomes.
use super::{VolumePolicyView, nullable};
use earph_core::{
    audio::ChannelLayout,
    control::{OutputBackend, OutputRoute, SourceKind},
    graph::FiniteGraphScalar,
    graph_control::GraphIntent,
    ids::*,
    profile_control::{CorrectionNormalization, ProfileCatalogReference},
    profiles::{
        ProfileCorrectionKind, ProfileCorrectionReference, ProfileLatencyMode, ProfileObjectHash,
        ProfileSourcePolicy,
    },
    safety::{FidelityPolicy, VolumeMode},
};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOutputSelectorView {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub device_id: DeviceId,
    #[serde(deserialize_with = "nullable")]
    pub manual_setup: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileVolumeIntentView {
    pub mode: VolumeMode,
    pub software_cap_db: f64,
    #[serde(deserialize_with = "nullable")]
    pub hardware_cap_db: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileLatencyIntentView {
    pub mode: ProfileLatencyMode,
    pub max_resident_bytes: u64,
    pub max_preparation_bytes: u64,
    #[serde(deserialize_with = "nullable")]
    pub max_latency_frames: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocumentView {
    pub schema_version: u32,
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub graph_sha256: ProfileObjectHash,
    pub source_policy: ProfileSourcePolicy,
    pub fidelity: FidelityPolicy,
    pub output: ProfileOutputSelectorView,
    pub volume: ProfileVolumeIntentView,
    pub latency: ProfileLatencyIntentView,
    pub corrections: Vec<ProfileCorrectionReference>,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplicationBindingView {
    pub profile: ProfileCatalogReference,
    #[serde(deserialize_with = "nullable")]
    pub device_identity_sha256: Option<ProfileObjectHash>,
    pub volume_mode: VolumeMode,
    pub fidelity: FidelityPolicy,
    pub graph_sha256: ProfileObjectHash,
    pub source: SourceKind,
    pub effective_policy: VolumePolicyView,
    pub software_gain_db: f64,
    pub latency: ProfileLatencyIntentView,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCorrectionObjectView {
    pub kind: ProfileCorrectionKind,
    pub object_sha256: ProfileObjectHash,
    pub document_json: Arc<str>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCorrectionContextView {
    pub sample_rate_hz: u32,
    pub channels: ChannelLayout,
    pub quantum_frames: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCorrectionPreviewView {
    pub context: ProfileCorrectionContextView,
    pub preamp_db: FiniteGraphScalar,
    pub channel_peak_bounds: Vec<FiniteGraphScalar>,
    pub required_headroom_db: FiniteGraphScalar,
    pub normalization: CorrectionNormalization,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRecordView {
    pub profile: Arc<ProfileDocumentView>,
    pub profile_sha256: ProfileObjectHash,
    pub graph: Arc<GraphIntent>,
    pub corrections: Arc<Vec<ProfileCorrectionObjectView>>,
    #[serde(deserialize_with = "nullable")]
    pub correction_preview: Option<Arc<ProfileCorrectionPreviewView>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSummaryView {
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub profile_sha256: ProfileObjectHash,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSaveStageView {
    Saving,
    Retiring,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSaveInspectionView {
    #[serde(deserialize_with = "nullable")]
    pub stage: Option<ProfileSaveStageView>,
    #[serde(deserialize_with = "nullable")]
    pub request_id: Option<RequestId>,
    #[serde(deserialize_with = "nullable")]
    pub deadline_monotonic_ns: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePageView {
    pub entries: Vec<ProfileSummaryView>,
    pub total: usize,
    #[serde(deserialize_with = "nullable")]
    pub next_offset: Option<usize>,
    pub operation: ProfileSaveInspectionView,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileRevisionView {
    pub id: ProfileId,
    pub revision: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSelectionReasonView {
    Manual,
    ExactDevice,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSelectionIssueView {
    NoVerifiedMatch,
    Ambiguous,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileResolutionView {
    Selected {
        profile: ProfileRevisionView,
        reason: ProfileSelectionReasonView,
    },
    SelectionRequired {
        reason: ProfileSelectionIssueView,
        candidates: Vec<ProfileRevisionView>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileApplicationCapabilitiesView {
    pub volume_modes: Vec<VolumeMode>,
    pub fixed_requires_current_arming: bool,
    pub fixed_requires_unity_gain: bool,
    pub fixed_requires_zero_gain_ramp: bool,
    pub requires_current_output: bool,
    pub requires_output_consent: bool,
    pub requires_matching_fidelity: bool,
    pub live_selected_profile_update: bool,
    pub finite_aggregate_src_latency: bool,
}
