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
fn request_widgets_keep_user_actions_visible_without_internal_poll_rows() {
    use earph_core::control::Command;
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.connected() || controller.busy() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    controller.handle(UiIntent::QueueEdit {
        basis: std::sync::Arc::new(controller.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: "/owned-visible-action.wav".into(),
        },
    });
    while controller.busy() || controller.queue().unwrap().entries.len() != 1 {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let action_id = controller
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::QueueAppend { .. }))
        .unwrap()
        .request
        .envelope()
        .request_id
        .as_str()
        .to_owned();
    transport.hold_next_state();
    controller.handle(UiIntent::Refresh);
    while !transport.state_held() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let poll_id = controller
        .receipts()
        .find(|r| r.in_flight && matches!(r.request.envelope().payload, Command::State))
        .unwrap()
        .request
        .envelope()
        .request_id
        .as_str()
        .to_owned();
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(controller);
    let tree = draw_frame(&context, &mut app, vec![]);
    transport.release_state();
    assert!(tree.nodes.iter().any(|(_, node)| {
        node.label()
            .is_some_and(|label| label.starts_with(&action_id))
    }));
    assert!(
        !tree.nodes.iter().any(|(_, node)| node
            .label()
            .is_some_and(|label| label.starts_with(&poll_id))),
        "internal polling added a changing user receipt row"
    );
}

#[test]
fn uncertain_snapshot_receipts_stay_visible_for_exact_reconciliation() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::drop_first_state(owner.runtime.clone());
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let id = loop {
        controller.tick(Instant::now());
        if let Some(record) = controller
            .receipts()
            .find(|r| r.presentation == earph_desktop::Presentation::Uncertain)
        {
            break record.request.envelope().request_id.as_str().to_owned();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(controller);
    let tree = draw_frame(&context, &mut app, vec![]);
    assert!(
        tree.nodes
            .iter()
            .any(|(_, node)| node.label().is_some_and(|label| label.starts_with(&id))),
        "uncertain observation lost its visible reconciliation receipt"
    );
}

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
        "Quit",
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
        let label_id = named(&tree, name);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.labelled_by().contains(&label_id)),
            "missing field label association for {name} at {label_id:?}"
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
fn signal_path_keeps_missing_playback_facts_unknown_after_real_owner_observation() {
    let owner = Owner::start();
    let before = owner.state();
    let labels = widgets::signal_path_labels(owner.runtime.to_str().unwrap());
    for expected in [
        "Playback observation: unavailable",
        "Backend format: unknown",
        "Native device identity: unknown",
        "Output activity: unavailable",
    ] {
        assert!(
            labels.iter().any(|label| label == expected),
            "missing observed fact: {expected}; {labels:?}"
        );
    }
    let after = owner.state();
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.epochs, before.epochs);
    assert!(after.muted && !after.output_consented && after.playback.is_none());
}

#[test]
fn output_widgets_label_drafts_and_keep_each_signal_action_explicit() {
    let owner = Owner::start();
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.connected() || controller.busy() {
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
        named(&tree, "Output / Safety"),
        egui::accesskit::Action::Click,
        None,
    );
    let tree = draw_frame(&context, &mut app, vec![click]);
    for label in [
        "Endpoint",
        "Configured device ID",
        "Configured profile ID",
        "Period frames",
        "Buffer frames",
        "Software level (millidecibels)",
        "Ramp frames",
        "Software cap (millidecibels)",
        "Hardware cap (millidecibels, optional)",
        "Source frame",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.labelled_by().contains(&id)),
            "field lacks accessible label: {label}"
        );
    }
    for label in [
        "Select output",
        "Approve current output",
        "Set independent caps",
        "Set software level",
        "Arm current fixed output",
        "Start selected local queue",
        "Pause",
        "Resume",
        "Seek to source frame",
        "Unmute current output",
        "Mute",
        "Stop",
        "Refresh devices",
        "Inspect endpoint",
    ] {
        named(&tree, label);
    }
    for text in [
        "Hardware volume: unsupported; readback unknown",
        "Selected output intent: none",
        "Opened backend: unknown",
        "Consent observed: false",
    ] {
        named(&tree, text);
    }
    assert!(owner.state().muted && owner.state().output.is_none());
}

#[test]
fn editable_output_widgets_capture_exact_draft_and_safety_cancels_the_unsent_action() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !controller.connected() || controller.busy() {
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
    let mut tree = draw_frame(&context, &mut app, vec![]);
    let click = action(
        &tree,
        named(&tree, "Output / Safety"),
        egui::accesskit::Action::Click,
        None,
    );
    tree = draw_frame(&context, &mut app, vec![click]);
    let click = action(
        &tree,
        named(&tree, "ALSA"),
        egui::accesskit::Action::Click,
        None,
    );
    tree = draw_frame(&context, &mut app, vec![click]);
    for (label, value) in [
        ("Endpoint", "owned-exact-endpoint"),
        ("Configured device ID", "local-device"),
        ("Configured profile ID", "local-profile"),
        ("Period frames", "128"),
        ("Buffer frames", "512"),
    ] {
        let label_id = named(&tree, label);
        let entry = tree
            .nodes
            .iter()
            .find(|(_, n)| n.labelled_by().contains(&label_id))
            .unwrap()
            .0;
        let set = action(
            &tree,
            entry,
            egui::accesskit::Action::SetValue,
            Some(egui::accesskit::ActionData::Value(value.into())),
        );
        tree = draw_frame(&context, &mut app, vec![set]);
    }
    let click = action(
        &tree,
        named(&tree, "Select output"),
        egui::accesskit::Action::Click,
        None,
    );
    draw_frame(&context, &mut app, vec![click]);
    tree = draw_frame(&context, &mut app, vec![]);
    assert!(
        tree.nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .any(|s| s.contains("not sent")
                && s.contains("owned-exact-endpoint")
                && s.contains("local-profile")
                && s.contains("period_frames: 128"))
    );
    for label in [
        "Select output",
        "Set software level",
        "Set independent caps",
    ] {
        let id = named(&tree, label);
        assert!(
            tree.nodes
                .iter()
                .find(|(n, _)| *n == id)
                .unwrap()
                .1
                .is_disabled()
        );
    }
    let click = action(
        &tree,
        named(&tree, "Mute"),
        egui::accesskit::Action::Click,
        None,
    );
    draw_frame(&context, &mut app, vec![click]);
    transport.release_state();
    assert!(owner.state().muted && owner.state().output.is_none());
}

#[test]
#[cfg(target_os = "linux")]
fn discovery_widgets_disambiguate_exact_endpoints_and_copy_only_selection_intent() {
    use common::process::ProcessOwner;
    use earph_core::control::OutputBackend;
    let artifacts = std::path::PathBuf::from("/tmp").join(unique());
    std::fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("alsa.conf");
    std::fs::write(&config, "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.null { type null hint { show on description \"Owned duplicate label\" } }\npcm.owned_other { type null hint { show on description \"Owned duplicate label\" } }\n").unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&config));
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut controller = GuiController::new();
    controller.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while !controller.connected() || controller.busy() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    controller.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    while controller.discovery().is_none() || controller.busy() {
        controller.tick(Instant::now());
        assert!(Instant::now() < deadline, "{:?}", controller.error());
        thread::sleep(Duration::from_millis(1));
    }
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(controller);
    let tree = draw_frame(&context, &mut app, vec![]);
    let click = action(
        &tree,
        named(&tree, "Output / Safety"),
        egui::accesskit::Action::Click,
        None,
    );
    let mut tree = draw_frame(&context, &mut app, vec![click]);
    for expected in ["Exact endpoint: null", "Exact endpoint: owned_other"] {
        named(&tree, expected);
    }
    let click = action(
        &tree,
        named(&tree, "Core Audio"),
        egui::accesskit::Action::Click,
        None,
    );
    tree = draw_frame(&context, &mut app, vec![click]);
    let inspect = named(&tree, "Inspect this endpoint");
    assert!(
        !tree
            .nodes
            .iter()
            .find(|(id, _)| *id == inspect)
            .unwrap()
            .1
            .is_disabled(),
        "inventory inspection used the unrelated draft backend"
    );
    let label = named(&tree, "Configured profile ID");
    let entry = tree
        .nodes
        .iter()
        .find(|(_, n)| n.labelled_by().contains(&label))
        .unwrap()
        .0;
    let set = action(
        &tree,
        entry,
        egui::accesskit::Action::SetValue,
        Some(egui::accesskit::ActionData::Value(
            "retained-profile-draft".into(),
        )),
    );
    tree = draw_frame(&context, &mut app, vec![set]);
    let click = action(
        &tree,
        named(&tree, "Copy backend and endpoint to draft"),
        egui::accesskit::Action::Click,
        None,
    );
    tree = draw_frame(&context, &mut app, vec![click]);
    let label = named(&tree, "Endpoint");
    let copied = tree
        .nodes
        .iter()
        .find(|(_, n)| n.labelled_by().contains(&label))
        .unwrap()
        .1
        .value()
        .unwrap();
    assert!(["null", "owned_other"].contains(&copied));
    let label = named(&tree, "Configured profile ID");
    assert_eq!(
        tree.nodes
            .iter()
            .find(|(_, n)| n.labelled_by().contains(&label))
            .unwrap()
            .1
            .value(),
        Some("retained-profile-draft")
    );
    assert!(transport.frames.lock().unwrap().iter().all(|b| {
        serde_json::from_slice::<earph_core::control::CommandEnvelope>(b)
            .unwrap()
            .payload
            .is_query()
    }));
    drop(app);
    owner.stop().unwrap();
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
