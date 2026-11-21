// ABOUTME: Exercises correction captures and explicit imported-fact review against a real owner.
// ABOUTME: Keeps local worker retirement, immutable save facts, and activation authority separate.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::*;
use earph_core::{
    audio::ChannelLayout, control::Command, profile_control::ProfileCatalogReference,
};
use earph_desktop::{
    Presentation,
    gui::{CorrectionPreviewRequest, CorrectionSource, GuiController, UiIntent},
};
use earph_media::correction_text::{
    CorrectionFormat, ImportContext, ImportMetadata, read_correction_report,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn until(gui: &mut GuiController, test: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.tick(Instant::now());
        if test(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "correction workflow stalled: {:?}",
            gui.error()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn attached(owner: &Owner) -> GuiController {
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui
}
fn request() -> CorrectionPreviewRequest {
    CorrectionPreviewRequest {
        source: CorrectionSource::Text {
            text: "Preamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n".into(),
            format: CorrectionFormat::AutoEq,
        },
        context: ImportContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            quantum_frames: 256,
        },
        metadata: ImportMetadata {
            headphone_model: Some("Owned public fixture".into()),
            ..Default::default()
        },
        fir_preamp: None,
    }
}
fn preview(gui: &mut GuiController) {
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: request(),
    });
    until(gui, |g| {
        g.correction_preview().is_some() && !g.queue_edit_pending()
    });
}
#[test]
fn actual_text_import_preserves_report_and_requires_exact_owner_record_review() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    preview(&mut gui);
    let expected = gui.correction_preview().unwrap().clone();
    let Command::ProfilesCreate { id, draft, .. } = profile_payload() else {
        panic!()
    };
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesImport {
            id,
            expected_profile_revision: None,
            draft,
            input: Arc::new(expected.input()),
        },
    });
    assert!(
        gui.queue_edit_pending(),
        "import refused: {:?}",
        gui.error()
    );
    until(&mut gui, |g| g.profile_record().is_some() && !g.busy());
    let record = gui.profile_record().unwrap();
    let report = read_correction_report(record.corrections[0].document_json.as_bytes()).unwrap();
    assert_eq!(report.source.sha256, expected.source_sha256());
    assert_eq!(
        report.metadata.headphone_model.as_deref(),
        Some("Owned public fixture")
    );
    assert_eq!(report.preamp_db, -6.0);
    let reference = ProfileCatalogReference {
        id: record.profile.id.clone(),
        revision: record.profile.revision,
        profile_sha256: record.profile_sha256,
    };
    assert!(!gui.correction_reviewed(&reference));
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesPrepare {
            id: reference.id.clone(),
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
        },
    });
    assert!(gui.error().unwrap().contains("Review"));
    gui.handle(UiIntent::ReviewCorrection {
        owner: gui.state().unwrap().owner_id.clone().unwrap(),
        reference: reference.clone(),
    });
    assert!(gui.correction_reviewed(&reference));
    gui.handle(UiIntent::InvalidateCorrection);
    assert!(!gui.correction_reviewed(&reference));
    assert!(
        gui.receipts()
            .any(|r| r.presentation == Presentation::Applied
                && matches!(r.request.envelope().payload, Command::ProfilesImport { .. }))
    );
    assert!(owner.state().muted);
}
#[test]
fn local_preview_uses_one_foreground_slot_and_close_waits_for_actual_retirement() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: request(),
    });
    assert!(gui.queue_edit_pending());
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: profile_payload(),
    });
    assert!(gui.error().is_some());
    gui.handle(UiIntent::CloseController);
    until(&mut gui, GuiController::ready_to_close);
    assert!(gui.correction_preview().is_none());
    assert!(!gui.local_correction_pending());
    assert!(gui.local_correction_status().contains("retired"));
    assert!(owner.state().muted);
}

fn import(gui: &mut GuiController, id: &str, revision: Option<u64>) {
    let Command::ProfilesCreate { draft, .. } = profile_payload() else {
        panic!()
    };
    let input = gui.correction_preview().unwrap().input();
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesImport {
            id: earph_core::ids::ProfileId::new(id).unwrap(),
            expected_profile_revision: revision,
            draft,
            input: Arc::new(input),
        },
    });
    assert!(
        gui.queue_edit_pending(),
        "correction import refused: {:?}",
        gui.error()
    );
}
fn owned_wav(sample: f32) -> Vec<u8> {
    let data: Vec<u8> = [sample, -sample]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend(2u16.to_le_bytes());
    bytes.extend(48000u32.to_le_bytes());
    bytes.extend(384000u32.to_le_bytes());
    bytes.extend(8u16.to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}
#[test]
fn fir_advisory_preview_never_claims_path_binding_and_actual_owner_hashes_use_wav_bytes() {
    use earph_core::{dsp_nodes::FirPurpose, graph::FiniteGraphScalar};
    let owner = Owner::start();
    let mut gui = attached(&owner);
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/desktop-correction-workflows/fixtures")
        .join(unique());
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("owned-advisory.wav");
    let original = owned_wav(0.5);
    let actual = owned_wav(0.25);
    std::fs::write(&path, &original).unwrap();
    let mut r = request();
    r.source = CorrectionSource::Fir {
        path: path.to_str().unwrap().into(),
        purpose: FirPurpose::Correction,
    };
    r.fir_preamp = Some(FiniteGraphScalar::new(-6.0).unwrap());
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: r.clone(),
    });
    until(&mut gui, |g| {
        g.correction_preview().is_some() && !g.queue_edit_pending()
    });
    let advisory_hash = gui.correction_preview().unwrap().source_sha256().to_owned();
    // This is our generated fixture. It demonstrates the public path API's deliberate reread.
    std::fs::write(&path, &actual).unwrap();
    import(&mut gui, "advisory-fir", None);
    until(&mut gui, |g| g.profile_record().is_some() && !g.busy());
    let record = gui.profile_record().unwrap();
    let manifest =
        earph_media::read_fir_manifest(record.corrections[0].document_json.as_bytes()).unwrap();
    assert_ne!(manifest.asset.sha256.as_str(), advisory_hash);
    let decoded = earph_media::decode_fir_wav(&actual).unwrap();
    assert_eq!(manifest.asset, decoded.reference);
    let reference = ProfileCatalogReference {
        id: record.profile.id.clone(),
        revision: record.profile.revision,
        profile_sha256: record.profile_sha256,
    };
    assert!(!gui.correction_reviewed(&reference));
    assert_eq!(manifest.preamp_db.get(), -6.0);
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: r,
    });
    until(&mut gui, |g| {
        g.correction_preview_current() && !g.queue_edit_pending()
    });
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::FirImport {
            path: path.to_str().unwrap().into(),
            purpose: FirPurpose::Correction,
        },
    });
    until(&mut gui, |g| g.fir_asset().is_some() && !g.busy());
    assert_eq!(gui.fir_asset(), Some(&decoded.reference));
    assert!(!gui.fir_asset_reviewed());
    gui.handle(UiIntent::ReviewFirAsset {
        owner: gui.state().unwrap().owner_id.clone().unwrap(),
        asset: decoded.reference,
    });
    assert!(gui.fir_asset_reviewed());
    gui.handle(UiIntent::InvalidateCorrection);
    assert!(!gui.fir_asset_reviewed());
    assert_eq!(std::fs::read(path).unwrap(), actual);
    assert!(owner.state().muted);
    let expected_hash = gui.fir_asset().unwrap().sha256.as_str().to_owned();
    let mut app = earph_desktop::gui::GuiApp::new(gui);
    let context = eframe::egui::Context::default();
    context.enable_accesskit();
    let mut render = |events| {
        let mut output = context.run_ui(
            eframe::egui::RawInput {
                events,
                ..Default::default()
            },
            |ui| app.draw(ui),
        );
        output.textures_delta.clear();
        output.platform_output.accesskit_update.unwrap()
    };
    let tree = render(vec![]);
    let target = tree
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() != eframe::egui::accesskit::Role::TextRun
                && node.label().or_else(|| node.value()) == Some("Profiles / DSP")
        })
        .unwrap()
        .0;
    let tree = render(vec![eframe::egui::Event::AccessKitActionRequest(
        eframe::egui::accesskit::ActionRequest {
            action: eframe::egui::accesskit::Action::Click,
            target_tree: tree.tree_id,
            target_node: target,
            data: None,
        },
    )]);
    let displayed = tree
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().or_else(|| node.value()))
        .find(|text| text.starts_with("Actual owner-verified FIR asset:"))
        .unwrap();
    assert!(
        displayed.contains(&expected_hash),
        "verified FIR hash is not readable: {displayed}"
    );
}
#[test]
fn correction_report_reconstruction_imports_an_explicit_new_revision_and_provenance() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    let mut r = request();
    let CorrectionSource::Text { text, .. } = &mut r.source else {
        panic!()
    };
    text.insert_str(0, "# owned historical comment\n");
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: r,
    });
    until(&mut gui, |g| {
        g.correction_preview_current() && !g.queue_edit_pending()
    });
    import(&mut gui, "roundtrip-correction", None);
    until(&mut gui, |g| g.profile_record().is_some() && !g.busy());
    let original = gui.profile_record().unwrap().clone();
    let original_report =
        read_correction_report(original.corrections[0].document_json.as_bytes()).unwrap();
    let reconstructed = CorrectionPreviewRequest::from_text_report(
        original.corrections[0].document_json.as_bytes(),
    )
    .unwrap();
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: reconstructed,
    });
    until(&mut gui, |g| {
        g.correction_preview_current() && !g.queue_edit_pending()
    });
    assert_ne!(
        gui.correction_preview().unwrap().source_sha256(),
        original_report.source.sha256
    );
    import(&mut gui, "roundtrip-correction", Some(1));
    until(&mut gui, |g| {
        g.profile_record().is_some_and(|r| r.profile.revision == 2) && !g.busy()
    });
    let updated = gui.profile_record().unwrap();
    let updated_report =
        read_correction_report(updated.corrections[0].document_json.as_bytes()).unwrap();
    assert_eq!(updated.graph, original.graph);
    assert_eq!(updated_report.metadata, original_report.metadata);
    assert_eq!(updated_report.preamp_db, original_report.preamp_db);
    assert_ne!(updated_report.source.sha256, original_report.source.sha256);
    assert_ne!(updated.profile_sha256, original.profile_sha256);
    assert_eq!(updated.profile.revision, 2);
}
#[test]
fn uncertain_correction_import_replays_exact_bytes_and_requires_explicit_get() {
    let owner = Owner::start();
    let transport = proxy::Proxy::drop_first_correction_import(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    preview(&mut gui);
    import(&mut gui, "uncertain-correction", None);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::ProfilesImport { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let saved = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::ProfilesImport { .. }))
        .unwrap();
    let id = saved.request.envelope().request_id.clone();
    let bytes = saved.request.bytes().to_vec();
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        })
    });
    assert!(gui.profile_record().is_none());
    let frames = transport.frames.lock().unwrap().clone();
    let imports: Vec<_> = frames
        .iter()
        .filter(|b| {
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(b)
                .is_ok_and(|e| matches!(e.payload, Command::ProfilesImport { .. }))
        })
        .collect();
    assert!(imports.len() >= 2);
    assert!(imports.iter().all(|b| b.as_slice() == bytes));
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesGet {
            id: earph_core::ids::ProfileId::new("uncertain-correction").unwrap(),
        },
    });
    until(&mut gui, |g| g.profile_record().is_some() && !g.busy());
    assert_eq!(gui.profile_record().unwrap().profile.revision, 1);
    assert!(owner.state().muted);
}

#[test]
fn source_edits_cancel_the_exact_unsent_import_and_safety_cancels_a_waiting_local_preview() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    preview(&mut gui);
    transport.hold_next_state();
    import(&mut gui, "canceled-correction", None);
    until(&mut gui, |_| transport.state_held());
    gui.handle(UiIntent::InvalidateCorrection);
    transport.release_state();
    until(&mut gui, |g| !g.busy());
    assert!(gui.profile_record().is_none());
    assert!(!transport.frames.lock().unwrap().iter().any(|b| {
        serde_json::from_slice::<earph_core::control::CommandEnvelope>(b)
            .is_ok_and(|e| matches!(e.payload, Command::ProfilesImport { .. }))
    }));
    transport.hold_next_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |_| transport.state_held());
    gui.handle(UiIntent::PreviewCorrection {
        basis: Arc::new(gui.state().unwrap().clone()),
        request: request(),
    });
    assert!(gui.local_correction_pending());
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Applied
        })
    });
    assert!(transport.state_held());
    assert!(!gui.local_correction_pending());
    transport.release_state();
    until(&mut gui, |g| !g.busy());
    assert!(gui.correction_preview().is_none());
    assert!(
        gui.local_correction_status()
            .contains("before worker start")
    );
    assert!(owner.state().muted);
}
#[test]
fn changed_text_report_or_owner_context_cannot_reuse_the_captured_preview() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    preview(&mut gui);
    let mut changed = gui.correction_preview().unwrap().input();
    let earph_core::profile_control::ProfileCorrectionInput::TextReport { source_text, .. } =
        &mut changed
    else {
        panic!()
    };
    source_text.push('\n');
    let Command::ProfilesCreate { id, draft, .. } = profile_payload() else {
        panic!()
    };
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesImport {
            id,
            expected_profile_revision: None,
            draft,
            input: Arc::new(changed),
        },
    });
    assert!(gui.error().unwrap().contains("exact previewed input"));
    assert!(!gui.queue_edit_pending());
    let old = gui.state().unwrap().clone();
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|state| state.revision != old.revision)
    });
    assert!(!gui.correction_preview_current());
    assert!(gui.profile_record().is_none());
    assert!(owner.state().muted);
}
