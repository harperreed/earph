// ABOUTME: Defines bounded owner save requests and immutable listening profile records.
// ABOUTME: Keeps portable intent separate from device qualification and catalog publication.
use crate::{
    EngineError, ErrorCode,
    graph::GraphDocument,
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
        Ok(())
    }
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        size_of::<Self>()
            .checked_add(self.profile.retained_capacity_bytes()?)
            .and_then(|n| n.checked_add(size_of::<GraphIntent>()))
            .and_then(|n| n.checked_add(self.graph.graph.retained_capacity_bytes().ok()?))
            .and_then(|n| n.checked_add(128))
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileCapabilities {
    pub save: bool,
    pub manual_resolution: bool,
    pub exact_associations: bool,
    pub apply: bool,
    pub correction_import: bool,
    pub packages: bool,
    pub max_profiles: usize,
    pub max_page: usize,
    pub max_profile_bytes: usize,
    pub max_graph_bytes: usize,
}
