// ABOUTME: Validates actual reachable package objects with the canonical profile and Media consumers.
// ABOUTME: Reuses prepared original bytes, prospective catalog admission, and shared owner publication.
use super::*;
use earph_core::{dsp_nodes::FirAssetReference, profile_control::*, profile_packages::*};
#[path = "profile_package_io.rs"]
mod io;
use io::{PackageDirectory, PackageStage};

fn kind(value: &str) -> Result<PackageObjectKind, EngineError> {
    Ok(match value {
        "profile" => PackageObjectKind::Profile,
        "graph" => PackageObjectKind::Graph,
        "text-report" => PackageObjectKind::TextReport,
        "text-source" => PackageObjectKind::TextSource,
        "fir-manifest" => PackageObjectKind::FirManifest,
        "wav" => PackageObjectKind::FirWav,
        _ => return Err(INVALID),
    })
}
fn rank(kind: PackageObjectKind) -> u8 {
    match kind {
        PackageObjectKind::Profile => 0,
        PackageObjectKind::Graph => 1,
        PackageObjectKind::TextReport => 2,
        PackageObjectKind::TextSource => 3,
        PackageObjectKind::FirManifest => 4,
        PackageObjectKind::FirWav => 5,
    }
}
struct Reader<'a> {
    runtime: &'a Path,
    package: Option<(PackageDirectory, PackageManifest)>,
    observed: Vec<PackageObjectObservation>,
    sources: Vec<PackageReviewSource>,
    text: Vec<(ProfileObjectHash, Vec<u8>)>,
}
impl<'a> Reader<'a> {
    fn fir_bytes(&self, reference: &FirAssetReference) -> Result<u64, EngineError> {
        let hash = ProfileObjectHash::try_from(reference.sha256.as_str().to_owned())?;
        let size = if let Some((_, manifest)) = &self.package {
            manifest
                .objects
                .iter()
                .find(|o| o.kind == PackageObjectKind::FirWav && o.sha256 == hash)
                .ok_or(io::MISSING)?
                .bytes
        } else {
            let path = self
                .runtime
                .join("assets")
                .join(format!("{}.wav", hash.as_str()));
            runtime::open_private_file(&path)?
                .ok_or(io::MISSING)?
                .metadata()
                .map_err(|_| runtime::IO)?
                .len()
        };
        if size > earph_media::MAX_FIR_WAV_BYTES as u64 {
            return Err(BUDGET);
        }
        Ok(size)
    }
    fn read(
        &mut self,
        hash: ProfileObjectHash,
        kind: PackageObjectKind,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, EngineError> {
        check(cancel)?;
        let mut object = PackageObject {
            kind,
            sha256: hash,
            bytes: 0,
        };
        let name = object.file_name();
        let path = if let Some((directory, manifest)) = &self.package {
            let declared = manifest
                .objects
                .iter()
                .find(|o| o.kind == kind && o.sha256 == hash)
                .ok_or(io::MISSING)?;
            if declared.bytes > limit as u64 {
                return Err(BUDGET);
            }
            if directory.length(&name)? != declared.bytes {
                return Err(INVALID);
            }
            directory.path.join(&name)
        } else if kind == PackageObjectKind::FirWav {
            self.runtime.join("assets").join(&name)
        } else {
            self.runtime.join("profiles").join(&name)
        };
        let bytes = if let Some((directory, _)) = &self.package {
            directory.read(&name, limit, cancel)?
        } else if kind == PackageObjectKind::FirWav {
            let file = runtime::open_private_file(&path)?.ok_or(io::MISSING)?;
            let size = usize::try_from(file.metadata().map_err(|_| runtime::IO)?.len())
                .map_err(|_| BUDGET)?;
            io::read_span(file, size, limit, cancel)?
        } else {
            runtime::read(&path, limit)?.ok_or(io::MISSING)?
        };
        if self::hash(&bytes)? != hash {
            return Err(INVALID);
        }
        object.bytes = bytes.len() as u64;
        if let Some((_, manifest)) = &self.package
            && !manifest.objects.contains(&object)
        {
            return Err(INVALID);
        }
        if !self
            .observed
            .iter()
            .any(|o| o.kind == kind && o.sha256 == hash)
        {
            if self.observed.len() >= MAX_PACKAGE_OBJECTS {
                return Err(BUDGET);
            }
            let total = self
                .observed
                .iter()
                .try_fold(object.bytes, |n, o| n.checked_add(o.bytes).ok_or(BUDGET))?;
            if total > MAX_PACKAGE_BYTES {
                return Err(BUDGET);
            }
            let local_path = path.to_str().ok_or(INVALID)?.to_owned();
            validate_package_path(&local_path)?;
            self.observed.push(PackageObjectObservation {
                kind,
                sha256: hash,
                bytes: object.bytes,
            });
            self.sources
                .push(PackageReviewSource { object, local_path });
            if kind == PackageObjectKind::TextSource {
                self.text.push((hash, bytes.clone()));
            }
        }
        check(cancel)?;
        Ok(bytes)
    }
}
struct PreparedPackage {
    prepared: PreparedProfile,
    result: ProfilePackageResult,
    manifest_bytes: Vec<u8>,
}
fn admit_fir_holdings(
    known: u64,
    scratch: u64,
    banks: u64,
    preparation: u64,
    retained: u64,
) -> Result<(), EngineError> {
    let peak = known
        .checked_add(scratch)
        .and_then(|n| n.checked_add(banks))
        .ok_or(BUDGET)?;
    if peak > preparation || retained.checked_add(peak).is_none_or(|n| n > LIMIT) {
        return Err(BUDGET);
    }
    Ok(())
}
fn validate_source_location(runtime: &Path, directory: &str) -> Result<(), EngineError> {
    let source = Path::new(directory);
    if source.starts_with(runtime) || runtime.starts_with(source) {
        return Err(EngineError::new(
            ErrorCode::AssetInvalid,
            "package source overlaps owner storage",
        ));
    }
    Ok(())
}
fn prepare(
    runtime: &Path,
    reference: &ProfileCatalogReference,
    directory: Option<&str>,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<PreparedPackage, EngineError> {
    if let Some(directory) = directory {
        validate_source_location(runtime, directory)?;
    }
    let package = directory
        .map(|path| {
            let directory = PackageDirectory::open(Path::new(path), cancel)?;
            let bytes =
                directory.read(PACKAGE_MANIFEST_FILE, MAX_PACKAGE_MANIFEST_BYTES, cancel)?;
            let manifest = read_package_manifest(&bytes)?;
            if manifest.profile != *reference {
                return Err(INVALID);
            }
            Ok::<_, EngineError>((directory, manifest, bytes))
        })
        .transpose()?;
    let original_manifest = package.as_ref().map(|(_, _, bytes)| bytes.clone());
    let mut reader = Reader {
        runtime,
        package: package.map(|(directory, manifest, _)| (directory, manifest)),
        observed: Vec::new(),
        sources: Vec::new(),
        text: Vec::new(),
    };
    let profile_bytes = reader.read(
        reference.profile_sha256,
        PackageObjectKind::Profile,
        MAX_PROFILE_BYTES,
        cancel,
    )?;
    let profile = read_profile(&profile_bytes)?;
    if profile.id != reference.id || profile.revision != reference.revision {
        return Err(INVALID);
    }
    let graph_bytes = reader.read(
        profile.graph_sha256,
        PackageObjectKind::Graph,
        MAX_GRAPH_INTENT_BYTES,
        cancel,
    )?;
    let graph: GraphIntent = serde_json::from_slice(&graph_bytes).map_err(|_| INVALID)?;
    graph.graph.validate_structure()?;
    let known_holding = (profile_bytes.capacity() as u64)
        .checked_add(graph_bytes.capacity() as u64)
        .and_then(|n| n.checked_add(SCRATCH))
        .ok_or(BUDGET)?;
    if known_holding > profile.latency.max_preparation_bytes
        || retained
            .checked_add(known_holding)
            .is_none_or(|n| n > LIMIT)
    {
        return Err(BUDGET);
    }
    let admission = assets::GraphAssets::inspect(&graph.graph)?;
    if admission.declared_bytes() > profile.latency.max_resident_bytes {
        return Err(BUDGET);
    }
    let scratch = admission.scratch_bytes();
    // Reject already-impossible geometry before source metadata or byte reads.
    admit_fir_holdings(
        known_holding,
        scratch,
        admission.declared_bytes(),
        profile.latency.max_preparation_bytes,
        retained,
    )?;
    let mut plans = Vec::new();
    let mut pending = 0u64;
    for asset in admission.references() {
        check(cancel)?;
        let bytes = reader.fir_bytes(asset)?;
        check(cancel)?;
        let estimate = assets::GraphAssets::coefficient_bytes(asset)?
            .checked_add(bytes)
            .and_then(|n| n.checked_add(size_of::<assets::PreparedFirSource>() as u64))
            .ok_or(BUDGET)?;
        pending = pending.checked_add(estimate).ok_or(BUDGET)?;
        plans.push((asset, bytes, estimate));
    }
    let mut fir = Vec::new();
    let slots = (plans.len() as u64)
        .checked_mul(size_of::<assets::PreparedFirSource>() as u64)
        .ok_or(BUDGET)?;
    admit_fir_holdings(
        known_holding.checked_add(slots).ok_or(BUDGET)?,
        scratch,
        pending,
        profile.latency.max_preparation_bytes,
        retained,
    )?;
    fir.try_reserve_exact(plans.len()).map_err(|_| BUDGET)?;
    let known = known_holding
        .checked_add(
            fir.capacity()
                .checked_mul(size_of::<assets::PreparedFirSource>())
                .ok_or(BUDGET)? as u64,
        )
        .ok_or(BUDGET)?;
    let mut held = 0u64;
    for (asset, bytes, estimate) in plans {
        admit_fir_holdings(
            known,
            scratch,
            held.checked_add(pending).ok_or(BUDGET)?,
            profile.latency.max_preparation_bytes,
            retained,
        )?;
        pending = pending.checked_sub(estimate).ok_or(BUDGET)?;
        let bytes = reader.read(
            ProfileObjectHash::try_from(asset.sha256.as_str().to_owned())?,
            PackageObjectKind::FirWav,
            usize::try_from(bytes).map_err(|_| BUDGET)?,
            cancel,
        )?;
        let actual = assets::GraphAssets::coefficient_bytes(asset)?
            .checked_add(bytes.capacity() as u64)
            .and_then(|n| n.checked_add(size_of::<assets::PreparedFirSource>() as u64))
            .ok_or(BUDGET)?;
        admit_fir_holdings(
            known,
            scratch,
            held.checked_add(pending)
                .and_then(|n| n.checked_add(actual))
                .ok_or(BUDGET)?,
            profile.latency.max_preparation_bytes,
            retained,
        )?;
        let source = assets::PreparedFirSource::from_bytes(bytes)?;
        check(cancel)?;
        if source.coefficients().reference() != asset {
            return Err(INVALID);
        }
        held = held
            .checked_add(source.retained_bytes()?.max(actual))
            .ok_or(BUDGET)?;
        admit_fir_holdings(
            known,
            scratch,
            held.checked_add(pending).ok_or(BUDGET)?,
            profile.latency.max_preparation_bytes,
            retained,
        )?;
        fir.push(source);
    }
    let (corrections, context) = correction_import::reconstruct(
        &profile,
        &graph,
        |hash, name, limit| reader.read(hash, kind(name)?, limit, cancel),
        |reference: &FirAssetReference| {
            fir.iter()
                .find(|s| s.coefficients().reference() == reference)
                .map(|s| s.source_bytes())
                .ok_or(INVALID)
        },
    )?;
    let manifest = if let Some((directory, manifest)) = &reader.package {
        manifest.validate_profile_references(&profile)?;
        manifest.validate_observed_inventory(&reader.observed)?;
        directory.validate_inventory(manifest, cancel)?;
        manifest.clone()
    } else {
        let mut objects = reader
            .sources
            .iter()
            .map(|s| s.object.clone())
            .collect::<Vec<_>>();
        objects.sort_by_key(|a| (rank(a.kind), a.sha256));
        let manifest = PackageManifest {
            schema_version: 1,
            profile: reference.clone(),
            objects,
        };
        manifest.validate()?;
        manifest
    };
    let manifest_bytes =
        original_manifest.unwrap_or(serde_json::to_vec(&manifest).map_err(|_| INVALID)?);
    read_package_manifest(&manifest_bytes)?;
    let result = ProfilePackageResult {
        operation: if directory.is_some() {
            ProfilePackageStage::Inspected
        } else {
            ProfilePackageStage::Previewed
        },
        manifest,
        manifest_sha256: hash(&manifest_bytes)?,
        manifest_bytes: manifest_bytes.len() as u64,
        total_bytes: reader.observed.iter().map(|o| o.bytes).sum::<u64>()
            + manifest_bytes.len() as u64,
        measurement_review_required: reader.observed.iter().any(|o| {
            matches!(
                o.kind,
                PackageObjectKind::TextSource | PackageObjectKind::FirWav
            )
        }),
        content_review_required: true,
        review_sources: reader.sources,
    };
    result.validate()?;
    let inputs = PreparedInputs::original(
        &profile,
        profile_bytes,
        graph_bytes,
        Some(PreparedCorrectionSource::Package {
            text: reader.text,
            fir,
        }),
    )?;
    let result_holding =
        result.retained_capacity_bytes()? as u64 + manifest_bytes.capacity() as u64;
    let retained = retained.checked_add(result_holding).ok_or(BUDGET)?;
    let bounds =
        validate_prepared_intent(runtime, &profile, &graph, retained, cancel, Some(&inputs))?;
    let preview = context
        .map(|(context, preamp)| correction_import::preview(context, preamp, bounds))
        .transpose()?;
    let prepared = prepare_record(profile, Arc::new(graph), corrections, preview, inputs)?;
    if prepared.record.reference() != *reference {
        return Err(INVALID);
    }
    Ok(PreparedPackage {
        prepared,
        result,
        manifest_bytes,
    })
}
fn original<'a>(
    prepared: &'a PreparedProfile,
    object: &PackageObject,
) -> Result<&'a [u8], EngineError> {
    match object.kind {
        PackageObjectKind::Profile => Ok(&prepared.inputs.profile_bytes),
        PackageObjectKind::Graph => Ok(&prepared.inputs.graph_bytes),
        PackageObjectKind::TextReport | PackageObjectKind::FirManifest => prepared
            .record
            .corrections
            .iter()
            .find(|o| o.object_sha256 == object.sha256)
            .map(|o| o.document_json.as_bytes())
            .ok_or(INVALID),
        PackageObjectKind::TextSource => {
            if let Some(PreparedCorrectionSource::Package { text, .. }) = &prepared.inputs.source {
                text.iter()
                    .find(|(hash, _)| *hash == object.sha256)
                    .map(|(_, bytes)| bytes.as_slice())
                    .ok_or(INVALID)
            } else {
                Err(INVALID)
            }
        }
        PackageObjectKind::FirWav => {
            if let Some(PreparedCorrectionSource::Package { fir, .. }) = &prepared.inputs.source {
                fir.iter()
                    .find(|s| {
                        s.coefficients().reference().sha256.as_str() == object.sha256.as_str()
                    })
                    .map(|s| s.original_bytes())
                    .ok_or(INVALID)
            } else {
                Err(INVALID)
            }
        }
    }
}
fn review(
    prepared: &PreparedPackage,
    redistribution: PackageRedistribution,
    cancel: &AtomicBool,
) -> Result<(), EngineError> {
    if prepared.result.measurement_review_required
        && redistribution == PackageRedistribution::RefuseMeasurementBytes
    {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "package measurement redistribution review is required",
        ));
    }
    for object in &prepared.result.manifest.objects {
        check(cancel)?;
        let bytes = original(&prepared.prepared, object)?;
        if object.kind == PackageObjectKind::FirWav {
            review_wav(bytes)?;
            continue;
        }
        let text = std::str::from_utf8(bytes).map_err(|_| INVALID)?;
        if forbidden(text) {
            return Err(EngineError::new(
                ErrorCode::AuthDenied,
                "package contains known private or runtime content",
            ));
        }
    }
    Ok(())
}
fn forbidden(text: &str) -> bool {
    fn strings(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => forbidden_text(text),
            serde_json::Value::Array(values) => values.iter().any(strings),
            serde_json::Value::Object(values) => values
                .iter()
                .any(|(key, value)| forbidden_text(&format!("\"{key}\"")) || strings(value)),
            _ => false,
        }
    }
    forbidden_text(text)
        || serde_json::from_str::<serde_json::Value>(text).is_ok_and(|value| strings(&value))
}
fn forbidden_text(text: &str) -> bool {
    // Fixed indicators reject known content; no scan proves arbitrary bytes are secret-free.
    let lower = text.to_ascii_lowercase();
    [
        "/home/",
        "/users/",
        "/tmp/",
        "/private/",
        "/var/",
        "/etc/",
        "file://",
        "bearer ",
        "-----begin ",
        "password=",
        "token=",
        "api_key=",
        "\\\\",
        "c:\\",
        "d:\\",
        "appleusbaudioengine:",
        "hw:",
    ]
    .iter()
    .any(|s| lower.contains(s))
        || [
            "endpoint",
            "output_consented",
            "fixed_armed",
            "readback",
            "verified_device_context",
            "associations",
            "recordings",
            "owner_token",
            "session_checkpoint",
            "persistent_uid",
            "device_uid",
            "client_secret",
        ]
        .iter()
        .any(|key| lower.contains(&format!("\"{key}\"")))
}
fn review_wav(bytes: &[u8]) -> Result<(), EngineError> {
    // Portable correction exports carry no arbitrary WAV metadata or executable chunks.
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(INVALID);
    }
    let end = u32::from_le_bytes(bytes[4..8].try_into().map_err(|_| INVALID)?) as usize + 8;
    if end != bytes.len() {
        return Err(INVALID);
    }
    let mut position = 12usize;
    while position < end {
        let header = bytes.get(position..position + 8).ok_or(INVALID)?;
        if !matches!(&header[..4], b"fmt " | b"data" | b"fact") {
            return Err(EngineError::new(
                ErrorCode::AuthDenied,
                "package WAV metadata requires removal before export",
            ));
        }
        let size = u32::from_le_bytes(header[4..8].try_into().map_err(|_| INVALID)?) as usize;
        position = position
            .checked_add(8)
            .and_then(|n| n.checked_add(size))
            .and_then(|n| n.checked_add(size % 2))
            .ok_or(BUDGET)?;
    }
    if position != end {
        return Err(INVALID);
    }
    Ok(())
}
fn run(
    runtime: &Path,
    operation: &ProfilePackageOperation,
    catalog: &[Arc<ProfileRecord>],
    retained: u64,
    cancel: &AtomicBool,
) -> Result<ProfileWorkOutput, EngineError> {
    match operation {
        ProfilePackageOperation::Inspect { directory } => {
            validate_source_location(runtime, directory)?;
            let selected = PackageDirectory::open(Path::new(directory), cancel)?;
            let manifest = read_package_manifest(&selected.read(
                PACKAGE_MANIFEST_FILE,
                MAX_PACKAGE_MANIFEST_BYTES,
                cancel,
            )?)?;
            let package = prepare(
                runtime,
                &manifest.profile,
                Some(directory),
                retained,
                cancel,
            )?;
            Ok(ProfileWorkOutput::Package(Arc::new(package.result)))
        }
        ProfilePackageOperation::Preview { profile } => Ok(ProfileWorkOutput::Package(Arc::new(
            prepare(runtime, profile, None, retained, cancel)?.result,
        ))),
        ProfilePackageOperation::Import {
            directory,
            profile,
            expected_manifest_sha256,
        } => {
            if catalog.iter().any(|record| record.profile.id == profile.id) {
                return Err(INVALID);
            }
            let package = prepare(runtime, profile, Some(directory), retained, cancel)?;
            if package.result.manifest_sha256 != *expected_manifest_sha256 {
                return Err(EngineError::new(
                    ErrorCode::StaleRevision,
                    "package manifest changed after inspection",
                ));
            }
            let retained = retained
                .checked_add(package.result.retained_capacity_bytes()? as u64)
                .and_then(|n| n.checked_add(package.manifest_bytes.capacity() as u64))
                .ok_or(BUDGET)?;
            validate_prospective_catalog(runtime, catalog, &package.prepared, retained, cancel)?;
            let record = persist_record(runtime, package.prepared, cancel)?;
            let restored = restore_with_retained(runtime, &[record.reference()], retained, cancel)?;
            Ok(ProfileWorkOutput::Profile(
                restored.into_iter().next().ok_or(INVALID)?,
            ))
        }
        ProfilePackageOperation::Export {
            directory,
            profile,
            expected_manifest_sha256,
            redistribution,
        } => {
            let package = prepare(runtime, profile, None, retained, cancel)?;
            if package.result.manifest_sha256 != *expected_manifest_sha256 {
                return Err(EngineError::new(
                    ErrorCode::StaleRevision,
                    "export candidate changed after review",
                ));
            }
            review(&package, *redistribution, cancel)?;
            let stage = PackageStage::create(Path::new(directory), runtime, cancel)?;
            for object in &package.result.manifest.objects {
                stage.write(
                    &object.file_name(),
                    original(&package.prepared, object)?,
                    cancel,
                )?;
            }
            stage.write(PACKAGE_MANIFEST_FILE, &package.manifest_bytes, cancel)?;
            drop(package);
            let verified = prepare(
                runtime,
                profile,
                Some(stage.path.to_str().ok_or(INVALID)?),
                retained,
                cancel,
            )?;
            if verified.result.manifest_sha256 != *expected_manifest_sha256 {
                return Err(INVALID);
            }
            drop(verified);
            stage.publish(cancel)?;
            // Return review locations in the final directory, never abandoned stage paths.
            let mut verified = prepare(runtime, profile, Some(directory), retained, cancel)?.result;
            verified.operation = ProfilePackageStage::Exported;
            Ok(ProfileWorkOutput::Package(Arc::new(verified)))
        }
    }
}
pub(super) fn execute(
    runtime: &Path,
    operation: &ProfilePackageOperation,
    catalog: &[Arc<ProfileRecord>],
    retained: u64,
    cancel: &AtomicBool,
) -> Result<ProfileWorkOutput, EngineError> {
    Ok(match run(runtime, operation, catalog, retained, cancel) {
        Ok(output) => output,
        Err(error) => package_failure(operation, error),
    })
}
fn package_failure(operation: &ProfilePackageOperation, error: EngineError) -> ProfileWorkOutput {
    let reason = if error == io::MISSING {
        ProfilePackageReason::MissingObject
    } else if error == io::INVENTORY {
        ProfilePackageReason::InventoryMismatch
    } else if error == io::DESTINATION {
        ProfilePackageReason::DestinationExists
    } else if error.code == ErrorCode::ResourceBudgetExceeded {
        ProfilePackageReason::ResourceLimit
    } else if error.code == ErrorCode::CapabilityUnsupported {
        ProfilePackageReason::UnsupportedContent
    } else if error.message == "package measurement redistribution review is required" {
        ProfilePackageReason::RedistributionReviewRequired
    } else if error.code == ErrorCode::AuthDenied {
        ProfilePackageReason::ContentReviewRequired
    } else if error.code == ErrorCode::InternalFault {
        ProfilePackageReason::IoFailure
    } else {
        ProfilePackageReason::InvalidObject
    };
    ProfileWorkOutput::PackageIssue {
        error,
        issue: ProfilePackageIssue {
            reason,
            object: None,
            filesystem_state: if operation.writes_files() {
                PackageFilesystemState::InspectRequired
            } else {
                PackageFilesystemState::Unchanged
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn known_private_content_cannot_hide_behind_json_escapes() {
        assert!(forbidden(
            r#"{"name":"\u002fhome\u002fharper\u002fprivate"}"#
        ));
        assert!(forbidden(
            r#"{"metadata":{"target":"Bearer owned-test-secret"}}"#
        ));
        assert!(!forbidden(
            r#"{"name":"Owned headphones","metadata":{"redistribution_license":"Owned test data"}}"#
        ));
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    fn source(value: f64) -> assets::PreparedFirSource {
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(68u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes.extend(2u16.to_le_bytes());
        bytes.extend(48000u32.to_le_bytes());
        bytes.extend(768000u32.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(64u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(32u32.to_le_bytes());
        for sample in [value, -0.25, 0.125, 0.0] {
            bytes.extend(sample.to_le_bytes());
        }
        assets::PreparedFirSource::from_bytes(bytes).unwrap()
    }
    #[test]
    fn retained_banks_intersect_the_owner_allowance_without_charging_arc_clones() {
        let first = source(0.5);
        let second = source(0.25);
        let shared = first.coefficients().clone();
        assert_eq!(
            shared.coefficients()[0].as_ptr(),
            first.coefficients().coefficients()[0].as_ptr()
        );
        let held = first.retained_bytes().unwrap();
        let known = SCRATCH + 4096;
        let scratch = 8 * 1024 * 1024;
        let peak = known + scratch + held;
        let retained = LIMIT - peak;
        assert_eq!(
            admit_fir_holdings(known, scratch, held, peak, retained),
            Ok(())
        );
        assert_eq!(
            admit_fir_holdings(known, scratch, held, peak, retained + 1),
            Err(BUDGET)
        );
        assert_eq!(
            admit_fir_holdings(
                known,
                scratch,
                held + second.retained_bytes().unwrap(),
                LIMIT,
                retained
            ),
            Err(BUDGET)
        );
        assert_eq!(
            admit_fir_holdings(known, scratch, held, peak - 1, 0),
            Err(BUDGET)
        );
        assert_eq!(
            admit_fir_holdings(known, scratch, u64::MAX, LIMIT, 0),
            Err(BUDGET)
        );
    }
}
