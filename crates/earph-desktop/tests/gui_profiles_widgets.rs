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
fn profile_editor_exposes_labeled_draft_and_separate_owner_actions_without_attachment() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(GuiController::new());
    let tree = draw(&context, &mut app, vec![]);
    let event = click(&tree, named(&tree, "Profiles / DSP"));
    let tree = draw(&context, &mut app, vec![event]);
    for label in [
        "Profile ID",
        "Profile name",
        "Profile configured device ID",
        "Manual listening setup",
        "Profile software cap (millidecibels)",
        "Profile hardware cap (millidecibels, optional)",
        "Profile resident byte limit",
        "Profile preparation byte limit",
        "Profile latency frame limit (optional)",
        "GraphIntent JSON",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.labelled_by().contains(&id)),
            "unlabeled input {label}"
        );
    }
    for action in [
        "Refresh profiles",
        "Load selected profile into draft",
        "Create profile",
        "Update selected profile",
        "Resolve profiles",
        "Inspect graph",
        "Validate against current source",
        "Prepare graph",
        "Commit prepared graph",
        "Prepare selected profile",
        "Apply prepared profile",
    ] {
        let id = named(&tree, action);
        assert!(
            tree.nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled(),
            "disconnected owner action enabled: {action}"
        );
    }
    for label in [
        "Correction import: unavailable in this desktop unit",
        "Packages and associations: unavailable in this desktop unit",
        "Accept raw edits",
        "Reconciled preparations remain receipt history. Use a fresh explicit Prepare before activation. The owner may retain its earlier candidate until expiry or a context change.",
    ] {
        named(&tree, label);
    }
}
