// ABOUTME: Exercises explicit diagnostic GUI workflows against the actual idle owner and filesystem.
// ABOUTME: Retains uncertain envelopes and generated artifacts without opening output or changing gain.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::{Owner, proxy, unique};
use earph_core::{control::Command, diagnostics::*, ids::RequestId};
use earph_desktop::{
    Presentation, ResultView, StateView,
    gui::{GuiController, UiIntent},
};
use std::{
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

fn artifact_root() -> PathBuf {
    let root = PathBuf::from("/tmp").join(unique());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    println!("retained diagnostic artifact fixture: {}", root.display());
    std::fs::canonicalize(root).unwrap()
}

fn until(gui: &mut GuiController, predicate: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.tick(Instant::now());
        if predicate(gui) {
            return;
        }
        if Instant::now() >= deadline {
            let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/desktop-diagnostics-gui/round-01/failures")
                .join(unique());
            std::fs::create_dir_all(&evidence).unwrap();
            let mut summary = format!(
                "runtime {:?}\nerror {:?}\nstate {:?}\nwaiting {:?}\n",
                gui.runtime(),
                gui.error(),
                gui.state(),
                gui.waiting_queue_edit()
            );
            for (index, record) in gui.receipts().enumerate() {
                summary.push_str(&format!(
                    "{} {:?} in_flight={} owner_changed={} {:?}\n",
                    record.request.envelope().request_id.as_str(),
                    record.presentation,
                    record.in_flight,
                    record.owner_changed,
                    record.outcome
                ));
                std::fs::write(
                    evidence.join(format!("{index:02}.request.json")),
                    record.request.bytes(),
                )
                .unwrap();
                if let Some(Ok(reply)) = record.outcome {
                    std::fs::write(
                        evidence.join(format!("{index:02}.reply.utf8")),
                        reply.bytes(),
                    )
                    .unwrap();
                }
            }
            std::fs::write(evidence.join("summary.txt"), summary).unwrap();
            panic!(
                "diagnostic GUI stalled; retained exact evidence {}",
                evidence.display()
            );
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn attached(runtime: &Path) -> GuiController {
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |gui| {
        gui.connected() && gui.diagnostic_read_available()
    });
    gui
}
fn action(gui: &mut GuiController, command: Command) {
    let basis = Arc::new(gui.state().unwrap().clone());
    gui.handle(UiIntent::DiagnosticAction { basis, command });
}
fn capture(gui: &mut GuiController) -> Arc<PreviewRead> {
    action(gui, Command::DiagnosticsPreview {});
    until(gui, |gui| {
        gui.diagnostic_preview().is_some() && !gui.queue_edit_pending()
    });
    Arc::new(gui.diagnostic_preview().unwrap().clone())
}
fn reviewed_export(gui: &mut GuiController, directory: &Path, preview: &PreviewRead) {
    gui.handle(UiIntent::DiagnosticDestination(
        directory.to_str().unwrap().into(),
    ));
    gui.handle(UiIntent::ReviewDiagnosticPreview);
    assert!(
        gui.diagnostic_preview_reviewed(),
        "review failed: {:?}",
        gui.error()
    );
    action(
        gui,
        Command::DiagnosticsExport {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
}
fn artifact(
    gui: &GuiController,
    directory: &Path,
    operation: DiagnosticArtifactOperation,
) -> Option<(RequestId, Presentation, Arc<DiagnosticArtifactReceipt>)> {
    gui.receipts()
        .filter_map(|record| {
            let Some(Ok(earph_desktop::Reply::Receipt(reply))) = record.outcome else {
                return None;
            };
            let Some(ResultView::DiagnosticArtifact(value)) = &reply.result else {
                return None;
            };
            (value.directory == directory.to_str().unwrap() && value.operation == operation)
                .then(|| (reply.request_id.clone(), record.presentation, value.clone()))
        })
        .last()
}
fn status(gui: &GuiController, id: &RequestId) -> Option<Presentation> {
    gui.receipts()
        .find(|record| &record.request.envelope().request_id == id)
        .map(|record| record.presentation)
}
fn non_queries(gui: &GuiController) -> Vec<RequestId> {
    gui.receipts()
        .filter(|record| !record.request.envelope().payload.is_query())
        .map(|record| record.request.envelope().request_id.clone())
        .collect()
}
fn safe_idle(before: &StateView, after: &StateView) {
    assert_eq!(after.owner_id, before.owner_id);
    assert_eq!(after.zone_id, before.zone_id);
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.epochs, before.epochs);
    assert_eq!(after.event_sequence, before.event_sequence);
    assert_eq!(after.graph_epoch, before.graph_epoch);
    assert_eq!(after.device_epoch, before.device_epoch);
    assert_eq!(after.software_gain_db, before.software_gain_db);
    assert!(after.muted && !after.output_consented && after.output.is_none());
    assert!(after.source.is_none() && after.playback.is_none());
}
fn close(gui: &mut GuiController, owner: &mut Owner) {
    gui.handle(UiIntent::CloseController);
    until(gui, GuiController::ready_to_close);
    owner.stop();
}

#[test]
fn actual_owner_reads_are_adopted_before_retirement_without_becoming_product_state() {
    let mut owner = Owner::start();
    let before = owner.state();
    let mut gui = attached(&owner.runtime);
    action(&mut gui, Command::SignalPathGet {});
    until(&mut gui, |gui| {
        gui.diagnostic_signal_path().is_some() && !gui.queue_edit_pending()
    });
    action(&mut gui, Command::MetricsGet {});
    until(&mut gui, |gui| {
        gui.diagnostic_metrics().is_some() && !gui.queue_edit_pending()
    });
    action(&mut gui, Command::EventsGet { cursor: None });
    until(&mut gui, |gui| {
        gui.diagnostic_events().is_some() && !gui.queue_edit_pending()
    });
    let preview = capture(&mut gui);
    assert!(
        gui.diagnostic_signal_path()
            .unwrap()
            .report
            .resolved
            .is_none()
    );
    assert!(
        gui.diagnostic_signal_path()
            .unwrap()
            .report
            .observed
            .is_none()
    );
    assert_eq!(
        gui.diagnostic_metrics().unwrap().report.underruns.value,
        None
    );
    assert_eq!(
        gui.diagnostic_metrics()
            .unwrap()
            .report
            .underruns
            .unavailable_reason,
        Some(UnavailableReason::Unsupported)
    );
    assert!(!gui.diagnostic_events().unwrap().report.complete_audit_log);
    assert!(DiagnosticBundle::decode(preview.report.bytes.as_bytes()).is_ok());
    assert!(
        !gui.receipts()
            .any(|record| record.request.envelope().payload.is_diagnostic()),
        "successful diagnostic reads churn retained receipt history"
    );
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}

#[test]
fn actual_owner_export_requires_exact_review_and_inspection_preserves_original_bytes() {
    let mut owner = Owner::start();
    let before = owner.state();
    let mut gui = attached(&owner.runtime);
    let preview = capture(&mut gui);
    let artifacts = artifact_root();
    let directory = artifacts.join("reviewed-diagnostics");
    gui.handle(UiIntent::DiagnosticDestination(
        directory.to_str().unwrap().into(),
    ));
    let records = non_queries(&gui);
    action(
        &mut gui,
        Command::DiagnosticsExport {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    assert_eq!(non_queries(&gui), records);
    assert!(!directory.exists(), "unreviewed export reached the owner");
    gui.handle(UiIntent::ReviewDiagnosticPreview);
    assert!(gui.diagnostic_preview_reviewed());
    let changed = artifacts.join("changed-destination");
    gui.handle(UiIntent::DiagnosticDestination(
        changed.to_str().unwrap().into(),
    ));
    assert!(!gui.diagnostic_preview_reviewed());
    reviewed_export(&mut gui, &directory, &preview);
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Export)
            .is_some_and(|(_, status, _)| status == Presentation::Applied)
    });
    let (_, _, exported) = artifact(&gui, &directory, DiagnosticArtifactOperation::Export).unwrap();
    assert_eq!(
        exported.effect,
        DiagnosticFilesystemEffect::InstalledVerified
    );
    assert!(
        gui.error().is_none(),
        "normal export left a controller error: {:?}",
        gui.error()
    );
    let bytes = std::fs::read(directory.join("diagnostics.json")).unwrap();
    let manifest_bytes = std::fs::read(directory.join("manifest.json")).unwrap();
    assert_eq!(bytes, preview.report.bytes.as_bytes());
    let manifest: DiagnosticArtifactManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest.validate().unwrap();
    assert_eq!(manifest.diagnostics_sha256, preview.report.sha256);
    assert_eq!(manifest.diagnostics_bytes, bytes.len() as u64);
    assert_eq!(manifest.cut, preview.report.cut);
    until(&mut gui, GuiController::diagnostic_read_available);
    action(
        &mut gui,
        Command::DiagnosticsInspect {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Inspect)
            .is_some_and(|(_, status, _)| status == Presentation::Applied)
    });
    let (_, _, inspected) =
        artifact(&gui, &directory, DiagnosticArtifactOperation::Inspect).unwrap();
    assert_eq!(inspected.effect, DiagnosticFilesystemEffect::Unchanged);
    assert_eq!(inspected.inspection, Some(DiagnosticInspection::Matching));
    assert_eq!(
        std::fs::read(directory.join("diagnostics.json")).unwrap(),
        bytes
    );
    assert_eq!(
        std::fs::read(directory.join("manifest.json")).unwrap(),
        manifest_bytes
    );
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}

#[test]
fn accepted_artifact_releases_send_workflow_and_cancel_ack_waits_for_original_terminal_receipt() {
    let mut owner = Owner::start();
    let before = owner.state();
    let mut gui = attached(&owner.runtime);
    let preview = capture(&mut gui);
    let directory = artifact_root().join("cancel-race-diagnostics");
    reviewed_export(&mut gui, &directory, &preview);
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Export)
            .is_some_and(|(_, status, _)| status == Presentation::Accepted)
    });
    let (export_id, _, _) =
        artifact(&gui, &directory, DiagnosticArtifactOperation::Export).unwrap();
    assert!(
        !gui.queue_edit_pending(),
        "Accepted artifact held the foreground workflow"
    );
    assert!(gui.diagnostic_read_available());
    assert!(
        !gui.control_edit_available(),
        "a second publication was available during unresolved work"
    );
    action(
        &mut gui,
        Command::DiagnosticsCancel {
            request_id: export_id.clone(),
        },
    );
    until(&mut gui, |gui| {
        gui.receipts().any(|record| matches!(record.outcome, Some(Ok(earph_desktop::Reply::Receipt(reply))) if matches!(&reply.result, Some(ResultView::DiagnosticCancel(value)) if value.request_id == export_id)))
    });
    let cancel = gui
        .receipts()
        .find_map(|record| match record.outcome {
            Some(Ok(earph_desktop::Reply::Receipt(reply))) => match &reply.result {
                Some(ResultView::DiagnosticCancel(value)) if value.request_id == export_id => {
                    Some(value.clone())
                }
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert!(matches!(
        cancel.state,
        DiagnosticCancelState::Requested | DiagnosticCancelState::AlreadyTerminal
    ));
    until(&mut gui, |gui| {
        status(gui, &export_id).is_some_and(|status| {
            matches!(
                status,
                Presentation::Applied | Presentation::Cancelled | Presentation::Failed
            )
        })
    });
    let (_, terminal, effect) =
        artifact(&gui, &directory, DiagnosticArtifactOperation::Export).unwrap();
    if terminal == Presentation::Applied {
        assert_eq!(effect.effect, DiagnosticFilesystemEffect::InstalledVerified);
    }
    assert!(effect.completed_monotonic_ns.is_some());
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}

#[test]
fn lost_export_reply_preserves_exact_identity_and_readonly_inspection_never_republishes() {
    let mut owner = Owner::start();
    let before = owner.state();
    let transport = proxy::Proxy::drop_first_diagnostic_export(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    let preview = capture(&mut gui);
    let directory = artifact_root().join("uncertain-diagnostics");
    reviewed_export(&mut gui, &directory, &preview);
    until(&mut gui, |gui| {
        gui.receipts().any(|record| {
            matches!(
                record.request.envelope().payload,
                Command::DiagnosticsExport { .. }
            ) && record.presentation == Presentation::Uncertain
        })
    });
    let original = gui
        .receipts()
        .find(|record| {
            matches!(
                record.request.envelope().payload,
                Command::DiagnosticsExport { .. }
            )
        })
        .unwrap()
        .request
        .clone();
    let id = original.envelope().request_id.clone();
    until(&mut gui, GuiController::diagnostic_read_available);
    assert!(!gui.control_edit_available());
    action(
        &mut gui,
        Command::DiagnosticsInspect {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Inspect)
            .is_some_and(|(_, status, _)| status == Presentation::Applied)
    });
    assert_eq!(
        status(&gui, &id),
        Some(Presentation::Uncertain),
        "inspection silently reconciled the publication"
    );
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |gui| {
        status(gui, &id) == Some(Presentation::Applied)
    });
    assert_eq!(
        gui.receipts()
            .find(|record| record.request.envelope().request_id == id)
            .unwrap()
            .request
            .bytes(),
        original.bytes()
    );
    let frames = transport.frames.lock().unwrap();
    let exports: Vec<_> = frames
        .iter()
        .filter(|frame| {
            serde_json::from_slice::<serde_json::Value>(frame).unwrap()["payload"]["method"]
                == "diagnostics.export"
        })
        .collect();
    assert!(exports.len() >= 2);
    assert!(
        exports
            .iter()
            .all(|frame| frame.as_slice() == original.bytes()),
        "recovery created a replacement publication identity"
    );
    drop(frames);
    assert_eq!(
        std::fs::read(directory.join("diagnostics.json")).unwrap(),
        preview.report.bytes.as_bytes()
    );
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}

#[test]
fn reconnect_to_replacement_owner_clears_review_and_does_not_retarget_retained_cancel() {
    let mut owner = Owner::start();
    let runtime = owner.runtime.clone();
    let mut gui = attached(&runtime);
    let preview = capture(&mut gui);
    let directory = artifact_root().join("replacement-owner-diagnostics");
    reviewed_export(&mut gui, &directory, &preview);
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Export)
            .is_some_and(|(_, status, _)| status == Presentation::Applied)
    });
    let (old_id, _, _) = artifact(&gui, &directory, DiagnosticArtifactOperation::Export).unwrap();
    let old_owner = gui.state().unwrap().owner_id.clone();
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |gui| !gui.busy());
    owner.stop();
    let mut replacement = Owner::at(runtime.clone());
    gui.handle(UiIntent::Attach {
        runtime: runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |gui| {
        gui.diagnostic_read_available()
            && gui.state().is_some_and(|state| state.owner_id != old_owner)
    });
    assert!(gui.diagnostic_preview().is_none() && !gui.diagnostic_preview_reviewed());
    assert!(
        gui.receipts()
            .any(|record| record.request.envelope().request_id == old_id && record.owner_changed)
    );
    let records = non_queries(&gui);
    action(&mut gui, Command::DiagnosticsCancel { request_id: old_id });
    assert_eq!(
        non_queries(&gui),
        records,
        "old cancellation was retargeted to a replacement owner"
    );
    assert_eq!(
        std::fs::read(directory.join("diagnostics.json")).unwrap(),
        preview.report.bytes.as_bytes()
    );
    close(&mut gui, &mut replacement);
}

#[test]
fn bounded_waiting_and_destination_changes_cancel_only_unsent_diagnostic_work() {
    let mut owner = Owner::start();
    let before = owner.state();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    let preview = capture(&mut gui);
    transport.hold_next_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |_| transport.state_held());
    action(&mut gui, Command::SignalPathGet {});
    assert!(matches!(
        gui.waiting_queue_edit(),
        Some(Command::SignalPathGet {})
    ));
    action(&mut gui, Command::MetricsGet {});
    assert!(
        matches!(gui.waiting_queue_edit(), Some(Command::SignalPathGet {})),
        "second foreground capture replaced bounded waiting work"
    );
    transport.release_state();
    until(&mut gui, |gui| {
        gui.diagnostic_signal_path().is_some() && !gui.queue_edit_pending()
    });
    assert!(gui.diagnostic_metrics().is_none());
    let artifacts = artifact_root();
    let directory = artifacts.join("unsent-diagnostics");
    gui.handle(UiIntent::DiagnosticDestination(
        directory.to_str().unwrap().into(),
    ));
    gui.handle(UiIntent::ReviewDiagnosticPreview);
    assert!(gui.diagnostic_preview_reviewed());
    transport.hold_next_state();
    action(
        &mut gui,
        Command::DiagnosticsExport {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    until(&mut gui, |_| transport.state_held());
    gui.handle(UiIntent::DiagnosticDestination(
        artifacts
            .join("different-unsent-destination")
            .to_str()
            .unwrap()
            .into(),
    ));
    assert!(!gui.diagnostic_preview_reviewed());
    transport.release_state();
    until(&mut gui, |gui| {
        gui.diagnostic_read_available() && !gui.busy()
    });
    assert!(
        !gui.receipts().any(|record| matches!(
            record.request.envelope().payload,
            Command::DiagnosticsExport { .. }
        )),
        "changed draft still submitted its publication"
    );
    assert!(!directory.exists());
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}

#[test]
fn owner_replaced_candidate_refusal_requires_a_new_explicit_capture_and_review() {
    let mut owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let old = capture(&mut gui);
    let directory = artifact_root().join("replaced-candidate-diagnostics");
    gui.handle(UiIntent::DiagnosticDestination(
        directory.to_str().unwrap().into(),
    ));
    gui.handle(UiIntent::ReviewDiagnosticPreview);
    gui.handle(UiIntent::Mute);
    until(&mut gui, |gui| {
        gui.diagnostic_read_available()
            && gui
                .state()
                .is_some_and(|state| state.revision > old.report.cut.revision)
    });
    let before = owner.state();
    let external = earph_desktop::Request::diagnostic_query(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::DiagnosticsPreview {},
    )
    .unwrap();
    let earph_desktop::Reply::Receipt(reply) = owner.client().request(&external).unwrap() else {
        panic!("external preview")
    };
    let Some(ResultView::DiagnosticPreview(replacement)) = reply.result else {
        panic!("replacement preview")
    };
    assert_ne!(replacement.report.sha256, old.report.sha256);
    action(
        &mut gui,
        Command::DiagnosticsExport {
            directory: directory.to_str().unwrap().into(),
            expected_sha256: old.report.sha256,
        },
    );
    until(&mut gui, |gui| {
        gui.receipts().any(|record| {
            matches!(
                record.request.envelope().payload,
                Command::DiagnosticsExport { .. }
            ) && record.presentation == Presentation::Rejected
        })
    });
    let refused = gui
        .receipts()
        .find_map(|record| match record.outcome {
            Some(Ok(earph_desktop::Reply::Receipt(reply)))
                if matches!(
                    record.request.envelope().payload,
                    Command::DiagnosticsExport { .. }
                ) =>
            {
                Some(reply)
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(
        refused.error.as_ref().unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    assert_eq!(
        gui.diagnostic_preview().unwrap().report.sha256,
        old.report.sha256,
        "export silently captured a replacement preview"
    );
    assert!(!directory.exists());
    until(&mut gui, GuiController::diagnostic_read_available);
    let current = capture(&mut gui);
    assert!(!gui.diagnostic_preview_reviewed());
    reviewed_export(&mut gui, &directory, &current);
    until(&mut gui, |gui| {
        artifact(gui, &directory, DiagnosticArtifactOperation::Export)
            .is_some_and(|(_, status, _)| status == Presentation::Applied)
    });
    assert_eq!(
        std::fs::read(directory.join("diagnostics.json")).unwrap(),
        current.report.bytes.as_bytes()
    );
    safe_idle(&before, &owner.state());
    close(&mut gui, &mut owner);
}
