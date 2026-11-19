// ABOUTME: Bounds declared profile package manifests and exact typed object references.
// ABOUTME: Compares owner-observed inventories without claiming byte, provenance, or export authorization checks.
//! Owner import preserves verified original object bytes through shared immutable publication, then reloads and validates them.
//! `crates/earph-engine/src/profiles.rs::persist_record` publishes retained original graph and profile bytes.
//! Do not retain imported hashes across reserialization or introduce another normalization or identity path.
//! Publish exact bytes through `crates/earph-engine/src/runtime.rs::publish_immutable_file`.
//! Reload with `crates/earph-engine/src/profiles.rs::read_object` and `crates/earph-engine/src/profiles.rs::restore`.
use crate::{
    EngineError, ErrorCode,
    profile_control::{ProfileCatalogReference, validate_catalog},
    profiles::{ProfileCorrectionKind, ProfileDocument, ProfileObjectHash},
};
use serde::{Deserialize, Serialize};

pub const MAX_PACKAGE_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_PACKAGE_OBJECTS: usize = 64;
pub const MAX_PACKAGE_BYTES: u64 = 32 * 1024 * 1024;
pub const PACKAGE_MANIFEST_FILE: &str = "manifest.json";
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid declared profile package");
const MISSING: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "required package object or reference is missing",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "profile package exceeds declared transport or retention bounds",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageObjectKind {
    Profile,
    Graph,
    TextReport,
    TextSource,
    FirManifest,
    FirWav,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageObject {
    pub kind: PackageObjectKind,
    pub sha256: ProfileObjectHash,
    pub bytes: u64,
}
impl PackageObject {
    /// Derive the existing typed owner filename; no imported path participates.
    pub fn file_name(&self) -> String {
        let suffix = match self.kind {
            PackageObjectKind::Profile => "profile.json",
            PackageObjectKind::Graph => "graph.json",
            PackageObjectKind::TextReport => "text-report.json",
            PackageObjectKind::TextSource => "text-source.txt",
            PackageObjectKind::FirManifest => "fir-manifest.json",
            PackageObjectKind::FirWav => "wav",
        };
        format!("{}.{suffix}", self.sha256.as_str())
    }
}

/// Facts supplied by the owner after hashing original bytes and validating their actual type.
/// This model performs no hashing, provenance reconstruction, filesystem inspection, or export approval.
/// Imported JSON cannot provide this observation directly.
/// ```compile_fail
/// let _: earph_core::profile_packages::PackageObjectObservation =
///     serde_json::from_str("{}").unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageObjectObservation {
    pub kind: PackageObjectKind,
    pub sha256: ProfileObjectHash,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageManifest {
    pub schema_version: u32,
    pub profile: ProfileCatalogReference,
    pub objects: Vec<PackageObject>,
}
impl PackageManifest {
    /// Validate declared transport bounds and identities, not the enclosed object contents.
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "unsupported profile package version",
            ));
        }
        validate_catalog(std::slice::from_ref(&self.profile), 1)?;
        if self.objects.capacity() > MAX_PACKAGE_OBJECTS
            || self.retained_capacity_bytes()? > MAX_PACKAGE_MANIFEST_BYTES
        {
            return Err(BUDGET);
        }
        for (index, object) in self.objects.iter().enumerate() {
            if object.bytes == 0 {
                return Err(INVALID);
            }
            for previous in &self.objects[..index] {
                if previous.sha256 == object.sha256
                    && (previous.kind == object.kind || previous.bytes != object.bytes)
                {
                    return Err(INVALID);
                }
            }
        }
        if self
            .objects
            .iter()
            .filter(|o| o.kind == PackageObjectKind::Profile)
            .count()
            != 1
            || self
                .objects
                .iter()
                .filter(|o| o.kind == PackageObjectKind::Graph)
                .count()
                != 1
        {
            return Err(INVALID);
        }
        self.require_object(PackageObjectKind::Profile, self.profile.profile_sha256)?;
        let encoded = serde_json::to_vec(self).map_err(|_| INVALID)?;
        self.validate_encoded_length(encoded.len())
    }

    /// Include spare identifier and object-vector capacity before retaining a declared manifest.
    pub fn retained_capacity_bytes(&self) -> Result<usize, EngineError> {
        size_of::<Self>()
            .checked_add(self.profile.id.capacity_bytes())
            .and_then(|n| {
                n.checked_add(
                    self.objects
                        .capacity()
                        .checked_mul(size_of::<PackageObject>())?,
                )
            })
            .ok_or(BUDGET)
    }

    /// Sum original declared object bytes without normalizing or rewriting them.
    pub fn total_object_bytes(&self) -> Result<u64, EngineError> {
        self.objects.iter().try_fold(0_u64, |sum, object| {
            sum.checked_add(object.bytes).ok_or(BUDGET)
        })
    }

    fn validate_encoded_length(&self, manifest_bytes: usize) -> Result<(), EngineError> {
        if manifest_bytes > MAX_PACKAGE_MANIFEST_BYTES
            || self
                .total_object_bytes()?
                .checked_add(u64::try_from(manifest_bytes).map_err(|_| BUDGET)?)
                .is_none_or(|n| n > MAX_PACKAGE_BYTES)
        {
            return Err(BUDGET);
        }
        Ok(())
    }

    fn require_object(
        &self,
        kind: PackageObjectKind,
        sha256: ProfileObjectHash,
    ) -> Result<(), EngineError> {
        self.objects
            .iter()
            .any(|o| o.kind == kind && o.sha256 == sha256)
            .then_some(())
            .ok_or(MISSING)
    }

    /// Check a parsed profile's declared references; the owner must bind it to its original byte hash.
    pub fn validate_profile_references(
        &self,
        profile: &ProfileDocument,
    ) -> Result<(), EngineError> {
        self.validate()?;
        profile.validate()?;
        if profile.id != self.profile.id || profile.revision != self.profile.revision {
            return Err(INVALID);
        }
        self.require_object(PackageObjectKind::Graph, profile.graph_sha256)?;
        for correction in &profile.corrections {
            let kind = match correction.kind {
                ProfileCorrectionKind::TextReport => PackageObjectKind::TextReport,
                ProfileCorrectionKind::FirManifest => PackageObjectKind::FirManifest,
            };
            self.require_object(kind, correction.object_sha256)?;
        }
        Ok(())
    }

    /// Compare the complete actual consumer read set against declared objects, including surplus objects.
    /// Only owner byte readers and existing type/provenance consumers may supply these facts.
    pub fn validate_observed_inventory(
        &self,
        observed: &[PackageObjectObservation],
    ) -> Result<(), EngineError> {
        self.validate()?;
        if observed.len() > MAX_PACKAGE_OBJECTS {
            return Err(BUDGET);
        }
        if observed.len() != self.objects.len() {
            return Err(INVALID);
        }
        for (index, observation) in observed.iter().enumerate() {
            if observed[..index]
                .iter()
                .any(|prior| prior.kind == observation.kind && prior.sha256 == observation.sha256)
            {
                return Err(INVALID);
            }
            let declared = self
                .objects
                .iter()
                .find(|o| o.kind == observation.kind && o.sha256 == observation.sha256)
                .ok_or(MISSING)?;
            if observation.bytes != declared.bytes {
                return Err(INVALID);
            }
        }
        Ok(())
    }

    /// Match an exact declared basename without path decoding, normalization, or filesystem access.
    pub fn object_named(&self, name: &str) -> Result<&PackageObject, EngineError> {
        self.validate()?;
        self.objects
            .iter()
            .find(|object| object.file_name() == name)
            .ok_or(MISSING)
    }
}

/// Read a bounded declaration. Owners must separately verify files, raw hashes, provenance, and export rights.
pub fn read_package_manifest(bytes: &[u8]) -> Result<PackageManifest, EngineError> {
    if bytes.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(BUDGET);
    }
    let manifest: PackageManifest = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
    manifest.validate()?;
    manifest.validate_encoded_length(bytes.len())?;
    Ok(manifest)
}
