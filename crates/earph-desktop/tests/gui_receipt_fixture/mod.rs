// ABOUTME: Exercises the real Unix client with retained drops and controlled Core receipt statuses.
// ABOUTME: Accepted safety cases prove consumer scheduling only and never claim playback semantics.
#![allow(dead_code)]
use super::unique;
use earph_core::{
    control::{Command, CommandEnvelope, Receipt, ReceiptStatus},
    ids::{RequestId, ZoneId},
};
use earph_desktop::{OwnerClient, Reply, Request, StateView};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct RealOwner {
    pub runtime: PathBuf,
    serve: Option<JoinHandle<Result<(), earph_core::EngineError>>>,
}
impl RealOwner {
    pub fn start() -> Self {
        let runtime = PathBuf::from("/tmp").join(unique());
        let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
        Self {
            runtime,
            serve: Some(thread::spawn(move || owner.serve())),
        }
    }
    pub fn state(&self) -> StateView {
        state(&self.runtime)
    }
}
impl Drop for RealOwner {
    fn drop(&mut self) {
        let serve = self.serve.take().unwrap();
        if !serve.is_finished() {
            let client = OwnerClient::attach(self.runtime.to_str().unwrap().into()).unwrap();
            let query = Request::query(
                RequestId::new(unique()).unwrap(),
                ZoneId::new("main").unwrap(),
                Command::State,
            )
            .unwrap();
            if let Ok(Reply::Receipt(receipt)) = client.request(&query)
                && !receipt.state().unwrap().shutdown_requested
            {
                let shutdown = Request::mutation(
                    RequestId::new(unique()).unwrap(),
                    receipt.state().unwrap(),
                    Command::OwnerShutdown,
                )
                .unwrap();
                let _ = client.request(&shutdown);
            }
        }
        serve.join().unwrap().unwrap();
    }
}
fn state(runtime: &std::path::Path) -> StateView {
    let client = OwnerClient::attach(runtime.to_str().unwrap().into()).unwrap();
    let request = Request::query(
        RequestId::new(unique()).unwrap(),
        ZoneId::new("main").unwrap(),
        Command::State,
    )
    .unwrap();
    let Reply::Receipt(receipt) = client.request(&request).unwrap() else {
        panic!("actual owner State")
    };
    receipt.state().unwrap().clone()
}

#[derive(Clone, Copy)]
enum Mode {
    Observe,
    DropDiscovery,
    DropShutdown,
    Accepted,
    DropSafetyThenAccepted,
}
pub struct ReceiptTransport {
    pub runtime: PathBuf,
    pub frames: Arc<Mutex<Vec<Vec<u8>>>>,
    owner: Arc<Mutex<PathBuf>>,
    ordinary: Arc<Mutex<Option<RequestId>>>,
    finish_ordinary: Arc<AtomicBool>,
    finish_safety: Arc<AtomicBool>,
    stopping: Arc<AtomicBool>,
    hold_state: Arc<AtomicBool>,
    state_held: Arc<AtomicBool>,
    state_gate: Arc<(Mutex<bool>, Condvar)>,
    serve: Option<JoinHandle<()>>,
}
impl ReceiptTransport {
    pub fn observation(owner: PathBuf) -> Self {
        Self::start(owner, Mode::Observe)
    }
    pub fn drop_discovery(owner: PathBuf) -> Self {
        Self::start(owner, Mode::DropDiscovery)
    }
    pub fn drop_shutdown(owner: PathBuf) -> Self {
        Self::start(owner, Mode::DropShutdown)
    }
    pub fn accepted(owner: PathBuf) -> Self {
        Self::start(owner, Mode::Accepted)
    }
    pub fn uncertain_safety(owner: PathBuf) -> Self {
        Self::start(owner, Mode::DropSafetyThenAccepted)
    }
    fn start(owner_path: PathBuf, mode: Mode) -> Self {
        let runtime = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(runtime.join("o-receipt.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::set_permissions(
            runtime.join("o-receipt.sock"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(
            runtime.join("endpoint.json"),
            br#"{"schema_version":1,"socket":"o-receipt.sock"}"#,
        )
        .unwrap();
        fs::set_permissions(
            runtime.join("endpoint.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        println!("Receipt consumer artifacts: {}", runtime.display());
        let owner = Arc::new(Mutex::new(owner_path));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let ordinary = Arc::new(Mutex::new(None));
        let finish_ordinary = Arc::new(AtomicBool::new(false));
        let finish_safety = Arc::new(AtomicBool::new(false));
        let stopping = Arc::new(AtomicBool::new(false));
        let hold_state = Arc::new(AtomicBool::new(false));
        let state_held = Arc::new(AtomicBool::new(false));
        let state_gate = Arc::new((Mutex::new(false), Condvar::new()));
        let dropping = Arc::new(AtomicBool::new(matches!(mode, Mode::DropShutdown)));
        let dropping_safety = Arc::new(AtomicBool::new(matches!(
            mode,
            Mode::DropSafetyThenAccepted
        )));
        let dropping_discovery = Arc::new(AtomicBool::new(matches!(mode, Mode::DropDiscovery)));
        let route = owner.clone();
        let captured = frames.clone();
        let target = ordinary.clone();
        let ordinary_done = finish_ordinary.clone();
        let safety_done = finish_safety.clone();
        let stop = stopping.clone();
        let artifacts = runtime.clone();
        let hold_next = hold_state.clone();
        let held = state_held.clone();
        let gate = state_gate.clone();
        let serve = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let route = route.clone();
                        let captured = captured.clone();
                        let target = target.clone();
                        let ordinary_done = ordinary_done.clone();
                        let safety_done = safety_done.clone();
                        let dropping = dropping.clone();
                        let dropping_safety = dropping_safety.clone();
                        let dropping_discovery = dropping_discovery.clone();
                        let artifacts = artifacts.clone();
                        let hold_next = hold_next.clone();
                        let held = held.clone();
                        let gate = gate.clone();
                        workers.push(thread::spawn(move || {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let bytes = read_frame(&mut stream);
                            let command: CommandEnvelope = serde_json::from_slice(&bytes).unwrap();
                            assert!(!matches!(
                                command.payload,
                                Command::Start { .. }
                                    | Command::OutputSelect { .. }
                                    | Command::OutputConsent { .. }
                                    | Command::Mute { muted: false }
                            ));
                            let index = {
                                let mut frames = captured.lock().unwrap();
                                assert!(frames.len() < 240, "fixture frame budget exhausted");
                                frames.push(bytes.clone());
                                frames.len()
                            };
                            fs::write(artifacts.join(format!("request-{index}.json")), &bytes)
                                .unwrap();
                            if matches!(command.payload, Command::State)
                                && hold_next.swap(false, Ordering::AcqRel)
                            {
                                held.store(true, Ordering::Release);
                                let guard = gate.0.lock().unwrap();
                                let _ = gate
                                    .1
                                    .wait_timeout_while(guard, Duration::from_secs(2), |released| {
                                        !*released
                                    })
                                    .unwrap();
                            }
                            if matches!(command.payload, Command::OwnerShutdown)
                                && dropping.swap(false, Ordering::AcqRel)
                            {
                                return;
                            }
                            let owner = route.lock().unwrap().clone();
                            let safety = matches!(
                                command.payload,
                                Command::Stop | Command::Mute { muted: true }
                            );
                            if safety && dropping_safety.swap(false, Ordering::AcqRel) {
                                return;
                            }
                            let accepted_mode =
                                matches!(mode, Mode::Accepted | Mode::DropSafetyThenAccepted);
                            let ordinary_request = if accepted_mode
                                && matches!(command.payload, Command::QueueGet { .. })
                            {
                                let mut target = target.lock().unwrap();
                                if target.is_none() {
                                    *target = Some(command.request_id.clone());
                                }
                                target.as_ref() == Some(&command.request_id)
                            } else {
                                false
                            };
                            let reply = if accepted_mode
                                && (safety
                                    || (ordinary_request && !ordinary_done.load(Ordering::Acquire)))
                            {
                                let observed = state(&owner);
                                assert!(observed.source.is_none() && observed.output.is_none());
                                // This controlled producer uses actual Core serialization, not actual Stop execution.
                                let receipt = Receipt {
                                    request_id: command.request_id.clone(),
                                    zone_id: command.zone_id.clone(),
                                    session_id: command.session_id.clone(),
                                    status: if safety && safety_done.load(Ordering::Acquire) {
                                        ReceiptStatus::Applied
                                    } else {
                                        ReceiptStatus::Accepted
                                    },
                                    error: None,
                                    revision: observed.revision,
                                    epochs: observed.epochs,
                                    activation_frame: None,
                                    result: None,
                                };
                                serde_json::to_vec(&serde_json::json!({"receipt": receipt}))
                                    .unwrap()
                            } else {
                                match earph_engine::request(&owner, &bytes) {
                                    Ok(reply) => reply,
                                    Err(error) => {
                                        fs::write(
                                            artifacts.join(format!("forward-error-{index}.json")),
                                            serde_json::to_vec(&error).unwrap(),
                                        )
                                        .unwrap();
                                        return;
                                    }
                                }
                            };
                            fs::write(artifacts.join(format!("reply-{index}.json")), &reply)
                                .unwrap();
                            if matches!(command.payload, Command::DevicesList { .. })
                                && dropping_discovery.swap(false, Ordering::AcqRel)
                            {
                                return;
                            }
                            let _ = stream
                                .write_all(&(reply.len() as u32).to_be_bytes())
                                .and_then(|_| stream.write_all(&reply));
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("receipt fixture accept: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            runtime,
            frames,
            owner,
            ordinary,
            finish_ordinary,
            finish_safety,
            stopping,
            hold_state,
            state_held,
            state_gate,
            serve: Some(serve),
        }
    }
    pub fn finish(&self) {
        self.finish_ordinary();
        self.finish_safety.store(true, Ordering::Release);
    }
    pub fn hold_next_state(&self) {
        *self.state_gate.0.lock().unwrap() = false;
        self.state_held.store(false, Ordering::Release);
        self.hold_state.store(true, Ordering::Release);
    }
    pub fn state_held(&self) -> bool {
        self.state_held.load(Ordering::Acquire)
    }
    pub fn release_state(&self) {
        *self.state_gate.0.lock().unwrap() = true;
        self.state_gate.1.notify_all();
    }
    pub fn finish_ordinary(&self) {
        self.finish_ordinary.store(true, Ordering::Release);
    }
    pub fn switch_owner(&self, runtime: PathBuf) {
        *self.owner.lock().unwrap() = runtime;
    }
    pub fn ordinary_id(&self) -> Option<RequestId> {
        self.ordinary.lock().unwrap().clone()
    }
    pub fn for_id(&self, id: &RequestId) -> Vec<Vec<u8>> {
        self.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|bytes| {
                serde_json::from_slice::<CommandEnvelope>(bytes)
                    .unwrap()
                    .request_id
                    == *id
            })
            .cloned()
            .collect()
    }
}
impl Drop for ReceiptTransport {
    fn drop(&mut self) {
        self.release_state();
        self.stopping.store(true, Ordering::Release);
        self.serve.take().unwrap().join().unwrap();
    }
}
fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let count = u32::from_be_bytes(header) as usize;
    assert!((1..=earph_engine::control_frame_limit()).contains(&count));
    let mut bytes = vec![0; count];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}
