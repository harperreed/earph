// ABOUTME: Defines portable listening intent without output handles or runtime authorization.
// ABOUTME: Bounds saved intent, intersects independent caps, and explains deterministic setup selection.
use crate::{
    EngineError, ErrorCode,
    control::{OutputBackend, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode, VolumePolicy},
};
use serde::{Deserialize, Serialize};

pub const MAX_PROFILE_BYTES: usize = 8 * 1024;
pub const MAX_PROFILE_CORRECTIONS: usize = 32;
pub const MAX_PROFILES: usize = 32;
pub const MAX_PROFILE_ASSOCIATIONS: usize = 64;
pub const MAX_PROFILE_ASSOCIATION_BYTES: usize = 512;
const NAME_BYTES: usize = 128;
const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid listening profile");
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "listening profile exceeds its storage or resource limits",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileObjectHash([u8; 64]);
impl ProfileObjectHash {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated ASCII object hash")
    }
}
impl TryFrom<String> for ProfileObjectHash {
    type Error = EngineError;
    fn try_from(value: String) -> Result<Self, EngineError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(INVALID);
        }
        Ok(Self(value.as_bytes().try_into().map_err(|_| INVALID)?))
    }
}
impl From<ProfileObjectHash> for String {
    fn from(value: ProfileObjectHash) -> Self {
        value.as_str().into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSourcePolicy {
    LocalFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileOutputSelector {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub device_id: DeviceId,
    pub manual_setup: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileVolumeIntent {
    pub mode: VolumeMode,
    pub software_cap_db: f64,
    pub hardware_cap_db: Option<f64>,
}
impl ProfileVolumeIntent {
    /// Intersect independent caps without copying fixed-output arming or hardware readback.
    pub fn effective_policy(&self, local: &VolumePolicy) -> Result<VolumePolicy, EngineError> {
        let requested = VolumePolicy::new(self.software_cap_db, self.hardware_cap_db)?;
        let hardware = match (local.hardware_cap_db(), requested.hardware_cap_db()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        VolumePolicy::new(
            local.software_cap_db().min(requested.software_cap_db()),
            hardware,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileLatencyMode {
    QualityFocused,
    Balanced,
    LowLatency,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileLatencyIntent {
    pub mode: ProfileLatencyMode,
    pub max_resident_bytes: u64,
    pub max_preparation_bytes: u64,
    pub max_latency_frames: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileCorrectionKind {
    TextReport,
    FirManifest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileCorrectionReference {
    pub kind: ProfileCorrectionKind,
    pub object_sha256: ProfileObjectHash,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileDocument {
    pub schema_version: u32,
    pub id: ProfileId,
    pub revision: u64,
    pub name: String,
    pub graph_sha256: ProfileObjectHash,
    pub source_policy: ProfileSourcePolicy,
    pub fidelity: FidelityPolicy,
    pub output: ProfileOutputSelector,
    pub volume: ProfileVolumeIntent,
    pub latency: ProfileLatencyIntent,
    pub corrections: Vec<ProfileCorrectionReference>,
}
impl ProfileDocument {
    /// Validate intent before retaining it; executable admission separately resolves its object references.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.retained_capacity_bytes()? > MAX_PROFILE_BYTES
            || self.corrections.len() > MAX_PROFILE_CORRECTIONS
        {
            return Err(BUDGET);
        }
        if self.schema_version != 1 {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported listening profile version",
            ));
        }
        if self.revision == 0 {
            return Err(INVALID);
        }
        text(&self.name)?;
        if let Some(setup) = &self.output.manual_setup {
            text(setup)?;
        }
        VolumePolicy::new(self.volume.software_cap_db, self.volume.hardware_cap_db)?;
        if self.latency.max_resident_bytes == 0
            || self.latency.max_preparation_bytes < self.latency.max_resident_bytes
        {
            return Err(BUDGET);
        }
        Ok(())
    }
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        let mut bytes = size_of::<Self>();
        for count in [
            self.id.capacity_bytes(),
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
        ] {
            bytes = bytes.checked_add(count).ok_or(BUDGET)?;
        }
        Ok(bytes)
    }
}

/// Read a bounded portable document; this never opens output or grants authorization.
pub fn read_profile(bytes: &[u8]) -> Result<ProfileDocument, EngineError> {
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(BUDGET);
    }
    let document: ProfileDocument = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    document.validate()?;
    Ok(document)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileAssociation {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub device_identity_sha256: ProfileObjectHash,
    pub profile_id: ProfileId,
}

/// The owner supplies actual device evidence. Imported profile data cannot deserialize this context.
#[derive(Debug, Clone)]
pub struct ProfileDeviceContext {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub device_id: DeviceId,
    pub verified_identity_sha256: Option<ProfileObjectHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileRevision {
    pub id: ProfileId,
    pub revision: u64,
}
impl From<&ProfileDocument> for ProfileRevision {
    fn from(profile: &ProfileDocument) -> Self {
        Self {
            id: profile.id.clone(),
            revision: profile.revision,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSelectionReason {
    Manual,
    ExactDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSelectionIssue {
    NoVerifiedMatch,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProfileResolution {
    Selected {
        profile: ProfileRevision,
        reason: ProfileSelectionReason,
    },
    SelectionRequired {
        reason: ProfileSelectionIssue,
        candidates: Vec<ProfileRevision>,
    },
}

/// Resolve saved intent only. Selection grants no execution, routing, or output authorization.
pub fn resolve_profile(
    profiles: &[ProfileDocument],
    associations: &[ProfileAssociation],
    manual: Option<&ProfileId>,
    device: Option<&ProfileDeviceContext>,
) -> Result<ProfileResolution, EngineError> {
    if profiles.len() > MAX_PROFILES || associations.len() > MAX_PROFILE_ASSOCIATIONS {
        return Err(BUDGET);
    }
    for (index, profile) in profiles.iter().enumerate() {
        profile.validate()?;
        if profiles[..index].iter().any(|prior| prior.id == profile.id) {
            return Err(INVALID);
        }
    }
    for association in associations {
        if size_of::<ProfileAssociation>()
            .checked_add(association.profile_id.capacity_bytes())
            .ok_or(BUDGET)?
            > MAX_PROFILE_ASSOCIATION_BYTES
        {
            return Err(BUDGET);
        }
        let profile = profiles
            .iter()
            .find(|p| p.id == association.profile_id)
            .ok_or(INVALID)?;
        if profile.output.backend != association.backend
            || profile.output.route != association.route
        {
            return Err(INVALID);
        }
    }
    if let Some(id) = manual {
        let profile = profiles.iter().find(|p| &p.id == id).ok_or(INVALID)?;
        if device.is_some_and(|d| !matches_device(profile, d)) {
            return Err(EngineError::new(
                ErrorCode::DeviceUnavailable,
                "selected profile targets another output",
            ));
        }
        return Ok(ProfileResolution::Selected {
            profile: profile.into(),
            reason: ProfileSelectionReason::Manual,
        });
    }
    let mut candidates = Vec::new();
    if let Some(device) = device
        && let Some(identity) = device.verified_identity_sha256
    {
        for profile in profiles {
            if matches_device(profile, device)
                && associations.iter().any(|a| {
                    a.backend == device.backend
                        && a.route == device.route
                        && a.device_identity_sha256 == identity
                        && a.profile_id == profile.id
                })
            {
                candidates.push(ProfileRevision::from(profile));
            }
        }
    }
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    if candidates.len() == 1 {
        return Ok(ProfileResolution::Selected {
            profile: candidates.pop().expect("one candidate"),
            reason: ProfileSelectionReason::ExactDevice,
        });
    }
    Ok(ProfileResolution::SelectionRequired {
        reason: if candidates.is_empty() {
            ProfileSelectionIssue::NoVerifiedMatch
        } else {
            ProfileSelectionIssue::Ambiguous
        },
        candidates,
    })
}
fn matches_device(profile: &ProfileDocument, device: &ProfileDeviceContext) -> bool {
    profile.output.backend == device.backend
        && profile.output.route == device.route
        && profile.output.device_id == device.device_id
}
fn text(value: &str) -> Result<(), EngineError> {
    if value.len() > NAME_BYTES {
        return Err(BUDGET);
    }
    if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
        return Err(INVALID);
    }
    Ok(())
}
