// ABOUTME: Exercises explicit output drafts and owner controls through the native workflow.
// ABOUTME: Proves stale and waiting actions cannot silently select, consent, or change gain.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
#[cfg(target_os = "linux")]
use common::process::ProcessOwner;
use common::*;
use earph_core::{
    control::{Command, OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
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
    until(&mut gui, |g| g.connected() && !g.busy());
    gui
}
fn output() -> OutputConfiguration {
    OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "explicit-unopened-endpoint".into(),
        device_id: DeviceId::new("configured-device").unwrap(),
        profile_id: ProfileId::new("configured-profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    }
}
fn edit(gui: &mut GuiController, command: Command) {
    gui.handle(UiIntent::ControlEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command,
    });
}

#[test]
fn output_selection_consent_caps_and_level_remain_separate_explicit_actions() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let gain = owner.state().software_gain_db;
    edit(&mut gui, Command::OutputSelect { output: output() });
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.is_some()
    });
    let selected = owner.state();
    assert!(!selected.output_consented && selected.muted);
    assert_eq!(selected.software_gain_db, gain);
    assert!(selected.playback.is_none() && selected.session_id.is_none());
    edit(
        &mut gui,
        Command::OutputConsent {
            device_id: output().device_id,
            profile_id: output().profile_id,
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output_consented
    });
    edit(
        &mut gui,
        Command::VolumeCaps {
            software_millidecibels: -20000,
            hardware_millidecibels: None,
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().local_volume_policy.software_cap_db == -20.0
    });
    edit(
        &mut gui,
        Command::VolumeSet {
            millidecibels: -30000,
            ramp_frames: 96,
        },
    );
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().software_gain_db == -30.0
    });
    assert_eq!(owner.state().gain_ramp_frames, 96);
    assert!(owner.state().muted && owner.state().playback.is_none());
    assert!(!gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Start { .. } | Command::FixedArm { .. } | Command::Mute { muted: false }
    )));
}

#[test]
fn stale_output_approval_is_refused_before_mutation_without_retargeting() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    edit(&mut gui, Command::OutputSelect { output: output() });
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.is_some()
    });
    let basis = Arc::new(gui.state().unwrap().clone());
    owner
        .client()
        .request(
            &Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                &basis,
                Command::VolumeCaps {
                    software_millidecibels: -20000,
                    hardware_millidecibels: None,
                },
            )
            .unwrap(),
        )
        .unwrap();
    gui.handle(UiIntent::ControlEdit {
        basis,
        command: Command::OutputConsent {
            device_id: output().device_id,
            profile_id: output().profile_id,
        },
    });
    until(&mut gui, |g| g.error().is_some());
    assert!(gui.error().unwrap().contains("changed"));
    assert!(!owner.state().output_consented);
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::OutputConsent { .. }))
    );
}

#[test]
fn output_and_queue_share_one_waiting_slot_and_mute_cancels_unsent_output() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    transport.hold_next_state();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |_| transport.state_held());
    assert!(gui.control_edit_available());
    edit(&mut gui, Command::OutputSelect { output: output() });
    assert!(gui.error().is_none());
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: "/refused.wav".into(),
        },
    });
    assert!(gui.error().is_some());
    gui.handle(UiIntent::Mute);
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::Mute { muted: true })
                && r.presentation == Presentation::Applied
        })
    });
    transport.release_state();
    until(&mut gui, |g| !g.busy());
    assert!(owner.state().output.is_none());
    assert!(owner.state().muted);
}

#[test]
fn control_admission_refuses_commands_outside_output_and_transport_scope() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    edit(&mut gui, Command::OwnerShutdown);
    assert!(gui.error().is_some());
    assert!(!owner.state().shutdown_requested);
}

#[test]
#[cfg(target_os = "linux")]
fn explicit_owner_discovery_retains_exact_inventory_and_inspection_without_selecting_output() {
    use earph_desktop::DeviceObservationView;
    let artifacts = std::path::PathBuf::from("/tmp").join(unique());
    std::fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("alsa.conf");
    std::fs::write(&config, "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.null { type null hint { show on description \"Owned duplicate label\" } }\npcm.owned_other { type null hint { show on description \"Owned duplicate label\" } }\n").unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&config));
    let proxy = proxy::Proxy::drop_first_discovery(owner.runtime.clone());
    let mut gui = attached(&proxy.runtime);
    assert!(gui.discovery().is_none());
    assert!(
        gui.capabilities()
            .unwrap()
            .devices
            .backends
            .contains(&OutputBackend::Alsa)
    );
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::DevicesList { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let uncertain = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::DevicesList { .. }))
        .unwrap();
    let id = uncertain.request.envelope().request_id.clone();
    let bytes = uncertain.request.bytes().to_vec();
    gui.handle(UiIntent::Reconcile(id));
    until(&mut gui, |g| !g.busy() && g.discovery().is_some());
    let result = gui.discovery().unwrap();
    let DeviceObservationView::List {
        devices,
        complete,
        failures,
    } = &result.observation
    else {
        panic!("list: {result:?}")
    };
    assert!(*complete && failures.is_empty());
    assert!(devices.iter().any(|d| d.endpoint == "null"));
    assert!(devices.iter().any(|d| d.endpoint == "owned_other"));
    let exact = result.clone();
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| !g.busy());
    assert_eq!(gui.discovery().unwrap(), &exact);
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: Some("owned_other".into()),
    });
    until(&mut gui, |g| !g.busy() && g.inspection().is_some());
    assert!(
        matches!(&gui.inspection().unwrap().observation, DeviceObservationView::Inspection { endpoint, current_properties: None, .. } if endpoint == "owned_other")
    );
    assert!(gui.state().unwrap().output.is_none() && gui.state().unwrap().muted);
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    assert!(gui.discovery().is_none() && gui.inspection().is_none());
    assert!(
        proxy
            .frames
            .lock()
            .unwrap()
            .iter()
            .filter(
                |b| serde_json::from_slice::<serde_json::Value>(b).unwrap()["payload"]["method"]
                    == "devices.list"
            )
            .all(|b| b == &bytes)
    );
    owner.stop().unwrap();
}

#[test]
fn owner_without_discovery_refuses_native_query_without_replacing_existing_state() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    assert!(gui.error().is_some());
    assert!(gui.discovery().is_none());
    assert!(owner.state().muted && owner.state().output.is_none());
}

#[test]
fn rejected_fixed_arming_and_unmute_never_raise_gain_or_approve_output() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    let mut fixed = output();
    fixed.volume_mode = VolumeMode::Fixed;
    edit(&mut gui, Command::OutputSelect { output: fixed });
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.is_some()
    });
    let gain = owner.state().software_gain_db;
    for command in [
        Command::FixedArm {
            device_id: output().device_id,
            profile_id: output().profile_id,
        },
        Command::Mute { muted: false },
    ] {
        edit(&mut gui, command.clone());
        until(&mut gui, |g| {
            !g.busy()
                && g.receipts().any(|r| {
                    r.request.envelope().payload == command
                        && r.presentation == Presentation::Rejected
                })
        });
        assert_eq!(owner.state().software_gain_db, gain);
        assert!(!owner.state().output_consented && owner.state().muted);
    }
}

#[test]
fn mute_preempts_a_started_preflight_before_the_output_command_is_sent() {
    let owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = attached(&transport.runtime);
    transport.hold_next_state();
    edit(&mut gui, Command::OutputSelect { output: output() });
    until(&mut gui, |_| transport.state_held());
    gui.handle(UiIntent::Mute);
    transport.release_state();
    until(&mut gui, |g| !g.busy());
    assert!(owner.state().output.is_none());
    assert!(
        !gui.receipts()
            .any(|r| matches!(r.request.envelope().payload, Command::OutputSelect { .. }))
    );
}

#[test]
fn rejected_start_does_not_approve_unmute_raise_gain_or_enable_takeover() {
    let owner = Owner::start();
    let mut gui = attached(&owner.runtime);
    edit(&mut gui, Command::OutputSelect { output: output() });
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().output.is_some()
    });
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueAppend {
            path: "/owned-but-unprepared.wav".into(),
        },
    });
    until(&mut gui, |g| {
        !g.busy() && g.queue().is_some_and(|q| !q.entries.is_empty())
    });
    let entry = gui.queue().unwrap().entries[0].id.clone();
    gui.handle(UiIntent::QueueEdit {
        basis: Arc::new(gui.state().unwrap().clone()),
        command: Command::QueueSelect { id: Some(entry) },
    });
    until(&mut gui, |g| {
        !g.busy() && g.queue().unwrap().selected.is_some()
    });
    let gain = owner.state().software_gain_db;
    gui.handle(UiIntent::StartLocal {
        basis: Arc::new(gui.state().unwrap().clone()),
        takeover: false,
    });
    until(&mut gui, |g| {
        !g.busy()
            && g.receipts().any(|r| {
                matches!(r.request.envelope().payload, Command::Start { .. })
                    && r.presentation == Presentation::Rejected
            })
    });
    assert!(owner.state().muted && !owner.state().output_consented);
    assert_eq!(owner.state().software_gain_db, gain);
    assert!(owner.state().source.is_none() && owner.state().playback.is_none());
    assert!(!gui.receipts().any(|r| matches!(
        r.request.envelope().payload,
        Command::Start { takeover: true, .. }
            | Command::OutputConsent { .. }
            | Command::Mute { muted: false }
            | Command::FixedArm { .. }
            | Command::VolumeSet { .. }
    )));
}

#[test]
#[cfg(target_os = "linux")]
fn failed_native_discovery_retains_a_typed_failure_instead_of_an_empty_inventory() {
    let artifacts = std::path::PathBuf::from("/tmp").join(unique());
    std::fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("invalid-alsa.conf");
    std::fs::write(
        &config,
        "pcm.owned { this deliberately invalid syntax !!!\n",
    )
    .unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&config));
    let mut gui = attached(&owner.runtime);
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    until(&mut gui, |g| !g.busy() && g.discovery().is_some());
    assert!(matches!(
        gui.discovery().unwrap().observation,
        earph_desktop::DeviceObservationView::Failed {
            cleanup_confirmed: true,
            ..
        }
    ));
    assert!(gui.state().unwrap().output.is_none() && gui.state().unwrap().muted);
    owner.stop().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn new_owner_invalidates_inventory_and_never_refreshes_native_discovery_automatically() {
    let artifacts = std::path::PathBuf::from("/tmp").join(unique());
    std::fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("alsa.conf");
    std::fs::write(&config, "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.owned { type null hint { show on description \"Owned hint\" } }\n").unwrap();
    let mut a = ProcessOwner::with_configuration(Some(&config));
    let b = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(a.runtime.clone());
    let mut gui = attached(&transport.runtime);
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    until(&mut gui, |g| !g.busy() && g.discovery().is_some());
    let count = transport
        .frames
        .lock()
        .unwrap()
        .iter()
        .filter(|b| {
            serde_json::from_slice::<serde_json::Value>(b).unwrap()["payload"]["method"]
                == "devices.list"
        })
        .count();
    transport.switch_owner(b.runtime.clone());
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().owner_id == b.state().owner_id
    });
    assert!(gui.discovery().is_none() && gui.inspection().is_none());
    assert_eq!(
        transport
            .frames
            .lock()
            .unwrap()
            .iter()
            .filter(
                |b| serde_json::from_slice::<serde_json::Value>(b).unwrap()["payload"]["method"]
                    == "devices.list"
            )
            .count(),
        count
    );
    a.stop().unwrap();
}

#[test]
#[cfg(target_os = "linux")]
fn old_discovery_uncertainty_cannot_be_reconciled_against_a_replacement_owner() {
    let artifacts = std::path::PathBuf::from("/tmp").join(unique());
    std::fs::create_dir(&artifacts).unwrap();
    let config = artifacts.join("alsa.conf");
    std::fs::write(&config, "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.owned { type null hint { show on description \"Owned hint\" } }\n").unwrap();
    let mut a = ProcessOwner::with_configuration(Some(&config));
    let b = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::drop_discovery(a.runtime.clone());
    let mut gui = attached(&transport.runtime);
    gui.handle(UiIntent::Discover {
        backend: OutputBackend::Alsa,
        endpoint: None,
    });
    until(&mut gui, |g| {
        g.receipts().any(|r| {
            matches!(r.request.envelope().payload, Command::DevicesList { .. })
                && r.presentation == Presentation::Uncertain
        })
    });
    let id = gui
        .receipts()
        .find(|r| matches!(r.request.envelope().payload, Command::DevicesList { .. }))
        .unwrap()
        .request
        .envelope()
        .request_id
        .clone();
    transport.switch_owner(b.runtime.clone());
    gui.handle(UiIntent::Refresh);
    until(&mut gui, |g| {
        !g.busy() && g.state().unwrap().owner_id == b.state().owner_id
    });
    let before = transport.for_id(&id).len();
    gui.handle(UiIntent::Reconcile(id.clone()));
    assert!(gui.error().is_some_and(|e| e.contains("owner changed")));
    assert_eq!(transport.for_id(&id).len(), before);
    assert!(gui.discovery().is_none());
    a.stop().unwrap();
}
