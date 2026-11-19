// ABOUTME: Verifies native-controller workflows against the actual shared owner.
// ABOUTME: Covers stale edits, uncertain replies, and disconnect without output or sources.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::*;
use earph_core::control::Command;
use earph_desktop::{
    Presentation, Request,
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
            "workflow timed out: {:?}",
            gui.error()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn attached(runtime: &std::path::Path) -> GuiController {
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| {
        g.connected() && g.queue().is_some() && g.capabilities().is_some()
    });
    gui
}
fn append(gui: &mut GuiController, path: &str) {
    let basis = Arc::new(gui.state().unwrap().clone());
    gui.handle(UiIntent::QueueEdit {
        basis,
        command: Command::QueueAppend { path: path.into() },
    });
}

#[test]
fn successful_background_polls_preserve_user_action_receipts() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    append(&mut gui, "/owned-history.wav");
    until(&mut gui, |g| {
        !g.busy() && g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    let action = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::QueueAppend { .. }))
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    for _ in 0..20 {
        let previous = gui.observed_at().unwrap();
        gui.tick(Instant::now() + Duration::from_secs(1));
        until(&mut gui, |g| {
            !g.busy() && g.observed_at().unwrap() > previous
        });
    }
    assert!(
        gui.receipts()
            .any(|r| r.request.envelope().request_id == action),
        "background observations evicted the user's action receipt"
    );
    assert!(
        !gui.receipts()
            .any(|r| r.presentation == Presentation::Applied
                && matches!(
                    r.request.envelope().payload,
                    Command::State | Command::Capabilities | Command::QueueGet { .. }
                ))
    );
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
}

#[test]
fn actual_queue_edits_publish_observed_order_and_close_leaves_owner_muted() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    append(&mut gui, "/generated-first.wav");
    until(&mut gui, |g| {
        g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    append(&mut gui, "/generated-second.wav");
    until(&mut gui, |g| {
        g.queue().is_some_and(|q| q.entries.len() == 2)
    });
    let second = gui.queue().unwrap().entries[1].id.clone();
    let basis = Arc::new(gui.state().unwrap().clone());
    gui.handle(UiIntent::QueueEdit {
        basis,
        command: Command::QueueMove {
            id: second.clone(),
            new_index: 0,
        },
    });
    until(&mut gui, |g| g.queue().unwrap().entries[0].id == second);
    assert!(
        gui.receipts()
            .any(|r| r.presentation == Presentation::Applied)
    );
    assert!(gui.state().unwrap().muted);
    assert!(gui.state().unwrap().output.is_none());
    assert!(gui.state().unwrap().source.is_none());
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
    assert!(!gui.connected());
    assert!(owner.state().muted);
    assert_eq!(owner.state().source, None);
}

#[test]
fn changed_revision_keeps_the_draft_from_becoming_a_rebased_mutation() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let basis = Arc::new(gui.state().unwrap().clone());
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::QueueAppend {
                    path: "/external.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::QueueEdit {
        basis,
        command: Command::QueueAppend {
            path: "/stale.wav".into(),
        },
    });
    until(&mut gui, |g| g.error().is_some());
    assert!(gui.error().unwrap().contains("changed"));
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| {
        g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    assert_eq!(gui.queue().unwrap().entries[0].path, "/external.wav");
}

#[test]
fn lost_real_reply_stays_uncertain_until_exact_explicit_reconciliation() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::drop_first_append(owner.runtime.clone());
    let mut gui = attached(&proxy.runtime);
    append(&mut gui, "/uncertain.wav");
    until(&mut gui, |g| {
        g.receipts()
            .any(|r| r.presentation == Presentation::Uncertain)
    });
    let id = gui
        .receipts()
        .find(|r| r.presentation == Presentation::Uncertain)
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    let original = gui
        .receipts()
        .find(|r| r.request.envelope().request_id == id)
        .unwrap()
        .request
        .bytes()
        .to_vec();
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        })
    });
    let frames = proxy.frames.lock().unwrap();
    let appends: Vec<_> = frames
        .iter()
        .filter(|b| {
            serde_json::from_slice::<serde_json::Value>(b).unwrap()["payload"]["method"]
                == "queue.append"
        })
        .collect();
    assert_eq!(appends.len(), 2);
    assert_eq!(appends[0].as_slice(), original);
    assert_eq!(appends[1].as_slice(), original);
}

#[test]
fn queue_intent_rejects_non_queue_commands_and_utf8_byte_overflow() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let basis = Arc::new(gui.state().unwrap().clone());
    gui.handle(UiIntent::QueueEdit {
        basis: basis.clone(),
        command: Command::Mute { muted: false },
    });
    assert!(gui.error().unwrap().contains("queue"));
    gui.handle(UiIntent::QueueEdit {
        basis,
        command: Command::QueueAppend {
            path: "é".repeat(600),
        },
    });
    assert!(gui.error().unwrap().contains("1,024"));
    assert!(owner.state().muted);
}

#[test]
fn new_state_marks_the_old_published_queue_stale_before_page_refresh_finishes() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let basis = gui.state().unwrap().clone();
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::QueueAppend {
                    path: "/external-new.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| g.state().unwrap().revision > basis.revision);
    assert!(
        gui.queue_stale(),
        "old queue must not look current after a newer snapshot arrives"
    );
}

#[test]
fn complete_queue_pages_publish_all_stable_entries_from_the_real_owner() {
    let owner = Owner::start();
    for index in 0..70 {
        let state = owner.state();
        owner
            .client()
            .request(
                &Request::mutation(
                    earph_core::ids::RequestId::new(unique()).unwrap(),
                    &state,
                    Command::QueueAppend {
                        path: format!("/page-{index}.wav"),
                    },
                )
                .unwrap(),
            )
            .unwrap();
    }
    let mut gui = attached(&owner.runtime);
    assert_eq!(gui.queue().unwrap().total, 70);
    assert_eq!(gui.queue().unwrap().entries.len(), 70);
    assert_eq!(gui.queue().unwrap().entries[69].path, "/page-69.wav");
    assert!(!gui.queue_stale());
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
}

#[test]
fn uncertain_bytes_survive_disconnect_and_reconcile_after_reattachment() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::drop_first_append(owner.runtime.clone());
    let mut gui = attached(&proxy.runtime);
    append(&mut gui, "/retained.wav");
    until(&mut gui, |g| {
        g.receipts()
            .any(|r| r.presentation == Presentation::Uncertain)
    });
    let record = gui
        .receipts()
        .find(|r| r.presentation == Presentation::Uncertain)
        .unwrap();
    let id = record.request.envelope().request_id.clone();
    let bytes = record.request.bytes().to_vec();
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    assert_eq!(
        gui.receipts()
            .find(|r| r.request.envelope().request_id == id)
            .unwrap()
            .request
            .bytes(),
        bytes
    );
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        })
    });
    assert_eq!(
        gui.receipts()
            .find(|r| r.request.envelope().request_id == id)
            .unwrap()
            .request
            .bytes(),
        bytes
    );
}

#[test]
fn close_during_a_real_held_query_returns_before_transport_cleanup() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::hold_first_capabilities(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |_| proxy.entered.try_recv().is_ok());
    let start = Instant::now();
    gui.handle(UiIntent::CloseController);
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "close waited for synchronous transport"
    );
    proxy.release();
    until(&mut gui, |g| g.ready_to_close());
    assert!(
        gui.receipts()
            .any(|r| r.presentation == Presentation::Uncertain)
    );
    assert!(owner.state().muted);
}

#[test]
fn restarted_owner_blocks_old_uncertain_mutation_without_retargeting() {
    let mut owner = Owner::start();
    let runtime = owner.runtime.clone();
    let proxy = proxy::Proxy::drop_first_append(runtime.clone());
    let mut gui = attached(&proxy.runtime);
    let old_owner = gui.state().unwrap().owner_id.clone();
    append(&mut gui, "/old-owner.wav");
    until(&mut gui, |g| {
        g.receipts()
            .any(|r| r.presentation == Presentation::Uncertain)
    });
    let id = gui
        .receipts()
        .find(|r| r.presentation == Presentation::Uncertain)
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    owner.stop();
    let replacement = Owner::at(runtime);
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| g.state().unwrap().owner_id != old_owner);
    assert!(
        gui.capabilities().is_none(),
        "capabilities from the retired owner must be invalidated"
    );
    gui.handle(UiIntent::Reconcile(id.clone()));
    assert!(gui.error().unwrap().contains("OwnerChanged"));
    assert!(
        gui.receipts()
            .find(|r| r.request.envelope().request_id == id)
            .unwrap()
            .owner_changed
    );
    assert!(replacement.state().muted);
}

#[test]
fn successful_attachment_can_recover_from_a_previous_launch_error() {
    let owner = Owner::start();
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Launch {
        runtime: std::path::PathBuf::from("/tmp")
            .join(unique())
            .to_str()
            .unwrap()
            .into(),
        executable: "/missing/earph-engine".into(),
    });
    until(&mut gui, |g| {
        matches!(
            g.host_status(),
            earph_desktop::gui::HostStatus::Failed { .. }
        )
    });
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && g.queue().is_some());
    assert!(
        gui.error().is_none(),
        "old launch error obscures a successful attachment"
    );
}

#[test]
fn mute_delivers_while_an_ordinary_gui_query_still_holds_its_reply() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::hold_first_capabilities(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |_| proxy.entered.try_recv().is_ok());
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Applied
        })
    });
    assert!(
        gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Capabilities) && r.in_flight)
    );
    assert!(owner.state().muted);
    proxy.release();
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
}

#[test]
fn rejected_shutdown_keeps_its_receipt_without_blocking_refresh_or_reattachment() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let old = gui.state().unwrap().clone();
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &old,
                Command::QueueAppend {
                    path: "/shutdown-revision-change.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::ShutdownAndQuit);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::OwnerShutdown)
                && r.presentation == Presentation::Rejected
        })
    });
    let record = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::OwnerShutdown))
        .unwrap();
    let id = record.request.envelope().request_id.clone();
    let bytes = record.request.bytes().to_vec();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| {
        g.state().unwrap().revision > old.revision && !g.busy()
    });
    assert!(!gui.ready_to_close());
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    assert!(!gui.attached());
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    let retained = gui
        .receipts()
        .find(|r| r.request.envelope().request_id == id)
        .unwrap();
    assert_eq!(retained.presentation, Presentation::Rejected);
    assert_eq!(retained.request.bytes(), bytes);
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn uncertain_shutdown_recovers_and_requires_explicit_exact_replay_without_autoquit() {
    let owner = receipt_fixture::RealOwner::start();
    let transport = receipt_fixture::ReceiptTransport::drop_shutdown(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    gui.handle(UiIntent::ShutdownAndQuit);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::OwnerShutdown)
                && r.presentation == Presentation::Uncertain
        })
    });
    let record = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::OwnerShutdown))
        .unwrap();
    let id = record.request.envelope().request_id.clone();
    let bytes = record.request.bytes().to_vec();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::ShutdownAndQuit);
    assert!(gui.error().unwrap().contains("Reconcile"));
    assert_eq!(
        gui.receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::OwnerShutdown))
            .count(),
        1
    );
    assert!(!gui.ready_to_close());
    assert!(!owner.state().shutdown_requested);
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        })
    });
    assert!(
        !gui.ready_to_close(),
        "reconciling historical shutdown must not close the controller"
    );
    let calls = transport.for_id(&id);
    assert_eq!(calls.len(), 2);
    assert!(calls.iter().all(|call| call == &bytes));
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
}

#[test]
fn controlled_receipt_producer_polls_two_accepted_safety_requests_without_ordinary_starvation() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::accepted(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::QueueGet { .. })
                && r.presentation == Presentation::Accepted
        })
    });
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Accepted
        })
    });
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Accepted
        })
    });
    let safety: Vec<_> = gui
        .receipts()
        .filter(|r| {
            matches!(
                r.request.envelope().payload,
                Command::Stop | Command::Mute { muted: true }
            )
        })
        .map(|r| {
            (
                r.request.envelope().request_id.clone(),
                r.request.bytes().to_vec(),
            )
        })
        .collect();
    let ordinary = transport.ordinary_id().unwrap();
    until(&mut gui, |g| g.receipts().all(|r| !r.in_flight));
    let initial_calls = transport.for_id(&ordinary).len()
        + safety
            .iter()
            .map(|(id, _)| transport.for_id(id).len())
            .sum::<usize>();
    let start = Instant::now() + Duration::from_secs(1);
    for index in 0..24 {
        gui.tick(start + Duration::from_millis(index * 100));
        thread::sleep(Duration::from_millis(3));
    }
    assert!(transport.for_id(&ordinary).len() > 1);
    for (id, bytes) in &safety {
        let calls = transport.for_id(id);
        assert!(
            calls.len() > 1,
            "Accepted safety receipt never polled: {id:?}"
        );
        assert!(calls.iter().all(|call| call == bytes));
    }
    let calls = transport.for_id(&ordinary).len()
        + safety
            .iter()
            .map(|(id, _)| transport.for_id(id).len())
            .sum::<usize>();
    assert!(
        calls <= initial_calls + 24,
        "more than one replay was admitted per polling interval"
    );
    transport.finish();
    for index in 24..100 {
        gui.tick(start + Duration::from_millis(index * 100));
        thread::sleep(Duration::from_millis(3));
        if safety.iter().all(|(id, _)| {
            gui.receipts().any(|r| {
                r.request.envelope().request_id == *id && r.presentation == Presentation::Applied
            })
        }) && gui.queue().is_some()
        {
            break;
        }
    }
    assert!(safety.iter().all(|(id, _)| {
        gui.receipts().any(|r| {
            r.request.envelope().request_id == *id && r.presentation == Presentation::Applied
        })
    }));
    assert!(gui.queue().is_some());
    assert!(
        owner.state().muted && owner.state().source.is_none() && owner.state().output.is_none()
    );
}

#[test]
fn controlled_accepted_safety_stops_replay_after_real_owner_change_without_losing_receipts() {
    let a = Owner::start();
    let b = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::accepted(a.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::QueueGet { .. })
                && r.presentation == Presentation::Accepted
        })
    });
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Accepted
        })
    });
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Accepted
        })
    });
    let safety: Vec<_> = gui
        .receipts()
        .filter(|r| {
            matches!(
                r.request.envelope().payload,
                Command::Stop | Command::Mute { muted: true }
            )
        })
        .map(|r| {
            (
                r.request.envelope().request_id.clone(),
                r.request.bytes().to_vec(),
            )
        })
        .collect();
    transport.switch_owner(b.runtime.clone());
    transport.finish_ordinary();
    let owner_b = b.state().owner_id;
    let start = Instant::now() + Duration::from_secs(1);
    let mut observed = false;
    for index in 0..80 {
        gui.tick(start + Duration::from_millis(index * 100));
        thread::sleep(Duration::from_millis(3));
        if gui.state().unwrap().owner_id == owner_b {
            observed = true;
        }
    }
    assert!(observed, "actual owner B snapshot never arrived");
    let calls: Vec<_> = safety
        .iter()
        .map(|(id, _)| transport.for_id(id).len())
        .collect();
    for index in 80..100 {
        gui.tick(start + Duration::from_millis(index * 100));
        thread::sleep(Duration::from_millis(3));
    }
    for ((id, bytes), calls) in safety.iter().zip(calls) {
        let record = gui
            .receipts()
            .find(|r| r.request.envelope().request_id == *id)
            .unwrap();
        assert!(record.owner_changed);
        assert_eq!(record.presentation, Presentation::Accepted);
        assert_eq!(record.request.bytes(), bytes);
        assert_eq!(
            transport.for_id(id).len(),
            calls,
            "old-owner reconciliation continued"
        );
    }
    gui.handle(UiIntent::Mute);
    assert!(
        gui.error().is_none(),
        "old accepted safety tracking still blocks new owner safety: {:?}",
        gui.error()
    );
    assert!(gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Mute { muted: true }
    ) && r.request.envelope().owner_id == owner_b));
    transport.finish();
}

#[test]
fn controlled_uncertain_safety_resumes_polling_after_explicit_reconciliation_returns_accepted() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::uncertain_safety(owner.runtime.clone());
    transport.finish_ordinary();
    let mut gui = attached(&transport.runtime);
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Uncertain
        })
    });
    let record = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::Stop))
        .unwrap();
    let id = record.request.envelope().request_id.clone();
    let bytes = record.request.bytes().to_vec();
    assert_eq!(transport.for_id(&id).len(), 1);
    gui.handle(UiIntent::Reconcile(id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Accepted
        })
    });
    transport.finish();
    let start = Instant::now() + Duration::from_secs(1);
    for index in 0..40 {
        gui.tick(start + Duration::from_millis(index * 100));
        thread::sleep(Duration::from_millis(3));
        if gui.receipts().any(|r| {
            r.request.envelope().request_id == id && r.presentation == Presentation::Applied
        }) {
            break;
        }
    }
    assert!(
        gui.receipts()
            .any(|r| r.request.envelope().request_id == id
                && r.presentation == Presentation::Applied),
        "Accepted safety lost its automatic tracking after explicit reconciliation"
    );
    let calls = transport.for_id(&id);
    assert!((3..=42).contains(&calls.len()));
    assert!(calls.iter().all(|call| call == &bytes));
    assert!(!gui.ready_to_close());
}

#[test]
fn controlled_uncertain_safety_reconciliation_keeps_the_two_slot_bound() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::uncertain_safety(owner.runtime.clone());
    transport.finish_ordinary();
    let mut gui = attached(&transport.runtime);
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Uncertain
        })
    });
    let id = gui
        .receipts()
        .find(|r| r.presentation == Presentation::Uncertain)
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Accepted
                && !r.in_flight
        })
    });
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Accepted
                && !r.in_flight
        })
    });
    gui.handle(UiIntent::Reconcile(id.clone()));
    assert!(
        gui.error().is_some_and(|error| error.contains("Safety")),
        "reconciliation exceeded the bounded safety tracking capacity"
    );
    assert_eq!(transport.for_id(&id).len(), 1);
    assert!(gui.receipts().any(
        |r| r.request.envelope().request_id == id && r.presentation == Presentation::Uncertain
    ));
    transport.finish();
}

fn held_queue_observation(owner: &Owner) -> (GuiController, receipt_fixture::ReceiptTransport) {
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    transport.hold_next_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |_| transport.state_held());
    assert!(!gui.queue_stale());
    (gui, transport)
}
fn append_frames(
    transport: &receipt_fixture::ReceiptTransport,
) -> Vec<earph_core::control::CommandEnvelope> {
    transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes).unwrap())
        .filter(|request| matches!(request.payload, Command::QueueAppend { .. }))
        .collect()
}

#[test]
fn queue_edit_waits_once_during_background_read_and_keeps_its_captured_intent() {
    let owner = Owner::start();
    let (mut gui, transport) = held_queue_observation(&owner);
    let basis = Arc::new(gui.queue_basis().unwrap().clone());
    gui.handle(UiIntent::QueueEdit {
        basis: basis.clone(),
        command: Command::QueueAppend {
            path: "/captured-first.wav".into(),
        },
    });
    assert!(
        gui.error().is_none(),
        "background observation blocked local edit admission: {:?}",
        gui.error()
    );
    assert!(
        append_frames(&transport).is_empty(),
        "waiting edit reached owner before preflight"
    );
    gui.handle(UiIntent::QueueEdit {
        basis: basis.clone(),
        command: Command::QueueAppend {
            path: "/refused-second.wav".into(),
        },
    });
    assert!(
        gui.error().is_some(),
        "a second edit replaced the captured intent"
    );
    transport.release_state();
    until(&mut gui, |g| {
        g.queue().is_some_and(|q| q.entries.len() == 1) && !g.busy()
    });
    let frames = append_frames(&transport);
    assert_eq!(frames.len(), 1);
    let request = &frames[0];
    assert_eq!(request.owner_id, basis.owner_id);
    assert_eq!(request.session_id, basis.session_id);
    assert_eq!(request.zone_id, basis.zone_id);
    assert_eq!(request.expected_revision, Some(basis.revision));
    assert_eq!(
        request.payload,
        Command::QueueAppend {
            path: "/captured-first.wav".into()
        }
    );
    assert_eq!(gui.queue().unwrap().entries[0].path, "/captured-first.wav");
}

#[test]
fn waiting_queue_edit_refuses_changed_context_before_sending_a_mutation() {
    let owner = Owner::start();
    let (mut gui, transport) = held_queue_observation(&owner);
    let basis = Arc::new(gui.queue_basis().unwrap().clone());
    gui.handle(UiIntent::QueueEdit {
        basis: basis.clone(),
        command: Command::QueueAppend {
            path: "/stale-waiting.wav".into(),
        },
    });
    assert!(gui.error().is_none(), "waiting intent was not admitted");
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::QueueAppend {
                    path: "/external-change.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    transport.release_state();
    until(&mut gui, |g| {
        g.error().is_some_and(|e| e.contains("changed"))
    });
    assert!(
        append_frames(&transport).is_empty(),
        "stale waiting intent became a mutation"
    );
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn disconnect_and_close_cancel_waiting_queue_intent_without_later_mutation() {
    for close in [false, true] {
        let owner = Owner::start();
        let (mut gui, transport) = held_queue_observation(&owner);
        append(&mut gui, "/cancelled-waiting.wav");
        assert!(gui.error().is_none(), "waiting intent was not admitted");
        gui.handle(if close {
            UiIntent::CloseController
        } else {
            UiIntent::Disconnect
        });
        transport.release_state();
        until(&mut gui, |g| !g.busy());
        if close {
            assert!(gui.ready_to_close());
        } else {
            gui.handle(UiIntent::Attach {
                runtime: transport.runtime.to_str().unwrap().into(),
            });
            until(&mut gui, |g| g.connected() && !g.busy());
        }
        assert!(append_frames(&transport).is_empty());
        assert!(owner.state().muted && !owner.state().shutdown_requested);
    }
}

#[test]
fn mute_remains_available_and_preempts_an_unsent_waiting_queue_edit() {
    let owner = Owner::start();
    let (mut gui, transport) = held_queue_observation(&owner);
    append(&mut gui, "/safety-cancelled.wav");
    assert!(gui.error().is_none(), "waiting intent was not admitted");
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
    assert!(
        append_frames(&transport).is_empty(),
        "safety action let the waiting edit leak later"
    );
    assert!(owner.state().muted);
}

#[test]
fn owner_change_cancels_waiting_queue_intent_without_retargeting_it() {
    let a = Owner::start();
    let b = Owner::start();
    let (mut gui, transport) = held_queue_observation(&a);
    append(&mut gui, "/old-owner-waiting.wav");
    assert!(gui.error().is_none(), "waiting intent was not admitted");
    let owner_b = b.state().owner_id;
    transport.switch_owner(b.runtime.clone());
    transport.release_state();
    until(&mut gui, |g| {
        g.state().unwrap().owner_id == owner_b && !g.busy()
    });
    assert!(
        append_frames(&transport).is_empty(),
        "waiting edit was silently retargeted"
    );
}

#[test]
fn rejected_shutdown_cancels_waiting_edit_before_refresh_recovers() {
    let owner = Owner::start();
    let (mut gui, transport) = held_queue_observation(&owner);
    append(&mut gui, "/shutdown-cancelled.wav");
    assert!(gui.error().is_none());
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &owner.state(),
                Command::QueueAppend {
                    path: "/external-before-shutdown.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::ShutdownAndQuit);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::OwnerShutdown)
                && r.presentation == Presentation::Rejected
        })
    });
    transport.release_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| !g.busy() && !g.queue_stale());
    assert!(
        append_frames(&transport).is_empty(),
        "rejected shutdown restored a cancelled local edit"
    );
    assert!(!gui.ready_to_close());
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn waiting_queue_selection_preserves_its_captured_entry_identity() {
    let owner = Owner::start();
    for path in ["/first-entry.wav", "/second-entry.wav"] {
        owner
            .client()
            .request(
                &Request::mutation(
                    earph_core::ids::RequestId::new(unique()).unwrap(),
                    &owner.state(),
                    Command::QueueAppend { path: path.into() },
                )
                .unwrap(),
            )
            .unwrap();
    }
    let (mut gui, transport) = held_queue_observation(&owner);
    let basis = Arc::new(gui.queue_basis().unwrap().clone());
    let selected = gui.queue().unwrap().entries[1].id.clone();
    gui.handle(UiIntent::QueueEdit {
        basis: basis.clone(),
        command: Command::QueueSelect {
            id: Some(selected.clone()),
        },
    });
    assert!(gui.error().is_none());
    transport.release_state();
    until(&mut gui, |g| {
        g.queue().unwrap().selected.as_ref() == Some(&selected) && !g.busy()
    });
    let edits: Vec<_> = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .map(|bytes| serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes).unwrap())
        .filter(|request| matches!(request.payload, Command::QueueSelect { .. }))
        .collect();
    assert_eq!(edits.len(), 1);
    assert_eq!(
        edits[0].payload,
        Command::QueueSelect { id: Some(selected) }
    );
    assert_eq!(edits[0].expected_revision, Some(basis.revision));
    assert_eq!(edits[0].owner_id, basis.owner_id);
    assert_eq!(edits[0].session_id, basis.session_id);
}

#[test]
fn uncertain_background_read_cancels_local_edit_before_observation_recovers() {
    let owner = Owner::start();
    let (mut gui, transport) = held_queue_observation(&owner);
    append(&mut gui, "/uncertain-read-cancelled.wav");
    assert!(gui.error().is_none());
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::State)
                && r.presentation == Presentation::Uncertain
        })
    });
    transport.release_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| !g.busy() && !g.queue_stale());
    assert!(
        append_frames(&transport).is_empty(),
        "failed background observation let local intent reach the owner later"
    );
}
