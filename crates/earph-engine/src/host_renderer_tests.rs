// ABOUTME: Qualifies renderer ownership against real HTTP, decoder, and native output workers.
// ABOUTME: Holds acknowledgement adoption to exercise exact cleanup without synthetic worker events.
use super::*;
use earph_core::{
    control::{ControlLimits, Receipt, decode_command},
    ids::{OwnerId, SessionId, ZoneId},
    network_source::{NetworkEventKind, NetworkRetirementAck, NetworkSourceBinding},
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, TcpListener, UdpSocket},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

const CHILD: &str = "EARPH_RENDERER_WORKER_CHILD";
const DIRECTORY: &str = "EARPH_RENDERER_WORKER_DIRECTORY";
const CASES: [&str; 7] = [
    "remote-stop",
    "renderer-revoke",
    "local-stop",
    "shutdown",
    "origin-revoke",
    "origin-replace",
    "origin-expire",
];
const BRIDGE_CASES: [&str; 11] = [
    "commit",
    "stop-prepare",
    "revoke-prepare",
    "stop-ready",
    "local-ready",
    "local-pending-stop",
    "local-pending-shutdown",
    "revoke-ack",
    "output-ready",
    "origin-expire",
    "incompatible",
];

#[test]
fn measured_renderer_graph_bridge_uses_fresh_preparation_and_exact_fences() {
    scenario(
        "bridge",
        "host::renderer_ownership::measured_renderer_graph_bridge_uses_fresh_preparation_and_exact_fences",
    );
}
#[test]
fn real_owner_soap_playback_uses_pinned_graph_and_shared_worker() {
    scenario(
        "owner",
        "host::renderer_ownership::real_owner_soap_playback_uses_pinned_graph_and_shared_worker",
    );
}

#[test]
fn real_owner_paused_play_uses_changed_staging_and_existing_takeover_policy() {
    scenario(
        "paused",
        "host::renderer_ownership::real_owner_paused_play_uses_changed_staging_and_existing_takeover_policy",
    );
}

#[test]
fn queued_renderer_source_never_fetches_after_its_authority_fence() {
    scenario(
        "queued",
        "host::renderer_ownership::queued_renderer_source_never_fetches_after_its_authority_fence",
    );
}
#[test]
fn started_renderer_source_joins_transport_before_exact_cleanup_settles() {
    scenario(
        "started",
        "host::renderer_ownership::started_renderer_source_joins_transport_before_exact_cleanup_settles",
    );
}
fn scenario(mode: &str, test: &str) {
    if let Ok(case) = std::env::var(CHILD) {
        if mode == "owner" || mode == "paused" {
            run_owner_case(&PathBuf::from(std::env::var_os(DIRECTORY).unwrap()), &case);
            return;
        }
        if mode == "bridge" {
            run_bridge_case(&case, &PathBuf::from(std::env::var_os(DIRECTORY).unwrap()));
            return;
        }
        run_case(
            mode,
            &case,
            &PathBuf::from(std::env::var_os(DIRECTORY).unwrap()),
        );
        return;
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/renderer-worker")
        .join(format!(
            "{}-{}-{mode}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&root).unwrap();
    let mut raw = Vec::new();
    for sample in 0..(65537 * 2) {
        raw.extend_from_slice(&((sample * 7919 % 65536 - 32768) as i16).to_le_bytes());
    }
    fs::write(root.join("public.pcm"), &raw).unwrap();
    encode(&root.join("public.pcm"), &root.join("public.flac"));
    let b = [0x34, 0x7b].repeat(65537 * 2);
    fs::write(root.join("b.pcm"), b).unwrap();
    encode(&root.join("b.pcm"), &root.join("b.flac"));
    for case in if mode == "bridge" {
        BRIDGE_CASES.as_slice()
    } else if mode == "owner" {
        &["play"][..]
    } else if mode == "paused" {
        &["takeover", "no-takeover"][..]
    } else {
        CASES.as_slice()
    } {
        let directory = root.join(case);
        fs::create_dir(&directory).unwrap();
        let capture = directory.join("native.pcm");
        let escaped = capture
            .to_str()
            .unwrap()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let config = directory.join("alsa.conf");
        fs::write(&config, format!("pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{escaped}\" format raw }}\n")).unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, case)
            .env(DIRECTORY, &directory)
            .env("ALSA_CONFIG_PATH", &config)
            .output()
            .unwrap();
        fs::write(directory.join("stdout.log"), &result.stdout).unwrap();
        fs::write(directory.join("stderr.log"), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{mode}/{case}: {} / {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
fn encode(source: &Path, destination: &Path) {
    let encoded = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin", "-v", "error", "-f", "s16le", "-ar", "48000", "-ac", "2", "-i",
        ])
        .arg(source)
        .args(["-c:a", "flac", "-frame_size", "4096"])
        .arg(destination)
        .output()
        .unwrap();
    assert!(
        encoded.status.success(),
        "{}",
        String::from_utf8_lossy(&encoded.stderr)
    );
}

// This is the existing service_playback CaptureGate pattern, confined to the child process.
struct Capture {
    file: Option<fs::File>,
    capacity: u64,
    release: Option<mpsc::SyncSender<()>>,
    done: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<Vec<u8>>>,
    path: PathBuf,
}
impl Capture {
    fn new(path: PathBuf) -> Self {
        let mut result = Self {
            file: None,
            capacity: 0,
            release: None,
            done: Arc::new(AtomicBool::new(false)),
            join: None,
            path,
        };
        {
            use std::os::unix::fs::OpenOptionsExt;
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                &result.path,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap())
                .open(&result.path)
                .unwrap();
            rustix::pipe::fcntl_setpipe_size(&file, 4096).unwrap();
            result.capacity = rustix::pipe::fcntl_getpipe_size(&file).unwrap() as u64;
            let mut reader = file.try_clone().unwrap();
            let (release, ready) = mpsc::sync_channel(1);
            let done = result.done.clone();
            result.join = Some(thread::spawn(move || {
                ready.recv().unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => bytes.extend_from_slice(&buffer[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            if done.load(Ordering::Acquire) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("native capture failed: {error}"),
                    }
                }
                bytes
            }));
            result.file = Some(file);
            result.release = Some(release);
        }
        result
    }
    fn full(&self) {
        let end = Instant::now() + Duration::from_secs(10);
        // A native period contains 64 stereo S16 frames, or 256 bytes.
        while rustix::io::ioctl_fionread(self.file.as_ref().unwrap()).unwrap() + 256
            <= self.capacity
        {
            assert!(
                Instant::now() < end,
                "real A native writer did not fill the FIFO"
            );
            thread::yield_now();
        }
    }
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            release.send(()).unwrap();
        }
    }
    fn finish(&mut self) -> Vec<u8> {
        self.release();
        self.done.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            join.join().unwrap()
        } else {
            Vec::new()
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.finish();
    }
}

// Two paths share one owned server. B withholds its body until the authority fence.
struct Media {
    origin: String,
    peer: IpAddr,
    a_requests: Arc<AtomicUsize>,
    b_requests: Arc<AtomicUsize>,
    b_closed: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl Media {
    fn new(body: Vec<u8>, b_body: Vec<u8>) -> Self {
        let route = UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let peer = route.local_addr().unwrap().ip();
        assert!(matches!(peer, IpAddr::V4(ip) if ip.is_private()));
        let listener = TcpListener::bind((peer, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let a_requests = Arc::new(AtomicUsize::new(0));
        let b_requests = Arc::new(AtomicUsize::new(0));
        let b_closed = Arc::new(AtomicBool::new(false));
        let release = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, closed, ready, stopping) = (
            b_requests.clone(),
            b_closed.clone(),
            release.clone(),
            stop.clone(),
        );
        let a_count = a_requests.clone();
        let join = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(20)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut header = Vec::new();
                        let mut byte = [0];
                        while !header.ends_with(b"\r\n\r\n") && header.len() < 8192 {
                            match stream.read(&mut byte) {
                                Ok(1) => header.push(byte[0]),
                                _ => break,
                            }
                        }
                        if header.starts_with(b"GET /a ") {
                            a_count.fetch_add(1, Ordering::AcqRel);
                        }
                        let b = header.starts_with(b"GET /b ");
                        if b {
                            count.fetch_add(1, Ordering::AcqRel);
                        }
                        if b {
                            while !ready.load(Ordering::Acquire)
                                && !stopping.load(Ordering::Acquire)
                            {
                                match stream.read(&mut byte) {
                                    Ok(0) => {
                                        closed.store(true, Ordering::Release);
                                        break;
                                    }
                                    Err(error)
                                        if matches!(
                                            error.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) => {}
                                    Ok(_) => {}
                                    Err(_) => {
                                        closed.store(true, Ordering::Release);
                                        break;
                                    }
                                }
                            }
                        }
                        if !stopping.load(Ordering::Acquire) {
                            let response = if b { &b_body } else { &body };
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                response.len()
                            );
                            let _ = stream.write_all(response);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("owned media accept failed: {error}"),
                }
            }
        });
        Self {
            origin,
            peer,
            a_requests,
            b_requests,
            b_closed,
            release,
            stop,
            join: Some(join),
        }
    }
}
impl Drop for Media {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}

struct Fixture {
    session: Session,
    worker: Option<Worker>,
    live: crate::network_source::LiveBinding,
    retirement: Option<NetworkSourceBinding>,
    clock: Instant,
    capture: Capture,
    media: Media,
    directory: PathBuf,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.capture.release();
        if let Some(worker) = self.worker.take() {
            worker.cancel_active();
            drop(worker);
            let bytes = self.capture.finish();
            fs::write(self.directory.join("captured.pcm"), bytes).unwrap();
        }
    }
}
impl Fixture {
    fn command(&self, id: &str, payload: Value) -> CommandEnvelope {
        let state = self.session.snapshot();
        decode_command(&serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":state.zone_id,"owner_id":state.owner_id,"session_id":state.session_id,"expected_revision":state.revision,"payload":payload})).unwrap(), &ControlLimits::default()).unwrap()
    }
    fn sync(&mut self) {
        synchronize_network(
            &self.session,
            self.worker.as_mut().unwrap(),
            &self.live,
            &mut self.retirement,
        )
        .unwrap();
    }
    fn local(&mut self, command: CommandEnvelope) -> Receipt {
        let receipt = self.session.submit_with_network_effects(
            command.clone(),
            monotonic(&self.clock),
            |_| unreachable!(),
            |s, c| crate::network_source::validate(s, c, &self.clock),
            |_| Ok(()),
        );
        assert_ne!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        self.sync();
        dispatch_with_network(
            &command,
            self.worker.as_mut().unwrap(),
            &self.session,
            &self.live,
            &self.clock,
        )
        .unwrap();
        receipt
    }
    fn renderer(&mut self, command: CommandEnvelope) -> Receipt {
        let mut lowered = None;
        let receipt = self.session.submit_renderer_with_network_effects(
            command,
            self.media.peer,
            monotonic(&self.clock),
            |s, c| {
                crate::network_source::validate(s, c, &self.clock)?;
                lowered = Some(c.clone());
                Ok(())
            },
            |_| Ok(()),
        );
        assert_ne!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
        self.sync();
        if let Some(command) = lowered {
            dispatch_with_network(
                &command,
                self.worker.as_mut().unwrap(),
                &self.session,
                &self.live,
                &self.clock,
            )
            .unwrap();
        }
        receipt
    }
    fn action(&self, controller: &str, action: Value, id: &str) -> CommandEnvelope {
        self.command(id, json!({"method":"renderer.request","controller_id":controller,"grant_generation":1,"action":action}))
    }
    fn stage_play(&mut self, controller: &str, generation: u64, path: &str) -> CommandEnvelope {
        let stage = self.action(controller, json!({"operation":"stage_uri","expected_staging_generation":generation-1,"uri":format!("{}{path}", self.media.origin),"metadata":"generated public fixture"}), &format!("stage-{controller}"));
        assert_eq!(self.renderer(stage).status, ReceiptStatus::Applied);
        let staged = self.session.renderer_snapshot().staged.unwrap();
        let mut play = self.action(controller, json!({"operation":"play","media_id":staged.media_id,"staging_generation":generation,"grant_generation":1,"device_id":"fixture"}), &format!("play-{controller}"));
        play.session_id = Some(SessionId::new(format!("session-{controller}")).unwrap());
        assert_eq!(self.renderer(play.clone()).status, ReceiptStatus::Accepted);
        play
    }
    fn event(&self) -> Event {
        self.worker
            .as_ref()
            .unwrap()
            .events
            .recv_timeout(Duration::from_secs(10))
            .expect("real worker event timed out")
    }
    fn apply_event(&mut self, event: Event) {
        match event {
            Event::Network(mut event) => {
                event.monotonic_ns = monotonic(&self.clock);
                self.session.advance_network(event).unwrap();
            }
            Event::NetworkGraphActivated(ack) => reconcile_network_graph_activation(
                &mut self.session,
                ack,
                monotonic(&self.clock),
                |_| Ok(()),
            )
            .unwrap(),
            other => {
                observation(&mut self.session, other, &self.clock).unwrap();
            }
        }
    }
    fn prepare_and_run_a(&mut self) {
        self.stage_play("a", 1, "/a");
        loop {
            let event = self.event();
            let prepared = matches!(&event, Event::Network(e) if matches!(e.kind, NetworkEventKind::Prepared { .. }));
            self.apply_event(event);
            if prepared {
                break;
            }
        }
        assert_eq!(
            self.session
                .snapshot()
                .network_playback
                .as_ref()
                .unwrap()
                .submitted_frames,
            0
        );
        let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
        let prepare = self.command("prepare-a", json!({"method":"graph.prepare","graph":{"schema_version":1,"nodes":[{"id":"input","kind":{"kind":"input"},"format":port},{"id":"output","kind":{"kind":"output"},"format":port}],"edges":[{"from":"input","to":"output","from_port":0,"to_port":0}],"output":"output"},"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}));
        let mut graphs = GraphJobs::spawn(self.directory.join("assets")).unwrap();
        let receipt = self.session.submit_with_network_effects(
            prepare,
            monotonic(&self.clock),
            |_| unreachable!(),
            |_, _| Ok(()),
            |s| graphs.submit(s, self.worker.as_ref().unwrap()),
        );
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        let end = Instant::now() + Duration::from_secs(10);
        while self.session.graph_inspection().prepared.is_none() {
            graphs
                .poll(&mut self.session, monotonic(&self.clock))
                .unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        let prepared = self.session.graph_inspection().prepared.unwrap();
        let commit = self.command("commit-a", json!({"method":"graph.commit","token":prepared.token,"graph_hash":prepared.preview.graph_hash}));
        let receipt = self.session.submit_with_network_effects(
            commit,
            monotonic(&self.clock),
            |_| unreachable!(),
            |_, _| Ok(()),
            |s| graphs.validate_activation(s),
        );
        assert_eq!(receipt.status, ReceiptStatus::Accepted);
        graphs
            .activate(self.worker.as_mut().unwrap(), &self.session)
            .unwrap();
        while self.session.graph_activation().is_some() {
            let event = self.event();
            self.apply_event(event);
        }
        let unmute = self.command("unmute-a", json!({"method":"mute.set","muted":false}));
        self.local(unmute);
        self.capture.full();
    }
    fn ack(&mut self, binding: &NetworkSourceBinding) -> NetworkRetirementAck {
        loop {
            match self.event() {
                Event::NetworkRetired(ack) => {
                    assert_eq!(&ack.binding, binding);
                    return ack;
                }
                // Stale progress/failure can precede retirement, exactly as in the real host loop.
                Event::Network(mut event) => {
                    event.monotonic_ns = monotonic(&self.clock);
                    let _ = self.session.advance_network(event);
                }
                other => panic!(
                    "unexpected retirement event: {}",
                    match other {
                        Event::NetworkGraphFailed { .. } => "graph failed",
                        _ => "local event",
                    }
                ),
            }
        }
    }
    fn adopt(&mut self, ack: NetworkRetirementAck) {
        let mut stale = ack.clone();
        stale.binding.source_generation += 10;
        assert_eq!(
            self.session
                .complete_network_retirement(stale, monotonic(&self.clock))
                .unwrap_err()
                .code,
            ErrorCode::StaleSession
        );
        self.session
            .complete_network_retirement(ack.clone(), monotonic(&self.clock))
            .unwrap();
        self.retirement = None;
        assert_eq!(
            self.session
                .complete_network_retirement(ack, monotonic(&self.clock))
                .unwrap_err()
                .code,
            ErrorCode::StaleSession
        );
        self.sync();
    }
}

fn run_case(mode: &str, case: &str, directory: &Path) {
    let queued = mode == "queued";
    let mut f = fixture(directory, case);
    f.prepare_and_run_a();
    let a = f.session.network_source().unwrap().binding.clone();
    let b_play = f.stage_play("b", 2, "/b");
    let b = f.session.network_source().unwrap();
    assert_eq!(
        f.session.closing_network_source().unwrap().context.binding,
        a
    );
    let held_a = if queued {
        None
    } else {
        f.capture.release();
        let ack = f.ack(&a);
        let end = Instant::now() + Duration::from_secs(5);
        while f.media.b_requests.load(Ordering::Acquire) == 0 {
            assert!(
                Instant::now() < end,
                "B never entered actual HTTP transport"
            );
            thread::sleep(Duration::from_millis(1));
        }
        Some(ack)
    };
    let remote = f.action(
        "b",
        json!({"operation":"stop","binding":b.binding}),
        "remote-stop",
    );
    let explicit = match case {
        "remote-stop" => {
            assert_eq!(f.renderer(remote.clone()).status, ReceiptStatus::Accepted);
            Some(remote.clone())
        }
        "renderer-revoke" => {
            let command = f.command("safety", json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"b"}));
            assert_eq!(f.local(command.clone()).status, ReceiptStatus::Accepted);
            Some(command)
        }
        "local-stop" | "shutdown" | "origin-revoke" | "origin-replace" => {
            assert_eq!(f.renderer(remote.clone()).status, ReceiptStatus::Accepted);
            let payload = match case {
                "local-stop" => json!({"method":"sessions.stop"}),
                "shutdown" => json!({"method":"owner.shutdown"}),
                "origin-revoke" => {
                    json!({"method":"sources.grants.revoke","expected_generation":1})
                }
                _ => {
                    json!({"method":"sources.grants.replace","expected_generation":1,"lifetime_ms":30000,"grants":{"origins":[{"origin":f.media.origin,"pins":[f.media.peer],"roots_der":[]}]}})
                }
            };
            let command = f.command("safety", payload);
            assert_eq!(f.local(command.clone()).status, ReceiptStatus::Accepted);
            Some(command)
        }
        "origin-expire" => {
            let expires = f.session.network_grants().unwrap().1;
            while monotonic(&f.clock) < expires {
                thread::sleep(Duration::from_millis(1));
            }
            f.session
                .expire_network_authority(monotonic(&f.clock))
                .unwrap();
            f.sync();
            None
        }
        _ => panic!("unknown owned scenario"),
    };
    assert!(f.session.network_source().is_none());
    assert!(
        !f.session
            .network_authority_valid(&b.binding, monotonic(&f.clock))
    );
    assert!(crate::network_source::permit(&f.session, &b, &f.clock).is_err());
    assert!(f.live.lock().unwrap().is_none());
    assert!(f.worker.as_ref().unwrap().source_cancellation().is_err());
    assert_eq!(
        f.session
            .renderer_snapshot()
            .closing
            .as_ref()
            .unwrap()
            .binding,
        a
    );
    if case == "renderer-revoke" {
        assert!(
            f.session
                .renderer_snapshot()
                .controllers
                .iter()
                .all(|c| c.id.as_str() != "b")
        );
    }
    f.capture.release();
    let a_ack = held_a.unwrap_or_else(|| f.ack(&a));
    f.adopt(a_ack);
    assert_eq!(
        f.session
            .renderer_snapshot()
            .closing
            .as_ref()
            .unwrap()
            .binding,
        b.binding
    );
    if let Some(command) = &explicit {
        let receipt = if case == "remote-stop" {
            f.renderer(command.clone())
        } else {
            f.local(command.clone())
        };
        assert_eq!(
            receipt.status,
            ReceiptStatus::Accepted,
            "A retirement cannot settle B cleanup"
        );
    }
    let b_ack = f.ack(&b.binding);
    if !queued {
        // Local retirement does not schedule the independent peer's EOF observer.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f.media.b_closed.load(Ordering::Acquire) {
            assert!(
                Instant::now() < deadline,
                "B peer did not observe transport closure"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    f.adopt(b_ack);
    f.session
        .expire_network_authority(monotonic(&f.clock))
        .unwrap();
    assert!(f.session.renderer_snapshot().active.is_none());
    assert!(f.session.renderer_snapshot().closing.is_none());
    if let Some(command) = explicit {
        let receipt = if case == "remote-stop" {
            f.renderer(command)
        } else {
            f.local(command)
        };
        assert_eq!(receipt.status, ReceiptStatus::Applied);
    } else {
        assert_eq!(f.renderer(b_play).status, ReceiptStatus::Failed);
    }
    if matches!(
        case,
        "local-stop" | "shutdown" | "origin-revoke" | "origin-replace"
    ) {
        assert_eq!(f.renderer(remote).status, ReceiptStatus::Cancelled);
    }
    assert_eq!(f.session.shutdown_requested(), case == "shutdown");
    let query = f.command("after-cleanup", json!({"method":"renderer.get"}));
    assert_eq!(f.local(query).status, ReceiptStatus::Applied);
    f.media.release.store(true, Ordering::Release);
    // Actual retirement has joined each owned transport. No queued work may start later.
    let worker = f.worker.take().unwrap();
    drop(worker);
    let captured = f.capture.finish();
    let original = fs::read(directory.parent().unwrap().join("public.pcm")).unwrap();
    assert!(
        !captured.is_empty(),
        "A's actual native output must be captured"
    );
    assert!(
        original.starts_with(&captured),
        "retired B wrote unexpected native samples"
    );
    assert_eq!(
        f.media.b_requests.load(Ordering::Acquire),
        usize::from(!queued),
        "B refetched after its fence"
    );
    fs::write(directory.join("captured.pcm"), &captured).unwrap();
    println!(
        "{mode}/{case}: real A and B acknowledgements adopted; HTTP requests={}; captured={} bytes; worker joined",
        usize::from(!queued),
        captured.len()
    );
}

fn fixture(directory: &Path, case: &str) -> Fixture {
    let capture = Capture::new(directory.join("native.pcm"));
    let media = Media::new(
        fs::read(directory.parent().unwrap().join("public.flac")).unwrap(),
        fs::read(directory.parent().unwrap().join("b.flac")).unwrap(),
    );
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("renderer-worker").unwrap(),
        ControlLimits::default(),
    );
    session.enable_local_playback();
    session.enable_network_playback();
    session.enable_renderer_control();
    let mut f = Fixture {
        session,
        worker: Some(Worker::spawn()),
        live: Arc::new(Mutex::new(None)),
        retirement: None,
        clock: Instant::now(),
        capture,
        media,
        directory: directory.into(),
    };
    for (id, payload) in [
        (
            "output",
            json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"earph_capture","device_id":"fixture","profile_id":"fixture","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}}),
        ),
        (
            "cap",
            json!({"method":"volume.caps.set","software_millidecibels":0,"hardware_millidecibels":null}),
        ),
        (
            "volume",
            json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}),
        ),
        (
            "consent",
            json!({"method":"output.consent","device_id":"fixture","profile_id":"fixture"}),
        ),
        (
            "controllers",
            json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"a","peers":[f.media.peer],"playback":null,"rights":["stage","transport"]},{"id":"b","peers":[f.media.peer],"playback":null,"rights":["stage","transport","takeover"]}]}}),
        ),
        (
            "origins",
            json!({"method":"sources.grants.replace","expected_generation":0,"lifetime_ms":if case == "origin-expire" { 3000 } else { 30000 },"grants":{"origins":[{"origin":f.media.origin,"pins":[f.media.peer],"roots_der":[]}]}}),
        ),
    ] {
        let mut payload = payload;
        if id == "controllers" {
            let port = json!({"domain":"pcm","sample_rate_hz":if case == "incompatible" {44100} else {48000},"channels":"stereo","precision":"float64","max_quantum_frames":64});
            let playback = json!({"intent":{"graph":{"schema_version":1,"nodes":[{"id":"input","kind":{"kind":"input"},"format":port},{"id":"output","kind":{"kind":"output"},"format":port}],"edges":[{"from":"input","to":"output","from_port":0,"to_port":0}],"output":"output"},"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}},"output":f.session.snapshot().output,"expected_device_epoch":f.session.snapshot().device_epoch});
            for controller in payload["grants"]["controllers"].as_array_mut().unwrap() {
                controller["playback"] = playback.clone();
            }
        }
        let command = f.command(id, payload);
        f.local(command);
    }
    f
}

fn run_bridge_case(case: &str, directory: &Path) {
    let mut f = fixture(directory, case);
    let IpAddr::V4(peer) = f.media.peer else {
        unreachable!()
    };
    let surface = earph_network::upnp::HttpSurface::bind(
        std::net::SocketAddrV4::new(peer, 0),
        earph_network::upnp::DeviceIdentity::new(
            "uuid:00000000-0000-0000-0000-000000000002",
            "Bridge fixture",
            "Earph/1 UPnP/1.0 Earph/1",
        )
        .unwrap(),
    )
    .unwrap();
    let mut renderer = crate::renderer::RendererRuntime::new(surface, &f.session);
    let mut graphs = GraphJobs::spawn(directory.join("assets")).unwrap();
    let mut artifacts = crate::profiles::ArtifactJobs::spawn(directory.into());
    let mut devices = crate::devices::DeviceJobs::spawn(None).unwrap();
    f.stage_play("a", 1, "/a");
    let pin = f.session.renderer_graph_continuation().unwrap();
    assert!(
        renderer
            .next_graph_command(&f.session, monotonic(&f.clock))
            .is_none()
    );
    loop {
        let event = f.event();
        let prepared = matches!(&event, Event::Network(event) if matches!(event.kind, NetworkEventKind::Prepared { .. }));
        f.apply_event(event);
        if prepared {
            break;
        }
    }
    assert_eq!(
        f.session.network_snapshot().lifecycle,
        Some(earph_core::network_source::NetworkLifecycle::Ready)
    );
    let (prepare, captured) = renderer
        .next_graph_command(&f.session, monotonic(&f.clock))
        .expect("measured Ready source must start fresh renderer graph preparation");
    assert_eq!(&*captured, &*pin);
    let Command::GraphPrepare { graph, processing } = &prepare.payload else {
        panic!("must prepare before commit")
    };
    assert_eq!(graph, &pin.authorization.intent.graph);
    assert_eq!(processing, &pin.authorization.intent.processing);
    let prepare_replay = prepare.clone();
    let receipt = OwnerControl {
        session: &mut f.session,
        worker: f.worker.as_mut().unwrap(),
        graphs: &mut graphs,
        artifacts: &mut artifacts,
        devices: &mut devices,
        directory,
        token: "bridge",
        clock: &f.clock,
        live_binding: &f.live,
        network_retirement: &mut f.retirement,
    }
    .execute(prepare, Ingress::Graph(captured))
    .unwrap()
    .receipt;
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    assert_eq!(
        f.session
            .graph_job()
            .unwrap()
            .renderer_application
            .as_deref(),
        Some(&*pin)
    );
    if case == "incompatible" {
        let end = Instant::now() + Duration::from_secs(10);
        while f.session.graph_job().is_some() {
            graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
            assert!(Instant::now() < end, "incompatible graph did not settle");
            thread::sleep(Duration::from_millis(1));
        }
        assert!(f.session.graph_inspection().prepared.is_none());
        assert!(f.session.graph_inspection().active.is_none());
        let failed = f.session.submit_renderer_graph_with_effects(
            prepare_replay,
            pin.clone(),
            monotonic(&f.clock),
            |_| panic!("failed preparation replay cannot persist"),
        );
        assert_eq!(failed.status, ReceiptStatus::Failed, "{failed:?}");
        assert!(matches!(
            failed.error.unwrap().code,
            ErrorCode::FormatUnsupported | ErrorCode::GraphInvalid
        ));
        assert!(
            renderer
                .next_graph_command(&f.session, monotonic(&f.clock))
                .is_none()
        );
        assert!(f.session.snapshot().muted);
        assert_eq!(
            f.session
                .snapshot()
                .network_playback
                .unwrap()
                .submitted_frames,
            0
        );
    } else if matches!(case, "stop-prepare" | "revoke-prepare" | "origin-expire") {
        if case == "origin-expire" {
            let expiry = f.session.network_source().unwrap().expires_monotonic_ns;
            let end = Instant::now() + Duration::from_secs(35);
            while monotonic(&f.clock) < expiry {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(1));
            }
            f.session
                .expire_network_authority(monotonic(&f.clock))
                .unwrap();
            f.sync();
        } else {
            let command = f.command("fence-preparation", if case == "stop-prepare" { json!({"method":"sessions.stop"}) } else { json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"a"}) });
            f.local(command);
        }
        f.session
            .expire_network_authority(monotonic(&f.clock))
            .unwrap();
        graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
        assert!(f.session.renderer_graph_continuation().is_none());
        assert!(f.session.graph_inspection().prepared.is_none());
        assert!(
            renderer
                .next_graph_command(&f.session, monotonic(&f.clock))
                .is_none()
        );
    } else {
        let end = Instant::now() + Duration::from_secs(10);
        while f.session.graph_inspection().prepared.is_none() {
            graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
            assert!(Instant::now() < end, "real graph compiler did not finish");
            thread::sleep(Duration::from_millis(1));
        }
        let ready = f.session.graph_inspection().prepared.unwrap();
        let (commit, captured) = renderer
            .next_graph_command(&f.session, monotonic(&f.clock))
            .expect("fresh preparation must commit its actual token");
        assert_eq!(
            commit.payload,
            Command::GraphCommit {
                token: ready.token.clone(),
                graph_hash: ready.preview.graph_hash.clone()
            }
        );
        if matches!(
            case,
            "stop-ready"
                | "local-ready"
                | "local-pending-stop"
                | "local-pending-shutdown"
                | "output-ready"
        ) {
            let payload = match case {
                "stop-ready" => json!({"method":"sessions.stop"}),
                "local-ready" | "local-pending-stop" | "local-pending-shutdown" => {
                    json!({"method":"graph.prepare","graph":pin.authorization.intent.graph,"processing":pin.authorization.intent.processing})
                }
                _ => {
                    json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"earph_capture","device_id":"other","profile_id":"fixture","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}})
                }
            };
            let command = f.command("fence-ready", payload);
            let receipt = OwnerControl {
                session: &mut f.session,
                worker: f.worker.as_mut().unwrap(),
                graphs: &mut graphs,
                artifacts: &mut artifacts,
                devices: &mut devices,
                directory,
                token: "bridge",
                clock: &f.clock,
                live_binding: &f.live,
                network_retirement: &mut f.retirement,
            }
            .execute(command, Ingress::Local)
            .unwrap()
            .receipt;
            if case == "output-ready" {
                assert_eq!(receipt.status, ReceiptStatus::Rejected, "{receipt:?}");
                assert_eq!(receipt.error.unwrap().code, ErrorCode::SourceBusy);
                assert!(
                    f.session
                        .renderer_graph_binding_valid(&pin, monotonic(&f.clock))
                );
                let stop = f.command(
                    "stop-before-output-change",
                    json!({"method":"sessions.stop"}),
                );
                f.local(stop);
            } else {
                assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            }
            if matches!(case, "local-pending-stop" | "local-pending-shutdown") {
                assert!(graphs.pending.is_some());
                let command = f.command(
                    "cancel-queued-local-graph",
                    if case == "local-pending-stop" {
                        json!({"method":"sessions.stop"})
                    } else {
                        json!({"method":"owner.shutdown"})
                    },
                );
                f.local(command);
                graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
                assert!(graphs.pending.is_none());
                assert!(f.session.graph_job().is_none());
            }
            let receipt = f.session.submit_renderer_graph_with_effects(
                commit,
                captured,
                monotonic(&f.clock),
                |_| panic!("stale commit cannot persist"),
            );
            assert_eq!(receipt.status, ReceiptStatus::Rejected);
            assert!(
                renderer
                    .next_graph_command(&f.session, monotonic(&f.clock))
                    .is_none()
            );
            if case == "local-ready" {
                assert!(
                    f.session
                        .graph_job()
                        .unwrap()
                        .renderer_application
                        .is_none()
                );
                let end = Instant::now() + Duration::from_secs(10);
                while f.session.graph_inspection().prepared.is_none() {
                    graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
                    assert!(
                        Instant::now() < end,
                        "local replacement did not prepare after exact renderer retirement"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                let replacement = f.session.graph_inspection().prepared.unwrap();
                assert_ne!(replacement.token, ready.token);
                assert_eq!(graphs.service.resources().pending_retirements, 0);
            }
        } else {
            let replay = commit.clone();
            let replay_binding = captured.clone();
            let receipt = OwnerControl {
                session: &mut f.session,
                worker: f.worker.as_mut().unwrap(),
                graphs: &mut graphs,
                artifacts: &mut artifacts,
                devices: &mut devices,
                directory,
                token: "bridge",
                clock: &f.clock,
                live_binding: &f.live,
                network_retirement: &mut f.retirement,
            }
            .execute(commit, Ingress::Graph(captured))
            .unwrap()
            .receipt;
            assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
            let checkpoint = fs::metadata(directory.join("queue.json")).unwrap();
            let replayed = OwnerControl {
                session: &mut f.session,
                worker: f.worker.as_mut().unwrap(),
                graphs: &mut graphs,
                artifacts: &mut artifacts,
                devices: &mut devices,
                directory,
                token: "bridge",
                clock: &f.clock,
                live_binding: &f.live,
                network_retirement: &mut f.retirement,
            }
            .execute(replay, Ingress::Graph(replay_binding))
            .unwrap()
            .receipt;
            assert_eq!(replayed, receipt);
            assert_eq!(
                std::os::unix::fs::MetadataExt::ino(&checkpoint),
                std::os::unix::fs::MetadataExt::ino(
                    &fs::metadata(directory.join("queue.json")).unwrap()
                )
            );
            assert!(
                renderer
                    .next_graph_command(&f.session, monotonic(&f.clock))
                    .is_none()
            );
            let ack = loop {
                match f.event() {
                    Event::NetworkGraphActivated(ack) => break ack,
                    event => f.apply_event(event),
                }
            };
            if case == "revoke-ack" {
                let command = f.command("fence-ack", json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"a"}));
                f.local(command);
                assert!(
                    f.session
                        .complete_network_graph_activation(ack, monotonic(&f.clock), |_| panic!(
                            "revoked native acknowledgement cannot persist"
                        ))
                        .is_err()
                );
                assert!(f.session.graph_inspection().active.is_none());
            } else {
                reconcile_network_graph_activation(
                    &mut f.session,
                    ack,
                    monotonic(&f.clock),
                    |_| Ok(()),
                )
                .unwrap();
                assert!(f.session.graph_inspection().active.is_some());
                assert!(
                    f.session.snapshot().muted,
                    "Play and preparation must remain muted"
                );
                let unmute = f.command(
                    "explicit-unmute-bridge",
                    json!({"method":"mute.set","muted":false}),
                );
                f.local(unmute);
                f.capture.full();
            }
        }
    }
    if !f.session.shutdown_requested() {
        let stop = f.command("cleanup-bridge", json!({"method":"sessions.stop"}));
        f.local(stop);
    }
    f.capture.release();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match f.worker.as_ref().unwrap().events.try_recv() {
            Ok(Event::NetworkRetired(ack)) => {
                if f.session
                    .complete_network_retirement(ack, monotonic(&f.clock))
                    .is_ok()
                {
                    f.retirement = None;
                }
            }
            Ok(Event::NetworkGraphFailed {
                request_id,
                error,
                actual,
            }) => {
                let _ = f
                    .session
                    .fail_network_graph_activation(&request_id, error, actual);
            }
            Ok(Event::NetworkGraphActivated(ack)) => {
                let _ =
                    f.session
                        .complete_network_graph_activation(ack, monotonic(&f.clock), |_| Ok(()));
            }
            Ok(Event::Network(mut event)) => {
                event.monotonic_ns = monotonic(&f.clock);
                let _ = f.session.advance_network(event);
            }
            Ok(event) => {
                let _ = observation(&mut f.session, event, &f.clock);
            }
            Err(_) => {}
        }
        f.session
            .expire_network_authority(monotonic(&f.clock))
            .unwrap();
        f.sync();
        graphs.poll(&mut f.session, monotonic(&f.clock)).unwrap();
        let resources = graphs.service.resources();
        if f.session.closing_network_source().is_none()
            && resources.resident_bytes == 0
            && resources.pending_retirements == 0
            && !resources.preparing_or_candidate
        {
            break;
        }
        assert!(
            Instant::now() < end,
            "actual renderer graph/source owners did not retire"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(f.session.snapshot().muted);
    assert!(f.session.renderer_graph_continuation().is_none());
    if !f.session.shutdown_requested() {
        let shutdown = f.command("shutdown-bridge", json!({"method":"owner.shutdown"}));
        f.local(shutdown);
    }
    let end = Instant::now() + Duration::from_secs(10);
    while !renderer.is_idle(&f.session) {
        renderer.poll(&mut f.session, monotonic(&f.clock)).unwrap();
        assert!(
            Instant::now() < end,
            "actual renderer listener did not join"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn owner_request(runtime: &Path, id: &str, payload: Value) -> Value {
    let read = json!({"schema_version":1,"request_id":format!("state-{id}"),"zone_id":"main","session_id":null,"expected_revision":null,"payload":{"method":"state.get"}});
    let state: Value = serde_json::from_slice(
        &crate::request(runtime, &serde_json::to_vec(&read).unwrap()).unwrap(),
    )
    .unwrap();
    let state = &state["receipt"]["result"]["value"];
    let envelope = json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":state["owner_id"],"session_id":state["session_id"],"expected_revision":state["revision"],"payload":payload});
    let bytes = serde_json::to_vec(&envelope).unwrap();
    decode_command(&bytes, &ControlLimits::default())
        .unwrap_or_else(|error| panic!("invalid owned fixture command {id}: {error:?}"));
    let response: Value =
        serde_json::from_slice(&crate::request(runtime, &bytes).unwrap()).unwrap();
    response["receipt"].clone()
}
fn owner_soap(
    address: std::net::SocketAddrV4,
    service: earph_network::upnp::Service,
    name: &str,
    args: &str,
) -> String {
    let mut stream = std::net::TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let ns = service.namespace();
    let body = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{name} xmlns:u="{ns}">{args}</u:{name}></s:Body></s:Envelope>"#
    );
    let header = format!(
        "POST /upnp/{}/control HTTP/1.1\r\nHost: {address}\r\nContent-Type: text/xml; charset=utf-8\r\nSOAPACTION: \"{ns}#{name}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        service.slug(),
        body.len()
    );
    // Fragment the actual request across headers and the authored URI/body.
    for fragment in header
        .as_bytes()
        .chunks(17)
        .chain(body.as_bytes().chunks(31))
    {
        stream.write_all(fragment).unwrap();
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
fn run_owner_case(directory: &Path, case: &str) {
    use earph_network::upnp::Service::{AvTransport, RenderingControl};
    let mut capture = Capture::new(directory.join("native.pcm"));
    let media = Media::new(
        fs::read(directory.parent().unwrap().join("public.flac")).unwrap(),
        fs::read(directory.parent().unwrap().join("b.flac")).unwrap(),
    );
    let IpAddr::V4(peer) = media.peer else {
        unreachable!()
    };
    let runtime = std::env::temp_dir().join(format!(
        "eh-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let owner = crate::EngineOwner::bind(&runtime)
        .unwrap()
        .with_renderer(
            crate::RendererConfiguration::new(
                std::net::SocketAddrV4::new(peer, 0),
                "uuid:00000000-0000-0000-0000-000000000003",
                "Owner software fixture",
                "Earph/1 UPnP/1.0 Earph/1",
            )
            .unwrap(),
        )
        .unwrap();
    let address = owner.renderer_address().unwrap();
    let owner = thread::spawn(move || owner.serve());
    for (id, payload) in [
        (
            "output",
            json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"earph_capture","device_id":"fixture","profile_id":"fixture","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}}),
        ),
        (
            "caps",
            json!({"method":"volume.caps.set","software_millidecibels":0,"hardware_millidecibels":null}),
        ),
        (
            "gain",
            json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}),
        ),
        (
            "consent",
            json!({"method":"output.consent","device_id":"fixture","profile_id":"fixture"}),
        ),
        (
            "queue",
            json!({"method":"queue.append","path":directory.parent().unwrap().join("public.flac")}),
        ),
        (
            "origins",
            json!({"method":"sources.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"origins":[{"origin":media.origin,"pins":[peer],"roots_der":[]}]}}),
        ),
    ] {
        let receipt = owner_request(&runtime, id, payload);
        assert_eq!(receipt["status"], "applied", "{receipt}");
    }
    let state =
        owner_request(&runtime, "grant-state", json!({"method":"state.get"}))["result"]["value"]
            .clone();
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let graph = json!({"schema_version":1,"nodes":[{"id":"input","kind":{"kind":"input"},"format":port},{"id":"half","kind":{"kind":"gain","gain_linear":0.5},"format":port},{"id":"output","kind":{"kind":"output"},"format":port}],"edges":[{"from":"input","to":"half","from_port":0,"to_port":0},{"from":"half","to":"output","from_port":0,"to_port":0}],"output":"output"});
    let mut rights = vec!["stage", "transport", "rendering_control", "events"];
    if case == "takeover" {
        rights.push("takeover");
    }
    let receipt = owner_request(
        &runtime,
        "controllers",
        json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"owner-controller","peers":[peer],"rights":rights,"playback":{"intent":{"graph":graph,"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}},"output":state["output"],"expected_device_epoch":state["device_epoch"]}}]}}),
    );
    assert_eq!(receipt["status"], "applied", "{receipt}");
    let queue = owner_request(
        &runtime,
        "queue-before",
        json!({"method":"queue.get","offset":0,"limit":32}),
    )["result"]["value"]["entries"]
        .clone();
    assert!(
        owner_soap(address, AvTransport, "Stop", "<InstanceID>0</InstanceID>")
            .starts_with("HTTP/1.1 200")
    );
    let stage = format!(
        "<InstanceID>0</InstanceID><CurrentURI>{}/b</CurrentURI><CurrentURIMetaData>public owner software fixture</CurrentURIMetaData>",
        media.origin
    );
    assert!(
        owner_soap(address, AvTransport, "SetAVTransportURI", &stage).starts_with("HTTP/1.1 200")
    );
    let response = owner_soap(
        address,
        AvTransport,
        "Play",
        "<InstanceID>0</InstanceID><Speed>1</Speed>",
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let early = owner_soap(
        address,
        RenderingControl,
        "SetMute",
        "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredMute>0</DesiredMute>",
    );
    assert!(
        early.starts_with("HTTP/1.1 500"),
        "early Unmute must fail: {early}"
    );
    media.release.store(true, Ordering::Release);
    let end = Instant::now() + Duration::from_secs(10);
    let mut serial = 0;
    loop {
        serial += 1;
        let state = owner_request(
            &runtime,
            &format!("ready-{serial}"),
            json!({"method":"state.get"}),
        );
        if state["result"]["value"]["network_playback"]["processing"].is_object() {
            break;
        }
        assert!(
            Instant::now() < end,
            "actual owner never prepared/activated its renderer graph: {state}"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(media.b_requests.load(Ordering::Acquire), 1);
    let state = owner_request(&runtime, "still-muted", json!({"method":"state.get"}));
    assert_eq!(state["result"]["value"]["muted"], true);
    assert!(
        owner_soap(address, AvTransport, "Pause", "<InstanceID>0</InstanceID>")
            .starts_with("HTTP/1.1 200")
    );
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        serial += 1;
        let state = owner_request(
            &runtime,
            &format!("paused-{serial}"),
            json!({"method":"state.get"}),
        );
        if state["result"]["value"]["network_playback"]["lifecycle"] == "paused" {
            break;
        }
        assert!(Instant::now() < end, "native Pause did not settle: {state}");
        thread::sleep(Duration::from_millis(1));
    }
    if case != "play" {
        verify_paused_staging_replacement(
            directory,
            &runtime,
            address,
            &media,
            case,
            &queue,
            &mut capture,
        );
        let shutdown = owner_request(
            &runtime,
            "shutdown-replacement",
            json!({"method":"owner.shutdown"}),
        );
        assert_ne!(shutdown["status"], "rejected", "{shutdown}");
        owner.join().unwrap().unwrap();
        let bytes = capture.finish();
        assert!(
            bytes.iter().all(|byte| *byte == 0),
            "replacement Play must remain muted"
        );
        fs::write(directory.join("captured-replacement.pcm"), bytes).unwrap();
        return;
    }
    assert!(
        owner_soap(
            address,
            AvTransport,
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>"
        )
        .starts_with("HTTP/1.1 200")
    );
    for (action, args, expected_gain, expected_zero) in [
        (
            "SelectPreset",
            "<InstanceID>0</InstanceID><PresetName>FactoryDefaults</PresetName>",
            -40.0,
            false,
        ),
        (
            "SetVolumeDB",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>-1536</DesiredVolume>",
            -6.0,
            false,
        ),
        (
            "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>0</DesiredVolume>",
            -6.0,
            true,
        ),
        (
            "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>30700</DesiredVolume>",
            0.0,
            false,
        ),
    ] {
        let response = owner_soap(address, RenderingControl, action, args);
        assert!(response.starts_with("HTTP/1.1 200"), "{action}: {response}");
        serial += 1;
        let state = owner_request(
            &runtime,
            &format!("volume-{serial}"),
            json!({"method":"state.get"}),
        );
        assert_eq!(state["result"]["value"]["software_gain_db"], expected_gain);
        assert_eq!(
            state["result"]["value"]["software_gain_zero"],
            expected_zero
        );
        assert_eq!(state["result"]["value"]["muted"], true);
    }
    for (service, action, args) in [
        (AvTransport, "GetMediaInfo", "<InstanceID>0</InstanceID>"),
        (
            AvTransport,
            "GetTransportInfo",
            "<InstanceID>0</InstanceID>",
        ),
        (AvTransport, "GetPositionInfo", "<InstanceID>0</InstanceID>"),
        (
            AvTransport,
            "GetDeviceCapabilities",
            "<InstanceID>0</InstanceID>",
        ),
        (
            AvTransport,
            "GetTransportSettings",
            "<InstanceID>0</InstanceID>",
        ),
        (
            AvTransport,
            "GetCurrentTransportActions",
            "<InstanceID>0</InstanceID>",
        ),
        (
            RenderingControl,
            "ListPresets",
            "<InstanceID>0</InstanceID>",
        ),
        (
            RenderingControl,
            "GetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            RenderingControl,
            "GetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            RenderingControl,
            "GetVolumeDB",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            RenderingControl,
            "GetVolumeDBRange",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            earph_network::upnp::Service::ConnectionManager,
            "GetProtocolInfo",
            "",
        ),
        (
            earph_network::upnp::Service::ConnectionManager,
            "GetCurrentConnectionIDs",
            "",
        ),
        (
            earph_network::upnp::Service::ConnectionManager,
            "GetCurrentConnectionInfo",
            "<ConnectionID>+0</ConnectionID>",
        ),
    ] {
        let response = owner_soap(address, service, action, args);
        assert!(response.starts_with("HTTP/1.1 200"), "{action}: {response}");
        if action == "GetVolumeDBRange" {
            assert!(response.contains("<MinValue>-32767</MinValue><MaxValue>0</MaxValue>"));
        }
        if action == "GetVolume" {
            assert!(response.contains("<CurrentVolume>30700</CurrentVolume>"));
        }
    }
    assert!(
        owner_soap(
            address,
            RenderingControl,
            "SetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredMute>0</DesiredMute>"
        )
        .starts_with("HTTP/1.1 200")
    );
    capture.full();
    let response = owner_soap(address, AvTransport, "Stop", "<InstanceID>0</InstanceID>");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    capture.release();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        serial += 1;
        let state = owner_request(
            &runtime,
            &format!("stopped-{serial}"),
            json!({"method":"state.get"}),
        );
        if state["result"]["value"]["source"].is_null() {
            break;
        }
        assert!(
            Instant::now() < end,
            "actual renderer source did not retire: {state}"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        owner_request(
            &runtime,
            "queue-after",
            json!({"method":"queue.get","offset":0,"limit":32})
        )["result"]["value"]["entries"],
        queue
    );
    let revoked = owner_request(
        &runtime,
        "revoke-media-after-stop",
        json!({"method":"sources.grants.revoke","expected_generation":1}),
    );
    assert_eq!(revoked["status"], "applied", "{revoked}");
    let callback = TcpListener::bind((peer, 0)).unwrap();
    let callback_address = callback.local_addr().unwrap();
    let callback_thread = thread::spawn(move || {
        let (mut stream, _) = callback.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
            assert!(headers.len() < 32768);
        }
        let headers = String::from_utf8(headers).unwrap();
        let count = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; count];
        stream.read_exact(&mut body).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        String::from_utf8(body).unwrap()
    });
    let granted = owner_request(
        &runtime,
        "callbacks-after-stop",
        json!({"method":"renderer.callbacks.replace","controller_id":"owner-controller","expected_controller_generation":1,"expected_generation":0,"lifetime_ms":60000,"grants":{"origins":[{"origin":format!("http://{callback_address}"),"pins":[peer]}]}}),
    );
    assert_eq!(granted["status"], "applied", "{granted}");
    let mut subscribe = std::net::TcpStream::connect(address).unwrap();
    subscribe
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    write!(subscribe,"SUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: {address}\r\nNT: upnp:event\r\nCALLBACK: <http://{callback_address}/after-stop>\r\nTIMEOUT: Second-60\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    subscribe.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let after_stop = callback_thread.join().unwrap();
    assert!(after_stop.contains("STOPPED"), "{after_stop}");
    assert_eq!(media.b_requests.load(Ordering::Acquire), 1);
    let shutdown = owner_request(&runtime, "shutdown", json!({"method":"owner.shutdown"}));
    assert_ne!(shutdown["status"], "rejected", "{shutdown}");
    owner.join().unwrap().unwrap();
    let bytes = capture.finish();
    let (samples, remainder) = bytes.as_chunks::<2>();
    assert!(remainder.is_empty());
    assert!(
        samples
            .iter()
            .any(|sample| i16::from_le_bytes(*sample) == 15770),
        "pinned half-gain graph never reached actual software output"
    );
    assert!(
        samples
            .iter()
            .all(|sample| matches!(i16::from_le_bytes(*sample), 0 | 15770)),
        "output inherited processing outside the pinned half-gain graph"
    );
    fs::write(directory.join("captured-owner.pcm"), bytes).unwrap();
}

fn verify_paused_staging_replacement(
    directory: &Path,
    runtime: &Path,
    address: std::net::SocketAddrV4,
    media: &Media,
    case: &str,
    queue: &Value,
    capture: &mut Capture,
) {
    use earph_network::upnp::Service::AvTransport;
    let active_before = owner_request(
        runtime,
        "active-before-replacement",
        json!({"method":"sources.network.get"}),
    )["result"]["value"]["active"]
        .clone();
    assert!(active_before.is_object());
    assert_eq!(media.b_requests.load(Ordering::Acquire), 1);
    assert_eq!(media.a_requests.load(Ordering::Acquire), 0);
    let uri = format!("{}/a", media.origin);
    let stage = format!(
        "<InstanceID>0</InstanceID><CurrentURI>{uri}</CurrentURI><CurrentURIMetaData>paused replacement fixture</CurrentURIMetaData>"
    );
    let response = owner_soap(address, AvTransport, "SetAVTransportURI", &stage);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let staged = owner_request(
        runtime,
        "staged-replacement",
        json!({"method":"renderer.get"}),
    )["result"]["value"]
        .clone();
    assert_eq!(staged["active"]["binding"], active_before);
    assert_ne!(staged["staged"]["media_id"], active_before["media_id"]);
    capture.release();
    let response = owner_soap(
        address,
        AvTransport,
        "Play",
        "<InstanceID>0</InstanceID><Speed>1</Speed>",
    );
    let mut active_after = owner_request(
        runtime,
        "active-after-replacement",
        json!({"method":"sources.network.get"}),
    )["result"]["value"]["active"]
        .clone();
    if case == "takeover" {
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(
            active_after["media_id"], staged["staged"]["media_id"],
            "accepted Play must bind the newly staged descriptor"
        );
        assert_ne!(active_after["session_id"], active_before["session_id"]);
        assert_ne!(
            active_after["source_generation"],
            active_before["source_generation"]
        );
        let end = Instant::now() + Duration::from_secs(10);
        let mut serial = 0;
        loop {
            serial += 1;
            let state = owner_request(
                runtime,
                &format!("replacement-ready-{serial}"),
                json!({"method":"state.get"}),
            );
            if state["result"]["value"]["network_playback"]["binding"] == active_after
                && state["result"]["value"]["network_playback"]["processing"].is_object()
            {
                assert_eq!(state["result"]["value"]["muted"], true);
                break;
            }
            if Instant::now() >= end {
                let graph = owner_request(
                    runtime,
                    "replacement-graph-diagnostic",
                    json!({"method":"graph.inspect"}),
                );
                let renderer = owner_request(
                    runtime,
                    "replacement-renderer-diagnostic",
                    json!({"method":"renderer.get"}),
                );
                fs::write(
                    directory.join("replacement-failure.json"),
                    serde_json::to_vec_pretty(
                        &json!({"state":state,"graph":graph,"renderer":renderer}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            assert!(
                Instant::now() < end,
                "replacement did not reach actual measured activation: {state}"
            );
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(media.a_requests.load(Ordering::Acquire), 1);
    } else {
        assert!(
            response.starts_with("HTTP/1.1 500") && response.contains("<errorCode>701</errorCode>"),
            "changed staging must respect the existing takeover requirement: {response}"
        );
        assert_eq!(active_after, active_before);
        let state = owner_request(
            runtime,
            "denied-replacement-state",
            json!({"method":"state.get"}),
        );
        assert_eq!(state["result"]["value"]["transport"], "paused");
        assert_eq!(state["result"]["value"]["muted"], true);
        assert_eq!(media.a_requests.load(Ordering::Acquire), 0);
    }
    assert_eq!(
        media.b_requests.load(Ordering::Acquire),
        1,
        "paused source must never be refetched"
    );
    let projection = owner_soap(
        address,
        AvTransport,
        "GetMediaInfo",
        "<InstanceID>0</InstanceID>",
    );
    assert!(
        projection.starts_with("HTTP/1.1 200")
            && projection.contains(&format!("<CurrentURI>{uri}</CurrentURI>"))
            && projection.contains("paused replacement fixture"),
        "{projection}"
    );
    fs::write(directory.join("replacement.json"), serde_json::to_vec_pretty(&json!({"case":case,"active_before":active_before,"staged":staged["staged"],"active_after":active_after,"a_requests":media.a_requests.load(Ordering::Acquire),"b_requests":media.b_requests.load(Ordering::Acquire),"projection":projection})).unwrap()).unwrap();
    let response = owner_soap(address, AvTransport, "Stop", "<InstanceID>0</InstanceID>");
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    capture.release();
    let end = Instant::now() + Duration::from_secs(10);
    let mut serial = 0;
    loop {
        serial += 1;
        let state = owner_request(
            runtime,
            &format!("replacement-stopped-{serial}"),
            json!({"method":"state.get"}),
        );
        if state["result"]["value"]["source"].is_null() {
            break;
        }
        assert!(
            Instant::now() < end,
            "replacement source did not retire: {state}"
        );
        thread::sleep(Duration::from_millis(1));
    }
    active_after = owner_request(
        runtime,
        "replacement-network-retired",
        json!({"method":"sources.network.get"}),
    )["result"]["value"]["active"]
        .clone();
    assert!(active_after.is_null());
    assert_eq!(
        &owner_request(
            runtime,
            "replacement-queue-after",
            json!({"method":"queue.get","offset":0,"limit":32})
        )["result"]["value"]["entries"],
        queue
    );
}
