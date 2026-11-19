// ABOUTME: Publishes exact reviewed diagnostic bytes with typed retained filesystem effects.
// ABOUTME: Inspects bounded private directories and cooperates with cancellation without deleting artifacts.
use super::artifact_io::{self, ArtifactDirectory, ArtifactLimits, ArtifactStage, StageSync};
use super::*;
use earph_core::{control::ReceiptStatus, diagnostics::*};
use std::{cell::RefCell, io::Write, time::Instant};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticArtifactFile {
    Manifest,
    Diagnostics,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticIoPoint {
    StageParentSync,
    StageContentSync,
    InstalledParentSync,
    BeforeInstall,
    StageReadback,
    InstalledReadback,
    BeforeWriteChunk(DiagnosticArtifactFile),
    AfterWriteChunk(DiagnosticArtifactFile),
    FileSync(DiagnosticArtifactFile),
}
const CANCEL: EngineError = EngineError::new(
    ErrorCode::OperationCancelled,
    "diagnostic artifact work was cancelled or expired",
);
struct Context<'a, F> {
    cancel: &'a AtomicBool,
    deadline: Instant,
    hook: RefCell<&'a mut F>,
}
impl<F: FnMut(DiagnosticIoPoint) -> std::io::Result<()>> Context<'_, F> {
    fn check(&self) -> Result<(), EngineError> {
        if self.cancel.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            Err(CANCEL)
        } else {
            Ok(())
        }
    }
    fn point(&self, point: DiagnosticIoPoint) -> std::io::Result<()> {
        (self.hook.borrow_mut())(point)
    }
    fn sync(&self, file: &File, point: StageSync) -> std::io::Result<()> {
        self.point(match point {
            StageSync::CreatedParent => DiagnosticIoPoint::StageParentSync,
            StageSync::Contents => DiagnosticIoPoint::StageContentSync,
            StageSync::InstalledParent => DiagnosticIoPoint::InstalledParentSync,
        })?;
        file.sync_all()
    }
    fn failed(
        &self,
        error: EngineError,
        mut receipt: DiagnosticArtifactReceipt,
    ) -> DiagnosticWorkCompletion {
        let cancelled = error.code == ErrorCode::OperationCancelled
            || self.cancel.load(Ordering::Acquire)
            || Instant::now() >= self.deadline;
        receipt.reason = Some(if Instant::now() >= self.deadline {
            DiagnosticArtifactReason::DeadlineExceeded
        } else if cancelled {
            DiagnosticArtifactReason::Cancelled
        } else if error == artifact_io::DESTINATION {
            DiagnosticArtifactReason::DestinationExists
        } else {
            match error.code {
                ErrorCode::ResourceBudgetExceeded => DiagnosticArtifactReason::ResourceLimit,
                ErrorCode::AuthDenied => DiagnosticArtifactReason::UnsafePath,
                ErrorCode::AssetInvalid => DiagnosticArtifactReason::InvalidObject,
                _ => DiagnosticArtifactReason::StorageUnavailable,
            }
        });
        DiagnosticWorkCompletion {
            receipt,
            status: if cancelled {
                ReceiptStatus::Cancelled
            } else {
                ReceiptStatus::Failed
            },
            error: Some(if cancelled {
                CANCEL
            } else {
                EngineError::new(
                    error.code,
                    "diagnostic artifact operation failed; inspect retained effects",
                )
            }),
        }
    }
}
fn limits() -> ArtifactLimits {
    ArtifactLimits {
        max_entries: 2,
        max_bytes: MAX_DIAGNOSTIC_ARTIFACT_BYTES as u64,
        private: true,
    }
}
fn readback(
    directory: &Path,
    expected: ProfileObjectHash,
    check: &mut impl FnMut() -> Result<(), EngineError>,
    expected_inode: Option<&File>,
) -> Result<(DiagnosticArtifactManifest, ProfileObjectHash, u64), EngineError> {
    let opened = ArtifactDirectory::open_with(directory, limits(), check)?;
    if let Some(expected) = expected_inode
        && !artifact_io::same_directory(&opened.file, expected)?
    {
        return Err(INVALID);
    }
    let object_error = |error| {
        if error == artifact_io::MISSING {
            artifact_io::INVENTORY
        } else {
            error
        }
    };
    let manifest_bytes = opened
        .read_with("manifest.json", MAX_PREVIEW_METADATA_BYTES, check)
        .map_err(object_error)?;
    let manifest = DiagnosticArtifactManifest::decode(&manifest_bytes)?;
    let bytes = opened
        .read_with("diagnostics.json", MAX_PREVIEW_BYTES, check)
        .map_err(object_error)?;
    manifest.validate_bundle(&bytes)?;
    if manifest.diagnostics_sha256 != expected {
        return Err(EngineError::new(
            ErrorCode::AssetInvalid,
            "diagnostic bundle differs from the expected hash",
        ));
    }
    opened.validate_exact(
        &[
            ("manifest.json".into(), manifest_bytes.len() as u64),
            ("diagnostics.json".into(), bytes.len() as u64),
        ],
        check,
    )?;
    Ok((
        manifest,
        hash(&manifest_bytes)?,
        manifest_bytes.len() as u64,
    ))
}
fn observed(
    receipt: &mut DiagnosticArtifactReceipt,
    manifest: &DiagnosticArtifactManifest,
    digest: ProfileObjectHash,
    len: u64,
) {
    receipt.observed_bundle_sha256 = Some(manifest.diagnostics_sha256);
    receipt.manifest_sha256 = Some(digest);
    receipt.diagnostics_bytes = Some(manifest.diagnostics_bytes);
    receipt.manifest_bytes = Some(len);
    receipt.bundle_cut = Some(manifest.cut);
}
pub(crate) fn execute(
    runtime: &Path,
    job: &DiagnosticWorkRequest,
    cancel: &AtomicBool,
    deadline: Instant,
) -> DiagnosticWorkCompletion {
    execute_with(runtime, job, cancel, deadline, &mut |_| Ok(()))
}
pub(crate) fn execute_with(
    runtime: &Path,
    job: &DiagnosticWorkRequest,
    cancel: &AtomicBool,
    deadline: Instant,
    hook: &mut impl FnMut(DiagnosticIoPoint) -> std::io::Result<()>,
) -> DiagnosticWorkCompletion {
    let context = Context {
        cancel,
        deadline,
        hook: RefCell::new(hook),
    };
    let mut receipt = job.pending_receipt();
    receipt.effect = DiagnosticFilesystemEffect::Unchanged;
    let destination = Path::new(&job.directory);
    let preflight = (|| {
        job.pending_receipt()
            .validate_for_status(ReceiptStatus::Accepted)?;
        context.check()?;
        if destination.starts_with(runtime) || runtime.starts_with(destination) {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "diagnostic path overlaps owner storage",
            ));
        }
        Ok(())
    })();
    if let Err(error) = preflight {
        return context.failed(error, receipt);
    }
    if job.operation == DiagnosticArtifactOperation::Inspect {
        return match readback(
            destination,
            job.expected_sha256,
            &mut || context.check(),
            None,
        ) {
            Ok((manifest, digest, len)) => {
                observed(&mut receipt, &manifest, digest, len);
                receipt.inspection = Some(DiagnosticInspection::Matching);
                DiagnosticWorkCompletion {
                    receipt,
                    status: ReceiptStatus::Applied,
                    error: None,
                }
            }
            Err(error) if error.code == ErrorCode::OperationCancelled => {
                context.failed(error, receipt)
            }
            Err(error) => {
                receipt.inspection = Some(if error == artifact_io::MISSING {
                    DiagnosticInspection::Missing
                } else if matches!(
                    error.code,
                    ErrorCode::AssetInvalid | ErrorCode::ResourceBudgetExceeded
                ) {
                    DiagnosticInspection::Conflicting
                } else {
                    DiagnosticInspection::Unreadable
                });
                if error != artifact_io::MISSING {
                    receipt.reason = Some(match error.code {
                        ErrorCode::AuthDenied => DiagnosticArtifactReason::UnsafePath,
                        ErrorCode::ResourceBudgetExceeded => {
                            DiagnosticArtifactReason::ResourceLimit
                        }
                        ErrorCode::AssetInvalid => DiagnosticArtifactReason::InvalidObject,
                        _ => DiagnosticArtifactReason::StorageUnavailable,
                    });
                }
                DiagnosticWorkCompletion {
                    receipt,
                    status: ReceiptStatus::Applied,
                    error: None,
                }
            }
        };
    }
    let preparation = (|| {
        let preview = job.preview.as_ref().ok_or(INVALID)?;
        if preview.binding.zone_id != job.binding.zone_id
            || preview.binding.owner_id != job.binding.owner_id
            || preview.report.sha256 != job.expected_sha256
        {
            return Err(INVALID);
        }
        let manifest = DiagnosticArtifactManifest::for_preview(preview)?;
        let bytes = serde_json::to_vec(&manifest).map_err(|_| INVALID)?;
        if bytes.capacity() > MAX_PREVIEW_METADATA_BYTES
            || bytes
                .len()
                .checked_add(preview.report.bytes.len())
                .is_none_or(|n| n > MAX_DIAGNOSTIC_ARTIFACT_BYTES)
        {
            return Err(BUDGET);
        }
        context.check()?;
        Ok((preview, bytes))
    })();
    let (preview, manifest_bytes) = match preparation {
        Ok(prepared) => prepared,
        Err(error) => return context.failed(error, receipt),
    };
    let mut stage = match ArtifactStage::create_with(
        destination,
        runtime,
        ".diagnostics-",
        1024,
        &mut || context.check(),
        &mut |file, point| context.sync(file, point),
    ) {
        Ok(stage) => stage,
        Err(failure) => {
            receipt.stage_directory = failure.stage.map(|p| p.to_string_lossy().into_owned());
            if receipt.stage_directory.is_some() {
                receipt.effect = DiagnosticFilesystemEffect::StageRetained;
            }
            return context.failed(failure.error, receipt);
        }
    };
    receipt.stage_directory = Some(
        stage
            .path
            .to_str()
            .expect("validated UTF8 destination")
            .to_owned(),
    );
    receipt.effect = DiagnosticFilesystemEffect::StageRetained;
    let write = (|| {
        for (name, bytes, kind) in [
            (
                "manifest.json",
                manifest_bytes.as_slice(),
                DiagnosticArtifactFile::Manifest,
            ),
            (
                "diagnostics.json",
                preview.report.bytes.as_bytes(),
                DiagnosticArtifactFile::Diagnostics,
            ),
        ] {
            context.check()?;
            stage.write_with(
                name,
                bytes,
                &mut || context.check(),
                &mut |file| {
                    context.point(DiagnosticIoPoint::FileSync(kind))?;
                    file.sync_all()
                },
                &mut |file, chunk| {
                    context.point(DiagnosticIoPoint::BeforeWriteChunk(kind))?;
                    context.check().map_err(std::io::Error::other)?;
                    file.write_all(chunk)?;
                    context.point(DiagnosticIoPoint::AfterWriteChunk(kind))
                },
            )?;
        }
        context
            .point(DiagnosticIoPoint::StageReadback)
            .map_err(|_| runtime::IO)?;
        let (manifest, digest, len) = readback(
            &stage.path,
            job.expected_sha256,
            &mut || context.check(),
            Some(&stage.file),
        )?;
        observed(&mut receipt, &manifest, digest, len);
        stage.publish_with(
            &mut || context.check(),
            &mut |file, point| context.sync(file, point),
            &mut || {
                context
                    .point(DiagnosticIoPoint::BeforeInstall)
                    .map_err(|_| runtime::IO)?;
                context.check()
            },
            true,
        )?;
        receipt.effect = DiagnosticFilesystemEffect::InspectRequired;
        context
            .point(DiagnosticIoPoint::InstalledReadback)
            .map_err(|_| runtime::IO)?;
        let (manifest, digest, len) = readback(
            destination,
            job.expected_sha256,
            &mut || context.check(),
            Some(&stage.file),
        )?;
        observed(&mut receipt, &manifest, digest, len);
        receipt.effect = DiagnosticFilesystemEffect::InstalledVerified;
        Ok(())
    })();
    match write {
        Ok(()) => DiagnosticWorkCompletion {
            receipt,
            status: ReceiptStatus::Applied,
            error: None,
        },
        Err(error) => {
            if stage.named_identity_uncertain() {
                receipt.effect = DiagnosticFilesystemEffect::InspectRequired;
            } else if stage.installed() {
                receipt.effect = if stage.installed_synced() {
                    DiagnosticFilesystemEffect::InspectRequired
                } else {
                    DiagnosticFilesystemEffect::InstalledSyncUncertain
                };
            }
            context.failed(error, receipt)
        }
    }
}
