// ABOUTME: Exercises the product Play action through a real owner and private software capture.
// ABOUTME: Verifies automatic preparation, explicit consent, cancellation, and exact request ownership.
#![cfg(target_os = "linux")]
mod common;
use common::{process::ProcessOwner, unique};
use earph_core::{
    control::{Command, OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
use earph_desktop::{
    Presentation, TransportView,
    gui::{GuiController, UiIntent},
};
use std::{
    fs,
    io::{Read, Write},
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
            "Play deadline: {:?}; {:?}",
            gui.error(),
            gui.state()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn settle(gui: &mut GuiController) {
    until(gui, |g| g.queue_edit_available() && !g.busy());
}
fn edit(gui: &mut GuiController, command: Command) {
    settle(gui);
    gui.handle(UiIntent::ControlEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
    settle(gui);
}
struct Playback {
    owner: ProcessOwner,
    _proxy: Option<common::proxy::Proxy>,
    _consumer: Option<PacedCapture>,
    gui: GuiController,
    capture: PathBuf,
    frames: u32,
}
struct PacedCapture {
    wake: Option<fs::File>,
    join: Option<thread::JoinHandle<()>>,
}
impl PacedCapture {
    fn start(fifo: &std::path::Path, capture: PathBuf) -> Self {
        assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .arg(fifo)
                .status()
                .unwrap()
                .success()
        );
        let wake = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(fifo)
            .unwrap();
        let mut reader = fs::File::open(fifo).unwrap();
        let join = thread::spawn(move || {
            let mut output = fs::File::create(capture).unwrap();
            let mut bytes = [0u8; 256];
            loop {
                let count = reader.read(&mut bytes).unwrap();
                if count == 0 {
                    break;
                }
                output.write_all(&bytes[..count]).unwrap();
                thread::sleep(Duration::from_millis(2));
            }
        });
        Self {
            wake: Some(wake),
            join: Some(join),
        }
    }
}
impl Drop for PacedCapture {
    fn drop(&mut self) {
        self.wake.take();
        if self.join.take().unwrap().join().is_err() && !thread::panicking() {
            panic!("Private capture consumer failed");
        }
    }
}
impl Playback {
    fn new(consent: bool) -> Self {
        Self::with_frames(consent, 48000)
    }
    fn with_frames(consent: bool, frames: u32) -> Self {
        let directory = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&directory).unwrap();
        let config = directory.join("alsa.conf");
        let capture = directory.join("capture.pcm");
        // A private FIFO gives long-source tests bounded native backpressure.
        // Its reader keeps native progress observable without assuming null-device timing.
        let fifo = directory.join("capture.fifo");
        let consumer = (frames > 48000).then(|| PacedCapture::start(&fifo, capture.clone()));
        let destination = if consumer.is_some() { &fifo } else { &capture };
        fs::write(&config, format!("pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\n", destination.display())).unwrap();
        let source = directory.join("tone.wav");
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
        for _ in 0..frames {
            wav.extend_from_slice(&1000i16.to_le_bytes());
            wav.extend_from_slice(&(-1000i16).to_le_bytes());
        }
        fs::write(&source, wav).unwrap();
        let owner = ProcessOwner::with_configuration(Some(&config));
        let proxy = (frames > 48000).then(|| common::proxy::Proxy::forward(owner.runtime.clone()));
        let runtime = proxy
            .as_ref()
            .map_or(&owner.runtime, |proxy| &proxy.runtime);
        if proxy.is_some() {
            println!("Real owner relay artifacts: {}", runtime.display());
        }
        let mut gui = GuiController::new();
        gui.handle(UiIntent::Attach {
            runtime: runtime.to_str().unwrap().into(),
        });
        settle(&mut gui);
        let output = OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "capture".into(),
            device_id: DeviceId::new("product-play-capture").unwrap(),
            profile_id: ProfileId::new("product-play-profile").unwrap(),
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
        if consent {
            edit(
                &mut gui,
                Command::OutputConsent {
                    device_id: output.device_id,
                    profile_id: output.profile_id,
                },
            );
        }
        gui.handle(UiIntent::QueueEdit {
            basis: Arc::new(gui.state().unwrap().clone()),
            command: Command::QueueAppend {
                path: source.to_str().unwrap().into(),
            },
        });
        settle(&mut gui);
        assert!(gui.queue().unwrap().selected.is_none());
        Self {
            owner,
            _proxy: proxy,
            _consumer: consumer,
            gui,
            capture,
            frames,
        }
    }
    fn play(&mut self) {
        self.gui.handle(UiIntent::PlayLocal {
            basis: Arc::new(self.gui.state().unwrap().clone()),
            takeover: false,
            entry_id: None,
        });
    }
    fn close(&mut self) {
        self.gui.handle(UiIntent::CloseController);
        until(&mut self.gui, GuiController::ready_to_close);
        self.owner.stop().unwrap();
    }
}

#[test]
fn one_play_starts_the_first_unselected_file_and_unmutes_once_after_preparation() {
    let mut p = Playback::new(true);
    assert!(p.gui.play_local_available());
    p.play();
    assert!(p.gui.error().is_none(), "{:?}", p.gui.error());
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Mute { muted: false })
                    && r.presentation == Presentation::Applied
            })
    });
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.transport == TransportView::Idle)
    });
    let pcm = fs::read(&p.capture).unwrap();
    assert_eq!(pcm.len(), p.frames as usize * 4);
    assert!(pcm.iter().any(|b| *b != 0));
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
            .count(),
        1
    );
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Mute { muted: false }))
            .count(),
        1
    );
    p.close();
}

#[test]
fn play_never_supplies_output_consent() {
    let mut p = Playback::new(false);
    assert!(!p.gui.play_local_available());
    p.play();
    assert!(p.gui.error().is_some());
    assert!(!p.gui.play_local_pending());
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Start { .. } | Command::OutputConsent { .. }
    )));
    assert!(!p.capture.exists());
    p.close();
}

#[test]
fn mute_cancels_play_before_the_start_preflight_completes() {
    let mut p = Playback::new(true);
    p.play();
    p.gui.handle(UiIntent::Mute);
    until(&mut p.gui, |g| !g.busy());
    assert!(!p.gui.play_local_pending());
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Mute { muted: false }))
    );
    assert!(!p.capture.exists());
    p.close();
}

#[test]
fn stop_cancels_an_accepted_start_even_before_its_source_is_observed() {
    let mut p = Playback::new(true);
    p.play();
    until(&mut p.gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Start { .. })
                && r.presentation == Presentation::Accepted
                && !r.in_flight
        })
    });
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.session_id.is_none() && s.transport == TransportView::Idle)
    });
    assert!(!p.gui.play_local_pending());
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Mute { muted: false }))
    );
    assert_eq!(fs::metadata(&p.capture).map(|m| m.len()).unwrap_or(0), 0);
    p.close();
}

#[test]
fn missing_source_fails_without_an_unmute_or_a_generic_receipt_message() {
    let mut p = Playback::new(true);
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: p
                .owner
                .artifacts
                .join("missing.wav")
                .to_str()
                .unwrap()
                .into(),
        },
    });
    settle(&mut p.gui);
    let selected = p.gui.queue().unwrap().entries.last().unwrap().id.clone();
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueSelect { id: Some(selected) },
    });
    settle(&mut p.gui);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending() && g.error().is_some()
    });
    assert!(
        !p.gui
            .error()
            .unwrap()
            .contains("inspect its retained receipt")
    );
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Mute { muted: false }))
    );
    assert_eq!(fs::metadata(&p.capture).map(|m| m.len()).unwrap_or(0), 0);
    p.close();
}

#[test]
fn a_stale_play_basis_never_starts_a_source() {
    let mut p = Playback::new(true);
    let basis = Arc::new(p.gui.state().unwrap().clone());
    edit(
        &mut p.gui,
        Command::VolumeCaps {
            software_millidecibels: -30000,
            hardware_millidecibels: None,
        },
    );
    p.gui.handle(UiIntent::PlayLocal {
        basis,
        takeover: false,
        entry_id: None,
    });
    assert!(p.gui.error().is_some());
    assert!(!p.gui.play_local_pending());
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
    );
    p.close();
}

#[test]
fn play_resumes_the_active_track_after_the_queue_advances() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    let first = p.gui.queue().unwrap().entries[0].clone();
    let second_path = PathBuf::from(&first.path).with_file_name("second.wav");
    fs::copy(&first.path, &second_path).unwrap();
    let mut first_wav = fs::read(&first.path).unwrap();
    first_wav.truncate(44 + 64 * 4);
    first_wav[4..8].copy_from_slice(&(36u32 + 64 * 4).to_le_bytes());
    first_wav[40..44].copy_from_slice(&(64u32 * 4).to_le_bytes());
    fs::write(&first.path, first_wav).unwrap();
    for command in [
        Command::QueueAppend {
            path: second_path.to_str().unwrap().into(),
        },
        Command::QueueSelect {
            id: Some(first.id.clone()),
        },
    ] {
        p.gui.handle(UiIntent::QueueEdit {
            basis: Arc::new(p.gui.state().unwrap().clone()),
            command,
        });
        settle(&mut p.gui);
    }
    let second = p.gui.queue().unwrap().entries[1].id.clone();
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state().is_some_and(|s| {
                s.transport == TransportView::Playing && s.active_entry_id.as_ref() == Some(&second)
            })
    });
    assert_eq!(p.gui.queue().unwrap().selected.as_ref(), Some(&first.id));
    let session = p.gui.state().unwrap().session_id.clone();
    edit(&mut p.gui, Command::Pause);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing)
    });
    assert_eq!(
        p.gui.state().unwrap().session_id,
        session,
        "Resume restarted an earlier queue selection"
    );
    assert_eq!(
        p.gui.state().unwrap().active_entry_id.as_ref(),
        Some(&second)
    );
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
            .count(),
        1
    );
    until(&mut p.gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Resume)
                && r.presentation == Presentation::Applied
        })
    });
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    p.close();
}

#[test]
fn repeated_play_does_not_restart_and_play_resumes_the_same_paused_entry() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing)
    });
    settle(&mut p.gui);
    let session = p.gui.state().unwrap().session_id.clone();
    p.play();
    assert!(!p.gui.play_local_pending());
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
            .count(),
        1
    );
    edit(&mut p.gui, Command::Pause);
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.transport == TransportView::Paused)
            && !g.busy()
    });
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing)
    });
    assert_eq!(p.gui.state().unwrap().session_id, session);
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
            .count(),
        1
    );
    assert_eq!(
        p.gui
            .receipts()
            .filter(|r| matches!(r.request.envelope().payload, Command::Resume))
            .count(),
        1
    );
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    p.close();
}

#[test]
fn play_on_a_different_local_entry_replaces_the_muted_preparation_without_a_takeover_checkbox() {
    let mut p = Playback::new(true);
    let first = p.gui.queue().unwrap().entries[0].clone();
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueSelect {
            id: Some(first.id.clone()),
        },
    });
    settle(&mut p.gui);
    p.gui.handle(UiIntent::StartLocal {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        takeover: false,
    });
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.muted && s.playback.as_ref().is_some_and(|p| p.backend.is_some()))
            && !g.busy()
    });
    settle(&mut p.gui);
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueAppend { path: first.path },
    });
    assert!(p.gui.error().is_none(), "{:?}", p.gui.error());
    settle(&mut p.gui);
    assert_eq!(p.gui.queue().unwrap().entries.len(), 2);
    let second = p.gui.queue().unwrap().entries.last().unwrap().id.clone();
    let old_session = p.gui.state().unwrap().session_id.clone();
    p.gui.handle(UiIntent::PlayLocal {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        takeover: false,
        entry_id: Some(second.clone()),
    });
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Mute { muted: false })
                    && r.presentation == Presentation::Applied
            })
    });
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.transport == TransportView::Idle)
    });
    let starts: Vec<_> = p
        .gui
        .receipts()
        .filter(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
        .collect();
    assert_eq!(starts.len(), 2);
    assert!(
        starts.iter().any(
            |r| matches!(r.request.envelope().payload, Command::Start { .. })
                && r.request.envelope().session_id != old_session
        ),
        "Replacement requests: {:?}",
        starts
            .iter()
            .map(|r| (
                &r.request.envelope().payload,
                &r.request.envelope().session_id
            ))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        p.gui
            .state()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .source_entry_id,
        second
    );
    assert_eq!(
        fs::metadata(&p.capture).unwrap().len(),
        u64::from(p.frames) * 4
    );
    p.close();
}

#[test]
fn absolute_volume_uses_current_progress_revision_and_preserves_mute() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    let basis = Arc::new(p.gui.state().unwrap().clone());
    let submitted = basis.playback.as_ref().unwrap().submitted_frames;
    until(&mut p.gui, |g| {
        g.state().is_some_and(|s| {
            s.playback
                .as_ref()
                .is_some_and(|p| p.submitted_frames > submitted)
        })
    });
    edit(
        &mut p.gui,
        Command::VolumeSet {
            millidecibels: -35000,
            ramp_frames: 64,
        },
    );
    assert!(p.gui.state().unwrap().revision > basis.revision);
    assert_eq!(p.gui.state().unwrap().epochs, basis.epochs);
    assert!(p.gui.volume_edit_available());
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -25000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(
                    r.request.envelope().payload,
                    Command::VolumeSet {
                        millidecibels: -25000,
                        ..
                    } | Command::VolumeSetGuarded {
                        millidecibels: -25000,
                        ..
                    }
                ) && r.presentation == Presentation::Applied
            })
    });
    assert!(!p.gui.state().unwrap().muted);
    until(&mut p.gui, |g| {
        g.state().is_some_and(|s| s.software_gain_db == -25.0)
    });
    let before_mute = p.gui.state().unwrap().revision;
    p.gui.handle(UiIntent::Mute);
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.muted && s.revision > before_mute)
            && g.volume_edit_available()
    });
    let basis = Arc::new(p.gui.state().unwrap().clone());
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -26000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.muted && s.software_gain_db == -26.0)
    });
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    p.close();
}

#[test]
fn absolute_volume_never_retargets_a_stopped_source_or_a_changed_policy() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    let basis = Arc::new(p.gui.state().unwrap().clone());
    p.gui.handle(UiIntent::Mute);
    until(&mut p.gui, |g| {
        g.state()
            .is_some_and(|s| s.muted && s.revision > basis.revision)
            && g.volume_edit_available()
    });
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -24000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| !g.busy());
    assert!(
        p.gui.error().is_some(),
        "Pre-Mute volume gesture was not cancelled"
    );
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::VolumeSet {
            millidecibels: -24000,
            ..
        } | Command::VolumeSetGuarded {
            millidecibels: -24000,
            ..
        }
    )));
    let basis = Arc::new(p.gui.state().unwrap().clone());
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -25000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| !g.busy() && g.error().is_some());
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::VolumeSet {
            millidecibels: -25000,
            ..
        } | Command::VolumeSetGuarded {
            millidecibels: -25000,
            ..
        }
    )));
    settle(&mut p.gui);
    let basis = Arc::new(p.gui.state().unwrap().clone());
    edit(
        &mut p.gui,
        Command::VolumeCaps {
            software_millidecibels: -30000,
            hardware_millidecibels: None,
        },
    );
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -35000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| !g.busy() && g.error().is_some());
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::VolumeSet {
            millidecibels: -35000,
            ..
        } | Command::VolumeSetGuarded {
            millidecibels: -35000,
            ..
        }
    )));
    settle(&mut p.gui);
    let basis = Arc::new(p.gui.state().unwrap().clone());
    let mut output = basis.output.clone().unwrap();
    output.device_id = DeviceId::new("different-output").unwrap();
    edit(&mut p.gui, Command::OutputSelect { output });
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -36000,
        ramp_frames: 64,
    });
    assert!(p.gui.error().is_some());
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::VolumeSet {
            millidecibels: -36000,
            ..
        } | Command::VolumeSetGuarded {
            millidecibels: -36000,
            ..
        }
    )));
    assert!(!p.gui.state().unwrap().output_consented);
    p.close();
}

#[test]
fn live_local_replacement_accepts_progress_but_not_an_unknown_draft_entry() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    let path = p.gui.queue().unwrap().entries[0].path.clone();
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueAppend { path },
    });
    settle(&mut p.gui);
    let second = p.gui.queue().unwrap().entries[1].id.clone();
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    let basis = Arc::new(p.gui.state().unwrap().clone());
    let old_session = basis.session_id.clone();
    let submitted = basis.playback.as_ref().unwrap().submitted_frames;
    until(&mut p.gui, |g| {
        g.play_local_available()
            && g.state().is_some_and(|s| {
                s.playback
                    .as_ref()
                    .is_some_and(|p| p.submitted_frames > submitted)
            })
    });
    let current = p.gui.state().unwrap().active_entry_id.clone();
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueSelect { id: current },
    });
    settle(&mut p.gui);
    assert!(p.gui.state().unwrap().revision > basis.revision);
    p.gui.handle(UiIntent::PlayLocal {
        basis: basis.clone(),
        takeover: false,
        entry_id: Some(earph_core::ids::QueueEntryId::new("absent-entry").unwrap()),
    });
    assert!(
        p.gui
            .error()
            .unwrap()
            .contains("no longer in the current queue")
    );
    assert!(!p.gui.play_local_pending());
    p.gui.handle(UiIntent::PlayLocal {
        basis,
        takeover: false,
        entry_id: Some(second.clone()),
    });
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state().is_some_and(|s| {
                s.active_entry_id.as_ref() == Some(&second)
                    && s.session_id != old_session
                    && s.transport == TransportView::Playing
                    && !s.muted
            })
    });
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    p.close();
}

#[test]
fn mute_during_local_replacement_stop_cancels_the_next_start() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    let path = p.gui.queue().unwrap().entries[0].path.clone();
    p.gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        command: Command::QueueAppend { path },
    });
    settle(&mut p.gui);
    let second = p.gui.queue().unwrap().entries[1].id.clone();
    p.play();
    until(&mut p.gui, |g| {
        g.play_local_available()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    let old_session = p.gui.state().unwrap().session_id.clone();
    p.gui.handle(UiIntent::PlayLocal {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        takeover: false,
        entry_id: Some(second),
    });
    until(&mut p.gui, |g| {
        g.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Stop))
    });
    p.gui.handle(UiIntent::Mute);
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Idle)
    });
    assert!(!p.gui.play_local_pending());
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Start { .. }
    ) && r.request.envelope().session_id != old_session));
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::QueueSelect { .. }))
    );
    p.close();
}

#[test]
fn external_volume_policy_change_during_preparation_cancels_automatic_unmute() {
    let mut p = Playback::new(true);
    p.play();
    until(&mut p.gui, |g| {
        g.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Start { .. }))
    });
    let client =
        earph_desktop::OwnerClient::attach(p.owner.runtime.to_str().unwrap().into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let prepared = loop {
        let earph_desktop::Reply::Receipt(reply) =
            client.request(&common::query(Command::State)).unwrap()
        else {
            panic!("Expected real State receipt")
        };
        let state = reply.state().unwrap();
        if state.playback.as_ref().is_some_and(|p| p.backend.is_some()) {
            break state.clone();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    let reply = client
        .request(
            &earph_desktop::Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &prepared,
                Command::VolumeCaps {
                    software_millidecibels: -10000,
                    hardware_millidecibels: None,
                },
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        matches!(reply,earph_desktop::Reply::Receipt(r) if r.status == earph_desktop::ReplyStatus::Applied)
    );
    until(&mut p.gui, |g| !g.play_local_pending());
    assert!(
        p.gui.error().is_some(),
        "Changed policy was silently adopted by product Play"
    );
    assert!(
        !p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Mute { muted: false })),
        "Changed policy allowed an automatic Unmute request"
    );
    assert_eq!(fs::metadata(&p.capture).map(|m| m.len()).unwrap_or(0), 0);
    p.close();
}

#[test]
fn an_older_volume_gesture_cannot_clear_a_later_exact_silence() {
    let mut p = Playback::new(true);
    let basis = Arc::new(p.gui.state().unwrap().clone());
    let client =
        earph_desktop::OwnerClient::attach(p.owner.runtime.to_str().unwrap().into()).unwrap();
    let reply = client
        .request(
            &earph_desktop::Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::VolumeSilence,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        matches!(reply, earph_desktop::Reply::Receipt(r) if r.status == earph_desktop::ReplyStatus::Applied)
    );
    p.gui.handle(UiIntent::Refresh);
    until(&mut p.gui, |g| {
        !g.busy() && g.state().is_some_and(|s| s.software_gain_zero)
    });
    assert!(p.gui.state().unwrap().software_gain_zero);
    assert_eq!(p.gui.state().unwrap().muted, basis.muted);
    assert_eq!(p.gui.state().unwrap().epochs, basis.epochs);
    p.gui.handle(UiIntent::SetVolume {
        basis,
        millidecibels: -25000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| !g.busy());
    assert!(
        p.gui.error().is_some(),
        "An older slider gesture cleared a later exact-zero choice"
    );
    assert!(p.gui.state().unwrap().software_gain_zero);
    assert!(!p.gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::VolumeSet {
            millidecibels: -25000,
            ..
        } | Command::VolumeSetGuarded {
            millidecibels: -25000,
            ..
        }
    )));
    p.close();
}

fn volume_after_preflight_safety(safety: Command) -> (Playback, Arc<earph_desktop::StateView>) {
    let mut p = Playback::with_frames(true, 48001);
    settle(&mut p.gui);
    let basis = Arc::new(p.gui.state().unwrap().clone());
    assert!(basis.muted);
    let proxy = p._proxy.as_ref().unwrap();
    proxy.hold_next_state_reply();
    p.gui.handle(UiIntent::SetVolume {
        basis: basis.clone(),
        millidecibels: -26000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |_| proxy.entered.try_recv().is_ok());
    let client =
        earph_desktop::OwnerClient::attach(p.owner.runtime.to_str().unwrap().into()).unwrap();
    let reply = client
        .request(
            &earph_desktop::Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                safety,
            )
            .unwrap(),
        )
        .unwrap();
    let earph_desktop::Reply::Receipt(receipt) = reply else {
        panic!("missing actual safety receipt")
    };
    assert_eq!(receipt.status, earph_desktop::ReplyStatus::Applied);
    assert!(receipt.revision > basis.revision);
    proxy.release();
    until(&mut p.gui, |g| {
        !g.busy()
            && (g.action_error().is_some()
                || g.state().is_some_and(|s| s.software_gain_db == -26.0))
    });
    (p, basis)
}

#[test]
fn absolute_volume_applies_once_when_revision_changes_after_its_preflight() {
    let (mut p, basis) = volume_after_preflight_safety(Command::Mute { muted: true });
    assert!(p.gui.action_error().is_none(), "{:?}", p.gui.action_error());
    let state = p.gui.state().unwrap();
    assert_eq!(state.software_gain_db, -26.0);
    assert!(state.muted);
    assert_eq!(state.session_id, basis.session_id);
    let requests = p._proxy.as_ref().unwrap().frames.lock().unwrap();
    let volume = requests
        .iter()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).unwrap())
        .filter(|value| value["payload"]["method"] == "volume.set_guarded")
        .collect::<Vec<_>>();
    assert_eq!(volume.len(), 1);
    assert_eq!(volume[0]["expected_revision"], basis.revision);
    drop(requests);
    p.close();
}

#[test]
fn a_later_exact_silence_rejects_the_intercepted_volume_and_explains_why() {
    let (mut p, basis) = volume_after_preflight_safety(Command::VolumeSilence);
    assert!(
        p.gui
            .action_error()
            .unwrap()
            .contains("volume context changed"),
        "{:?}",
        p.gui.action_error()
    );
    until(&mut p.gui, |g| {
        g.state().is_some_and(|s| s.software_gain_zero)
    });
    let state = p.gui.state().unwrap();
    assert!(state.software_gain_zero);
    assert_eq!(state.software_gain_db, basis.software_gain_db);
    assert_eq!(state.muted, basis.muted);
    p.close();
}

#[test]
fn an_older_zero_gesture_cannot_clear_a_newer_repeated_silence() {
    let mut p = Playback::with_frames(true, 48001);
    let client =
        earph_desktop::OwnerClient::attach(p.owner.runtime.to_str().unwrap().into()).unwrap();
    for repeated in [false, true] {
        let request = earph_desktop::Request::mutation(
            earph_core::ids::RequestId::new(unique()).unwrap(),
            p.gui.state().unwrap(),
            Command::VolumeSilence,
        )
        .unwrap();
        let reply = client.request(&request).unwrap();
        assert!(
            matches!(reply, earph_desktop::Reply::Receipt(receipt) if receipt.status == earph_desktop::ReplyStatus::Applied)
        );
        if !repeated {
            p.gui.handle(UiIntent::Refresh);
            settle(&mut p.gui);
            assert!(p.gui.state().unwrap().software_gain_zero);
        }
    }
    let original = Arc::new(p.gui.state().unwrap().clone());
    p.gui.handle(UiIntent::SetVolume {
        basis: original.clone(),
        millidecibels: -26000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| {
        !g.busy()
            && (g.action_error().is_some()
                || g.state().is_some_and(|s| s.software_gain_db == -26.0))
    });
    assert!(
        p.gui.action_error().is_some(),
        "An old zero-based gesture cleared a later repeated Silence"
    );
    let reply = client.request(&common::query(Command::State)).unwrap();
    let earph_desktop::Reply::Receipt(receipt) = reply else {
        panic!("missing current owner state")
    };
    let state = receipt.state().unwrap();
    assert!(state.software_gain_zero);
    assert_eq!(state.software_gain_db, original.software_gain_db);
    p.gui.handle(UiIntent::Refresh);
    settle(&mut p.gui);
    p.gui.handle(UiIntent::SetVolume {
        basis: Arc::new(p.gui.state().unwrap().clone()),
        millidecibels: -26000,
        ramp_frames: 64,
    });
    until(&mut p.gui, |g| {
        !g.busy()
            && g.state()
                .is_some_and(|s| !s.software_gain_zero && s.software_gain_db == -26.0)
    });
    assert!(p.gui.action_error().is_none(), "{:?}", p.gui.action_error());
    p.close();
}

#[test]
fn stop_after_mute_targets_the_same_source_during_native_transport_changes() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    let revision = p.gui.state().unwrap().revision;
    p.gui.handle(UiIntent::Mute);
    until(&mut p.gui, |g| {
        g.state().is_some_and(|s| s.muted && s.revision > revision)
    });
    p.gui.handle(UiIntent::Stop);
    until(&mut p.gui, |g| {
        g.queue_edit_available()
            && g.state().is_some_and(|s| {
                s.transport == TransportView::Idle && s.muted && s.session_id.is_none()
            })
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Stop)
                    && !r.in_flight
                    && !matches!(
                        r.presentation,
                        Presentation::Queued | Presentation::Accepted
                    )
            })
    });
    assert!(
        p.gui
            .receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Applied)
    );
    assert!(
        p.gui
            .state()
            .is_some_and(|s| s.transport == TransportView::Idle
                && s.muted
                && s.session_id.is_none())
    );
    assert!(p.gui.error().is_none(), "{:?}", p.gui.error());
    p.close();
}

#[test]
fn quit_during_playback_stops_the_source_and_the_engine_exits() {
    let mut p = Playback::with_frames(true, 48000 * 60);
    p.play();
    until(&mut p.gui, |g| {
        !g.play_local_pending()
            && g.state()
                .is_some_and(|s| s.transport == TransportView::Playing && !s.muted)
    });
    p.gui.handle(UiIntent::Quit);
    until(&mut p.gui, GuiController::ready_to_close);
    let deadline = Instant::now() + Duration::from_secs(8);
    let status = loop {
        if let Some(status) = p.owner.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "Quit left the Engine playing");
        thread::sleep(Duration::from_millis(2));
    };
    assert!(status.success(), "Engine exit after Quit: {status}");
    assert!(
        fs::metadata(&p.capture).unwrap().len() < u64::from(p.frames) * 4,
        "the whole source played before the Engine exited"
    );
}
