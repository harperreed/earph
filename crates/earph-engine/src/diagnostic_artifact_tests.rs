// ABOUTME: Publishes actual Session preview bytes into real private diagnostic directories.
// ABOUTME: Checks exact readback, typed effects and hostile filesystem inventory without an owner process.
use super::*;
use earph_core::{control::*, diagnostics::*, ids::*};
use std::{
    os::unix::fs::FileTypeExt,
    time::{Duration, Instant},
};
fn root() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/diagnostic-artifact-tests")
        .join(format!(
            "{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .unwrap();
    fs::canonicalize(root).unwrap()
}
fn session() -> Session {
    Session::new_owner(
        ZoneId::new("zone").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    )
}
fn command(s: &Session, id: &str, payload: Command) -> CommandEnvelope {
    let st = s.snapshot();
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(id).unwrap(),
        zone_id: st.zone_id,
        owner_id: st.owner_id,
        session_id: st.session_id,
        expected_revision: Some(st.revision),
        payload,
    }
}
fn export(s: &mut Session, directory: &Path) -> Arc<DiagnosticWorkRequest> {
    let c = command(s, "preview", Command::DiagnosticsPreview {});
    let r = s.submit(c);
    let Some(QueryResult::DiagnosticPreview(p)) = r.result else {
        panic!("actual preview")
    };
    let c = command(
        s,
        "export",
        Command::DiagnosticsExport {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: p.report.sha256,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Accepted);
    s.diagnostic_work().unwrap()
}
#[test]
fn diagnostic_real_publication_roundtrip_preserves_the_exact_reviewed_bytes() {
    let root = root();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let complete = super::diagnostic_artifacts::execute(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(
        complete.status,
        ReceiptStatus::Applied,
        "{:?}",
        complete.error
    );
    assert_eq!(
        complete.receipt.effect,
        DiagnosticFilesystemEffect::InstalledVerified
    );
    assert_eq!(
        fs::read(destination.join("diagnostics.json")).unwrap(),
        job.preview.as_ref().unwrap().report.bytes.as_bytes()
    );
    let manifest_bytes = fs::read(destination.join("manifest.json")).unwrap();
    let manifest = DiagnosticArtifactManifest::decode(&manifest_bytes).unwrap();
    manifest
        .validate_bundle(&fs::read(destination.join("diagnostics.json")).unwrap())
        .unwrap();
    assert_eq!(
        complete.receipt.manifest_sha256,
        Some(hash(&manifest_bytes).unwrap())
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
    s.complete_diagnostic_work(&job.request_id, &job.binding, 2, complete)
        .unwrap();
    let c = command(
        &s,
        "inspect",
        Command::DiagnosticsInspect {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: job.expected_sha256,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Accepted);
    let inspect = s.diagnostic_work().unwrap();
    let result = super::diagnostic_artifacts::execute(
        &root.join("runtime"),
        &inspect,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(
        result.receipt.inspection,
        Some(DiagnosticInspection::Matching)
    );
}
#[test]
fn diagnostic_pre_stage_cancellation_has_no_filesystem_effect() {
    let root = root();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let complete = super::diagnostic_artifacts::execute(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(true),
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(complete.status, ReceiptStatus::Cancelled);
    assert_eq!(
        complete.receipt.effect,
        DiagnosticFilesystemEffect::Unchanged
    );
    assert!(complete.receipt.stage_directory.is_none() && !destination.exists());
}

#[test]
fn diagnostic_actual_sync_and_write_faults_retain_their_effect_phase() {
    use super::diagnostic_artifacts::{
        DiagnosticArtifactFile as Object, DiagnosticIoPoint as Point, execute_with,
    };
    for (point, phase, installed) in [
        (
            Point::StageParentSync,
            DiagnosticFilesystemEffect::StageRetained,
            false,
        ),
        (
            Point::BeforeWriteChunk(Object::Diagnostics),
            DiagnosticFilesystemEffect::StageRetained,
            false,
        ),
        (
            Point::FileSync(Object::Diagnostics),
            DiagnosticFilesystemEffect::StageRetained,
            false,
        ),
        (
            Point::BeforeInstall,
            DiagnosticFilesystemEffect::StageRetained,
            false,
        ),
        (
            Point::InstalledParentSync,
            DiagnosticFilesystemEffect::InstalledSyncUncertain,
            true,
        ),
        (
            Point::InstalledReadback,
            DiagnosticFilesystemEffect::InspectRequired,
            true,
        ),
    ] {
        let root = root();
        let destination = root.join("bundle");
        let mut s = session();
        let job = export(&mut s, &destination);
        let result = execute_with(
            &root.join("runtime"),
            &job,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
            &mut |observed| {
                if observed == point {
                    Err(std::io::Error::other(
                        "controlled real-filesystem boundary failure",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(result.status, ReceiptStatus::Failed, "{point:?}");
        assert_eq!(result.receipt.effect, phase, "{point:?}");
        assert_eq!(destination.exists(), installed, "{point:?}");
        let stage = PathBuf::from(result.receipt.stage_directory.as_ref().unwrap());
        if !installed {
            assert!(stage.is_dir(), "created stage must remain: {stage:?}");
        }
        s.complete_diagnostic_work(&job.request_id, &job.binding, 2, result)
            .unwrap();
        let c = command(
            &s,
            "export",
            Command::DiagnosticsExport {
                directory: destination.to_str().unwrap().into(),
                expected_sha256: job.expected_sha256,
            },
        );
        let replay = s.submit(c);
        assert_eq!(replay.status, ReceiptStatus::Failed);
        let Some(QueryResult::DiagnosticArtifact(receipt)) = replay.result else {
            panic!("effect receipt")
        };
        assert_eq!(receipt.effect, phase);
        receipt.validate_for_status(replay.status).unwrap();
    }
}
#[test]
fn diagnostic_cancel_during_real_chunk_write_and_before_install_retains_stages() {
    use super::diagnostic_artifacts::{
        DiagnosticArtifactFile as Object, DiagnosticIoPoint as Point, execute_with,
    };
    for at in [
        Point::AfterWriteChunk(Object::Diagnostics),
        Point::BeforeInstall,
        Point::InstalledParentSync,
    ] {
        let root = root();
        let destination = root.join("bundle");
        let mut s = session();
        for n in 0..180 {
            let c = command(&s, &format!("mute-{n}"), Command::Mute { muted: true });
            assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
        }
        let job = export(&mut s, &destination);
        assert!(job.preview.as_ref().unwrap().report.byte_len > 16 * 1024);
        let cancel = AtomicBool::new(false);
        let result = execute_with(
            &root.join("runtime"),
            &job,
            &cancel,
            Instant::now() + Duration::from_secs(5),
            &mut |point| {
                if point == at {
                    cancel.store(true, Ordering::Release);
                }
                Ok(())
            },
        );
        assert_eq!(result.status, ReceiptStatus::Cancelled);
        assert_eq!(
            result.receipt.effect,
            if at == Point::InstalledParentSync {
                DiagnosticFilesystemEffect::InspectRequired
            } else {
                DiagnosticFilesystemEffect::StageRetained
            }
        );
        assert_eq!(destination.exists(), at == Point::InstalledParentSync);
        let stage = PathBuf::from(result.receipt.stage_directory.unwrap());
        if !destination.exists() {
            assert!(stage.is_dir());
        }
        if at == Point::AfterWriteChunk(Object::Diagnostics) {
            let lengths = fs::read_dir(&stage)
                .unwrap()
                .map(|e| e.unwrap().metadata().unwrap().len())
                .collect::<Vec<_>>();
            assert!(lengths.contains(&(16 * 1024)));
        }
    }
}
#[test]
fn diagnostic_destination_race_never_replaces_existing_files() {
    use super::diagnostic_artifacts::{DiagnosticIoPoint, execute_with};
    let root = root();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let result = execute_with(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
        &mut |point| {
            if point == DiagnosticIoPoint::BeforeInstall {
                fs::DirBuilder::new().mode(0o700).create(&destination)?;
                fs::write(destination.join("kept"), b"existing destination survives")?;
            }
            Ok(())
        },
    );
    assert_eq!(result.status, ReceiptStatus::Failed);
    assert_eq!(
        result.receipt.effect,
        DiagnosticFilesystemEffect::StageRetained
    );
    assert_eq!(
        fs::read(destination.join("kept")).unwrap(),
        b"existing destination survives"
    );
    assert!(Path::new(result.receipt.stage_directory.as_ref().unwrap()).is_dir());
}

#[test]
fn diagnostic_stage_name_replacement_cannot_publish_an_unreviewed_directory() {
    use super::diagnostic_artifacts::{DiagnosticIoPoint, execute_with};
    let root = root();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let result = execute_with(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
        &mut |point| {
            if point == DiagnosticIoPoint::BeforeInstall {
                let staged = fs::read_dir(&root)?
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .starts_with(".diagnostics-")
                    })
                    .unwrap();
                fs::rename(&staged, root.join("retained-reviewed-stage"))?;
                fs::DirBuilder::new().mode(0o700).create(&staged)?;
            }
            Ok(())
        },
    );
    assert_eq!(result.status, ReceiptStatus::Failed);
    assert!(
        !destination.exists(),
        "a replaced stage name must never publish another directory"
    );
    assert_eq!(
        result.receipt.effect,
        DiagnosticFilesystemEffect::InspectRequired
    );
    assert_eq!(
        fs::read(root.join("retained-reviewed-stage/diagnostics.json")).unwrap(),
        job.preview.as_ref().unwrap().report.bytes.as_bytes()
    );
}
fn inspect_job(destination: &Path, expected: ProfileObjectHash) -> Arc<DiagnosticWorkRequest> {
    let mut s = session();
    let c = command(
        &s,
        "inspect",
        Command::DiagnosticsInspect {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: expected,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Accepted);
    s.diagnostic_work().unwrap()
}
fn inspect(
    root: &Path,
    destination: &Path,
    expected: ProfileObjectHash,
) -> DiagnosticWorkCompletion {
    let job = inspect_job(destination, expected);
    super::diagnostic_artifacts::execute(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
    )
}
fn private_write(path: &Path, bytes: &[u8]) {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
}
#[test]
fn diagnostic_inspection_rejects_corrupt_extra_and_linked_objects_without_repair() {
    for fault in [
        "bytes",
        "extra",
        "symlink",
        "hardlink",
        "oversized",
        "fifo",
        "socket",
        "directory",
        "executable",
    ] {
        let root = if fault == "socket" {
            let root = Path::new("/tmp").join(format!(
                "eds-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            root
        } else {
            root()
        };
        let destination = root.join("bundle");
        let mut s = session();
        let job = export(&mut s, &destination);
        assert_eq!(
            super::diagnostic_artifacts::execute(
                &root.join("runtime"),
                &job,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(5)
            )
            .status,
            ReceiptStatus::Applied
        );
        match fault {
            "bytes" => {
                fs::rename(
                    destination.join("diagnostics.json"),
                    root.join("retained-original-diagnostics.json"),
                )
                .unwrap();
                private_write(&destination.join("diagnostics.json"), b"{}");
            }
            "extra" => private_write(&destination.join("extra"), b"unexpected object"),
            "symlink" => {
                fs::rename(
                    destination.join("diagnostics.json"),
                    root.join("retained-original-diagnostics.json"),
                )
                .unwrap();
                std::os::unix::fs::symlink(
                    root.join("retained-original-diagnostics.json"),
                    destination.join("diagnostics.json"),
                )
                .unwrap();
            }
            "hardlink" => fs::hard_link(
                destination.join("diagnostics.json"),
                root.join("retained-hardlink"),
            )
            .unwrap(),
            "oversized" => {
                fs::rename(
                    destination.join("manifest.json"),
                    root.join("retained-original-manifest.json"),
                )
                .unwrap();
                private_write(
                    &destination.join("manifest.json"),
                    &vec![b' '; MAX_PREVIEW_METADATA_BYTES + 1],
                );
            }
            "fifo" | "socket" | "directory" => {
                let object = destination.join("diagnostics.json");
                fs::rename(&object, root.join("retained-original-diagnostics.json")).unwrap();
                match fault {
                    "fifo" => {
                        let created = std::process::Command::new("mkfifo")
                            .args(["-m", "600"])
                            .arg(&object)
                            .output()
                            .unwrap();
                        assert!(
                            created.status.success(),
                            "{}",
                            String::from_utf8_lossy(&created.stderr)
                        );
                        assert!(fs::symlink_metadata(&object).unwrap().file_type().is_fifo());
                    }
                    "socket" => {
                        let _listener = std::os::unix::net::UnixListener::bind(&object).unwrap();
                    }
                    _ => fs::DirBuilder::new().mode(0o700).create(&object).unwrap(),
                }
            }
            "executable" => {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    destination.join("diagnostics.json"),
                    fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let result = inspect(&root, &destination, job.expected_sha256);
        assert_eq!(
            result.receipt.inspection,
            Some(DiagnosticInspection::Conflicting),
            "{fault}"
        );
        assert_eq!(result.receipt.effect, DiagnosticFilesystemEffect::Unchanged);
        assert!(destination.is_dir());
    }
}
#[test]
fn diagnostic_parent_permissions_and_symlink_walk_fail_before_staging() {
    use std::os::unix::fs::PermissionsExt;
    for symlink in [false, true] {
        let root = root();
        let parent = root.join("parent");
        fs::DirBuilder::new()
            .mode(if symlink { 0o700 } else { 0o755 })
            .create(&parent)
            .unwrap();
        let selected = if symlink {
            let link = root.join("alias");
            std::os::unix::fs::symlink(&parent, &link).unwrap();
            link
        } else {
            parent.clone()
        };
        let destination = selected.join("bundle");
        let mut s = session();
        let job = export(&mut s, &destination);
        let result = super::diagnostic_artifacts::execute(
            &root.join("runtime"),
            &job,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(5),
        );
        assert_eq!(result.status, ReceiptStatus::Failed);
        assert_eq!(result.receipt.effect, DiagnosticFilesystemEffect::Unchanged);
        assert!(result.receipt.stage_directory.is_none());
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 0);
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            if symlink { 0o700 } else { 0o755 }
        );
    }
}

#[test]
fn diagnostic_shared_worker_publishes_and_retires_before_normal_join() {
    let root = root();
    let runtime = root.join("runtime");
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let mut worker = super::ArtifactJobs::spawn(runtime.clone());
    worker.submit(&s, 1).unwrap();
    assert!(!worker.is_idle());
    let end = Instant::now() + Duration::from_secs(5);
    while !worker.is_idle() {
        worker.poll(&mut s, 2, &runtime, "diagnostic-test").unwrap();
        assert!(Instant::now() < end);
        std::thread::yield_now();
    }
    assert!(s.diagnostic_work().is_none());
    let c = command(
        &s,
        "export",
        Command::DiagnosticsExport {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: job.expected_sha256,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    drop(worker);
    assert!(destination.join("diagnostics.json").is_file());
}

#[test]
fn diagnostic_inspection_distinguishes_missing_destination_from_missing_objects() {
    let root = root();
    let expected = hash(b"known expected bytes").unwrap();
    let missing = inspect(&root, &root.join("absent"), expected);
    assert_eq!(
        missing.receipt.inspection,
        Some(DiagnosticInspection::Missing)
    );
    let empty = root.join("empty");
    fs::DirBuilder::new().mode(0o700).create(&empty).unwrap();
    let conflicting = inspect(&root, &empty, expected);
    assert_eq!(
        conflicting.receipt.inspection,
        Some(DiagnosticInspection::Conflicting)
    );
}

#[test]
fn diagnostic_strict_consumer_rejects_false_absence_and_backwards_completion() {
    let root = root();
    let destination = root.join("bundle");
    let mut s = session();
    let job = export(&mut s, &destination);
    let complete = super::diagnostic_artifacts::execute(
        &root.join("runtime"),
        &job,
        &AtomicBool::new(false),
        Instant::now() + Duration::from_secs(5),
    );
    assert_eq!(complete.status, ReceiptStatus::Applied);
    s.complete_diagnostic_work(&job.request_id, &job.binding, 2, complete)
        .unwrap();
    let c = command(
        &s,
        "export",
        Command::DiagnosticsExport {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: job.expected_sha256,
        },
    );
    let receipt = s.submit(c);
    let Some(QueryResult::DiagnosticArtifact(original)) = receipt.result else {
        panic!("real terminal receipt")
    };
    let mut bad = (*original).clone();
    bad.operation = DiagnosticArtifactOperation::Inspect;
    bad.effect = DiagnosticFilesystemEffect::Unchanged;
    bad.stage_directory = None;
    bad.inspection = Some(DiagnosticInspection::Missing);
    assert!(
        bad.validate_for_status(ReceiptStatus::Applied).is_err(),
        "missing cannot retain validated present-object facts"
    );
    bad = (*original).clone();
    bad.deadline_monotonic_ns = DIAGNOSTIC_JOB_DEADLINE_NS + 1;
    bad.completed_monotonic_ns = Some(0);
    assert!(
        bad.validate_for_status(ReceiptStatus::Applied).is_err(),
        "completion cannot precede admission"
    );
}
