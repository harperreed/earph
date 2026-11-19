// ABOUTME: Exercises the desktop model against a real standalone owner process.
// ABOUTME: Uses generated queue files without preparing a source or opening output.
mod common;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DesktopClient, OwnerClient, Presentation, Reply, Request, ResultView};
use serde_json::json;
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as ProcessCommand},
    thread,
    time::{Duration, Instant},
};

struct ProcessOwner {
    child: Child,
    runtime: PathBuf,
    artifacts: PathBuf,
}
impl ProcessOwner {
    fn start() -> Self {
        let binary = PathBuf::from(std::env::var_os("EARPH_ENGINE_TEST_BINARY").expect(
            "run crates/earph-desktop/scripts/check; E2E requires the real built earph-engine",
        ));
        assert!(binary.is_absolute() && binary.is_file());
        let artifacts = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = artifacts.join("owner");
        let child = ProcessCommand::new(&binary)
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .stdout(File::create(artifacts.join("owner.stdout.log")).unwrap())
            .stderr(File::create(artifacts.join("owner.stderr.log")).unwrap())
            .spawn()
            .unwrap();
        let mut owner = Self {
            child,
            runtime,
            artifacts,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while !owner.runtime.join("endpoint.json").exists() {
            assert!(
                owner.child.try_wait().unwrap().is_none(),
                "actual owner exited at startup"
            );
            assert!(
                Instant::now() < deadline,
                "actual owner startup exceeded deadline"
            );
            thread::sleep(Duration::from_millis(1));
        }
        owner
    }
    fn stop(&mut self) -> Result<(), String> {
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("owner exit: {status}"))
            };
        }
        let client = OwnerClient::attach(self.runtime.to_str().unwrap().to_owned())
            .map_err(|error| format!("{error:?}"))?;
        let reply = client
            .request(&query(Command::State))
            .map_err(|error| format!("{error:?}"))?;
        let Reply::Receipt(receipt) = reply else {
            return Err("missing owner state receipt".into());
        };
        let request = Request::mutation(
            RequestId::new(unique()).unwrap(),
            receipt.state().ok_or("missing state")?,
            Command::OwnerShutdown,
        )
        .unwrap();
        client
            .request(&request)
            .map_err(|error| format!("{error:?}"))?;
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("owner exit: {status}"))
                };
            }
            if Instant::now() >= deadline {
                return Err("owned process did not exit after shutdown".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for ProcessOwner {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            let _ = fs::write(self.artifacts.join("cleanup-error.txt"), error);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn settle(client: &mut DesktopClient, request: Request) -> RequestId {
    let id = request.envelope().request_id.clone();
    client.submit(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        client.poll();
        if !client.model().record(&id).unwrap().in_flight {
            return id;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}
fn refresh(client: &mut DesktopClient) -> std::sync::Arc<earph_desktop::StateView> {
    settle(client, query(Command::State));
    client.model().latest_state().unwrap().state.clone()
}
fn wav() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&52u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&96000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    for sample in [0i16, 1, -1, 17, -17, 32767, -32768, 0] {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[test]
fn standalone_owner_queue_and_saved_intent_flow_remains_muted_without_any_source_start() {
    let mut owner = ProcessOwner::start();
    let fixture = owner.artifacts.join("generated.wav");
    let source = wav();
    fs::write(&fixture, &source).unwrap();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let initial = refresh(&mut client);
    assert!(initial.muted && !initial.output_consented && initial.output.is_none());
    let caps = settle(&mut client, query(Command::Capabilities));
    let Some(Ok(Reply::Receipt(reply))) = &client.model().record(&caps).unwrap().outcome else {
        panic!("capabilities")
    };
    let Some(ResultView::Capabilities(caps)) = &reply.result else {
        panic!("typed capabilities")
    };
    assert!(caps.profiles.save && caps.profiles.manual_resolution);
    assert!(!caps.profiles.apply && !caps.profiles.exact_associations);
    let append = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &initial,
        Command::QueueAppend {
            path: fixture.to_str().unwrap().into(),
        },
    )
    .unwrap();
    let append_id = settle(&mut client, append);
    assert_eq!(
        client.model().record(&append_id).unwrap().presentation,
        Presentation::Applied
    );
    let queue_id = settle(
        &mut client,
        query(Command::QueueGet {
            offset: 0,
            limit: 64,
        }),
    );
    let Some(Ok(Reply::Receipt(reply))) = &client.model().record(&queue_id).unwrap().outcome else {
        panic!("queue")
    };
    let Some(ResultView::Queue(queue)) = &reply.result else {
        panic!("typed queue")
    };
    assert_eq!(queue.entries.len(), 1);
    assert_eq!(queue.entries[0].path, fixture.to_str().unwrap());
    assert_eq!(
        queue.entries[0].status,
        earph_core::queue::PreparationStatus::Unprepared
    );
    let state = refresh(&mut client);
    let profile =
        Request::mutation(RequestId::new(unique()).unwrap(), &state, profile_payload()).unwrap();
    let profile_id = settle(&mut client, profile);
    assert_eq!(
        client.model().record(&profile_id).unwrap().presentation,
        Presentation::Accepted
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while client.model().record(&profile_id).unwrap().presentation == Presentation::Accepted {
        client.reconcile(&profile_id).unwrap();
        loop {
            client.poll();
            if !client.model().record(&profile_id).unwrap().in_flight {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(
        client.model().record(&profile_id).unwrap().presentation,
        Presentation::Applied
    );
    let final_state = refresh(&mut client);
    assert!(final_state.muted && !final_state.output_consented && final_state.output.is_none());
    assert_eq!(final_state.source, None);
    assert_eq!(final_state.playback, None);
    assert_eq!(fs::read(&fixture).unwrap(), source);
    let (model, shutdown) = client.disconnect();
    assert!(!model.attached);
    shutdown.join().unwrap();
    assert!(
        owner.child.try_wait().unwrap().is_none(),
        "controller detach must leave the owner alive"
    );
    owner.stop().unwrap();
    assert!(owner.child.try_wait().unwrap().unwrap().success());
    assert!(
        fs::read(owner.artifacts.join("owner.stderr.log"))
            .unwrap()
            .is_empty()
    );
    fs::write(owner.artifacts.join("e2e-result.json"), serde_json::to_vec_pretty(&json!({"status":"pass","owner_pid":owner.child.id(),"runtime":owner.runtime,"fixture_bytes_unchanged":true,"source_start":false,"native_output":false,"owner_exit":0})).unwrap()).unwrap();
    println!("E2E artifacts: {}", owner.artifacts.display());
}
