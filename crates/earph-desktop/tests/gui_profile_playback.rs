// ABOUTME: Exercises explicit graph and profile preparation against a standalone owned software sink.
// ABOUTME: Proves activation receipts, token kinds, source fences, and muted gain policy without physical output.
#![cfg(target_os = "linux")]
mod common;
use common::{process::ProcessOwner, *};
use earph_core::{
    control::{Command, OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
use earph_desktop::{
    Presentation, Reply, ResultView,
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
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        gui.tick(Instant::now());
        if predicate(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "live profile workflow timed out: {:?}; {:?}",
            gui.error(),
            gui.state()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn profile(gui: &mut GuiController, command: Command) {
    until(gui, |g| !g.busy() && g.control_edit_available());
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
}
fn control(gui: &mut GuiController, command: Command) {
    until(gui, |g| !g.busy() && g.control_edit_available());
    gui.handle(UiIntent::ControlEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
}
fn queue(gui: &mut GuiController, command: Command) {
    until(gui, |g| !g.busy() && g.queue_edit_available());
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
    assert!(gui.error().is_none(), "{:?}", gui.error());
}

#[test]
fn actual_muted_graph_validation_commit_and_saved_profile_apply_keep_kind_and_authorization_separate()
 {
    let artifacts = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&artifacts).unwrap();
    let configuration = artifacts.join("alsa.conf");
    let capture = artifacts.join("capture.pcm");
    let config_bytes = format!(
        "pcm.null {{ type null }}\npcm.owned_capture {{ type file slave.pcm null file \"{}\" format raw }}\n",
        capture.display()
    );
    fs::write(&configuration, &config_bytes).unwrap();
    let source = artifacts.join("source.wav");
    let frames = 4096u32;
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
    fs::write(&source, &samples).unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&configuration));
    let transport = proxy::Proxy::drop_first_graph_prepare(owner.runtime.clone());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    profile(&mut gui, profile_payload());
    until(&mut gui, |g| !g.busy() && g.profile_record().is_some());
    let saved = gui.profile_record().unwrap().clone();
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "owned_capture".into(),
        device_id: DeviceId::new("offline").unwrap(),
        profile_id: ProfileId::new("desktop-profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    control(
        &mut gui,
        Command::OutputSelect {
            output: output.clone(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.as_ref() == Some(&output)
    });
    control(
        &mut gui,
        Command::OutputConsent {
            device_id: output.device_id.clone(),
            profile_id: output.profile_id.clone(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output_consented
    });
    queue(
        &mut gui,
        Command::QueueAppend {
            path: source.to_str().unwrap().into(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.queue().is_some_and(|q| !q.entries.is_empty())
    });
    let entry = gui.queue().unwrap().entries[0].id.clone();
    queue(
        &mut gui,
        Command::QueueSelect {
            id: Some(entry.clone()),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.queue().unwrap().selected.as_ref() == Some(&entry)
    });
    gui.handle(UiIntent::StartLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
    });
    until(&mut gui, |g| {
        !g.busy()
            && g.state()
                .unwrap()
                .playback
                .as_ref()
                .is_some_and(|p| p.backend.is_some())
    });
    let gain = gui.state().unwrap().software_gain_db;
    profile(
        &mut gui,
        Command::GraphValidate {
            graph: saved.graph.graph.clone(),
            processing: saved.graph.processing,
        },
    );
    until(&mut gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::GraphValidate { .. })
                    && r.presentation == Presentation::Applied
            })
    });
    assert!(
        gui.prepared_graph().is_none(),
        "validation unexpectedly granted an activation token"
    );
    profile(
        &mut gui,
        Command::GraphPrepare {
            graph: saved.graph.graph.clone(),
            processing: saved.graph.processing,
        },
    );
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::GraphPrepare { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let request = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::GraphPrepare { .. }))
        .unwrap();
    let lost_id = request.request.envelope().request_id.clone();
    let exact_bytes = request.request.bytes().to_vec();
    // A changed canonical draft discards its selected candidate before exact recovery.
    gui.handle(UiIntent::DiscardPreparedGraph);
    gui.handle(UiIntent::Reconcile(lost_id.clone()));
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            r.request.envelope().request_id == lost_id && r.presentation == Presentation::Applied
        })
    });
    until(&mut gui, |g| !g.busy() && g.control_edit_available());
    assert!(gui.prepared_graph().is_none());
    let recovered = gui
        .receipts()
        .find_map(|r| {
            if r.request.envelope().request_id != lost_id {
                return None;
            }
            let Some(Ok(Reply::Receipt(receipt))) = r.outcome else {
                return None;
            };
            let Some(ResultView::GraphPrepared(prepared)) = &receipt.result else {
                return None;
            };
            Some(prepared.clone())
        })
        .unwrap();
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::GraphCommit {
            token: recovered.token,
            graph_hash: recovered.preview.graph_hash.as_str().into(),
        },
    });
    assert!(
        gui.error()
            .is_some_and(|e| e.contains("fresh prepared candidate")),
        "reconciled candidate refusal: {:?}",
        gui.error()
    );
    let prepares: Vec<_> = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|bytes| {
            matches!(
                serde_json::from_slice::<earph_core::control::CommandEnvelope>(bytes)
                    .unwrap()
                    .payload,
                Command::GraphPrepare { .. }
            )
        })
        .cloned()
        .collect();
    assert!(prepares.len() >= 2 && prepares.iter().all(|bytes| bytes == &exact_bytes));
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::GraphCommit { .. }))
    );
    // The owner still owns the discarded candidate. A separate safety action changes context.
    let previous_revision = gui.state().unwrap().revision;
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().revision > previous_revision
    });
    profile(
        &mut gui,
        Command::GraphPrepare {
            graph: saved.graph.graph.clone(),
            processing: saved.graph.processing,
        },
    );
    until(&mut gui, |g| !g.busy() && g.prepared_graph().is_some());
    let ordinary = gui.prepared_graph().unwrap().clone();
    assert!(ordinary.preview.profile_application.is_none());
    let before = gui.state().unwrap().revision;
    gui.handle(UiIntent::ProfileAction {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::ProfilesApply {
            id: saved.profile.id.clone(),
            expected_profile_revision: saved.profile.revision,
            profile_sha256: saved.profile_sha256,
            token: ordinary.token.clone(),
            graph_hash: ordinary.preview.graph_hash.as_str().into(),
        },
    });
    assert!(gui.error().is_some());
    assert_eq!(gui.state().unwrap().revision, before);
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::ProfilesApply { .. }))
    );
    profile(
        &mut gui,
        Command::GraphCommit {
            token: ordinary.token,
            graph_hash: ordinary.preview.graph_hash.as_str().into(),
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.graph_inspection().is_some_and(|v| v.active.is_some())
    });
    assert!(gui.state().unwrap().muted && gui.state().unwrap().configured_profile.is_none());
    profile(
        &mut gui,
        Command::ProfilesPrepare {
            id: saved.profile.id.clone(),
            expected_profile_revision: saved.profile.revision,
            profile_sha256: saved.profile_sha256,
        },
    );
    until(&mut gui, |g| {
        !g.busy()
            && g.prepared_graph()
                .is_some_and(|p| p.preview.profile_application.is_some())
    });
    let prepared = gui.prepared_graph().unwrap().clone();
    let binding = prepared.preview.profile_application.as_ref().unwrap();
    assert_eq!(binding.profile.id, saved.profile.id);
    assert_eq!(binding.profile.revision, saved.profile.revision);
    assert_eq!(binding.profile.profile_sha256, saved.profile_sha256);
    profile(
        &mut gui,
        Command::ProfilesApply {
            id: saved.profile.id.clone(),
            expected_profile_revision: saved.profile.revision,
            profile_sha256: saved.profile_sha256,
            token: prepared.token,
            graph_hash: prepared.preview.graph_hash.as_str().into(),
        },
    );
    until(&mut gui, |g| {
        !g.busy()
            && g.state()
                .unwrap()
                .configured_profile
                .as_ref()
                .is_some_and(|r| r.profile_sha256 == saved.profile_sha256)
    });
    assert!(gui.state().unwrap().muted && !gui.state().unwrap().output_consented);
    assert_eq!(gui.state().unwrap().software_gain_db, gain);
    assert!(
        gui.graph_inspection()
            .unwrap()
            .active
            .as_ref()
            .unwrap()
            .profile_application
            .is_some()
    );
    assert!(gui.prepared_graph().is_none());
    assert_eq!(fs::metadata(&capture).map(|m| m.len()).unwrap_or(0), 0);
    assert_eq!(fs::read(&source).unwrap(), samples);
    assert_eq!(fs::read_to_string(&configuration).unwrap(), config_bytes);
    gui.handle(UiIntent::Stop);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Stop)
                && r.presentation == Presentation::Applied
        })
    });
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
    owner.stop().unwrap();
    fs::write(artifacts.join("profile-gui-result.json"), serde_json::to_vec_pretty(&serde_json::json!({"status":"pass","owner_exit":0,"source_unchanged":true,"muted":true,"capture_bytes":0,"gain_db":gain,"saved_profile_hash":saved.profile_sha256,"physical_output":false})).unwrap()).unwrap();
    println!("Profile GUI software fixture: {}", artifacts.display());
}
