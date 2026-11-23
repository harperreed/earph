// ABOUTME: Defines bounded local artifact jobs, effect receipts, and safe exported manifests.
// ABOUTME: Separates filesystem outcomes from product state and keeps cancellation uncertainty explicit.
use super::*;
use crate::ids::RequestId;
use crate::{
    control::{CommandEnvelope, ReceiptStatus},
    profiles::ProfileObjectHash,
    provenance::RunningBuildProvenance,
};
use std::sync::Arc;
pub const DIAGNOSTIC_JOB_DEADLINE_NS: u64 = 5_000_000_000;
pub const MAX_DIAGNOSTIC_ARTIFACT_BYTES: usize = MAX_PREVIEW_BYTES + MAX_PREVIEW_METADATA_BYTES;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticArtifactOperation {
    Export,
    Inspect,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticFilesystemEffect {
    Unchanged,
    StageRetained,
    InstalledVerified,
    InstalledSyncUncertain,
    InspectRequired,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticInspection {
    Missing,
    Matching,
    Conflicting,
    Unreadable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticArtifactReason {
    Cancelled,
    DeadlineExceeded,
    UnsafePath,
    DestinationExists,
    InvalidObject,
    HashMismatch,
    StorageUnavailable,
    ResourceLimit,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticArtifactReceipt {
    pub schema_version: u32,
    pub binding: DiagnosticBinding,
    pub request_id: RequestId,
    pub operation: DiagnosticArtifactOperation,
    pub directory: String,
    pub expected_sha256: ProfileObjectHash,
    pub effect: DiagnosticFilesystemEffect,
    #[serde(deserialize_with = "nullable")]
    pub stage_directory: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub inspection: Option<DiagnosticInspection>,
    #[serde(deserialize_with = "nullable")]
    pub observed_bundle_sha256: Option<ProfileObjectHash>,
    #[serde(deserialize_with = "nullable")]
    pub manifest_sha256: Option<ProfileObjectHash>,
    #[serde(deserialize_with = "nullable")]
    pub diagnostics_bytes: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub manifest_bytes: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub bundle_cut: Option<DiagnosticCut>,
    pub deadline_monotonic_ns: u64,
    #[serde(deserialize_with = "nullable")]
    pub completed_monotonic_ns: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub reason: Option<DiagnosticArtifactReason>,
}
pub fn validate_diagnostic_directory(path: &String) -> Result<(), EngineError> {
    crate::profile_control::validate_package_path(path)?;
    if path == "/" || path.ends_with('/') {
        return Err(INVALID);
    }
    Ok(())
}
impl DiagnosticArtifactReceipt {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.binding.validate()?;
        validate_diagnostic_directory(&self.directory)?;
        if self.schema_version != 1
            || self.request_id.capacity_bytes() > 128
            || self.binding.owner_id.is_none()
            || self
                .deadline_monotonic_ns
                .checked_sub(DIAGNOSTIC_JOB_DEADLINE_NS)
                .is_none_or(|created| created < self.binding.monotonic_ns)
        {
            return Err(INVALID);
        }
        if let Some(stage) = &self.stage_directory {
            validate_diagnostic_directory(stage)?;
            let path = std::path::Path::new(stage);
            if path.parent() != std::path::Path::new(&self.directory).parent()
                || !path
                    .file_name()
                    .is_some_and(|n| n.to_str().is_some_and(|n| n.starts_with(".diagnostics-")))
            {
                return Err(INVALID);
            }
        }
        if self.effect == DiagnosticFilesystemEffect::Unchanged && self.stage_directory.is_some()
            || matches!(
                self.effect,
                DiagnosticFilesystemEffect::StageRetained
                    | DiagnosticFilesystemEffect::InstalledVerified
                    | DiagnosticFilesystemEffect::InstalledSyncUncertain
            ) && self.stage_directory.is_none()
            || self.operation == DiagnosticArtifactOperation::Inspect
                && (self.effect != DiagnosticFilesystemEffect::Unchanged
                    || self.stage_directory.is_some())
            || self.operation == DiagnosticArtifactOperation::Export && self.inspection.is_some()
            || self.diagnostics_bytes.is_some() != self.manifest_bytes.is_some()
            || self.manifest_sha256.is_some() != self.manifest_bytes.is_some()
            || self.observed_bundle_sha256.is_some() != self.manifest_bytes.is_some()
            || self.manifest_bytes.is_some() != self.bundle_cut.is_some()
            || self
                .diagnostics_bytes
                .is_some_and(|n| n == 0 || n > MAX_PREVIEW_BYTES as u64)
            || self
                .manifest_bytes
                .is_some_and(|n| n == 0 || n > MAX_PREVIEW_METADATA_BYTES as u64)
            || self.completed_monotonic_ns.is_some_and(|done| {
                self.deadline_monotonic_ns
                    .checked_sub(DIAGNOSTIC_JOB_DEADLINE_NS)
                    .is_none_or(|created| done < created)
            })
            || self.effect == DiagnosticFilesystemEffect::InstalledVerified
                && (self.observed_bundle_sha256 != Some(self.expected_sha256)
                    || self.manifest_sha256.is_none())
            || self.inspection == Some(DiagnosticInspection::Missing)
                && (self.manifest_sha256.is_some() || self.reason.is_some())
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub fn validate_for_status(&self, status: ReceiptStatus) -> Result<(), EngineError> {
        self.validate()?;
        if status == ReceiptStatus::Accepted {
            if self.completed_monotonic_ns.is_some()
                || self.reason.is_some()
                || self.inspection.is_some()
                || self.manifest_sha256.is_some()
                || self.stage_directory.is_some()
                || self.effect
                    != if self.operation == DiagnosticArtifactOperation::Export {
                        DiagnosticFilesystemEffect::InspectRequired
                    } else {
                        DiagnosticFilesystemEffect::Unchanged
                    }
            {
                return Err(INVALID);
            }
        } else {
            if self.completed_monotonic_ns.is_none()
                || !matches!(
                    status,
                    ReceiptStatus::Applied | ReceiptStatus::Failed | ReceiptStatus::Cancelled
                )
            {
                return Err(INVALID);
            }
            if status == ReceiptStatus::Applied {
                match self.operation {
                    DiagnosticArtifactOperation::Export
                        if self.effect != DiagnosticFilesystemEffect::InstalledVerified
                            || self.observed_bundle_sha256 != Some(self.expected_sha256)
                            || self.manifest_sha256.is_none()
                            || self.reason.is_some() =>
                    {
                        return Err(INVALID);
                    }
                    DiagnosticArtifactOperation::Inspect if self.inspection.is_none() => {
                        return Err(INVALID);
                    }
                    DiagnosticArtifactOperation::Inspect
                        if self.inspection == Some(DiagnosticInspection::Matching)
                            && (self.observed_bundle_sha256 != Some(self.expected_sha256)
                                || self.manifest_sha256.is_none()
                                || self.reason.is_some()) =>
                    {
                        return Err(INVALID);
                    }
                    _ => {}
                }
            } else if self.reason.is_none() {
                return Err(INVALID);
            }
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8], status: ReceiptStatus) -> Result<Self, EngineError> {
        if bytes.len() > MAX_PREVIEW_METADATA_BYTES {
            return Err(BUDGET);
        }
        let result: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        result.validate_for_status(status)?;
        Ok(result)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCancelState {
    Requested,
    AlreadyTerminal,
    UnknownOperation,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticCancelReceipt {
    pub schema_version: u32,
    pub binding: DiagnosticBinding,
    pub request_id: RequestId,
    pub state: DiagnosticCancelState,
}
impl DiagnosticCancelReceipt {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.binding.validate()?;
        if self.schema_version != 1
            || self.request_id.capacity_bytes() > 128
            || self.binding.owner_id.is_none()
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
#[derive(Debug, Clone)]
pub struct DiagnosticWorkRequest {
    pub request_id: RequestId,
    pub binding: DiagnosticBinding,
    pub operation: DiagnosticArtifactOperation,
    pub directory: String,
    pub expected_sha256: ProfileObjectHash,
    pub preview: Option<Arc<PreviewRead>>,
    pub deadline_monotonic_ns: u64,
}
impl DiagnosticWorkRequest {
    pub fn pending_receipt(&self) -> DiagnosticArtifactReceipt {
        DiagnosticArtifactReceipt {
            schema_version: 1,
            binding: self.binding.clone(),
            request_id: self.request_id.clone(),
            operation: self.operation,
            directory: self.directory.clone(),
            expected_sha256: self.expected_sha256,
            effect: if self.operation == DiagnosticArtifactOperation::Export {
                DiagnosticFilesystemEffect::InspectRequired
            } else {
                DiagnosticFilesystemEffect::Unchanged
            },
            stage_directory: None,
            inspection: None,
            observed_bundle_sha256: None,
            manifest_sha256: None,
            diagnostics_bytes: None,
            manifest_bytes: None,
            bundle_cut: None,
            deadline_monotonic_ns: self.deadline_monotonic_ns,
            completed_monotonic_ns: None,
            reason: None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct DiagnosticWorkCompletion {
    pub receipt: DiagnosticArtifactReceipt,
    pub status: ReceiptStatus,
    pub error: Option<EngineError>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticArtifactManifest {
    pub schema_version: u32,
    pub policy_version: u32,
    pub diagnostics_sha256: ProfileObjectHash,
    pub diagnostics_bytes: u64,
    pub cut: DiagnosticCut,
    pub running_build: RunningBuildProvenance,
    pub test_inventory: TestInventory,
    pub reproduction: ReproductionCoverage,
    pub missing_evidence: [MissingEvidence; 5],
}
impl DiagnosticArtifactManifest {
    pub fn for_preview(preview: &PreviewRead) -> Result<Self, EngineError> {
        preview.validate()?;
        let bundle = DiagnosticBundle::decode(preview.report.bytes.as_bytes())?;
        Ok(Self {
            schema_version: 1,
            policy_version: 1,
            diagnostics_sha256: preview.report.sha256,
            diagnostics_bytes: preview.report.byte_len as u64,
            cut: bundle.cut,
            running_build: bundle.running_build,
            test_inventory: bundle.test_inventory,
            reproduction: bundle.reproduction,
            missing_evidence: bundle.missing_evidence,
        })
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        self.running_build.validate_for_engine()?;
        if self.schema_version != 1
            || self.policy_version != 1
            || self.diagnostics_bytes == 0
            || self.diagnostics_bytes > MAX_PREVIEW_BYTES as u64
            || self.missing_evidence != MISSING_DIAGNOSTIC_EVIDENCE
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub fn validate_bundle(&self, bytes: &[u8]) -> Result<DiagnosticBundle, EngineError> {
        self.validate()?;
        if self.diagnostics_bytes != bytes.len() as u64
            || self.diagnostics_sha256 != super::preview_digest(bytes)?
        {
            return Err(INVALID);
        }
        let bundle = DiagnosticBundle::decode(bytes)?;
        if bundle.cut != self.cut
            || bundle.running_build != self.running_build
            || bundle.test_inventory != self.test_inventory
            || bundle.reproduction != self.reproduction
            || bundle.missing_evidence != self.missing_evidence
        {
            return Err(INVALID);
        }
        Ok(bundle)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        if bytes.len() > MAX_PREVIEW_METADATA_BYTES {
            return Err(BUDGET);
        }
        let result: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        result.validate()?;
        Ok(result)
    }
}
pub(crate) type DiagnosticRecord = Arc<(CommandEnvelope, crate::control::Receipt)>;
