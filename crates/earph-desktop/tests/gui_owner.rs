// ABOUTME: Exercises explicit desktop launch and shutdown against the standalone owner binary.
// ABOUTME: Keeps generated files and logs while avoiding output, devices, and source starts.
mod common;
use common::*;
use earph_desktop::gui::{GuiController, HostStatus, UiIntent};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

fn until(gui: &mut GuiController, predicate: impl Fn(&GuiController) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        gui.tick(Instant::now());
        if predicate(gui) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "launch timed out: {:?}, {:?}",
            gui.host_status(),
            gui.error()
        );
        thread::sleep(Duration::from_millis(2));
    }
}
fn binary() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("earph-engine");
    assert!(
        path.is_file(),
        "build the actual earph-engine before this test"
    );
    path
}

#[test]
fn explicit_launch_attaches_second_controller_and_shutdown_proves_owned_exit() {
    let artifacts = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&artifacts).unwrap();
    fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
    let runtime = artifacts.join("owner").to_str().unwrap().to_owned();
    let fixture = artifacts.join("generated.wav");
    let bytes = [
        b"RIFF".as_slice(),
        &52u32.to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &1u16.to_le_bytes(),
        &1u16.to_le_bytes(),
        &48000u32.to_le_bytes(),
        &96000u32.to_le_bytes(),
        &2u16.to_le_bytes(),
        &16u16.to_le_bytes(),
        b"data",
        &16u32.to_le_bytes(),
        &[0u8; 16],
    ]
    .concat();
    fs::write(&fixture, &bytes).unwrap();
    let mut first = GuiController::new();
    first.handle(UiIntent::Launch {
        runtime: runtime.clone(),
        executable: binary(),
    });
    until(&mut first, |g| {
        matches!(g.host_status(), HostStatus::OwnedRunning { .. })
            && g.connected()
            && g.queue().is_some()
    });
    let HostStatus::OwnedRunning { pid, .. } = first.host_status() else {
        unreachable!()
    };
    let owner_pid = *pid;
    let identity = first.state().unwrap().owner_id.clone();
    first.handle(UiIntent::QueueEdit {
        basis: std::sync::Arc::new(first.state().unwrap().clone()),
        command: earph_core::control::Command::QueueAppend {
            path: fixture.to_str().unwrap().into(),
        },
    });
    until(&mut first, |g| {
        g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    let mut second = GuiController::new();
    second.handle(UiIntent::Launch {
        runtime: runtime.clone(),
        executable: binary(),
    });
    until(&mut second, |g| {
        matches!(g.host_status(), HostStatus::AttachedExisting { .. }) && g.connected()
    });
    assert_eq!(second.state().unwrap().owner_id, identity);
    second.handle(UiIntent::CloseController);
    until(&mut second, |g| g.ready_to_close());
    first.handle(UiIntent::Refresh);
    until(&mut first, |g| {
        g.connected() && g.queue().is_some_and(|q| q.entries.len() == 1)
    });
    assert!(first.state().unwrap().muted);
    assert!(first.state().unwrap().source.is_none());
    assert!(first.state().unwrap().output.is_none());
    first.handle(UiIntent::ShutdownAndQuit);
    until(&mut first, |g| g.ready_to_close());
    assert!(matches!(
        first.host_status(),
        HostStatus::Exited { pid, success: true } if *pid == owner_pid
    ));
    assert_eq!(fs::read(&fixture).unwrap(), bytes);
    fs::write(artifacts.join("gui-process-result.json"), serde_json::to_vec_pretty(&serde_json::json!({"status":"pass","owner_id":identity,"owner_pid":owner_pid,"source_start":false,"output":false,"fixture_bytes_unchanged":true,"owned_exit":0})).unwrap()).unwrap();
    println!("GUI process artifacts: {}", artifacts.display());
}

#[test]
fn invalid_runtime_and_executable_fail_without_a_running_owner() {
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Launch {
        runtime: "relative".into(),
        executable: binary(),
    });
    assert!(gui.error().unwrap().contains("absolute"));
    gui.handle(UiIntent::Launch {
        runtime: PathBuf::from("/tmp")
            .join(unique())
            .to_str()
            .unwrap()
            .into(),
        executable: PathBuf::from("/nonexistent/earph-engine"),
    });
    until(&mut gui, |g| {
        matches!(g.host_status(), HostStatus::Failed { .. })
    });
    assert!(!gui.connected());
    gui.handle(UiIntent::CloseController);
    until(&mut gui, |g| g.ready_to_close());
}

#[test]
fn shutdown_of_an_attached_owner_does_not_wait_for_another_owned_process() {
    let first_runtime = PathBuf::from("/tmp").join(unique());
    let second_runtime = PathBuf::from("/tmp").join(unique());
    let second_owner = earph_engine::EngineOwner::bind(&second_runtime).unwrap();
    let second_join = thread::spawn(move || second_owner.serve());
    let mut gui = GuiController::new();
    gui.handle(UiIntent::Launch {
        runtime: first_runtime.to_str().unwrap().into(),
        executable: binary(),
    });
    until(&mut gui, |g| {
        matches!(g.host_status(), HostStatus::OwnedRunning { .. }) && g.connected()
    });
    gui.handle(UiIntent::Disconnect);
    until(&mut gui, |g| !g.busy());
    gui.handle(UiIntent::Attach {
        runtime: second_runtime.to_str().unwrap().into(),
    });
    until(&mut gui, |g| g.connected() && !g.busy());
    gui.handle(UiIntent::ShutdownAndQuit);
    let deadline = Instant::now() + Duration::from_secs(2);
    while !gui.ready_to_close() && Instant::now() < deadline {
        gui.tick(Instant::now());
        thread::sleep(Duration::from_millis(2));
    }
    let closed_without_stopping_first = gui.ready_to_close();
    let observer =
        earph_desktop::OwnerClient::attach(first_runtime.to_str().unwrap().into()).unwrap();
    let earph_desktop::Reply::Receipt(receipt) = observer
        .request(&query(earph_core::control::Command::State))
        .unwrap()
    else {
        panic!("first owner state")
    };
    assert!(receipt.state().unwrap().muted);
    observer
        .request(
            &earph_desktop::Request::mutation(
                earph_core::ids::RequestId::new(unique()).unwrap(),
                receipt.state().unwrap(),
                earph_core::control::Command::OwnerShutdown,
            )
            .unwrap(),
        )
        .unwrap();
    until(&mut gui, |g| {
        matches!(g.host_status(), HostStatus::Exited { success: true, .. })
    });
    second_join.join().unwrap().unwrap();
    assert!(
        closed_without_stopping_first,
        "attached shutdown waited for a different owned process"
    );
}
