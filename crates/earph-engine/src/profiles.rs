// ABOUTME: Validates and stores immutable listening intent outside the owner control loop.
// ABOUTME: Verifies every reachable object before restoring the bounded Session catalog.
use crate::{assets, runtime};
use earph_core::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    graph::{GraphProcessingRequest, MAX_GRAPH_INTENT_BYTES, NodeKind},
    graph_control::GraphIntent,
    ids::RequestId,
    profile_control::{
        ProfileCatalogReference, ProfileCorrectionObject, ProfileCorrectionPreview, ProfileRecord,
        ProfileSaveJob, ProfileWorkOperation, ProfileWorkOutput, ProfileWorkRequest,
    },
    profiles::{MAX_PROFILE_BYTES, ProfileDocument, ProfileObjectHash, read_profile},
    safety::{FidelityPolicy, SampleDomain},
    session::Session,
};
use earph_dsp::{CompileContext, GraphBudget, ProcessingPolicy};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "profile object is missing, corrupt, or differs from its reference",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "profile validation exceeds resource admission",
);
const CANCELLED: EngineError =
    EngineError::new(ErrorCode::OperationCancelled, "profile save was cancelled");
const LIMIT: u64 = 256 * 1024 * 1024;
// Covers bounded graph metadata and object buffers; compiler and asset peaks are admitted separately.
const SCRATCH: u64 = 4 * 1024 * 1024;
#[path = "artifact_io.rs"]
mod artifact_io;
#[path = "artifact_jobs.rs"]
mod artifact_jobs;
#[path = "profile_import.rs"]
mod correction_import;
#[path = "diagnostic_artifacts.rs"]
mod diagnostic_artifacts;
#[path = "profile_package.rs"]
mod package;
pub(crate) use artifact_jobs::ArtifactJobs;

fn check(cancel: &AtomicBool) -> Result<(), EngineError> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED)
    } else {
        Ok(())
    }
}
fn hash(bytes: &[u8]) -> Result<ProfileObjectHash, EngineError> {
    ProfileObjectHash::try_from(
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
}
fn directory(runtime_path: &Path, create: bool) -> Result<PathBuf, EngineError> {
    let path = runtime::directory(runtime_path, false)?.join("profiles");
    if create {
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {
                File::open(runtime_path)
                    .and_then(|f| f.sync_all())
                    .map_err(|_| runtime::IO)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(runtime::IO),
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| INVALID)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "profile directory must be private and owner controlled",
        ));
    }
    Ok(path)
}
fn read_object(
    directory: &Path,
    hash: ProfileObjectHash,
    kind: &str,
    limit: usize,
) -> Result<Vec<u8>, EngineError> {
    let bytes = runtime::read(&object_path(directory, hash, kind), limit)?.ok_or(INVALID)?;
    if self::hash(&bytes)? != hash {
        return Err(INVALID);
    }
    Ok(bytes)
}
fn object_path(directory: &Path, hash: ProfileObjectHash, kind: &str) -> PathBuf {
    directory.join(format!(
        "{}.{}.{}",
        hash.as_str(),
        kind,
        if kind == "text-source" { "txt" } else { "json" }
    ))
}
fn write_object(
    directory: &Path,
    bytes: &[u8],
    kind: &str,
    cancel: &AtomicBool,
) -> Result<ProfileObjectHash, EngineError> {
    write_object_with_sync(directory, bytes, kind, cancel, &mut File::sync_all)
}
fn write_object_with_sync(
    directory: &Path,
    bytes: &[u8],
    kind: &str,
    cancel: &AtomicBool,
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
) -> Result<ProfileObjectHash, EngineError> {
    check(cancel)?;
    let hash = hash(bytes)?;
    let path = object_path(directory, hash, kind);
    runtime::publish_immutable_file(&path, bytes, &mut || check(cancel), sync_file)?;
    Ok(hash)
}

#[derive(Debug)]
enum PreparedCorrectionSource {
    Text(Vec<u8>),
    Fir(assets::PreparedFirSource),
    Package {
        text: Vec<(ProfileObjectHash, Vec<u8>)>,
        fir: Vec<assets::PreparedFirSource>,
    },
}
#[derive(Debug)]
struct PreparedInputs {
    graph_bytes: Vec<u8>,
    profile_bytes: Vec<u8>,
    source: Option<PreparedCorrectionSource>,
}
impl PreparedInputs {
    fn new(
        profile: &ProfileDocument,
        graph_bytes: Vec<u8>,
        source: Option<PreparedCorrectionSource>,
    ) -> Result<Self, EngineError> {
        let profile_bytes = serde_json::to_vec(profile).map_err(|_| INVALID)?;
        Self::original(profile, profile_bytes, graph_bytes, source)
    }
    fn original(
        profile: &ProfileDocument,
        profile_bytes: Vec<u8>,
        graph_bytes: Vec<u8>,
        source: Option<PreparedCorrectionSource>,
    ) -> Result<Self, EngineError> {
        if graph_bytes.len() > MAX_GRAPH_INTENT_BYTES || hash(&graph_bytes)? != profile.graph_sha256
        {
            return Err(BUDGET);
        }
        if profile_bytes.len() > MAX_PROFILE_BYTES {
            return Err(BUDGET);
        }
        if read_profile(&profile_bytes)? != *profile {
            return Err(INVALID);
        }
        Ok(Self {
            graph_bytes,
            profile_bytes,
            source,
        })
    }
    fn fir(
        &self,
        reference: &earph_core::dsp_nodes::FirAssetReference,
    ) -> Option<&assets::PreparedFirSource> {
        match &self.source {
            Some(PreparedCorrectionSource::Fir(source))
                if source.coefficients().reference() == reference =>
            {
                Some(source)
            }
            Some(PreparedCorrectionSource::Package { fir, .. }) => fir
                .iter()
                .find(|s| s.coefficients().reference() == reference),
            _ => None,
        }
    }
    fn holding_bytes(&self, graph: &GraphIntent) -> Result<u64, EngineError> {
        let source = match &self.source {
            None => 0,
            Some(PreparedCorrectionSource::Text(bytes)) => bytes.capacity() as u64,
            Some(PreparedCorrectionSource::Fir(source)) => {
                let mut bytes = source.retained_bytes()?;
                if graph.graph.nodes.iter().any(|node| {
                    matches!(&node.kind,
                    NodeKind::Convolution { asset } if asset == source.coefficients().reference())
                }) {
                    // This immutable Arc bank is charged by the compiler for this graph.
                    bytes = bytes
                        .checked_sub(source.coefficients().resident_bytes())
                        .ok_or(BUDGET)?;
                }
                bytes
            }
            Some(PreparedCorrectionSource::Package { text, fir }) => {
                let mut bytes = (text.capacity() * size_of::<(ProfileObjectHash, Vec<u8>)>()
                    + fir.capacity() * size_of::<assets::PreparedFirSource>())
                    as u64;
                for (_, source) in text {
                    bytes = bytes.checked_add(source.capacity() as u64).ok_or(BUDGET)?;
                }
                for source in fir {
                    let mut held = source.retained_bytes()?;
                    if graph.graph.nodes.iter().any(|node|matches!(&node.kind,NodeKind::Convolution{asset} if asset==source.coefficients().reference())) { held=held.checked_sub(source.coefficients().resident_bytes()).ok_or(BUDGET)?; }
                    bytes = bytes.checked_add(held).ok_or(BUDGET)?;
                }
                bytes
            }
        };
        (self.graph_bytes.capacity() as u64)
            .checked_add(self.profile_bytes.capacity() as u64)
            .and_then(|bytes| bytes.checked_add(source))
            .and_then(|bytes| bytes.checked_add(size_of::<Self>() as u64))
            .ok_or(BUDGET)
    }
}
#[derive(Debug)]
struct PreparedProfile {
    record: Arc<ProfileRecord>,
    inputs: PreparedInputs,
}
fn prepare_record(
    profile: ProfileDocument,
    graph: Arc<GraphIntent>,
    corrections: Arc<Vec<ProfileCorrectionObject>>,
    correction_preview: Option<Arc<ProfileCorrectionPreview>>,
    inputs: PreparedInputs,
) -> Result<PreparedProfile, EngineError> {
    let record = Arc::new(ProfileRecord {
        profile: Arc::new(profile),
        profile_sha256: hash(&inputs.profile_bytes)?,
        graph,
        corrections,
        correction_preview,
    });
    record.validate()?;
    Ok(PreparedProfile { record, inputs })
}
fn validate_intent(
    runtime_path: &Path,
    profile: &ProfileDocument,
    intent: &GraphIntent,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<Vec<f64>, EngineError> {
    validate_prepared_intent(runtime_path, profile, intent, retained, cancel, None)
}
fn validate_prepared_intent(
    runtime_path: &Path,
    profile: &ProfileDocument,
    intent: &GraphIntent,
    retained: u64,
    cancel: &AtomicBool,
    inputs: Option<&PreparedInputs>,
) -> Result<Vec<f64>, EngineError> {
    check(cancel)?;
    profile.validate()?;
    intent.graph.validate_structure()?;
    let input = intent
        .graph
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Input))
        .ok_or(INVALID)?;
    let output = intent
        .graph
        .nodes
        .iter()
        .find(|n| n.id == intent.graph.output)
        .ok_or(INVALID)?;
    if input.format.domain != SampleDomain::Pcm || output.format.domain != SampleDomain::Pcm {
        return Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "profile validation requires an explicit PCM graph domain",
        ));
    }
    let float = matches!(intent.processing, GraphProcessingRequest::Float64 { .. });
    if !float
        && intent
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Meter))
    {
        return Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "integer graph metering is not implemented",
        ));
    }
    if float && profile.fidelity == FidelityPolicy::StrictPreservation {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "float processing conflicts with strict preservation",
        ));
    }
    if float
        && (input.format.precision != earph_core::audio::PcmRepresentation::Float64
            || output.format.precision != earph_core::audio::PcmRepresentation::Float64)
    {
        return Err(EngineError::new(
            ErrorCode::FormatUnsupported,
            "float processing requires declared float64 ports",
        ));
    }
    let asset_admission = assets::GraphAssets::inspect(&intent.graph)?;
    let scratch = SCRATCH
        .checked_add(asset_admission.scratch_bytes())
        .ok_or(BUDGET)?;
    let holding = inputs.map_or(Ok(0), |inputs| inputs.holding_bytes(intent))?;
    let available = LIMIT
        .checked_sub(retained)
        .and_then(|n| n.checked_sub(scratch))
        .and_then(|n| n.checked_sub(holding))
        .ok_or(BUDGET)?;
    let resident = available.min(profile.latency.max_resident_bytes);
    let preparation = available.min(
        profile
            .latency
            .max_preparation_bytes
            .checked_sub(scratch)
            .and_then(|bytes| bytes.checked_sub(holding))
            .ok_or(BUDGET)?,
    );
    let resolved = asset_admission.resolve_with(
        resident.min(preparation),
        || check(cancel),
        |reference| {
            if let Some(source) = inputs.and_then(|inputs| inputs.fir(reference)) {
                return Ok(source.coefficients().clone());
            }
            assets::coefficients(runtime_path, reference)
        },
    )?;
    check(cancel)?;
    let quantum = intent
        .graph
        .nodes
        .iter()
        .map(|n| n.format.max_quantum_frames)
        .min()
        .filter(|n| *n > 0 && *n <= 8192)
        .ok_or(BUDGET)?;
    let context = CompileContext {
        input_format: PcmFormat::new(
            input.format.sample_rate_hz,
            input.format.channels,
            input.format.precision,
        )?,
        output_format: PcmFormat::new(
            output.format.sample_rate_hz,
            output.format.channels,
            output.format.precision,
        )?,
        quantum_frames: quantum,
        policy: if float {
            ProcessingPolicy::AllowProcessing
        } else {
            ProcessingPolicy::StrictPreservation
        },
        budget: GraphBudget {
            max_buffer_bytes: resident.min(preparation),
            max_latency_frames: profile.latency.max_latency_frames.unwrap_or(u64::MAX),
            ..GraphBudget::default()
        },
    };
    let bounds = if intent
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Src { .. }))
    {
        if !float {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "SRC requires float processing",
            ));
        }
        let plan = earph_dsp::compile_stream(
            &intent.graph,
            &context,
            &resolved,
            earph_dsp::stream::StreamBudget {
                max_resident_bytes: resident,
                max_preparation_bytes: preparation,
                ..earph_dsp::stream::StreamBudget::default()
            },
        )
        .map_err(|e| EngineError::new(e.code, e.message))?;
        plan.output_peak_bounds().to_vec()
    } else {
        let plan = earph_dsp::compile_with_assets(&intent.graph, &context, &resolved)
            .map_err(|e| EngineError::new(e.code, e.message))?;
        let usage = plan.resources();
        if usage
            .buffer_bytes
            .checked_add(plan.preparation_scratch_bytes())
            .is_none_or(|n| n > preparation)
        {
            return Err(BUDGET);
        }
        plan.output_peak_bounds().to_vec()
    };
    check(cancel)?;
    Ok(bounds)
}
fn prepare_save(
    runtime_path: &Path,
    job: &ProfileSaveJob,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<PreparedProfile, EngineError> {
    let graph_bytes = serde_json::to_vec(&*job.graph).map_err(|_| INVALID)?;
    if graph_bytes.len() > MAX_GRAPH_INTENT_BYTES {
        return Err(BUDGET);
    }
    let profile = job
        .draft
        .document(job.id.clone(), job.revision, hash(&graph_bytes)?)?;
    let inputs = PreparedInputs::new(&profile, graph_bytes, None)?;
    validate_prepared_intent(
        runtime_path,
        &profile,
        &job.graph,
        retained,
        cancel,
        Some(&inputs),
    )?;
    prepare_record(
        profile,
        job.graph.clone(),
        Arc::new(Vec::new()),
        None,
        inputs,
    )
}
fn persist_record(
    runtime_path: &Path,
    prepared: PreparedProfile,
    cancel: &AtomicBool,
) -> Result<Arc<ProfileRecord>, EngineError> {
    check(cancel)?;
    let PreparedProfile { record, inputs } = prepared;
    if let Some(PreparedCorrectionSource::Fir(source)) = &inputs.source {
        source.publish(runtime_path, &mut || check(cancel))?;
    }
    if let Some(PreparedCorrectionSource::Package { text, fir }) = &inputs.source {
        for source in fir {
            source.publish(runtime_path, &mut || check(cancel))?;
        }
        let destination = directory(runtime_path, true)?;
        for (expected, bytes) in text {
            if write_object(&destination, bytes, "text-source", cancel)? != *expected {
                return Err(INVALID);
            }
        }
    }
    let directory = directory(runtime_path, true)?;
    if let Some(PreparedCorrectionSource::Text(bytes)) = &inputs.source {
        write_object(&directory, bytes, "text-source", cancel)?;
    }
    for object in record.corrections.iter() {
        let kind = match object.kind {
            earph_core::profiles::ProfileCorrectionKind::TextReport => "text-report",
            earph_core::profiles::ProfileCorrectionKind::FirManifest => "fir-manifest",
        };
        if write_object(&directory, object.document_json.as_bytes(), kind, cancel)?
            != object.object_sha256
        {
            return Err(INVALID);
        }
    }
    if write_object(&directory, &inputs.graph_bytes, "graph", cancel)?
        != record.profile.graph_sha256
    {
        return Err(INVALID);
    }
    if write_object(&directory, &inputs.profile_bytes, "profile", cancel)? != record.profile_sha256
    {
        return Err(INVALID);
    }
    check(cancel)?;
    Ok(record)
}
fn validate_catalog_records(
    runtime_path: &Path,
    records: &[Arc<ProfileRecord>],
    live_retained: u64,
    cancel: &AtomicBool,
) -> Result<(), EngineError> {
    validate_prepared_catalog(runtime_path, records, live_retained, cancel, None)
}
fn validate_prepared_catalog(
    runtime_path: &Path,
    records: &[Arc<ProfileRecord>],
    live_retained: u64,
    cancel: &AtomicBool,
    inputs: Option<&PreparedInputs>,
) -> Result<(), EngineError> {
    if records.len() > earph_core::profiles::MAX_PROFILES {
        return Err(BUDGET);
    }
    let catalog = records.iter().try_fold(0_u64, |total, record| {
        record.validate()?;
        total
            .checked_add(record.retained_capacity_bytes()? as u64)
            .and_then(|n| n.checked_add(256 + record.profile.id.as_str().len() as u64))
            .ok_or(BUDGET)
    })?;
    if catalog > earph_core::profile_control::MAX_PROFILE_STORAGE_BYTES {
        return Err(BUDGET);
    }
    // Charge the whole prospective catalog and its staging owner, independent of ID ordering.
    let retained = live_retained.max(catalog.checked_mul(2).ok_or(BUDGET)?);
    for record in records {
        validate_prepared_intent(
            runtime_path,
            &record.profile,
            &record.graph,
            retained,
            cancel,
            inputs,
        )?;
    }
    Ok(())
}
fn validate_prospective_catalog(
    runtime_path: &Path,
    records: &[Arc<ProfileRecord>],
    candidate: &PreparedProfile,
    live_retained: u64,
    cancel: &AtomicBool,
) -> Result<(), EngineError> {
    let mut prospective = Vec::with_capacity(records.len().saturating_add(1));
    prospective.extend(
        records
            .iter()
            .filter(|r| r.profile.id != candidate.record.profile.id)
            .cloned(),
    );
    prospective.push(candidate.record.clone());
    validate_prepared_catalog(
        runtime_path,
        &prospective,
        live_retained,
        cancel,
        Some(&candidate.inputs),
    )
}
pub(crate) fn restore(
    runtime_path: &Path,
    references: &[ProfileCatalogReference],
) -> Result<Vec<Arc<ProfileRecord>>, EngineError> {
    restore_with_retained(runtime_path, references, 0, &AtomicBool::new(false))
}
fn restore_with_retained(
    runtime_path: &Path,
    references: &[ProfileCatalogReference],
    live_retained: u64,
    cancel: &AtomicBool,
) -> Result<Vec<Arc<ProfileRecord>>, EngineError> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let directory = directory(runtime_path, false)?;
    let mut records = Vec::with_capacity(references.len());
    let mut retained = live_retained;
    for reference in references {
        check(cancel)?;
        let bytes = read_object(
            &directory,
            reference.profile_sha256,
            "profile",
            MAX_PROFILE_BYTES,
        )?;
        let profile = read_profile(&bytes)?;
        if profile.id != reference.id || profile.revision != reference.revision {
            return Err(INVALID);
        }
        let bytes = read_object(
            &directory,
            profile.graph_sha256,
            "graph",
            MAX_GRAPH_INTENT_BYTES,
        )?;
        let graph: GraphIntent = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
        let (corrections, correction_preview) =
            correction_import::load(runtime_path, &profile, &graph, retained, cancel)?;
        let record = Arc::new(ProfileRecord {
            profile: Arc::new(profile),
            profile_sha256: reference.profile_sha256,
            graph: Arc::new(graph),
            corrections,
            correction_preview,
        });
        record.validate()?;
        retained = retained
            .checked_add(record.retained_capacity_bytes()? as u64)
            .ok_or(BUDGET)?;
        records.push(record);
    }
    validate_catalog_records(runtime_path, &records, live_retained, cancel)?;
    Ok(records)
}

fn execute_profile(
    job: &ProfileWorkRequest,
    catalog: &Arc<Vec<Arc<ProfileRecord>>>,
    retained: u64,
    cancel: &AtomicBool,
    directory: &Path,
) -> Result<ProfileWorkOutput, EngineError> {
    let publish = |prepared| {
        validate_prospective_catalog(directory, catalog, &prepared, retained, cancel)?;
        persist_record(directory, prepared, cancel)
    };
    match &job.operation {
        ProfileWorkOperation::Package(operation) => {
            package::execute(directory, operation, catalog, retained, cancel)
        }
        ProfileWorkOperation::Save {
            id,
            revision,
            draft,
            graph,
        } => prepare_save(
            directory,
            &ProfileSaveJob {
                request_id: job.request_id.clone(),
                binding: job.binding.clone(),
                id: id.clone(),
                revision: *revision,
                draft: draft.clone(),
                graph: graph.clone(),
                deadline_monotonic_ns: job.deadline_monotonic_ns,
            },
            retained,
            cancel,
        )
        .and_then(&publish)
        .map(ProfileWorkOutput::Profile),
        ProfileWorkOperation::Import {
            id,
            revision,
            draft,
            input,
        } => correction_import::prepare_import(
            directory,
            id.clone(),
            *revision,
            draft,
            input,
            retained,
            cancel,
        )
        .and_then(&publish)
        .map(ProfileWorkOutput::Profile),
        ProfileWorkOperation::FirAsset { path, .. } => {
            correction_import::import_asset(directory, path, retained, cancel)
                .map(ProfileWorkOutput::FirAsset)
        }
    }
}
fn preparation_retained_bytes(session: &Session) -> Result<u64, EngineError> {
    earph_core::profile_control::MAX_PROFILE_STORAGE_BYTES
        .checked_mul(2)
        .and_then(|n| n.checked_add(session.profile_environment_bytes().ok()?))
        .ok_or(BUDGET)
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

    fn persist_fixture(
        runtime: &Path,
        profile: ProfileDocument,
        graph: Arc<GraphIntent>,
        corrections: Arc<Vec<ProfileCorrectionObject>>,
        preview: Option<Arc<ProfileCorrectionPreview>>,
        cancel: &AtomicBool,
    ) -> Result<Arc<ProfileRecord>, EngineError> {
        let inputs = PreparedInputs::new(&profile, serde_json::to_vec(&*graph).unwrap(), None)?;
        let prepared = prepare_record(profile, graph, corrections, preview, inputs)?;
        persist_record(runtime, prepared, cancel)
    }

    #[test]
    fn catalog_admission_is_independent_of_record_order() {
        use earph_core::ids::ProfileId;
        let runtime = PathBuf::from("/tmp").join(format!(
            "epc-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let draft: earph_core::profile_control::ProfileDraft = serde_json::from_value(serde_json::json!({"name":"Owned", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"owned","manual_setup":null},
            "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})).unwrap();
        let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
        let graph: Arc<GraphIntent> = Arc::new(serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"output":"output","nodes":[
            {"id":"input","kind":{"kind":"input"},"format":port}, {"id":"delay","kind":{"kind":"delay","delay_frames":16449536},"format":port},
            {"id":"output","kind":{"kind":"output"},"format":port}], "edges":[{"from":"input","to":"delay","from_port":0,"to_port":0},{"from":"delay","to":"output","from_port":0,"to_port":0}]},
            "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}})).unwrap());
        let cancel = AtomicBool::new(false);
        let profile = draft
            .document(
                ProfileId::new("z-large").unwrap(),
                1,
                hash(&serde_json::to_vec(&graph).unwrap()).unwrap(),
            )
            .unwrap();
        let large = persist_fixture(
            &runtime,
            profile,
            graph,
            Arc::new(Vec::new()),
            None,
            &cancel,
        )
        .unwrap();
        assert_eq!(restore(&runtime, &[large.reference()]).unwrap().len(), 1);
        let ids: Vec<_> = (0..128)
            .map(|i| format!("node-{i:03}{}", "x".repeat(120)))
            .collect();
        let nodes: Vec<_> = ids.iter().enumerate().map(|(i, id)| serde_json::json!({"id":id,"kind":{"kind":if i==0 {"input"} else if i==127 {"output"} else {"bypass"}},"format":port})).collect();
        let edges: Vec<_> = ids
            .windows(2)
            .map(|p| serde_json::json!({"from":p[0],"to":p[1],"from_port":0,"to_port":0}))
            .collect();
        let cheap: Arc<GraphIntent> = Arc::new(serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"output":ids[127],"nodes":nodes,"edges":edges},"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}})).unwrap());
        let mut refs = vec![large.reference()];
        for index in 0..20 {
            let profile = draft
                .document(
                    ProfileId::new(format!("a-{index:02}")).unwrap(),
                    1,
                    hash(&serde_json::to_vec(&cheap).unwrap()).unwrap(),
                )
                .unwrap();
            refs.push(
                persist_fixture(
                    &runtime,
                    profile,
                    cheap.clone(),
                    Arc::new(Vec::new()),
                    None,
                    &cancel,
                )
                .unwrap()
                .reference(),
            );
        }
        let large_first = restore(&runtime, &refs);
        refs.sort_by(|a, b| a.id.cmp(&b.id));
        let large_last = restore(&runtime, &refs);
        assert!(
            large_last.is_err(),
            "fixture did not reach aggregate catalog admission"
        );
        assert!(
            large_first.is_err(),
            "the same catalog passed only because its large graph sorted first"
        );
    }

    #[test]
    fn live_save_checks_the_existing_catalog_before_publication() {
        catalog_refusal_preserves_objects(|_| None);
    }

    #[test]
    fn refused_text_import_does_not_publish_candidate_dependencies() {
        catalog_refusal_preserves_objects(|_| {
            use earph_media::correction_text::{
                CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
            };
            let source = "Preamp: -6 dB\nFilter 1: ON PK Fc 1000 Hz Gain 0 dB Q 1\n";
            let report = import_correction_text(
                source.as_bytes(),
                CorrectionFormat::AutoEq,
                ImportContext {
                    sample_rate_hz: 48000,
                    channels: earph_core::audio::ChannelLayout::Mono,
                    quantum_frames: 64,
                },
                ImportMetadata::default(),
            )
            .unwrap();
            Some(Arc::new(
                earph_core::profile_control::ProfileCorrectionInput::TextReport {
                    report_json: serde_json::to_string(&report).unwrap(),
                    source_text: source.into(),
                },
            ))
        });
    }

    #[test]
    fn refused_fir_import_does_not_publish_candidate_dependencies() {
        catalog_refusal_preserves_objects(|runtime| Some(fir_input(runtime)));
    }
    fn fir_input(runtime: &Path) -> Arc<earph_core::profile_control::ProfileCorrectionInput> {
        let source = runtime.join("candidate.wav");
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(44_u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(3_u16.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(48000_u32.to_le_bytes());
        bytes.extend(384000_u32.to_le_bytes());
        bytes.extend(8_u16.to_le_bytes());
        bytes.extend(64_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(8_u32.to_le_bytes());
        bytes.extend(0.5_f64.to_le_bytes());
        fs::write(&source, bytes).unwrap();
        Arc::new(earph_core::profile_control::ProfileCorrectionInput::Fir {
            path: source.to_str().unwrap().into(),
            purpose: earph_core::dsp_nodes::FirPurpose::Correction,
            context: earph_core::profile_control::ProfileCorrectionContext {
                sample_rate_hz: 48000,
                channels: earph_core::audio::ChannelLayout::Mono,
                quantum_frames: 64,
            },
            metadata_json: "{}".into(),
            preamp_db: earph_core::graph::FiniteGraphScalar::new(0.0).unwrap(),
        })
    }

    fn import_draft() -> Arc<earph_core::profile_control::ProfileDraft> {
        Arc::new(serde_json::from_value(serde_json::json!({"name":"Owned", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"owned","manual_setup":null},
            "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})).unwrap())
    }

    #[test]
    fn cancelled_prepared_fir_keeps_all_dependencies_unpublished() {
        let runtime = PathBuf::from("/tmp").join(format!(
            "epc-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let input = fir_input(&runtime);
        let cancel = AtomicBool::new(false);
        let prepared = correction_import::prepare_import(
            &runtime,
            earph_core::ids::ProfileId::new("cancelled").unwrap(),
            1,
            &import_draft(),
            &input,
            0,
            &cancel,
        )
        .unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(
            persist_record(&runtime, prepared, &cancel)
                .unwrap_err()
                .code,
            ErrorCode::OperationCancelled
        );
        assert!(!runtime.join("assets").exists());
        assert!(!runtime.join("profiles").exists());
    }

    #[test]
    fn admitted_fir_publishes_the_original_prepared_bytes_after_source_change() {
        let runtime = PathBuf::from("/tmp").join(format!(
            "epf-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let input = fir_input(&runtime);
        let source = runtime.join("candidate.wav");
        let original = fs::read(&source).unwrap();
        let cancel = AtomicBool::new(false);
        let prepared = correction_import::prepare_import(
            &runtime,
            earph_core::ids::ProfileId::new("frozen").unwrap(),
            1,
            &import_draft(),
            &input,
            0,
            &cancel,
        )
        .unwrap();
        let Some(PreparedCorrectionSource::Fir(prepared_source)) = &prepared.inputs.source else {
            panic!("expected one prepared FIR source")
        };
        let reference = prepared_source.coefficients().reference().clone();
        assert!(!runtime.join("assets").exists());
        assert!(!runtime.join("profiles").exists());
        fs::write(&source, b"changed owned source after preparation").unwrap();
        validate_prospective_catalog(&runtime, &[], &prepared, 0, &cancel).unwrap();
        let record = persist_record(&runtime, prepared, &cancel).unwrap();
        assert_eq!(
            fs::read(
                runtime
                    .join("assets")
                    .join(format!("{}.wav", reference.sha256.as_str()))
            )
            .unwrap(),
            original
        );
        assert_eq!(
            assets::resolve(&runtime, &reference).unwrap().taps[0],
            [0.5]
        );
        assert_eq!(restore(&runtime, &[record.reference()]).unwrap()[0], record);
        assert_eq!(
            fs::read(&source).unwrap(),
            b"changed owned source after preparation"
        );
    }

    fn catalog_refusal_preserves_objects(
        input: fn(&Path) -> Option<Arc<earph_core::profile_control::ProfileCorrectionInput>>,
    ) {
        use earph_core::{
            control::{Command, CommandEnvelope, ControlLimits, ReceiptStatus},
            ids::{OwnerId, ProfileId, RequestId, ZoneId},
        };
        let runtime = PathBuf::from("/tmp").join(format!(
            "epl-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let draft: Arc<earph_core::profile_control::ProfileDraft> = Arc::new(serde_json::from_value(serde_json::json!({"name":"Owned", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"owned","manual_setup":null},
            "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})).unwrap());
        let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
        let graph: Arc<GraphIntent> = Arc::new(serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"output":"output","nodes":[
            {"id":"input","kind":{"kind":"input"},"format":port}, {"id":"delay","kind":{"kind":"delay","delay_frames":10000000},"format":port},
            {"id":"output","kind":{"kind":"output"},"format":port}], "edges":[{"from":"input","to":"delay","from_port":0,"to_port":0},{"from":"delay","to":"output","from_port":0,"to_port":0}]},
            "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}})).unwrap());
        let profile = draft
            .document(
                ProfileId::new("z-existing").unwrap(),
                1,
                hash(&serde_json::to_vec(&graph).unwrap()).unwrap(),
            )
            .unwrap();
        let old = persist_fixture(
            &runtime,
            profile,
            graph.clone(),
            Arc::new(Vec::new()),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        let loaded = restore(&runtime, &[old.reference()]).unwrap();
        let mut checkpoint = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("old").unwrap(),
            ControlLimits::default(),
        )
        .checkpoint();
        checkpoint.profile_catalog.push(old.reference());
        let mut session = Session::restore_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("fresh").unwrap(),
            ControlLimits::default(),
            checkpoint,
        )
        .unwrap();
        session.restore_profile_records(loaded).unwrap();
        let mut cheap = (*graph).clone();
        Arc::make_mut(&mut cheap.graph).nodes[1].kind = NodeKind::Bypass;
        let input = input(&runtime);
        let stored_objects = || {
            [runtime.join("profiles"), runtime.join("assets")]
                .into_iter()
                .filter(|directory| directory.exists())
                .flat_map(|directory| fs::read_dir(directory).unwrap())
                .map(|entry| {
                    let path = entry.unwrap().path();
                    (
                        path.strip_prefix(&runtime).unwrap().to_owned(),
                        Sha256::digest(fs::read(&path).unwrap()),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let original_objects = stored_objects();
        let state = session.snapshot();
        let command = CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new("cheap-save").unwrap(),
            zone_id: state.zone_id,
            owner_id: state.owner_id,
            session_id: state.session_id,
            expected_revision: Some(state.revision),
            payload: if let Some(input) = input {
                Command::ProfilesImport {
                    id: ProfileId::new("a-cheap").unwrap(),
                    expected_profile_revision: None,
                    draft,
                    input,
                }
            } else {
                Command::ProfilesCreate {
                    id: ProfileId::new("a-cheap").unwrap(),
                    draft,
                    graph: Arc::new(cheap),
                }
            },
        };
        assert_eq!(
            session.submit_graph(command.clone(), 1).status,
            ReceiptStatus::Accepted
        );
        let mut jobs = ArtifactJobs::spawn(runtime.clone());
        jobs.submit(&session, 1).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            jobs.poll(&mut session, 2, &runtime, "catalog-check")
                .unwrap();
            if !session.profile_worker_busy() {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let receipt = session.submit(command);
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        assert_eq!(
            receipt.error.unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(session.profile_references(), &[old.reference()]);
        assert_eq!(
            stored_objects(),
            original_objects,
            "ordinary catalog refusal published candidate objects"
        );
        assert_eq!(
            restore(&runtime, session.profile_references())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn captured_preparation_allowance_covers_real_query_history_growth() {
        use earph_core::{
            control::{Command, CommandEnvelope, ControlLimits, QueryResult},
            ids::{OwnerId, ProfileId, RequestId, ZoneId},
        };
        let runtime = PathBuf::from("/tmp").join(format!(
            "epb-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owner").unwrap(),
            ControlLimits::default(),
        );
        let draft = serde_json::from_value(serde_json::json!({"name":"Owned", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"owned","manual_setup":null},
            "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})).unwrap();
        let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
        let graph = serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"output":"output","nodes":[
            {"id":"input","kind":{"kind":"input"},"format":port}, {"id":"output","kind":{"kind":"output"},"format":port}],
            "edges":[{"from":"input","to":"output","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}})).unwrap();
        let envelope = |session: &Session, id: &str, payload: Command| {
            let state = session.snapshot();
            CommandEnvelope {
                schema_version: 1,
                request_id: RequestId::new(id).unwrap(),
                zone_id: state.zone_id,
                owner_id: state.owner_id,
                session_id: state.session_id,
                expected_revision: Some(state.revision),
                payload,
            }
        };
        let create = envelope(
            &session,
            "create",
            Command::ProfilesCreate {
                id: ProfileId::new("owned").unwrap(),
                draft: Arc::new(draft),
                graph: Arc::new(graph),
            },
        );
        session.submit_graph(create, 1);
        let job = session.profile_job().unwrap();
        let record = prepare_save(
            &runtime,
            &job,
            preparation_retained_bytes(&session).unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
        let record = persist_record(&runtime, record, &AtomicBool::new(false)).unwrap();
        session
            .complete_profile_save(&job.request_id, &job.binding, 2, Ok(record), |_| Ok(()))
            .unwrap();
        let update = envelope(
            &session,
            "pending-update",
            Command::ProfilesUpdate {
                id: ProfileId::new("owned").unwrap(),
                expected_profile_revision: 1,
                draft: job.draft.clone(),
                graph: job.graph.clone(),
            },
        );
        assert_eq!(
            session.submit_graph(update, 3).status,
            earph_core::control::ReceiptStatus::Accepted
        );
        let captured = preparation_retained_bytes(&session).unwrap();
        for index in 0..32 {
            let get = envelope(
                &session,
                &format!("get-{index}"),
                Command::ProfilesGet {
                    id: ProfileId::new("owned").unwrap(),
                },
            );
            let receipt = session.submit(get);
            assert!(matches!(receipt.result, Some(QueryResult::Profile(_))));
        }
        assert!(
            captured >= session.profile_storage_bytes() * 2,
            "live history escaped the compiler's captured metadata allowance"
        );
    }

    #[test]
    fn interrupted_object_sync_preserves_stage_without_poisoning_hash_name() {
        let runtime = PathBuf::from("/tmp").join(format!(
            "stage-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let directory = directory(&runtime, true).unwrap();
        let bytes = b"complete immutable graph bytes";
        let path = object_path(&directory, hash(bytes).unwrap(), "graph");
        let result = write_object_with_sync(
            &directory,
            bytes,
            "graph",
            &AtomicBool::new(false),
            &mut |file| {
                file.sync_all()?;
                Err(std::io::Error::other(
                    "controlled interruption before publication",
                ))
            },
        );
        assert!(result.is_err());
        assert!(
            !path.exists(),
            "interrupted object became visible at its hash name"
        );
        let abandoned: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(abandoned.len(), 1);
        assert_eq!(fs::read(&abandoned[0]).unwrap(), bytes);
        write_object(&directory, bytes, "graph", &AtomicBool::new(false)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read(&abandoned[0]).unwrap(), bytes);
    }

    #[test]
    fn existing_complete_object_is_file_synced_before_it_becomes_reachable() {
        let runtime = PathBuf::from("/tmp").join(format!(
            "epd-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let directory = directory(&runtime, true).unwrap();
        let bytes = b"{\"schema_version\":1}";
        let expected = hash(bytes).unwrap();
        let path = directory.join(format!("{}.graph.json", expected.as_str()));
        let mut interrupted = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        interrupted.write_all(bytes).unwrap();
        let metadata = interrupted.metadata().unwrap();
        drop(interrupted);
        let mut synced = 0;
        let observed = write_object_with_sync(
            &directory,
            bytes,
            "graph",
            &AtomicBool::new(false),
            &mut |file| {
                let observed = file.metadata()?;
                assert_eq!(
                    (observed.dev(), observed.ino()),
                    (metadata.dev(), metadata.ino())
                );
                file.sync_all()?;
                synced += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(observed, expected);
        assert_eq!(
            synced, 1,
            "retry did not synchronize the verified object file"
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[cfg(test)]
#[path = "diagnostic_artifact_tests.rs"]
mod diagnostic_artifact_tests;
