// ABOUTME: Exercises saved profiles and graph observations through the single GUI owner workflow.
// ABOUTME: Uses actual owner serialization for target, revision, uncertainty, and safety checks.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::*;
use earph_core::control::Command;
use earph_desktop::{
    Presentation,
    gui::{GuiController, UiIntent},
};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn until(gui: &mut GuiController, predicate: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gui.tick(Instant::now());
        if predicate(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "profile workflow timed out: {:?}",
            gui.error()
        );
        thread::sleep(Duration::from_millis(2));
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
fn action(gui: &mut GuiController, command: Command) {
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(
        gui.queue_edit_pending(),
        "action was not admitted: {:?}",
        gui.error()
    );
}

#[test]
fn explicit_profile_observation_refreshes_revision_without_retargeting_an_owner() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    let captured = Arc::new(gui.state().unwrap().clone());
    let edit = earph_desktop::Request::mutation(
        earph_core::ids::RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: "/owned-read-refresh.wav".into(),
        },
    )
    .unwrap();
    owner.client().request(&edit).unwrap();
    assert_ne!(owner.state().revision, captured.revision);
    gui.handle(UiIntent::ProfileAction {
        basis: captured.clone(),
        command: Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    });
    until(&mut gui, |g| !g.queue_edit_pending());
    assert!(
        gui.profile_catalog().is_some(),
        "fresh read refused: {:?}",
        gui.error()
    );
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    let replacement = Owner::start();
    gui.handle(UiIntent::Attach {
        runtime: replacement.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::ProfileAction {
        basis: captured,
        command: Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    });
    until(&mut gui, |g| !g.queue_edit_pending());
    assert!(
        gui.profile_catalog().is_none(),
        "read retargeted a replacement owner"
    );
    assert!(gui.error().is_some());
}

#[test]
fn profile_waiting_slot_is_bounded_and_safety_cancels_only_its_unsent_save() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    transport.hold_next_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |_| transport.state_held());
    action(&mut gui, profile_payload());
    assert!(matches!(
        gui.waiting_queue_edit(),
        Some(Command::ProfilesCreate { .. })
    ));
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: "/refused-second-intent.wav".into(),
        },
    });
    assert!(gui.error().is_some());
    assert!(matches!(
        gui.waiting_queue_edit(),
        Some(Command::ProfilesCreate { .. })
    ));
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Applied
        })
    });
    assert!(transport.state_held());
    transport.release_state();
    until(&mut gui, |g| !g.busy());
    let frames = transport.frames.lock().unwrap();
    assert!(!frames.iter().any(|bytes| {
        matches!(
            serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                .unwrap()
                .payload,
            Command::ProfilesCreate { .. } | Command::QueueAppend { .. }
        )
    }));
    assert!(owner.state().muted);
}

#[test]
fn waiting_profile_read_cancels_on_close_and_owner_change_without_late_adoption() {
    for replace in [false, true] {
        let owner = Owner::start();
        let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
        let mut gui = GuiController::new();
        gui.handle(UiIntent::Attach {
            runtime: transport.runtime.to_str().unwrap().into(),
        });
        until(&mut gui, |g| g.connected() && !g.busy());
        transport.hold_next_state();
        gui.handle(UiIntent::Refresh);
        until(&mut gui, |_| transport.state_held());
        action(
            &mut gui,
            Command::ProfilesList {
                offset: 0,
                limit: 16,
            },
        );
        let replacement = replace.then(Owner::start);
        if let Some(replacement) = &replacement {
            transport.switch_owner(replacement.runtime.clone());
        } else {
            gui.handle(UiIntent::CloseController);
        }
        transport.release_state();
        until(&mut gui, |g| !g.busy());
        assert!(gui.profile_catalog().is_none());
        assert!(!transport.frames.lock().unwrap().iter().any(|bytes| {
            matches!(
                serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                    .unwrap()
                    .payload,
                Command::ProfilesList { .. }
            )
        }));
        if let Some(replacement) = &replacement {
            assert_eq!(gui.state().unwrap().owner_id, replacement.state().owner_id);
        } else {
            assert!(gui.ready_to_close());
        }
    }
}

#[test]
fn uncertain_profile_save_reconnect_replays_exact_bytes_then_requires_explicit_observation() {
    let owner = Owner::start();
    let transport = proxy::Proxy::drop_first_profile_save(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    action(&mut gui, profile_payload());
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::ProfilesCreate { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let saved = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::ProfilesCreate { .. }))
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
    let saves: Vec<_> = frames
        .iter()
        .filter(|bytes| {
            matches!(
                serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                    .unwrap()
                    .payload,
                Command::ProfilesCreate { .. }
            )
        })
        .collect();
    assert!(saves.len() >= 2);
    assert!(saves.iter().all(|frame| frame.as_slice() == bytes));
    action(
        &mut gui,
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    until(&mut gui, |g| !g.busy() && g.profile_catalog().is_some());
    let entries = &gui.profile_catalog().unwrap().entries;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].revision, 1);
    let profile_id = entries[0].id.clone();
    action(&mut gui, Command::ProfilesGet { id: profile_id });
    until(&mut gui, |g| !g.busy() && g.profile_record().is_some());
    assert_eq!(gui.profile_record().unwrap().profile.revision, 1);
    assert!(owner.state().muted && !owner.state().output_consented);
}
#[test]
fn real_owner_create_get_list_resolve_and_update_publish_exact_saved_intent_without_application() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    let before = owner.state();
    action(&mut gui, profile_payload());
    until(&mut gui, |g| !g.busy() && g.profile_record().is_some());
    let saved = gui.profile_record().unwrap();
    assert_eq!(saved.profile.revision, 1);
    let id = saved.profile.id.clone();
    let hash = saved.profile_sha256;
    action(
        &mut gui,
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    until(&mut gui, |g| !g.busy() && g.profile_catalog().is_some());
    assert_eq!(gui.profile_catalog().unwrap().entries.len(), 1);
    assert_eq!(
        gui.profile_catalog().unwrap().entries[0].profile_sha256,
        hash
    );
    action(&mut gui, Command::ProfilesGet { id: id.clone() });
    until(&mut gui, |g| {
        !g.busy() && g.profile_record().unwrap().profile.id == id
    });
    let Command::ProfilesCreate { draft, graph, .. } = profile_payload() else {
        panic!("create")
    };
    action(
        &mut gui,
        Command::ProfilesResolve {
            manual_profile_id: Some(id.clone()),
            output: draft.output.clone(),
        },
    );
    until(&mut gui, |g| !g.busy() && g.profile_resolution().is_some());
    assert!(
        matches!(gui.profile_resolution().unwrap(), earph_desktop::ProfileResolutionView::Selected { profile, .. } if profile.id == id && profile.revision == 1)
    );
    action(
        &mut gui,
        Command::ProfilesUpdate {
            id: id.clone(),
            expected_profile_revision: 1,
            draft,
            graph,
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.profile_record().unwrap().profile.revision == 2
    });
    assert_ne!(gui.profile_record().unwrap().profile_sha256, hash);
    let after = owner.state();
    assert!(
        after.muted && !after.output_consented && after.output.is_none() && after.source.is_none()
    );
    assert_eq!(after.software_gain_db, before.software_gain_db);
    assert_eq!(after.local_volume_policy, before.local_volume_policy);
}

#[test]
fn stale_save_is_refused_before_submission_and_graph_inspection_is_an_actual_owner_result() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    let basis = Arc::new(gui.state().unwrap().clone());
    owner
        .client()
        .request(
            &earph_desktop::Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::QueueAppend {
                    path: "/external.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::ProfileAction {
        basis,
        command: profile_payload(),
    });
    until(&mut gui, |g| g.error().is_some());
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::ProfilesCreate { .. }))
    );
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| !g.busy() && !g.queue_stale());
    action(&mut gui, Command::GraphInspect {});
    until(&mut gui, |g| !g.busy() && g.graph_inspection().is_some());
    assert!(gui.graph_inspection().unwrap().active.is_none());
    assert!(!gui.graph_inspection().unwrap().recovery_required);
    assert!(
        !gui.receipts()
            .any(|r| r.presentation == Presentation::Uncertain)
    );
}

#[test]
fn adopted_profile_and_graph_reads_retire_without_displacing_the_saved_action_receipt() {
    let owner = Owner::start();
    let mut gui = attached(&owner);
    action(&mut gui, profile_payload());
    until(&mut gui, |g| !g.busy() && g.profile_record().is_some());
    let record = gui.profile_record().unwrap();
    let id = record.profile.id.clone();
    let Command::ProfilesCreate { draft, .. } = profile_payload() else {
        panic!("create")
    };
    for command in [
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
        Command::ProfilesGet { id: id.clone() },
        Command::ProfilesResolve {
            manual_profile_id: Some(id),
            output: draft.output.clone(),
        },
        Command::GraphInspect {},
    ] {
        action(&mut gui, command);
        until(&mut gui, |g| !g.busy());
    }
    let retained_reads: Vec<_> = gui
        .receipts()
        .filter(|r| {
            r.presentation == Presentation::Applied
                && matches!(
                    r.request.envelope().payload,
                    Command::ProfilesList { .. }
                        | Command::ProfilesGet { .. }
                        | Command::ProfilesResolve { .. }
                        | Command::GraphInspect { .. }
                )
        })
        .map(|r| r.request.envelope().payload.clone())
        .collect();
    assert!(
        retained_reads.is_empty(),
        "adopted internal reads remain in the bounded history: {retained_reads:?}"
    );
    assert!(gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::ProfilesCreate { .. }
    ) && r.presentation == Presentation::Applied));
    assert!(
        gui.profile_catalog().is_some()
            && gui.profile_record().is_some()
            && gui.profile_resolution().is_some()
            && gui.graph_inspection().is_some()
    );
}

#[test]
fn pending_raw_graph_edits_disable_caller_save_until_explicit_restore() {
    use earph_desktop::gui::GuiApp;
    use eframe::egui;
    let owner = Owner::start();
    let gui = attached(&owner);
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut app = GuiApp::new(gui);
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
    fn event(
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
    let tree = draw(&context, &mut app, vec![]);
    let click = event(
        &tree,
        named(&tree, "Profiles / DSP"),
        egui::accesskit::Action::Click,
        None,
    );
    let tree = draw(&context, &mut app, vec![click]);
    assert!(
        !tree
            .nodes
            .iter()
            .find(|(id, _)| *id == named(&tree, "Create profile"))
            .unwrap()
            .1
            .is_disabled()
    );
    let label = named(&tree, "GraphIntent JSON");
    let input = tree
        .nodes
        .iter()
        .find(|(_, n)| n.labelled_by().contains(&label))
        .unwrap()
        .0;
    let set = event(
        &tree,
        input,
        egui::accesskit::Action::SetValue,
        Some(egui::accesskit::ActionData::Value(
            "{pending invalid graph".into(),
        )),
    );
    draw(&context, &mut app, vec![set]);
    let tree = draw(&context, &mut app, vec![]);
    assert!(
        tree.nodes
            .iter()
            .find(|(id, _)| *id == named(&tree, "Create profile"))
            .unwrap()
            .1
            .is_disabled(),
        "caller submitted the old canonical graph while raw edits remained pending"
    );
    let restore = event(
        &tree,
        named(&tree, "Restore canonical JSON"),
        egui::accesskit::Action::Click,
        None,
    );
    draw(&context, &mut app, vec![restore]);
    let tree = draw(&context, &mut app, vec![]);
    assert!(
        !tree
            .nodes
            .iter()
            .find(|(id, _)| *id == named(&tree, "Create profile"))
            .unwrap()
            .1
            .is_disabled()
    );
    assert!(
        owner.state().output.is_none() && owner.state().source.is_none() && owner.state().muted
    );
}
