// ABOUTME: Runs explicit native transport controls against an owned standalone software output.
// ABOUTME: Verifies Accepted muted operations never block observation or independent safety actions.
#![cfg(target_os = "linux")]
mod common;
use common::process::ProcessOwner;
use common::*;
use earph_core::{
    control::{Command, OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
use earph_desktop::{
    Presentation,
    gui::{GuiController, UiIntent},
};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

#[track_caller]
fn until(gui: &mut GuiController, predicate: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        gui.tick(Instant::now());
        if predicate(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "transport timed out: {:?}; busy {}; receipts {:?}; {:?}",
            gui.error(),
            gui.busy(),
            gui.receipts()
                .map(|r| (&r.request.envelope().payload, r.presentation, r.in_flight))
                .collect::<Vec<_>>(),
            gui.state()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn edit(gui: &mut GuiController, command: Command) {
    until(gui, |g| g.control_edit_available() && !g.busy());
    gui.handle(UiIntent::ControlEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
}
#[test]
#[cfg(target_os = "linux")]
fn muted_start_and_resume_keep_exact_receipts_while_pause_seek_and_safety_remain_available() {
    let artifacts = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("alsa.conf");
    let capture = artifacts.join("capture.pcm");
    fs::write(&config, format!("pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{}\" format raw hint {{ show on description \"Owned software sink\" }} }}\n", capture.display())).unwrap();
    let fixture = artifacts.join("zero.wav");
    let frames = 48000u32 * 20;
    let mut samples = [
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
    samples.resize(44 + frames as usize * 4, 0);
    fs::write(&fixture, &samples).unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&config));
    let proxy = proxy::Proxy::drop_first_start(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "earph_capture".into(),
        device_id: DeviceId::new("owned-software").unwrap(),
        profile_id: ProfileId::new("owned-profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    edit(
        &mut gui,
        Command::OutputSelect {
            output: output.clone(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.is_some()
    });
    edit(
        &mut gui,
        Command::OutputConsent {
            device_id: output.device_id.clone(),
            profile_id: output.profile_id.clone(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output_consented
    });
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: fixture.to_str().unwrap().into(),
        },
    });
    until(&mut gui, |g| {
        !g.busy() && g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    let selected = gui.queue().unwrap().entries[0].id.clone();
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueSelect {
            id: Some(selected.clone()),
        },
    });
    until(&mut gui, |g| {
        !g.busy() && g.queue().unwrap().selected.as_ref() == Some(&selected)
    });
    gui.handle(UiIntent::StartLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Start { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let uncertain = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
        .unwrap();
    let retained_id = uncertain.request.envelope().request_id.clone();
    let retained_bytes = uncertain.request.bytes().to_vec();
    gui.handle(UiIntent::StartLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
    });
    assert!(
        gui.error().is_some(),
        "unknown Start allowed replacement identity"
    );
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: proxy.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    assert!(!gui.control_edit_available());
    gui.handle(UiIntent::Reconcile(retained_id.clone()));
    until(&mut gui, |g| {
        g.state()
            .unwrap()
            .playback
            .as_ref()
            .is_some_and(|p| p.backend.is_some())
            && !g.busy()
            && g.receipts().any(|r| {
                r.request.envelope().request_id == retained_id
                    && r.presentation == Presentation::Accepted
                    && !r.in_flight
            })
    });
    let start = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
        .unwrap();
    assert_eq!(start.presentation, Presentation::Accepted);
    assert_eq!(start.request.envelope().request_id, retained_id);
    assert_eq!(start.request.bytes(), retained_bytes);
    let id = start.request.envelope().request_id.clone();
    let bytes = start.request.bytes().to_vec();
    let session = start.request.envelope().session_id.clone();
    assert_eq!(gui.state().unwrap().session_id, session);
    assert!(gui.state().unwrap().muted);
    assert!(gui.control_edit_available());
    gui.handle(UiIntent::ControlEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::Seek {
            position_frames: u64::from(frames) + 1,
        },
    });
    assert!(gui.error().is_some());
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Seek { .. }))
    );
    let stale_seek_basis = Arc::new(gui.state().unwrap().clone());
    edit(&mut gui, Command::Pause);
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().transport == earph_desktop::TransportView::Paused
    });
    gui.handle(UiIntent::ControlEdit {
        basis: stale_seek_basis,
        command: Command::Seek {
            position_frames: 128,
        },
    });
    until(&mut gui, |g| {
        g.error().is_some_and(|e| e.contains("changed"))
    });
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Seek { .. }))
    );
    edit(
        &mut gui,
        Command::Seek {
            position_frames: 128,
        },
    );
    until(&mut gui, |g| {
        !g.busy()
            && g.state()
                .unwrap()
                .playback
                .as_ref()
                .is_some_and(|p| p.source_position_frames == 128)
    });
    edit(&mut gui, Command::Pause);
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().transport == earph_desktop::TransportView::Paused
    });
    edit(&mut gui, Command::Resume);
    until(&mut gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Resume)
                    && r.presentation == Presentation::Accepted
            })
    });
    assert!(gui.state().unwrap().muted);
    assert_eq!(fs::metadata(&capture).map(|m| m.len()).unwrap_or(0), 0);
    let gain = gui.state().unwrap().software_gain_db;
    edit(&mut gui, Command::Mute { muted: false });
    until(&mut gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Mute { muted: false })
                    && r.presentation == Presentation::Applied
            })
    });
    assert_eq!(gui.state().unwrap().software_gain_db, gain);
    let before_mute = gui.state().unwrap().revision;
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.state().unwrap().muted && g.state().unwrap().revision > before_mute && !g.busy()
    });
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Stop)
                    && r.presentation == Presentation::Applied
            })
    });
    if let Some(start) = gui
        .receipts()
        .find(|r| r.request.envelope().request_id == id)
    {
        assert_eq!(start.request.bytes(), bytes);
    }
    assert_eq!(fs::read(&fixture).unwrap(), samples);
    assert!(gui.state().unwrap().muted);
    let captured = fs::read(&capture).unwrap();
    assert!(
        !captured.is_empty(),
        "explicit Unmute never reached the owned software sink"
    );
    assert!(
        captured.iter().all(|byte| *byte == 0),
        "generated zero PCM changed in the software fixture"
    );
    fs::write(artifacts.join("start-request.json"), &retained_bytes).unwrap();
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
    owner.stop().unwrap();
    let starts: Vec<_> = proxy
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|b| {
            serde_json::from_slice::<serde_json::Value>(b).unwrap()["payload"]["method"]
                == "sessions.start"
        })
        .cloned()
        .collect();
    assert!(starts.len() >= 2);
    assert!(starts.iter().all(|b| b == &retained_bytes));
    fs::write(artifacts.join("transport-result.json"), serde_json::to_vec_pretty(&serde_json::json!({"status":"pass","owner_pid":owner.child.id(),"session_id":session,"capture_bytes":captured.len(),"capture_zero_pcm":true,"source_bytes_unchanged":true,"start_exact_replay":true,"muted_start_accepted":true,"muted_resume_accepted":true,"gain_db":gain,"physical_output":false,"owner_exit":0})).unwrap()).unwrap();
    println!(
        "Transport software fixture retained at {}",
        artifacts.display()
    );
}
