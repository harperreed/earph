// ABOUTME: Injects bounded transport faults while forwarding commands to the real owner.
// ABOUTME: Preserves actual frames and owns every socket and handler until joined.
use super::unique;
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
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub struct Proxy {
    pub runtime: PathBuf,
    pub frames: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    gate: Arc<(Mutex<bool>, Condvar)>,
    pub entered: Receiver<()>,
    join: Option<JoinHandle<()>>,
}
impl Proxy {
    pub fn drop_first_append(owner: PathBuf) -> Self {
        Self::start(owner, Some("queue.append"), false)
    }
    pub fn drop_first_state(owner: PathBuf) -> Self {
        Self::start(owner, Some("state.get"), false)
    }
    pub fn drop_first_start(owner: PathBuf) -> Self {
        Self::start(owner, Some("sessions.start"), false)
    }
    pub fn drop_first_discovery(owner: PathBuf) -> Self {
        Self::start(owner, Some("devices.list"), false)
    }
    pub fn drop_first_profile_save(owner: PathBuf) -> Self {
        Self::start(owner, Some("profiles.create"), false)
    }
    pub fn drop_first_graph_prepare(owner: PathBuf) -> Self {
        Self::start(owner, Some("graph.prepare"), false)
    }
    pub fn hold_first_capabilities(owner: PathBuf) -> Self {
        Self::start(owner, None, true)
    }
    pub fn hold_first_queue_reply(owner: PathBuf) -> Self {
        Self::configured(owner, None, false, true)
    }
    fn start(owner: PathBuf, drop_method: Option<&'static str>, hold_capabilities: bool) -> Self {
        Self::configured(owner, drop_method, hold_capabilities, false)
    }
    fn configured(
        owner: PathBuf,
        drop_method: Option<&'static str>,
        hold_capabilities: bool,
        hold_queue_reply: bool,
    ) -> Self {
        let runtime = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&runtime).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(runtime.join("o-proxy.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::set_permissions(
            runtime.join("o-proxy.sock"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(
            runtime.join("endpoint.json"),
            br#"{"schema_version":1,"socket":"o-proxy.sock"}"#,
        )
        .unwrap();
        fs::set_permissions(
            runtime.join("endpoint.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let frames = Arc::new(Mutex::new(Vec::new()));
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (entered_tx, entered) = mpsc::sync_channel(1);
        let stopping = stop.clone();
        let captured = frames.clone();
        let waiting = gate.clone();
        let retained = runtime.clone();
        let join = thread::spawn(move || {
            let first_append = Arc::new(AtomicBool::new(drop_method.is_some()));
            let first_capabilities = Arc::new(AtomicBool::new(hold_capabilities));
            let first_queue_reply = Arc::new(AtomicBool::new(hold_queue_reply));
            let mut handlers = Vec::new();
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let owner = owner.clone();
                        let captured = captured.clone();
                        let waiting = waiting.clone();
                        let entered = entered_tx.clone();
                        let first_append = first_append.clone();
                        let first_capabilities = first_capabilities.clone();
                        let first_queue_reply = first_queue_reply.clone();
                        let retained = retained.clone();
                        handlers.push(thread::spawn(move || {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            stream
                                .set_write_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let bytes = read_frame(&mut stream);
                            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                            let method = value["payload"]["method"].as_str().unwrap();
                            let index = {
                                let mut frames = captured.lock().unwrap();
                                frames.push(bytes.clone());
                                frames.len()
                            };
                            fs::write(retained.join(format!("request-{index}.json")), &bytes)
                                .unwrap();
                            if method == "capabilities.get"
                                && first_capabilities.swap(false, Ordering::AcqRel)
                            {
                                entered.send(()).unwrap();
                                let (lock, changed) = &*waiting;
                                let guard = lock.lock().unwrap();
                                let _ = changed
                                    .wait_timeout_while(guard, Duration::from_secs(2), |released| {
                                        !*released
                                    })
                                    .unwrap();
                            }
                            let reply = earph_engine::request(&owner, &bytes).unwrap();
                            fs::write(retained.join(format!("reply-{index}.json")), &reply)
                                .unwrap();
                            if method == "queue.get"
                                && first_queue_reply.swap(false, Ordering::AcqRel)
                            {
                                entered.send(()).unwrap();
                                let (lock, changed) = &*waiting;
                                let guard = lock.lock().unwrap();
                                let _ = changed
                                    .wait_timeout_while(guard, Duration::from_secs(2), |released| {
                                        !*released
                                    })
                                    .unwrap();
                            }
                            if Some(method) == drop_method
                                && first_append.swap(false, Ordering::AcqRel)
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
                    Err(error) => panic!("proxy accept: {error}"),
                }
            }
            for handler in handlers {
                handler.join().unwrap();
            }
        });
        Self {
            runtime,
            frames,
            stop,
            gate,
            entered,
            join: Some(join),
        }
    }
    pub fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.release();
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}
fn read_frame(stream: &mut UnixStream) -> Vec<u8> {
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_be_bytes(header) as usize;
    assert!((1..=earph_engine::control_frame_limit()).contains(&length));
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}
