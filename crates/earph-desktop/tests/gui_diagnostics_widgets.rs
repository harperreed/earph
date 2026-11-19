// ABOUTME: Exercises diagnostic controls through actual native renderer accessibility actions.
// ABOUTME: Keeps offline draft presentation separate from owner and filesystem qualification.
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
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1800.0, 10000.0),
            )),
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
        .find(|(_, node)| {
            node.role() != egui::accesskit::Role::TextRun
                && node.label().or_else(|| node.value()) == Some(label)
        })
        .unwrap_or_else(|| panic!("missing diagnostic control {label}"))
        .0
}

#[test]
fn diagnostic_forms_require_an_owner_and_keep_review_export_and_inspection_separate() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(GuiController::new());
    let tree = draw(&context, &mut app, vec![]);
    let action = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Click,
        target_tree: tree.tree_id,
        target_node: named(&tree, "Signal Path / Recovery"),
        data: None,
    });
    let tree = draw(&context, &mut app, vec![action]);
    for label in [
        "Diagnostic destination directory",
        "Diagnostic inspection SHA-256",
        "Event cursor (after sequence)",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.labelled_by().contains(&id)),
            "unlabeled diagnostic input {label}"
        );
    }
    for label in [
        "Read diagnostic signal path",
        "Read diagnostic metrics",
        "Read event recovery",
        "Read event page",
        "Capture redacted diagnostic preview",
        "Mark diagnostic preview reviewed",
        "Export reviewed diagnostic preview",
        "Inspect diagnostic artifact",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled(),
            "disconnected diagnostic action enabled: {label}"
        );
    }
    named(
        &tree,
        "Only the exact redacted payload is shareable. Owner identities and raw receipts stay local.",
    );
    named(
        &tree,
        "Cancellation acknowledgment does not prove worker retirement or absence of filesystem effects.",
    );
    named(&tree, "No owner snapshot has been observed.");
}
