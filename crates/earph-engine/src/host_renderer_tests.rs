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
    for case in CASES {
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
            json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"a","peers":[f.media.peer],"rights":["stage","transport"]},{"id":"b","peers":[f.media.peer],"rights":["stage","transport","takeover"]}]}}),
        ),
        (
            "origins",
            json!({"method":"sources.grants.replace","expected_generation":0,"lifetime_ms":if case == "origin-expire" { 3000 } else { 30000 },"grants":{"origins":[{"origin":f.media.origin,"pins":[f.media.peer],"roots_der":[]}]}}),
        ),
    ] {
        let command = f.command(id, payload);
        f.local(command);
    }
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
