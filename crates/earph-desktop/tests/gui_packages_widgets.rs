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
fn packages_require_explicit_content_privacy_and_redistribution_reviews() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(GuiController::new());
    let tree = draw(&context, &mut app, vec![]);
    let event = click(&tree, named(&tree, "Profiles / DSP"));
    let tree = draw(&context, &mut app, vec![event]);
    for label in [
        "Package directory",
        "Package destination",
        "Inspect package directory",
        "Preview selected profile package",
        "Read exact object bytes",
        "Acknowledge exact candidate",
        "Acknowledge no private data",
        "Refuse measurement bytes",
        "Reviewed required measurements",
        "Import inspected package",
        "Export reviewed package",
        "Bind selected profile to opened device",
    ] {
        named(&tree, label);
    }
    for label in [
        "Inspect package directory",
        "Preview selected profile package",
        "Read exact object bytes",
        "Import inspected package",
        "Export reviewed package",
        "Bind selected profile to opened device",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .find(|(n, _)| *n == id)
                .unwrap()
                .1
                .is_disabled(),
            "disconnected operation enabled: {label}"
        );
    }
}
