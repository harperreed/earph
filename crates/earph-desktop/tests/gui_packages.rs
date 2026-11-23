// ABOUTME: Exercises exact package review and publication through actual owned owner instances.
// ABOUTME: Preserves generated package bytes, uncertainty identities, and explicit filesystem issues.
mod common;
use common::*;
use earph_core::{control::Command, profile_control::ProfileCatalogReference};
use earph_desktop::{
    PackageFilesystemStateView, Presentation, ProfilePackageReasonView, ProfilePackageStageView,
    Reply, ResultView,
    gui::{GuiController, UiIntent},
};
use std::{
    os::unix::fs::PermissionsExt,
    sync::Arc,
    time::{Duration, Instant},
};
#[track_caller]
fn until(gui: &mut GuiController, test: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        gui.tick(Instant::now());
        if test(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "package workflow stalled: error {:?}; state {:?}; receipts {:?}",
            gui.error(),
            gui.state().map(|state| (
                &state.owner_id,
                state.revision,
                state.profile_worker_busy,
                &state.session_id
            )),
            gui.receipts()
                .map(|record| (
                    record.request.envelope().request_id.clone(),
                    record.presentation,
                    record.in_flight,
                    record.owner_changed
                ))
                .collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn attached(runtime: &std::path::Path) -> GuiController {
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui
}
fn action(gui: &mut GuiController, command: Command) {
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(
        gui.queue_edit_pending(),
        "package action refused: {:?}",
        gui.error()
    );
}
fn root() -> std::path::PathBuf {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/desktop-packages-workflows/fixtures")
        .join(unique());
    std::fs::create_dir_all(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.canonicalize().unwrap()
}
fn saved(gui: &mut GuiController) -> ProfileCatalogReference {
    action(gui, profile_payload());
    until(gui, |g| g.profile_record().is_some() && !g.busy());
    let record = gui.profile_record().unwrap();
    ProfileCatalogReference {
        id: record.profile.id.clone(),
        revision: record.profile.revision,
        profile_sha256: record.profile_sha256,
    }
}
fn review(gui: &mut GuiController, source: &str, destination: &str) {
    gui.handle(UiIntent::PackageInputs {
        directory: source.into(),
        destination: destination.into(),
    });
    let count = gui.package_candidate().unwrap().review_sources.len();
    assert!(count > 0);
    for index in 0..count {
        gui.handle(UiIntent::ReadPackageObject { index });
        assert!(gui.queue_edit_pending(), "read refused: {:?}", gui.error());
        until(gui, |g| !g.busy());
        assert!(gui.error().is_none(), "read failed: {:?}", gui.error());
        gui.handle(UiIntent::MarkPackageObjectReviewed);
        assert!(gui.error().is_none(), "review failed: {:?}", gui.error());
    }
    gui.handle(UiIntent::PackageAcknowledgments {
        exact: true,
        private: true,
        measurements: false,
    });
    assert!(gui.error().is_none());
}
fn preview(gui: &mut GuiController, reference: &ProfileCatalogReference) {
    let before = gui.state().unwrap().revision;
    action(
        gui,
        Command::ProfilesPackagePreview {
            profile: reference.clone(),
        },
    );
    until(gui, |g| {
        g.package_candidate()
            .is_some_and(|p| p.operation == ProfilePackageStageView::Previewed)
            && !g.busy()
    });
    assert!(gui.state().unwrap().revision > before);
    assert_eq!(
        &gui.package_candidate().unwrap().manifest.profile,
        reference
    );
    assert!(!gui.package_export_available());
}
fn export(gui: &mut GuiController, destination: &std::path::Path) {
    review(gui, "", destination.to_str().unwrap());
    assert!(gui.package_export_available());
    gui.handle(UiIntent::ExportReviewedPackage);
    assert!(gui.queue_edit_pending());
    until(gui, |g| {
        g.package_candidate()
            .is_some_and(|p| p.operation == ProfilePackageStageView::Exported)
            && !g.busy()
    });
}
#[test]
fn actual_package_preview_review_export_inspect_import_preserve_original_bytes_and_saved_intent() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let reference = saved(&mut gui);
    let original = gui.profile_record().unwrap().clone();
    let initial = owner.state();
    preview(&mut gui, &reference);
    assert_eq!(gui.profile_record(), Some(&original));
    let directory = root().join("exported");
    export(&mut gui, &directory);
    let produced = gui.package_candidate().unwrap().clone();
    let manifest_bytes = std::fs::read(directory.join("manifest.json")).unwrap();
    let manifest = earph_core::profile_packages::read_package_manifest(&manifest_bytes).unwrap();
    assert_eq!(manifest.profile, reference);
    let bytes: Vec<_> = produced
        .manifest
        .objects
        .iter()
        .map(|object| {
            (
                object.clone(),
                std::fs::read(directory.join(object.file_name())).unwrap(),
            )
        })
        .collect();
    let imported_owner = Owner::start();
    let mut imported_gui = attached(&imported_owner.runtime);
    action(
        &mut imported_gui,
        Command::ProfilesPackageInspect {
            directory: directory.to_str().unwrap().into(),
        },
    );
    until(&mut imported_gui, |g| {
        g.package_candidate().is_some() && !g.busy()
    });
    assert_eq!(
        imported_gui.package_candidate().unwrap().manifest_sha256,
        produced.manifest_sha256
    );
    review(&mut imported_gui, directory.to_str().unwrap(), "");
    assert!(imported_gui.package_import_available(directory.to_str().unwrap()));
    action(
        &mut imported_gui,
        Command::ProfilesImportPackage {
            directory: directory.to_str().unwrap().into(),
            expected_manifest_sha256: produced.manifest_sha256,
            profile: reference.clone(),
        },
    );
    until(&mut imported_gui, |g| {
        g.profile_record().is_some() && !g.busy()
    });
    let actual = imported_gui.profile_record().unwrap();
    assert_eq!(actual.profile_sha256, original.profile_sha256);
    assert_eq!(actual.graph, original.graph);
    assert_eq!(actual.profile.volume, original.profile.volume);
    for (object, expected) in bytes {
        assert_eq!(
            std::fs::read(directory.join(object.file_name())).unwrap(),
            expected
        );
    }
    assert_eq!(
        std::fs::read(directory.join("manifest.json")).unwrap(),
        manifest_bytes
    );
    assert!(owner.state().muted && imported_owner.state().muted);
    assert_eq!(owner.state().software_gain_db, initial.software_gain_db);
    assert_eq!(owner.state().output, initial.output);
    // The occupied ID must fail without a rename or a second saved revision.
    action(
        &mut imported_gui,
        Command::ProfilesImportPackage {
            directory: directory.to_str().unwrap().into(),
            expected_manifest_sha256: produced.manifest_sha256,
            profile: reference,
        },
    );
    until(&mut imported_gui, |g| !g.busy());
    assert!(
        imported_gui
            .receipts()
            .any(|r| r.presentation == Presentation::Rejected
                && matches!(
                    r.request.envelope().payload,
                    Command::ProfilesImportPackage { .. }
                ))
    );
    assert!(imported_gui.receipts().any(|record| {
        record.presentation == Presentation::Rejected
            && matches!(
                record.request.envelope().payload,
                Command::ProfilesImportPackage { .. }
            )
            && matches!(record.outcome, Some(Ok(Reply::Receipt(receipt)))
                if matches!(&receipt.result, Some(ResultView::ProfilePackageIssue(issue))
                    if issue.reason == ProfilePackageReasonView::IdentityCollision
                        && issue.object.is_none()
                        && issue.filesystem_state == PackageFilesystemStateView::Unchanged))
    }));
    assert_eq!(imported_gui.profile_record().unwrap().profile.revision, 1);
}
#[test]
fn destination_change_clears_reviews_and_occupied_export_is_an_actual_owner_issue() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let reference = saved(&mut gui);
    preview(&mut gui, &reference);
    let parent = root();
    let first = parent.join("first");
    review(&mut gui, "", first.to_str().unwrap());
    assert!(gui.package_export_available());
    let second = parent.join("occupied");
    std::fs::create_dir(&second).unwrap();
    std::fs::write(second.join("owned-sentinel"), b"keep").unwrap();
    gui.handle(UiIntent::PackageInputs {
        directory: "".into(),
        destination: second.to_str().unwrap().into(),
    });
    assert!(!gui.package_export_available());
    review(&mut gui, "", second.to_str().unwrap());
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| !g.busy());
    assert!(gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::ProfilesExport { .. }
    ) && r.presentation == Presentation::Failed));
    assert_eq!(
        std::fs::read(second.join("owned-sentinel")).unwrap(),
        b"keep"
    );
    assert!(!second.join("manifest.json").exists());
    assert!(!gui.package_export_available());
    assert!(owner.state().muted);
}
#[test]
fn uncertain_export_reconciles_exact_envelope_without_another_destination_or_automatic_import() {
    let owner = Owner::start();
    let transport = proxy::Proxy::package_export_recovery(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    let reference = saved(&mut gui);
    preview(&mut gui, &reference);
    let directory = root().join("exact-export");
    review(&mut gui, "", directory.to_str().unwrap());
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::ProfilesExport { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let record = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::ProfilesExport { .. }))
        .unwrap();
    let id = record.request.envelope().request_id.clone();
    let original = record.request.bytes().to_vec();
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesPackageInspect {
            directory: directory.to_str().unwrap().into(),
        },
    });
    assert!(gui.error().is_some());
    assert!(!gui.receipts().any(|receipt| matches!(
        receipt.request.envelope().payload,
        Command::ProfilesPackageInspect { .. }
    )));
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|record| {
            record.request.envelope().request_id == id
                && record.presentation == Presentation::Accepted
        })
    });
    assert!(gui.package_candidate().is_none());
    until(&mut gui, |_| transport.terminal_held());
    transport.release();
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        })
    });
    assert!(gui.package_candidate().is_none());
    until(&mut gui, |g| {
        !g.busy() && !g.state().unwrap().profile_worker_busy
    });
    action(
        &mut gui,
        Command::ProfilesPackageInspect {
            directory: directory.to_str().unwrap().into(),
        },
    );
    until(&mut gui, |g| {
        g.package_candidate()
            .is_some_and(|candidate| candidate.operation == ProfilePackageStageView::Inspected)
            && !g.busy()
    });
    assert_eq!(gui.package_candidate().unwrap().manifest.profile, reference);
    let frames = transport.frames.lock().unwrap();
    let exports: Vec<_> = frames
        .iter()
        .filter(|bytes| {
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                .is_ok_and(|e| matches!(e.payload, Command::ProfilesExport { .. }))
        })
        .collect();
    assert!(exports.len() >= 2);
    assert!(exports.iter().all(|bytes| bytes.as_slice() == original));
    assert!(!frames.iter().any(|bytes| {
        serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
            .is_ok_and(|e| matches!(e.payload, Command::ProfilesImportPackage { .. }))
    }));
    assert!(directory.join("manifest.json").is_file());
    assert!(owner.state().muted);
}

#[test]
fn actual_measurement_export_requires_explicit_redistribution_and_preserves_original_source() {
    use earph_desktop::gui::{CorrectionPreviewRequest, CorrectionSource};
    use earph_media::correction_text::{CorrectionFormat, ImportContext, ImportMetadata};
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let source = "# Owned public correction fixture\nPreamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n";
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: CorrectionPreviewRequest {
            source: CorrectionSource::Text {
                text: source.into(),
                format: CorrectionFormat::AutoEq,
            },
            context: ImportContext {
                sample_rate_hz: 48000,
                channels: earph_core::audio::ChannelLayout::Stereo,
                quantum_frames: 256,
            },
            metadata: ImportMetadata::default(),
            fir_preamp: None,
        },
    });
    until(&mut gui, |g| g.correction_preview_current() && !g.busy());
    let Command::ProfilesCreate { id, draft, .. } = profile_payload() else {
        panic!()
    };
    let input = gui.correction_preview().unwrap().input();
    action(
        &mut gui,
        Command::ProfilesImport {
            id,
            expected_profile_revision: None,
            draft,
            input: Arc::new(input),
        },
    );
    until(&mut gui, |g| g.profile_record().is_some() && !g.busy());
    let record = gui.profile_record().unwrap();
    let reference = ProfileCatalogReference {
        id: record.profile.id.clone(),
        revision: record.profile.revision,
        profile_sha256: record.profile_sha256,
    };
    preview(&mut gui, &reference);
    assert!(gui.package_candidate().unwrap().measurement_review_required);
    let destination = root().join("measurement-package");
    review(&mut gui, "", destination.to_str().unwrap());
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| !g.busy());
    assert!(!destination.exists());
    assert!(!gui.package_export_available());
    assert!(gui.receipts().any(|r| {
        let Some(Ok(earph_desktop::Reply::Receipt(receipt))) = r.outcome else { return false; };
        matches!(&receipt.result, Some(earph_desktop::ResultView::ProfilePackageIssue(issue)) if issue.reason == earph_desktop::ProfilePackageReasonView::RedistributionReviewRequired && issue.filesystem_state == earph_desktop::PackageFilesystemStateView::InspectRequired)
    }));
    action(
        &mut gui,
        Command::ProfilesPackageInspect {
            directory: destination.to_str().unwrap().into(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && !g.state().unwrap().profile_worker_busy
    });
    assert!(
        gui.unresolved_package_write()
            .is_some_and(|(_, path, _)| path == destination.to_str().unwrap())
    );
    // This separate explicit destination choice preserves the earlier refusal and inspection facts.
    let approved_destination = destination
        .parent()
        .unwrap()
        .join("explicit-approved-measurements");
    review(&mut gui, "", approved_destination.to_str().unwrap());
    gui.handle(UiIntent::PackageAcknowledgments {
        exact: true,
        private: true,
        measurements: true,
    });
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| {
        g.package_candidate()
            .is_some_and(|c| c.operation == ProfilePackageStageView::Exported)
            && !g.busy()
    });
    let text = gui
        .package_candidate()
        .unwrap()
        .manifest
        .objects
        .iter()
        .find(|o| o.kind == earph_core::profile_packages::PackageObjectKind::TextSource)
        .unwrap();
    assert_eq!(
        std::fs::read(approved_destination.join(text.file_name())).unwrap(),
        source.as_bytes()
    );
    assert!(owner.state().muted);
}

#[test]
fn exact_binding_stays_unavailable_without_an_advertised_opened_identity_and_preserves_manual_resolution()
 {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let reference = saved(&mut gui);
    assert!(
        !gui.capabilities()
            .unwrap()
            .profiles
            .exact_association_backends
            .contains(&earph_core::control::OutputBackend::Alsa)
    );
    assert!(!gui.association_available());
    let count = gui
        .receipts()
        .filter(|r| matches!(r.request.envelope().payload, Command::ProfilesBind { .. }))
        .count();
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesBind {
            id: reference.id.clone(),
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
            expected_device_epoch: gui.state().unwrap().device_epoch,
        },
    });
    assert!(gui.error().is_some());
    assert_eq!(
        count,
        gui.receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::ProfilesBind { .. }))
            .count()
    );
    let Command::ProfilesCreate { draft, .. } = profile_payload() else {
        panic!()
    };
    action(
        &mut gui,
        Command::ProfilesResolve {
            manual_profile_id: Some(reference.id.clone()),
            output: draft.output.clone(),
        },
    );
    until(&mut gui, |g| g.profile_resolution().is_some() && !g.busy());
    assert!(
        matches!(gui.profile_resolution(), Some(earph_desktop::ProfileResolutionView::Selected { profile, reason }) if profile.id == reference.id && *reason == earph_desktop::ProfileSelectionReasonView::Manual)
    );
    assert!(owner.state().muted);
    assert!(owner.state().output.is_none());
}

#[test]
fn changed_generated_package_bytes_invalidate_review_before_any_owner_import_is_sent() {
    let producer = Owner::start();
    let mut producer_gui = attached(&producer.runtime);
    let reference = saved(&mut producer_gui);
    preview(&mut producer_gui, &reference);
    let parent = root();
    let directory = parent.join("owned-to-change");
    export(&mut producer_gui, &directory);
    let consumer = Owner::start();
    let mut gui = attached(&consumer.runtime);
    action(
        &mut gui,
        Command::ProfilesPackageInspect {
            directory: directory.to_str().unwrap().into(),
        },
    );
    until(&mut gui, |g| g.package_candidate().is_some() && !g.busy());
    let candidate = gui.package_candidate().unwrap().clone();
    review(&mut gui, directory.to_str().unwrap(), "");
    let object = candidate
        .manifest
        .objects
        .iter()
        .find(|object| object.kind == earph_core::profile_packages::PackageObjectKind::Graph)
        .unwrap();
    let path = directory.join(object.file_name());
    let original = std::fs::read(&path).unwrap();
    std::fs::write(parent.join("original-owned-graph-bytes"), &original).unwrap();
    // Alter only this generated exported fixture, preserving the original bytes beside it.
    std::fs::write(&path, b"changed").unwrap();
    action(
        &mut gui,
        Command::ProfilesImportPackage {
            directory: directory.to_str().unwrap().into(),
            expected_manifest_sha256: candidate.manifest_sha256,
            profile: candidate.manifest.profile,
        },
    );
    until(&mut gui, |g| !g.busy());
    assert!(
        gui.error()
            .is_some_and(|error| error.contains("bytes changed")),
        "actual error: {:?}",
        gui.error()
    );
    assert!(!gui.package_import_available(directory.to_str().unwrap()));
    assert!(!gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::ProfilesImportPackage { .. }
    )));
    assert_eq!(std::fs::read(path).unwrap(), b"changed");
    assert_eq!(
        std::fs::read(parent.join("original-owned-graph-bytes")).unwrap(),
        original
    );
    let earph_desktop::Reply::Receipt(receipt) = consumer
        .client()
        .request(&query(Command::ProfilesList {
            offset: 0,
            limit: 16,
        }))
        .unwrap()
    else {
        panic!()
    };
    assert!(
        matches!(receipt.result, Some(earph_desktop::ResultView::Profiles(page)) if page.entries.is_empty())
    );
    assert!(consumer.state().muted);
}

#[test]
fn historical_accepted_export_cannot_replay_against_a_replacement_owner() {
    let mut owner = Owner::start();
    let transport = proxy::Proxy::package_export_recovery(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    let reference = saved(&mut gui);
    preview(&mut gui, &reference);
    let destination = root().join("old-owner-export");
    review(&mut gui, "", destination.to_str().unwrap());
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::ProfilesExport { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let id = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::ProfilesExport { .. }))
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Accepted
        })
    });
    until(&mut gui, |_| transport.terminal_held());
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    let previous_owner = owner.state().owner_id;
    let runtime = owner.runtime.clone();
    owner.stop();
    let mut replacement = Owner::at(runtime);
    assert_ne!(replacement.state().owner_id, previous_owner);
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| {
        g.connected() && !g.busy() && g.state().unwrap().owner_id == replacement.state().owner_id
    });
    let before = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|bytes| {
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                .is_ok_and(|e| matches!(e.payload, Command::ProfilesExport { .. }))
        })
        .count();
    gui.handle(UiIntent::Reconcile(id.clone()));
    assert!(gui.error().is_some());
    assert!(
        gui.receipts()
            .any(|r| r.request.envelope().request_id == id && r.owner_changed)
    );
    for _ in 0..80 {
        gui.tick(Instant::now());
        std::thread::sleep(Duration::from_millis(2));
    }
    let after = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|bytes| {
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                .is_ok_and(|e| matches!(e.payload, Command::ProfilesExport { .. }))
        })
        .count();
    assert_eq!(before, after);
    assert!(gui.package_candidate().is_none());
    assert!(replacement.state().muted);
    transport.release();
    gui.handle(UiIntent::CloseController);
    until(&mut gui, GuiController::ready_to_close);
    replacement.stop();
}

fn held_export_receipt(transport: &proxy::Proxy) -> earph_desktop::ReceiptView {
    assert!(transport.terminal_held());
    let index = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, bytes)| {
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                .is_ok_and(|envelope| matches!(envelope.payload, Command::ProfilesExport { .. }))
                .then_some(index + 1)
        })
        .unwrap();
    let bytes = std::fs::read(transport.runtime.join(format!("reply-{index}.json"))).unwrap();
    let earph_desktop::Reply::Receipt(receipt) = earph_desktop::decode_reply(bytes).unwrap() else {
        panic!("actual held producer receipt")
    };
    receipt
}
#[test]
fn mute_and_idle_stop_settle_while_a_retired_export_result_is_still_held() {
    let owner = Owner::start();
    let transport = proxy::Proxy::package_export_recovery(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    let reference = saved(&mut gui);
    preview(&mut gui, &reference);
    let destination = root().join("safety-during-history");
    review(&mut gui, "", destination.to_str().unwrap());
    gui.handle(UiIntent::ExportReviewedPackage);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::ProfilesExport { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let id = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::ProfilesExport { .. }))
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Accepted
        })
    });
    until(&mut gui, |_| transport.terminal_held());
    let actual_terminal = held_export_receipt(&transport);
    assert_eq!(actual_terminal.request_id, id);
    assert_eq!(actual_terminal.status, earph_desktop::ReplyStatus::Applied);
    assert!(
        matches!(actual_terminal.result, Some(earph_desktop::ResultView::ProfilePackage(candidate)) if candidate.operation == ProfilePackageStageView::Exported && candidate.manifest.profile == reference)
    );
    until(&mut gui, |_| !owner.state().profile_worker_busy);
    assert!(owner.state().session_id.is_none());
    gui.handle(UiIntent::Mute);
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        [Command::Mute { muted: true }, Command::Stop]
            .iter()
            .all(|command| {
                g.receipts().any(|record| {
                    &record.request.envelope().payload == command
                        && !record.in_flight
                        && !matches!(
                            record.presentation,
                            Presentation::Accepted | Presentation::Queued
                        )
                })
            })
    });
    assert!(
        transport.terminal_held(),
        "both safety responses must arrive before the ordinary result is released"
    );
    assert!(gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Mute { muted: true }
    ) && r.presentation == Presentation::Applied));
    let stop = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::Stop))
        .unwrap();
    assert_eq!(stop.presentation, Presentation::Rejected);
    assert!(
        matches!(stop.outcome, Some(Ok(earph_desktop::Reply::Receipt(receipt))) if receipt.error.as_ref().is_some_and(|error| error.code == earph_core::ErrorCode::StaleSession))
    );
    transport.release();
    until(&mut gui, |g| {
        g.receipts().any(|record| {
            record.request.envelope().request_id == id
                && !record.in_flight
                && !matches!(
                    record.presentation,
                    Presentation::Accepted | Presentation::Queued
                )
        })
    });
    let export = gui
        .receipts()
        .find(|r| r.request.envelope().request_id == id)
        .unwrap();
    assert_eq!(
        export.presentation,
        Presentation::Applied,
        "actual terminal: {:?}",
        export.outcome
    );
    assert!(gui.package_candidate().is_none());
    assert!(owner.state().muted);
}
