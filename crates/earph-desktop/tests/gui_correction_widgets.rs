// ABOUTME: Exercises profile and DSP forms through actual renderer accessibility actions.
// ABOUTME: Keeps these pure draft checks separate from real-owner and native qualification.
use earph_desktop::gui::{GuiApp, GuiController};
use eframe::egui;

fn draw(
    context: &egui::Context,
    app: &mut GuiApp,
    events: Vec<egui::Event>,
) -> egui::accesskit::TreeUpdate {
    let mut output = context.run_ui(
        egui::RawInput {
            events,
            ..Default::default()
        },
        |ui| app.draw(ui),
    );
    output.textures_delta.clear();
    output.platform_output.accesskit_update.unwrap()
}
fn named(tree: &egui::accesskit::TreeUpdate, label: &str) -> egui::accesskit::NodeId {
    tree.nodes
        .iter()
        .find(|(_, n)| {
            n.role() != egui::accesskit::Role::TextRun
                && n.label().or_else(|| n.value()) == Some(label)
        })
        .unwrap_or_else(|| panic!("missing widget {label}"))
        .0
}
fn click(tree: &egui::accesskit::TreeUpdate, target: egui::accesskit::NodeId) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Click,
        target_tree: tree.tree_id,
        target_node: target,
        data: None,
    })
}

#[test]
fn correction_form_exposes_explicit_preview_import_and_actual_review() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(GuiController::new());
    let tree = draw(&context, &mut app, vec![]);
    let event = click(&tree, named(&tree, "Profiles / DSP"));
    let tree = draw(&context, &mut app, vec![event]);
    for label in [
        "Correction source path",
        "Correction source text",
        "Correction sample rate (Hz)",
        "Correction quantum (frames)",
        "FIR preamp (dB, required)",
        "Headphone model",
        "Measurement provider",
        "Target",
        "Parameter set",
        "Metadata revision",
        "Redistribution license",
        "Preview correction source",
        "Import correction as new profile",
        "Import correction into loaded revision",
        "Verify FIR asset",
        "Review actual imported correction",
        "Add reviewed FIR asset to graph",
    ] {
        named(&tree, label);
    }
    for action in [
        "Import correction as new profile",
        "Import correction into loaded revision",
        "Verify FIR asset",
        "Review actual imported correction",
        "Add reviewed FIR asset to graph",
    ] {
        let id = named(&tree, action);
        assert!(
            tree.nodes
                .iter()
                .find(|(n, _)| *n == id)
                .unwrap()
                .1
                .is_disabled(),
            "unreviewed action enabled: {action}"
        );
    }
}
