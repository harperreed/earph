// ABOUTME: Defines bounded owner save requests and immutable listening profile records.
// ABOUTME: Keeps portable intent separate from device qualification and catalog publication.
use crate::{
    EngineError, ErrorCode,
    audio::ChannelLayout,
    dsp_nodes::{FirAssetReference, FirPurpose},
    graph::{FiniteGraphScalar, GraphDocument},
    graph_control::GraphIntent,
    ids::*,
    profiles::*,
    safety::{FidelityPolicy, VolumeMode},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_PROFILE_PAGE: usize = 16;
pub const MAX_PROFILE_STORAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const PROFILE_SAVE_DEADLINE_NS: u64 = 5_000_000_000;
pub const MAX_PROFILE_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_PROFILE_CORRECTION_BYTES: usize = 256 * 1024;
pub const MAX_PROFILE_METADATA_BYTES: usize = 4 * 1024;
pub(crate) const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "profile retention exceeds admission",
);
pub(crate) const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "profile object or catalog is invalid",
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileVolumeDraft {
    pub mode: VolumeMode,
    pub software_cap_millidecibels: i32,
    pub hardware_cap_millidecibels: Option<i32>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDraft {
    pub name: String,
    pub source_policy: ProfileSourcePolicy,
    pub fidelity: FidelityPolicy,
    pub output: ProfileOutputSelector,
    pub volume: ProfileVolumeDraft,
    pub latency: ProfileLatencyIntent,
    pub corrections: Vec<ProfileCorrectionReference>,
}
impl ProfileDraft {
    pub fn document(
        &self,
        id: ProfileId,
        revision: u64,
        graph_sha256: ProfileObjectHash,
    ) -> Result<ProfileDocument, EngineError> {
        self.validate_storage()?;
        let document = ProfileDocument {
            schema_version: 1,
            id,
            revision,
            name: self.name.clone(),
            graph_sha256,
            source_policy: self.source_policy,
            fidelity: self.fidelity,
            output: self.output.clone(),
            volume: ProfileVolumeIntent {
                mode: self.volume.mode,
                software_cap_db: f64::from(self.volume.software_cap_millidecibels) / 1000.0,
                hardware_cap_db: self
                    .volume
                    .hardware_cap_millidecibels
                    .map(|n| f64::from(n) / 1000.0),
            },
            latency: self.latency.clone(),
            corrections: self.corrections.clone(),
        };
        document.validate()?;
        Ok(document)
    }
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        [
            size_of::<Self>(),
            self.name.capacity(),
            self.output.device_id.capacity_bytes(),
            self.output
                .manual_setup
                .as_ref()
                .map_or(0, String::capacity),
            self.corrections
                .capacity()
                .checked_mul(size_of::<ProfileCorrectionReference>())
                .ok_or(BUDGET)?,
        ]
        .into_iter()
        .try_fold(0_usize, |a, b| a.checked_add(b).ok_or(BUDGET))
    }
    pub(crate) fn validate_storage(&self) -> Result<(), EngineError> {
        if self.retained_capacity_bytes()? > MAX_PROFILE_BYTES {
            return Err(BUDGET);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCatalogReference {
    pub id: ProfileId,
    pub revision: u64,
    pub profile_sha256: ProfileObjectHash,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileApplicationBinding {
    pub profile: ProfileCatalogReference,
    pub device_identity_sha256: Option<ProfileObjectHash>,
    pub volume_mode: VolumeMode,
    pub fidelity: FidelityPolicy,
    pub graph_sha256: ProfileObjectHash,
    pub source: crate::control::SourceKind,
    pub effective_policy: crate::safety::VolumePolicy,
    pub software_gain_db: f64,
    pub latency: ProfileLatencyIntent,
}
impl ProfileApplicationBinding {
    pub fn validate(&self) -> Result<(), EngineError> {
        validate_catalog(std::slice::from_ref(&self.profile), 1)?;
        if self.source != crate::control::SourceKind::LocalFile {
            return Err(INVALID);
        }
        if self.volume_mode == VolumeMode::Hardware {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "hardware profile application is unavailable",
            ));
        }
        if self.volume_mode == VolumeMode::Fixed {
            self.effective_policy.validate_gain_constraints(
                self.volume_mode,
                crate::safety::SampleDomain::Pcm,
                self.fidelity,
                self.software_gain_db,
                None,
            )?;
        }
        crate::safety::VolumePolicy::new(
            self.effective_policy.software_cap_db(),
            self.effective_policy.hardware_cap_db(),
        )?;
        if !self.software_gain_db.is_finite()
            || !(-120.0..=self.effective_policy.software_cap_db()).contains(&self.software_gain_db)
        {
            return Err(INVALID);
        }
        if self.latency.max_resident_bytes == 0
            || self.latency.max_preparation_bytes < self.latency.max_resident_bytes
        {
            return Err(BUDGET);
        }
        Ok(())
    }
}
pub fn validate_catalog(
    references: &[ProfileCatalogReference],
    capacity: usize,
) -> Result<(), EngineError> {
    if capacity > MAX_PROFILES {
        return Err(BUDGET);
    }
    for (index, reference) in references.iter().enumerate() {
        if reference.id.capacity_bytes() > 128 {
            return Err(BUDGET);
        }
        if reference.revision == 0 || references[..index].iter().any(|p| p.id == reference.id) {
            return Err(INVALID);
        }
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileRecord {
    pub profile: Arc<ProfileDocument>,
    pub profile_sha256: ProfileObjectHash,
    pub graph: Arc<GraphIntent>,
    pub corrections: Arc<Vec<ProfileCorrectionObject>>,
    pub correction_preview: Option<Arc<ProfileCorrectionPreview>>,
}
impl ProfileRecord {
    pub fn reference(&self) -> ProfileCatalogReference {
        ProfileCatalogReference {
            id: self.profile.id.clone(),
            revision: self.profile.revision,
            profile_sha256: self.profile_sha256,
        }
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        self.profile.validate()?;
        validate_graph_storage(&self.graph.graph)?;
        if self.corrections.capacity() > MAX_PROFILE_CORRECTIONS
            || self.corrections.len() != self.profile.corrections.len()
        {
            return Err(INVALID);
        }
        let mut bytes = 0_usize;
        for (reference, object) in self.profile.corrections.iter().zip(self.corrections.iter()) {
            if reference.kind != object.kind || reference.object_sha256 != object.object_sha256 {
                return Err(INVALID);
            }
            bytes = bytes
                .checked_add(object.document_json.len())
                .ok_or(BUDGET)?;
        }
        if bytes > MAX_PROFILE_CORRECTION_BYTES {
            return Err(BUDGET);
        }
        if self.corrections.is_empty() != self.correction_preview.is_none() {
            return Err(INVALID);
        }
        if let Some(preview) = &self.correction_preview {
            preview.validate()?;
        }
        Ok(())
    }
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        size_of::<Self>()
            .checked_add(self.profile.retained_capacity_bytes()?)
            .and_then(|n| n.checked_add(size_of::<GraphIntent>()))
            .and_then(|n| n.checked_add(self.graph.graph.retained_capacity_bytes().ok()?))
            .and_then(|n| {
                n.checked_add(
                    128 + self.corrections.capacity() * size_of::<ProfileCorrectionObject>(),
                )
            })
            .and_then(|n| {
                self.corrections
                    .iter()
                    .try_fold(n, |n, o| n.checked_add(o.document_json.len() + 128))
            })
            .and_then(|n| {
                n.checked_add(self.correction_preview.as_ref().map_or(0, |p| {
                    size_of::<ProfileCorrectionPreview>()
                        + p.channel_peak_bounds.capacity() * size_of::<FiniteGraphScalar>()
                        + 128
                }))
            })
            .ok_or(BUDGET)
    }
}
pub(crate) fn validate_graph_storage(graph: &GraphDocument) -> Result<(), EngineError> {
    graph.validate_storage()?;
    if graph.nodes.capacity() > 128 || graph.edges.capacity() > 256 {
        return Err(BUDGET);
    }
    for node in &graph.nodes {
        if let crate::graph::NodeKind::Matrix { coefficients, .. } = &node.kind
            && coefficients.capacity() > 4
        {
            return Err(BUDGET);
        }
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileSummary {
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub profile_sha256: ProfileObjectHash,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfilePage {
    pub entries: Vec<ProfileSummary>,
    pub total: usize,
    pub next_offset: Option<usize>,
    pub operation: ProfileSaveInspection,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSaveStage {
    Saving,
    Retiring,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileSaveInspection {
    pub stage: Option<ProfileSaveStage>,
    pub request_id: Option<RequestId>,
    pub deadline_monotonic_ns: Option<u64>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSaveBinding {
    pub owner_id: Option<OwnerId>,
    pub session_id: Option<SessionId>,
    pub epochs: Epochs,
    pub device_epoch: u64,
    pub expected_revision: u64,
}
#[derive(Debug, Clone)]
pub struct ProfileSaveJob {
    pub request_id: RequestId,
    pub binding: ProfileSaveBinding,
    pub id: ProfileId,
    pub revision: u64,
    pub draft: Arc<ProfileDraft>,
    pub graph: Arc<GraphIntent>,
    pub deadline_monotonic_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCorrectionContext {
    pub sample_rate_hz: u32,
    pub channels: ChannelLayout,
    pub quantum_frames: usize,
}
impl ProfileCorrectionContext {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.sample_rate_hz == 0 || !(1..=8192).contains(&self.quantum_frames) {
            return Err(INVALID);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionNormalization {
    Preserved,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileCorrectionInput {
    TextReport {
        report_json: String,
        source_text: String,
    },
    Fir {
        path: String,
        purpose: FirPurpose,
        context: ProfileCorrectionContext,
        metadata_json: String,
        preamp_db: FiniteGraphScalar,
    },
}
impl ProfileCorrectionInput {
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let bytes = match self {
            Self::TextReport {
                report_json,
                source_text,
            } => {
                if report_json.capacity() > MAX_PROFILE_CORRECTION_BYTES
                    || source_text.capacity() > MAX_PROFILE_TEXT_BYTES
                {
                    return Err(BUDGET);
                }
                report_json.capacity() + source_text.capacity()
            }
            Self::Fir {
                path,
                metadata_json,
                ..
            } => {
                crate::queue::validate_path(path)?;
                if path.capacity() > 1024 || metadata_json.capacity() > MAX_PROFILE_METADATA_BYTES {
                    return Err(BUDGET);
                }
                path.capacity() + metadata_json.capacity()
            }
        };
        size_of::<Self>().checked_add(bytes).ok_or(BUDGET)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCorrectionObject {
    pub kind: ProfileCorrectionKind,
    pub object_sha256: ProfileObjectHash,
    pub document_json: Arc<str>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCorrectionPreview {
    pub context: ProfileCorrectionContext,
    pub preamp_db: FiniteGraphScalar,
    pub channel_peak_bounds: Vec<FiniteGraphScalar>,
    pub required_headroom_db: FiniteGraphScalar,
    pub normalization: CorrectionNormalization,
}
impl ProfileCorrectionPreview {
    /// Computes conservative attenuation from the largest declared channel peak.
    pub fn minimum_headroom_db(bounds: &[FiniteGraphScalar]) -> f64 {
        20.0 * bounds
            .iter()
            .map(|peak| peak.get())
            .fold(1.0_f64, f64::max)
            .log10()
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        self.context.validate()?;
        if self.preamp_db.get().abs() > 150.0
            || self.channel_peak_bounds.len() != self.context.channels.count()
            || self.channel_peak_bounds.capacity() > 2
            || self.channel_peak_bounds.iter().any(|p| p.get() < 0.0)
            || self.required_headroom_db.get()
                < Self::minimum_headroom_db(&self.channel_peak_bounds)
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub enum ProfileWorkOperation {
    Package(ProfilePackageOperation),
    Save {
        id: ProfileId,
        revision: u64,
        draft: Arc<ProfileDraft>,
        graph: Arc<GraphIntent>,
    },
    Import {
        id: ProfileId,
        revision: u64,
        draft: Arc<ProfileDraft>,
        input: Arc<ProfileCorrectionInput>,
    },
    FirAsset {
        path: String,
        purpose: FirPurpose,
    },
}
#[derive(Debug, Clone)]
pub struct ProfileWorkRequest {
    pub request_id: RequestId,
    pub binding: ProfileSaveBinding,
    pub deadline_monotonic_ns: u64,
    pub operation: ProfileWorkOperation,
}
impl ProfileWorkRequest {
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let bytes = match &self.operation {
            ProfileWorkOperation::Package(operation) => operation.retained_capacity_bytes()?,
            ProfileWorkOperation::Save {
                id, draft, graph, ..
            } => {
                id.capacity_bytes()
                    + draft.retained_capacity_bytes()?
                    + graph.graph.retained_capacity_bytes()?
            }
            ProfileWorkOperation::Import {
                id, draft, input, ..
            } => {
                id.capacity_bytes()
                    + draft.retained_capacity_bytes()?
                    + input.retained_capacity_bytes()?
            }
            ProfileWorkOperation::FirAsset { path, .. } => path.capacity(),
        };
        bytes.checked_add(size_of::<Self>() + 1024).ok_or(BUDGET)
    }
}
#[derive(Debug, Clone)]
pub enum ProfileWorkOutput {
    Package(Arc<ProfilePackageResult>),
    PackageIssue {
        error: EngineError,
        issue: ProfilePackageIssue,
    },
    Profile(Arc<ProfileRecord>),
    FirAsset(FirAssetReference),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCapabilities {
    pub save: bool,
    pub manual_resolution: bool,
    pub exact_associations: bool,
    pub exact_association_backends: Vec<crate::control::OutputBackend>,
    pub apply: bool,
    pub application: ProfileApplicationCapabilities,
    pub correction_import: bool,
    pub packages: bool,
    pub package: ProfilePackageCapabilities,
    pub max_profiles: usize,
    pub max_page: usize,
    pub max_profile_bytes: usize,
    pub max_graph_bytes: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfilePackageCapabilities {
    pub schema_version: u32,
    pub format: &'static str,
    pub max_manifest_bytes: usize,
    pub max_objects: usize,
    pub max_total_bytes: u64,
    pub new_ids_only: bool,
    pub preserves_imported_revision: bool,
    pub max_correction_chains: usize,
    pub requires_export_review: bool,
    pub automatic_privacy_proof: bool,
    pub automatic_application: bool,
    pub archives: bool,
}
impl Default for ProfilePackageCapabilities {
    fn default() -> Self {
        Self {
            schema_version: 1,
            format: "directory",
            max_manifest_bytes: crate::profile_packages::MAX_PACKAGE_MANIFEST_BYTES,
            max_objects: crate::profile_packages::MAX_PACKAGE_OBJECTS,
            max_total_bytes: crate::profile_packages::MAX_PACKAGE_BYTES,
            new_ids_only: true,
            preserves_imported_revision: true,
            max_correction_chains: 1,
            requires_export_review: true,
            automatic_privacy_proof: false,
            automatic_application: false,
            archives: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileApplicationCapabilities {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageExportReview {
    ReviewedExactCandidate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageContentReview {
    ReviewedNoPrivateData,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageRedistribution {
    RefuseMeasurementBytes,
    ReviewedRequiredMeasurements,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePackageStage {
    Inspected,
    Previewed,
    Exported,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageReviewSource {
    pub object: crate::profile_packages::PackageObject,
    pub local_path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfilePackageResult {
    pub operation: ProfilePackageStage,
    pub manifest: crate::profile_packages::PackageManifest,
    pub manifest_sha256: ProfileObjectHash,
    pub manifest_bytes: u64,
    pub total_bytes: u64,
    pub measurement_review_required: bool,
    pub content_review_required: bool,
    pub review_sources: Vec<PackageReviewSource>,
}
impl ProfilePackageResult {
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let bytes = size_of::<Self>()
            .checked_add(self.manifest.retained_capacity_bytes()?)
            .and_then(|n| {
                n.checked_add(
                    self.review_sources
                        .capacity()
                        .checked_mul(size_of::<PackageReviewSource>())?,
                )
            })
            .ok_or(BUDGET)?;
        self.review_sources.iter().try_fold(bytes, |n, s| {
            n.checked_add(s.local_path.capacity()).ok_or(BUDGET)
        })
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        use crate::profile_packages::*;
        self.manifest.validate()?;
        if self.manifest_bytes > MAX_PACKAGE_MANIFEST_BYTES as u64
            || self.total_bytes > MAX_PACKAGE_BYTES
            || self
                .manifest
                .total_object_bytes()?
                .checked_add(self.manifest_bytes)
                != Some(self.total_bytes)
            || self.review_sources.capacity() > MAX_PACKAGE_OBJECTS
            || self.review_sources.len() != self.manifest.objects.len()
            || self.retained_capacity_bytes()? > 256 * 1024
        {
            return Err(BUDGET);
        }
        for (index, source) in self.review_sources.iter().enumerate() {
            validate_package_path(&source.local_path)?;
            if !self.manifest.objects.contains(&source.object)
                || self.review_sources[..index]
                    .iter()
                    .any(|p| p.object == source.object)
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePackageReason {
    MissingObject,
    InvalidObject,
    InventoryMismatch,
    IdentityCollision,
    ContentReviewRequired,
    RedistributionReviewRequired,
    DestinationExists,
    UnsupportedContent,
    ResourceLimit,
    IoFailure,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageFilesystemState {
    Unchanged,
    InspectRequired,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfilePackageIssue {
    pub reason: ProfilePackageReason,
    pub object: Option<crate::profile_packages::PackageObject>,
    pub filesystem_state: PackageFilesystemState,
}
#[derive(Debug, Clone)]
pub enum ProfilePackageOperation {
    Inspect {
        directory: String,
    },
    Preview {
        profile: ProfileCatalogReference,
    },
    Import {
        directory: String,
        expected_manifest_sha256: ProfileObjectHash,
        profile: ProfileCatalogReference,
    },
    Export {
        directory: String,
        profile: ProfileCatalogReference,
        expected_manifest_sha256: ProfileObjectHash,
        redistribution: PackageRedistribution,
    },
}
impl ProfilePackageOperation {
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let (path, profile) = match self {
            Self::Inspect { directory } => (Some(directory), None),
            Self::Preview { profile } => (None, Some(profile)),
            Self::Import {
                directory, profile, ..
            }
            | Self::Export {
                directory, profile, ..
            } => (Some(directory), Some(profile)),
        };
        size_of::<Self>()
            .checked_add(path.map_or(0, String::capacity))
            .and_then(|n| n.checked_add(profile.map_or(0, |p| p.id.capacity_bytes())))
            .ok_or(BUDGET)
    }
    pub fn writes_files(&self) -> bool {
        matches!(self, Self::Import { .. } | Self::Export { .. })
    }
}
pub fn validate_package_path(path: &String) -> Result<(), EngineError> {
    if path.capacity() > 1024 {
        return Err(BUDGET);
    }
    if !path.starts_with('/')
        || path.chars().any(char::is_control)
        || path.split('/').any(|c| c == "." || c == "..")
    {
        return Err(INVALID);
    }
    Ok(())
}
