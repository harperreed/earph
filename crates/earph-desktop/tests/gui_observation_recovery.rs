// ABOUTME: Reproduces a queue fence race against a real owner and subsequent automatic recovery.
// ABOUTME: Keeps recovered background warnings separate from retained foreground failures.
mod common;
use common::*;
use earph_core::control::Command;
use earph_desktop::{
    Request,
    gui::{GuiController, UiIntent},
};
use std::{
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
            "observation timed out: {:?}",
            gui.error()
        );
        thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn successful_fenced_refresh_clears_its_recovered_background_warning() {
    let owner = Owner::start();
    let proxy = proxy::Proxy::hold_first_queue_reply(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |_| proxy.entered.try_recv().is_ok());
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &owner.state(),
                Command::QueueAppend {
                    path: "/owned-observation-race.wav".into(),
                },
            )
            .unwrap(),
        )
        .unwrap();
    proxy.release();
    until(&mut gui, |g| {
        g.error().is_some_and(|e| e.contains("changed during read"))
    });
    until(&mut gui, |g| {
        !g.busy() && !g.queue_stale() && g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    assert!(
        gui.error().is_none(),
        "successful automatic queue adoption retained its old warning: {:?}",
        gui.error()
    );
}

#[test]
fn successful_background_refresh_does_not_clear_an_unrelated_foreground_error() {
    let owner = Owner::start();
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::ControlEdit {
        basis: std::sync::Arc::new(gui.state().unwrap().clone()),
        command: Command::OwnerShutdown,
    });
    let error = gui.error().unwrap().to_owned();
    let before = gui.observed_at().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while gui.observed_at().unwrap() == before || gui.busy() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(gui.error(), Some(error.as_str()));
}
