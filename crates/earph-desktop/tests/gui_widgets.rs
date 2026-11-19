// ABOUTME: Checks the product widget tree and local themes without claiming native AX proof.
// ABOUTME: Keeps real-window and platform accessibility validation in the native harness.
use earph_desktop::gui::{GuiApp, GuiController};
use eframe::egui;
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::*;
use earph_desktop::gui::UiIntent;
use std::{
    thread,
    time::{Duration, Instant},
};

#[test]
fn disconnected_window_exposes_labeled_connect_actions_without_playback_controls() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(GuiController::new());
    let mut output = context.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
    output.textures_delta.clear();
    let tree = output.platform_output.accesskit_update.unwrap();
    let labels: Vec<_> = tree
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().or_else(|| node.value()))
        .collect();
    for label in [
        "Runtime directory",
        "Owner executable",
        "Attach",
        "Start local engine",
        "Close controller",
    ] {
        assert!(
            labels.contains(&label),
            "missing accessible label: {label}; {labels:?}"
        );
    }
    assert!(
        !labels
            .iter()
            .any(|label| *label == "Unmute" || *label == "Play")
    );
    assert!(!output.shapes.is_empty());
    for name in ["Runtime directory", "Owner executable"] {
        let label_id = tree
            .nodes
            .iter()
            .find(|(_, n)| n.label().or_else(|| n.value()) == Some(name))
            .unwrap()
            .0;
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.labelled_by().contains(&label_id))
        );
    }
}

fn draw_frame(
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
fn named(tree: &egui::accesskit::TreeUpdate, name: &str) -> egui::accesskit::NodeId {
    tree.nodes
        .iter()
        .find(|(_, node)| {
            node.role() != egui::accesskit::Role::TextRun
                && node.label().or_else(|| node.value()) == Some(name)
        })
        .unwrap_or_else(|| panic!("missing widget {name}"))
        .0
}
fn action(
    tree: &egui::accesskit::TreeUpdate,
    target: egui::accesskit::NodeId,
    action: egui::accesskit::Action,
    data: Option<egui::accesskit::ActionData>,
) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action,
        target_tree: tree.tree_id,
        target_node: target,
        data,
    })
}

#[test]
fn queue_widgets_accept_one_action_during_observation_and_show_local_waiting() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.connected() || controller.queue().is_none() || controller.busy() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    transport.hold_next_state();
    controller.handle(UiIntent::Refresh);
    while !transport.state_held() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(controller);
    let tree = draw_frame(&context, &mut app, vec![]);
    let click = action(
        &tree,
        named(&tree, "Queue"),
        egui::accesskit::Action::Click,
        None,
    );
    let tree = draw_frame(&context, &mut app, vec![click]);
    for name in ["Add file", "Add folder"] {
        let id = named(&tree, name);
        assert!(
            !tree
                .nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled(),
            "background observation disabled {name}"
        );
    }
    let label = named(&tree, "File path");
    let entry = tree
        .nodes
        .iter()
        .find(|(_, node)| node.labelled_by().contains(&label))
        .expect("File path label must name its entry")
        .0;
    let value = action(
        &tree,
        entry,
        egui::accesskit::Action::SetValue,
        Some(egui::accesskit::ActionData::Value(
            "/widget-waiting.wav".into(),
        )),
    );
    let tree = draw_frame(&context, &mut app, vec![value]);
    let click = action(
        &tree,
        named(&tree, "Add file"),
        egui::accesskit::Action::Click,
        None,
    );
    draw_frame(&context, &mut app, vec![click]);
    let tree = draw_frame(&context, &mut app, vec![]);
    assert!(
        tree.nodes
            .iter()
            .filter_map(|(_, node)| node.label().or_else(|| node.value()))
            .any(|text| text.contains("waiting")
                && text.contains("not sent")
                && text.contains("/widget-waiting.wav"))
    );
    for name in ["Add file", "Add folder"] {
        let id = named(&tree, name);
        assert!(
            tree.nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled(),
            "waiting edit left another action enabled"
        );
    }
    for name in ["Mute", "Stop"] {
        let id = named(&tree, name);
        assert!(
            !tree
                .nodes
                .iter()
                .find(|(node, _)| *node == id)
                .unwrap()
                .1
                .is_disabled()
        );
    }
    transport.release_state();
}
