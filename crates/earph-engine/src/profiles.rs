// ABOUTME: Validates and stores immutable listening intent outside the owner control loop.
// ABOUTME: Verifies every reachable object before restoring the bounded Session catalog.
use crate::{assets, runtime};
use earph_core::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    graph::{GraphProcessingRequest, MAX_GRAPH_INTENT_BYTES, NodeKind},
    graph_control::GraphIntent,
    ids::RequestId,
    profile_control::{ProfileCatalogReference, ProfileRecord, ProfileSaveBinding, ProfileSaveJob},
    profiles::{MAX_PROFILE_BYTES, ProfileDocument, ProfileObjectHash, read_profile},
    safety::{FidelityPolicy, SampleDomain},
    session::Session,
};
use earph_dsp::{CompileContext, GraphBudget, ProcessingPolicy};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
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
    let bytes = runtime::read(
        &directory.join(format!("{}.{}.json", hash.as_str(), kind)),
        limit,
    )?
    .ok_or(INVALID)?;
    if self::hash(&bytes)? != hash {
        return Err(INVALID);
    }
    Ok(bytes)
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
    let path = directory.join(format!("{}.{}.json", hash.as_str(), kind));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(bytes).map_err(|_| runtime::IO)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(runtime::IO),
    }
    check(cancel)?;
    runtime::verify_immutable_file(&path, bytes, sync_file)?;
    // Both new and existing verified objects need file and directory durability before reachability.
    File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(|_| runtime::IO)?;
    check(cancel)?;
    Ok(hash)
}

fn validate_intent(
    runtime_path: &Path,
    profile: &ProfileDocument,
    intent: &GraphIntent,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<(), EngineError> {
    check(cancel)?;
    profile.validate()?;
    intent.graph.validate_structure()?;
    if !profile.corrections.is_empty() {
        return Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "correction object import is unavailable",
        ));
    }
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
    let available = LIMIT
        .checked_sub(retained)
        .and_then(|n| n.checked_sub(scratch))
        .ok_or(BUDGET)?;
    let resident = available.min(profile.latency.max_resident_bytes);
    let preparation = available.min(
        profile
            .latency
            .max_preparation_bytes
            .checked_sub(scratch)
            .ok_or(BUDGET)?,
    );
    let resolved =
        asset_admission.resolve(Some(runtime_path), resident.min(preparation), || {
            check(cancel)
        })?;
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
    if intent
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
        earph_dsp::compile_stream(
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
    }
    check(cancel)
}
fn save(
    runtime_path: &Path,
    job: &ProfileSaveJob,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<Arc<ProfileRecord>, EngineError> {
    let graph_bytes = serde_json::to_vec(&*job.graph).map_err(|_| INVALID)?;
    if graph_bytes.len() > MAX_GRAPH_INTENT_BYTES {
        return Err(BUDGET);
    }
    let profile = job
        .draft
        .document(job.id.clone(), job.revision, hash(&graph_bytes)?)?;
    validate_intent(runtime_path, &profile, &job.graph, retained, cancel)?;
    let bytes = serde_json::to_vec(&profile).map_err(|_| INVALID)?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(BUDGET);
    }
    check(cancel)?;
    let directory = directory(runtime_path, true)?;
    if write_object(&directory, &graph_bytes, "graph", cancel)? != profile.graph_sha256 {
        return Err(INVALID);
    }
    let record = Arc::new(ProfileRecord {
        profile: Arc::new(profile),
        profile_sha256: write_object(&directory, &bytes, "profile", cancel)?,
        graph: job.graph.clone(),
    });
    record.validate()?;
    check(cancel)?;
    Ok(record)
}
pub(crate) fn restore(
    runtime_path: &Path,
    references: &[ProfileCatalogReference],
) -> Result<Vec<Arc<ProfileRecord>>, EngineError> {
    if references.is_empty() {
        return Ok(Vec::new());
    }
    let directory = directory(runtime_path, false)?;
    let mut records = Vec::with_capacity(references.len());
    let mut retained = 0_u64;
    for reference in references {
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
        validate_intent(
            runtime_path,
            &profile,
            &graph,
            retained,
            &AtomicBool::new(false),
        )?;
        let record = Arc::new(ProfileRecord {
            profile: Arc::new(profile),
            profile_sha256: reference.profile_sha256,
            graph: Arc::new(graph),
        });
        record.validate()?;
        retained = retained
            .checked_add(record.retained_capacity_bytes()? as u64)
            .ok_or(BUDGET)?;
        records.push(record);
    }
    Ok(records)
}

struct Work {
    job: ProfileSaveJob,
    retained: u64,
    cancel: Arc<AtomicBool>,
}
struct Completion {
    id: RequestId,
    binding: ProfileSaveBinding,
    result: Result<Arc<ProfileRecord>, EngineError>,
}
pub(crate) struct ProfileJobs {
    sender: Option<SyncSender<Work>>,
    receiver: Receiver<Completion>,
    active: Option<(RequestId, Arc<AtomicBool>)>,
    join: Option<JoinHandle<()>>,
}
impl ProfileJobs {
    pub(crate) fn spawn(directory: PathBuf) -> Self {
        let (sender, work) = mpsc::sync_channel::<Work>(1);
        let (complete, receiver) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            while let Ok(work) = work.recv() {
                let result = save(&directory, &work.job, work.retained, &work.cancel);
                if complete
                    .send(Completion {
                        id: work.job.request_id,
                        binding: work.job.binding,
                        result,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            receiver,
            active: None,
            join: Some(join),
        }
    }
    pub(crate) fn submit(&mut self, session: &Session) -> Result<(), EngineError> {
        if self.active.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "profile worker is still retiring an operation",
            ));
        }
        let job = session.profile_job().ok_or(INVALID)?;
        let cancel = Arc::new(AtomicBool::new(false));
        let retained = session
            .profile_storage_bytes()
            .checked_mul(2)
            .and_then(|n| n.checked_add(session.profile_environment_bytes().ok()?))
            .ok_or(BUDGET)?;
        self.sender
            .as_ref()
            .ok_or(runtime::IO)?
            .try_send(Work {
                job: job.clone(),
                retained,
                cancel: cancel.clone(),
            })
            .map_err(|_| runtime::IO)?;
        self.active = Some((job.request_id, cancel));
        Ok(())
    }
    pub(crate) fn poll(
        &mut self,
        session: &mut Session,
        now: u64,
        directory: &Path,
        token: &str,
    ) -> Result<(), EngineError> {
        session.expire_profile_work(now);
        if let Some((id, cancel)) = &self.active
            && session.profile_job().is_none_or(|j| &j.request_id != id)
        {
            cancel.store(true, Ordering::Release);
        }
        while let Ok(completion) = self.receiver.try_recv() {
            if self
                .active
                .as_ref()
                .is_none_or(|(id, _)| id != &completion.id)
            {
                continue;
            }
            self.active = None;
            let mut persistence_error = None;
            let _ = session.complete_profile_save(
                &completion.id,
                &completion.binding,
                now,
                completion.result,
                |staged| {
                    let bytes = serde_json::to_vec(&staged.checkpoint()).map_err(|_| INVALID)?;
                    runtime::write_checkpoint(
                        directory,
                        &format!("{token}-profile-{}", staged.snapshot().revision),
                        &bytes,
                    )
                    .inspect_err(|e| persistence_error = Some(*e))
                },
            );
            if let Some(error) = persistence_error {
                return Err(error);
            }
        }
        Ok(())
    }
}
impl Drop for ProfileJobs {
    fn drop(&mut self) {
        if let Some((_, cancel)) = self.active.take() {
            cancel.store(true, Ordering::Release);
        }
        self.sender = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
