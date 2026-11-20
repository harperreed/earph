// ABOUTME: Validates exact immutable redacted preview bytes and honest evidence coverage.
// ABOUTME: Binds a digest to one safe owner cut without granting export or output authority.
use super::*;
use crate::{profiles::ProfileObjectHash, provenance::RunningBuildProvenance};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub const MAX_PREVIEW_BYTES: usize = 512 * 1024;
pub const MAX_PREVIEW_METADATA_BYTES: usize = 64 * 1024;
pub const MAX_DIAGNOSTIC_RETAINED_BYTES: usize = 8 * 1024 * 1024;
pub const DIAGNOSTIC_CANDIDATE_LIFETIME_NS: u64 = 30_000_000_000;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestInventory {
    NotAttached,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproductionCoverage {
    Incomplete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingEvidence {
    PrivateMedia,
    AuthoredSettings,
    TestArtifacts,
    RuntimeExecutableIdentity,
    ExecutionCheckout,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OmittedCategory {
    Credentials,
    Serials,
    PrivatePaths,
    AuthoredIdentifiers,
    AuthoredText,
    Audio,
    ErrorsAndLogs,
    RouteFingerprints,
}
pub const DIAGNOSTIC_OMISSIONS: [OmittedCategory; 8] = [
    OmittedCategory::Credentials,
    OmittedCategory::Serials,
    OmittedCategory::PrivatePaths,
    OmittedCategory::AuthoredIdentifiers,
    OmittedCategory::AuthoredText,
    OmittedCategory::Audio,
    OmittedCategory::ErrorsAndLogs,
    OmittedCategory::RouteFingerprints,
];
pub const MISSING_DIAGNOSTIC_EVIDENCE: [MissingEvidence; 5] = [
    MissingEvidence::PrivateMedia,
    MissingEvidence::AuthoredSettings,
    MissingEvidence::TestArtifacts,
    MissingEvidence::RuntimeExecutableIdentity,
    MissingEvidence::ExecutionCheckout,
];
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticBundle {
    pub schema_version: u32,
    pub policy_version: u32,
    pub cut: DiagnosticCut,
    pub signal_path: SignalPathReport,
    pub metrics: MetricsReport,
    pub history: EventsReport,
    pub running_build: RunningBuildProvenance,
    pub test_inventory: TestInventory,
    pub reproduction: ReproductionCoverage,
    pub missing_evidence: [MissingEvidence; 5],
    pub omitted_categories: [OmittedCategory; 8],
}
impl DiagnosticBundle {
    pub fn validate(&self) -> Result<(), EngineError> {
        self.signal_path.validate()?;
        self.metrics.validate()?;
        self.history.validate_history(MAX_DIAGNOSTIC_HISTORY)?;
        self.running_build.validate_for_engine()?;
        if self.schema_version != 1
            || self.policy_version != 1
            || self.signal_path.cut != self.cut
            || self.metrics.cut != self.cut
            || self.history.cut != self.cut
            || self.signal_path.requested.muted != self.history.snapshot.muted
            || self.signal_path.requested.software_gain_db != self.history.snapshot.software_gain_db
            || self.signal_path.requested.software_gain_zero
                != self.history.snapshot.software_gain_zero
            || self.history.has_more
            || self.missing_evidence != MISSING_DIAGNOSTIC_EVIDENCE
            || self.omitted_categories != DIAGNOSTIC_OMISSIONS
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        if bytes.len() > MAX_PREVIEW_BYTES {
            return Err(BUDGET);
        }
        let bundle: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        bundle.validate()?;
        Ok(bundle)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticDigestAlgorithm {
    Sha256,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewReport {
    pub schema_version: u32,
    pub policy_version: u32,
    pub cut: DiagnosticCut,
    pub digest_algorithm: DiagnosticDigestAlgorithm,
    pub sha256: ProfileObjectHash,
    pub byte_len: usize,
    pub bytes: Arc<str>,
    pub created_monotonic_ns: u64,
    pub expires_monotonic_ns: u64,
}
pub type PreviewRead = DiagnosticRead<PreviewReport>;
impl DiagnosticReport for PreviewReport {
    const MAX_READ_BYTES: usize = 1024 * 1024;
    fn cut(&self) -> DiagnosticCut {
        self.cut
    }
    fn validate(&self) -> Result<(), EngineError> {
        if self.bytes.len() > MAX_PREVIEW_BYTES {
            return Err(BUDGET);
        }
        if self.schema_version != 1
            || self.policy_version != 1
            || self.byte_len != self.bytes.len()
            || self
                .created_monotonic_ns
                .checked_add(DIAGNOSTIC_CANDIDATE_LIFETIME_NS)
                != Some(self.expires_monotonic_ns)
            || self.sha256 != diagnostic_digest(self.bytes.as_bytes())?
            || DiagnosticBundle::decode(self.bytes.as_bytes())?.cut != self.cut
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
pub(crate) fn diagnostic_digest(bytes: &[u8]) -> Result<ProfileObjectHash, EngineError> {
    ProfileObjectHash::try_from(
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
}
