// ABOUTME: Projects directory package capabilities and outcomes from the existing owner.
// ABOUTME: Retains exact review facts and possible filesystem effects after failed operations.
use super::nullable;
use earph_core::{
    profile_control::ProfileCatalogReference, profile_packages::PackageObject,
    profiles::ProfileObjectHash,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePackageFormatView {
    Directory,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePackageCapabilitiesView {
    pub schema_version: u32,
    pub format: ProfilePackageFormatView,
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifestView {
    pub schema_version: u32,
    pub profile: ProfileCatalogReference,
    pub objects: Vec<PackageObject>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePackageStageView {
    Inspected,
    Previewed,
    Exported,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageReviewSourceView {
    pub object: PackageObject,
    pub local_path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePackageResultView {
    pub operation: ProfilePackageStageView,
    pub manifest: PackageManifestView,
    pub manifest_sha256: ProfileObjectHash,
    pub manifest_bytes: u64,
    pub total_bytes: u64,
    pub measurement_review_required: bool,
    pub content_review_required: bool,
    pub review_sources: Vec<PackageReviewSourceView>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfilePackageReasonView {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageFilesystemStateView {
    Unchanged,
    InspectRequired,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfilePackageIssueView {
    pub reason: ProfilePackageReasonView,
    #[serde(deserialize_with = "nullable")]
    pub object: Option<PackageObject>,
    pub filesystem_state: PackageFilesystemStateView,
}
