// ABOUTME: Exercises playback expiry and safety at actual reply-adoption boundaries.
// ABOUTME: Uses real owner replies and a private software output without physical audio.
use super::super::test_common as common;
use super::*;
use earph_core::{
    control::{OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
use std::thread;

#[test]
fn a_crossed_queue_read_reports_refresh_status_without_an_action_failure() {
    let mut owner = common::Owner::start();
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    while !gui.queue_edit_available() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    gui.handle(UiIntent::Refresh);
    while !matches!(gui.pending.as_ref(), Some((_, Task::Page(_)))) {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let page = gui.pending.as_ref().unwrap().0.clone();
    while gui.outcome(&page).is_none() {
        gui.client.as_mut().unwrap().poll();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let append = Request::mutation(
        RequestId::new(common::unique()).unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: owner
                .runtime
                .join("not-opened.wav")
                .to_str()
                .unwrap()
                .into(),
        },
    )
    .unwrap();
    assert!(
        matches!(owner.client().request(&append).unwrap(), Reply::Receipt(r) if r.status == crate::ReplyStatus::Applied)
    );
    while gui.observation_error().is_none() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(gui.queue_stale());
    assert!(gui.error().is_some());
    assert!(gui.action_error().is_none());
    while !gui.queue_edit_available() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(gui.observation_error().is_none());
    assert_eq!(gui.queue().unwrap().total, 1);
    gui.handle(UiIntent::CloseController);
    while !gui.ready_to_close() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    owner.stop();
}

#[test]
fn an_expired_play_cannot_dispatch_start_when_its_fresh_preflight_reply_is_already_delivered() {
    let mut owner = common::Owner::start();
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "null".into(),
        device_id: DeviceId::new("expiry-null").unwrap(),
        profile_id: ProfileId::new("expiry-profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    for command in [
        Command::OutputSelect {
            output: output.clone(),
        },
        Command::OutputConsent {
            device_id: output.device_id,
            profile_id: output.profile_id,
        },
        Command::QueueAppend {
            path: owner
                .runtime
                .join("never-opened.wav")
                .to_str()
                .unwrap()
                .into(),
        },
    ] {
        let reply = owner
            .client()
            .request(
                &Request::mutation(
                    RequestId::new(common::unique()).unwrap(),
                    &owner.state(),
                    command,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            matches!(reply,Reply::Receipt(receipt) if receipt.status==crate::ReplyStatus::Applied)
        );
    }
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(8);
    while !gui.play_local_available() || gui.busy() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    gui.handle(UiIntent::PlayLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
        entry_id: None,
    });
    let preflight = gui.pending.as_ref().unwrap().0.clone();
    loop {
        gui.client.as_mut().unwrap().poll();
        if gui.outcome(&preflight).is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(gui.local_play.is_some());
    gui.tick(Instant::now() + Duration::from_secs(31));
    assert!(
        !gui.receipts()
            .any(|receipt| matches!(receipt.request.envelope().payload, Command::Start { .. })),
        "An expired product action dispatched Start before checking its deadline"
    );
    assert!(!gui.play_local_pending());
    gui.handle(UiIntent::CloseController);
    while !gui.ready_to_close() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(owner.state().session_id.is_none());
    owner.stop();
}

fn prepared_source_after_cancelled_unmute() -> (common::Owner, GuiController) {
    let owner = common::Owner::start();
    let path = owner.runtime.join("silent.wav");
    let frames = 48000u32;
    let mut wav = [
        b"RIFF".as_slice(),
        &(36 + frames * 4).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &1u16.to_le_bytes(),
        &2u16.to_le_bytes(),
        &48000u32.to_le_bytes(),
        &192000u32.to_le_bytes(),
        &4u16.to_le_bytes(),
        &16u16.to_le_bytes(),
        b"data",
        &(frames * 4).to_le_bytes(),
    ]
    .concat();
    wav.resize(wav.len() + frames as usize * 4, 0);
    std::fs::write(&path, wav).unwrap();
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "null".into(),
        device_id: DeviceId::new("cancel-null").unwrap(),
        profile_id: ProfileId::new("cancel-profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    for command in [
        Command::OutputSelect {
            output: output.clone(),
        },
        Command::OutputConsent {
            device_id: output.device_id,
            profile_id: output.profile_id,
        },
        Command::QueueAppend {
            path: path.to_str().unwrap().into(),
        },
    ] {
        let reply = owner
            .client()
            .request(
                &Request::mutation(
                    RequestId::new(common::unique()).unwrap(),
                    &owner.state(),
                    command,
                )
                .unwrap(),
            )
            .unwrap();
        assert!(
            matches!(reply,Reply::Receipt(receipt) if receipt.status==crate::ReplyStatus::Applied)
        );
    }
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: owner.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(12);
    while !gui.play_local_available() || gui.busy() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    gui.handle(UiIntent::PlayLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
        entry_id: None,
    });
    loop {
        gui.tick(Instant::now());
        if matches!(
            gui.pending.as_ref(),
            Some((
                _,
                Task::Preflight {
                    play: Some(PlayMutation::Unmute),
                    ..
                }
            ))
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "{:?}", gui.error());
        thread::sleep(Duration::from_millis(1));
    }
    let preflight = gui.pending.as_ref().unwrap().0.clone();
    loop {
        gui.client.as_mut().unwrap().poll();
        if gui.outcome(&preflight).is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    gui.handle(UiIntent::Mute);
    while gui.busy() || !gui.state().is_some_and(|state| state.muted) {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(!gui.play_local_pending());
    assert!(!gui.receipts().any(|receipt| matches!(
        receipt.request.envelope().payload,
        Command::Mute { muted: false }
    )));
    assert!(owner.state().muted);
    (owner, gui)
}

#[test]
fn mute_cancels_a_delivered_unmute_preflight_before_it_can_submit() {
    let (mut owner, mut gui) = prepared_source_after_cancelled_unmute();
    let deadline = Instant::now() + Duration::from_secs(8);
    gui.handle(UiIntent::CloseController);
    while !gui.ready_to_close() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    owner.stop();
}

#[test]
fn stop_keeps_its_exact_source_when_revision_changes_after_the_targeted_read() {
    let (mut owner, mut gui) = prepared_source_after_cancelled_unmute();
    let deadline = Instant::now() + Duration::from_secs(8);
    gui.handle(UiIntent::Stop);
    let read = gui.stop_preflights.last().unwrap().0.clone();
    loop {
        gui.client.as_mut().unwrap().poll();
        if gui.outcome(&read).is_some() {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let Some((Presentation::Applied, Some(Ok(Reply::Receipt(receipt))))) = gui.outcome(&read)
    else {
        panic!("Expected actual targeted State reply")
    };
    let captured = receipt.state().unwrap().clone();
    let external = Request::mutation(
        RequestId::new(common::unique()).unwrap(),
        &owner.state(),
        Command::VolumeSet {
            millidecibels: -35000,
            ramp_frames: 64,
        },
    )
    .unwrap();
    let external_reply = owner.client().request(&external).unwrap();
    assert!(
        matches!(&external_reply, Reply::Receipt(receipt) if receipt.status == crate::ReplyStatus::Applied)
    );
    let current = owner.state();
    assert!(current.revision > captured.revision);
    assert_eq!(current.session_id, captured.session_id);
    assert_eq!(current.epochs, captured.epochs);
    assert_eq!(current.output, captured.output);
    loop {
        gui.tick(Instant::now());
        if gui.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && !r.in_flight
                && !matches!(
                    r.presentation,
                    Presentation::Queued | Presentation::Accepted
                )
        }) {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let stop = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::Stop))
        .unwrap();
    let evidence = owner.runtime.join("stop-revision-boundary.json");
    std::fs::write(&evidence, serde_json::to_vec_pretty(&serde_json::json!({"captured":captured,"current":current,"external_request":external.envelope(),"external_reply":format!("{external_reply:?}"),"stop_request":stop.request.envelope(),"stop_presentation":format!("{:?}",stop.presentation),"stop_outcome":format!("{:?}",stop.outcome)})).unwrap()).unwrap();
    println!("Actual Stop boundary evidence: {}", evidence.display());
    assert_eq!(
        stop.presentation,
        Presentation::Applied,
        "An unrelated revision change prevented stopping the same exact source"
    );
    let stopped = owner.state();
    assert!(stopped.muted);
    assert!(stopped.session_id.is_none());
    assert_eq!(stopped.transport, TransportView::Idle);
    gui.handle(UiIntent::CloseController);
    while !gui.ready_to_close() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    owner.stop();
}
