// ABOUTME: Reads the actual Signal Path widget tree after attaching to a test-owned engine.
// ABOUTME: Uses normal GUI observation and navigation without sending playback mutations.
use earph_desktop::gui::{GuiApp, GuiController, UiIntent};
use eframe::egui;
use std::{
    thread,
    time::{Duration, Instant},
};

pub fn signal_path_labels(runtime: &str) -> Vec<String> {
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: runtime.into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.connected() || controller.busy() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline, "GUI observation did not finish");
        thread::sleep(Duration::from_millis(1));
    }
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(controller);
    let mut first = context.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
    first.textures_delta.clear();
    let tree = first.platform_output.accesskit_update.unwrap();
    let target = tree
        .nodes
        .iter()
        .find(|(_, node)| {
            node.role() == egui::accesskit::Role::RadioButton
                && node.label() == Some("Signal Path / Recovery")
        })
        .or_else(|| {
            tree.nodes.iter().find(|(_, node)| {
                node.role() != egui::accesskit::Role::TextRun
                    && node.label() == Some("Signal Path / Recovery")
            })
        })
        .expect("Signal Path navigation must exist")
        .0;
    let event = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        action: egui::accesskit::Action::Click,
        target_tree: tree.tree_id,
        target_node: target,
        data: None,
    });
    let mut output = context.run_ui(
        egui::RawInput {
            events: vec![event],
            ..Default::default()
        },
        |ui| app.draw(ui),
    );
    output.textures_delta.clear();
    output
        .platform_output
        .accesskit_update
        .unwrap()
        .nodes
        .into_iter()
        .filter_map(|(_, node)| node.label().or_else(|| node.value()).map(str::to_owned))
        .collect()
}
