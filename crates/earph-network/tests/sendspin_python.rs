// ABOUTME: Qualifies the actual outbound driver against pinned aiohttp and Sendspin producers.
// ABOUTME: Retains private fixture readbacks, peer input hashes, and uncertain cleanup evidence.
#[path = "support/sendspin_fixture.rs"]
mod fixture;
use earph_core::ids::OwnerId;
use earph_network::sendspin::*;
use earph_sendspin::{noise, pairing::*, session::*};
use fixture::*;
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{IpAddr, UdpSocket},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
struct Peer {
    child: Child,
    lines: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}
struct GateRelease(Option<mpsc::Sender<()>>);
impl GateRelease {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for GateRelease {
    fn drop(&mut self) {
        self.release();
    }
}
struct StorageThreads(Arc<Mutex<Vec<JoinHandle<()>>>>);
impl Drop for StorageThreads {
    fn drop(&mut self) {
        for thread in self.0.lock().unwrap().drain(..) {
            let _ = thread.join();
        }
    }
}
impl Peer {
    fn new(ip: IpAddr, mode: &str, directory: &std::path::Path) -> Self {
        Self::spawn([ip.to_string(), mode.to_owned()], directory, "peer")
    }
    fn readback(directory: &std::path::Path) -> Self {
        Self::spawn(
            ["--readback".into(), directory.to_str().unwrap().to_owned()],
            directory,
            "restart",
        )
    }
    fn spawn(arguments: [String; 2], directory: &std::path::Path, name: &str) -> Self {
        let mut child = Command::new(
            std::env::var_os("EARPH_SENDSPIN_PEER").expect("set pinned peer interpreter"),
        )
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/sendspin_peer.py"
        ))
        .args(arguments)
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            fs::File::create(directory.join(format!("{name}-stderr.log"))).unwrap(),
        ))
        .spawn()
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::sync_channel(32);
        let mut log = fs::File::create(directory.join(format!("{name}-stdout.log"))).unwrap();
        let reader = thread::spawn(move || {
            let mut source = BufReader::new(stdout);
            let mut total = 0;
            loop {
                let mut line = Vec::new();
                loop {
                    let available = source.fill_buf().unwrap();
                    if available.is_empty() {
                        return;
                    }
                    let end = available.iter().position(|b| *b == b'\n').map(|i| i + 1);
                    let count = end.unwrap_or(available.len());
                    assert!(line.len() + count <= 32000);
                    line.extend_from_slice(&available[..count]);
                    source.consume(count);
                    if end.is_some() {
                        break;
                    }
                }
                total += line.len();
                assert!(total < 128000);
                log.write_all(&line).unwrap();
                tx.try_send(serde_json::from_slice(&line).unwrap()).unwrap();
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }
    fn read(&self) -> Value {
        self.lines
            .recv_timeout(Duration::from_secs(10))
            .expect("real peer stage deadline")
    }
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "peer failed; inspect retained stderr");
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        self.reader.take().unwrap().join().unwrap();
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn interface() -> IpAddr {
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.connect("192.0.2.1:9").unwrap();
    let ip = s.local_addr().unwrap().ip();
    assert!(matches!(ip,IpAddr::V4(p) if p.is_private()));
    ip
}
fn until(mut check: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < end, "transport fixture deadline");
        thread::sleep(Duration::from_millis(2));
    }
}
fn exercise_backpressured_write(
    handle: &SendspinHandle,
    observer: &SendspinObserver,
    expire: bool,
) {
    for transmitted in 0..200 {
        let completed = observer.observation().writes_completed;
        handle
            .try_send(ApplicationMessage::json(
                "client/time",
                json!({"client_transmitted":transmitted,"padding":"a".repeat(60000)}),
            ))
            .unwrap();
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let state = observer.observation();
            if let Some(terminal) = state.terminal {
                assert!(expire && transmitted > 1);
                assert_eq!(terminal, DriverError::Deadline);
                return;
            }
            assert!(
                observer.next_event().is_none(),
                "unexpected rejected pressure producer"
            );
            if state.writes_completed > completed {
                break;
            }
            if Instant::now() >= deadline && state.pending_write {
                assert!(
                    !expire,
                    "the absolute write deadline must expire before this fixture wait"
                );
                assert!(transmitted > 1, "pressure must cross multiple real writes");
                handle.cancel();
                until(|| observer.observation().terminal.is_some());
                assert_eq!(
                    observer.observation().terminal,
                    Some(DriverError::Cancelled)
                );
                assert_eq!(observer.observation().writes_completed, completed);
                return;
            }
            assert!(
                Instant::now() < deadline || state.pending_write,
                "producer did not enter its owned write"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
    panic!("owned receive window did not produce bounded socket backpressure");
}
fn scenario(mode: &str) {
    let store = FixtureStore::allocate(mode);
    println!("private fixture: {}", store.directory().display());
    let ip = interface();
    let mut peer = Peer::new(ip, mode, store.directory());
    let ready = peer.read();
    assert_eq!(ready["stage"], "ready");
    assert_eq!(ready["source_hashes"].as_object().unwrap().len(), 13);
    fs::write(
        store.directory().join("peer-input-receipt.json"),
        serde_json::to_vec_pretty(&ready).unwrap(),
    )
    .unwrap();
    let p = SendspinConnectPermit::new(
        OwnerId::new("socket-fixture").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(30),
        &format!(
            "ws://fixture.invalid:{}/sendspin",
            ready["port"].as_u64().unwrap()
        ),
        vec![ip],
    )
    .unwrap();
    let closed_commit = matches!(
        mode,
        "commit_close" | "unchanged" | "uncertain" | "pressure" | "commit_timeout"
    );
    let (start_tx, start_rx) = mpsc::channel();
    let started = Arc::new(AtomicBool::new(false));
    let threads = Arc::new(Mutex::new(Vec::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let _owned_threads = StorageThreads(threads.clone());
    let stalled = (mode.starts_with("admission_") || mode.starts_with("candidate_"))
        && (mode.ends_with("_stall") || mode.ends_with("_timeout"));
    let stall = if mode.starts_with("admission_") {
        Some(ServiceKind::Admission)
    } else if mode.starts_with("candidate_") {
        Some(ServiceKind::Candidate)
    } else {
        None
    };
    let services = FixtureServices {
        directory: store.directory().to_owned(),
        started: started.clone(),
        release: (closed_commit || matches!(mode, "arrival" | "write_timeout")).then_some(start_rx),
        threads: threads.clone(),
        outcome: match mode {
            "unchanged" => Some(CommitOutcome::Unchanged),
            "uncertain" => Some(CommitOutcome::Uncertain),
            _ => None,
        },
        stall,
        cancelled: cancelled.clone(),
        candidate_delay_ms: if mode == "stale_candidate" { 250 } else { 0 },
    };
    let config = SessionConfig {
        private: noise::generate_static_private_key().unwrap(),
        pairing_psk: [7; 32],
        connection_generation: 1,
        hello: json!({"name":"roleless fixture","supported_roles":[]}),
        catalog: store.read(),
        protected: vec![],
        limits: roleless_session_limits(),
    };
    let limits = DriverLimits {
        write_ms: if mode == "write_timeout" { 100 } else { 5000 },
        retire_ms: 20,
        events: if mode == "pressure" { 1 } else { 32 },
        service_ms: if matches!(
            mode,
            "commit_timeout" | "admission_timeout" | "candidate_timeout"
        ) {
            40
        } else {
            5000
        },
        ..DriverLimits::default()
    };
    let mut worker = SendspinWorker::default();
    let handle = worker
        .start(p.clone(), &p, config, Box::new(services), limits)
        .unwrap();
    let observer = handle.observer();
    let mut release = GateRelease(Some(start_tx));
    let mut generations = Vec::new();
    let mut arrivals = Vec::new();
    let mut suppressed = false;
    let mut queued_during_rotation = false;
    if mode == "write_timeout" {
        until(|| started.load(Ordering::SeqCst));
        assert!(observer.observation().pending_commit);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            observer.observation().terminal,
            None,
            "write timeout must not shorten the acknowledged storage deadline"
        );
        release.release();
    }
    let preserved_arrival = if mode == "arrival" {
        assert_eq!(peer.read()["stage"], "paired");
        let sent = peer.read();
        assert_eq!(sent["stage"], "deferred_sent");
        let count = sent["messages"].as_u64().unwrap();
        until(|| observer.observation().received_messages == count);
        assert!(observer.observation().pending_commit);
        let arrival = observer.observation().last_arrival_ms.unwrap();
        release.release();
        Some(arrival)
    } else {
        None
    };
    if stalled {
        until(|| {
            if stall == Some(ServiceKind::Admission) {
                observer.observation().pending_admission
            } else {
                observer.observation().pending_candidate
            }
        });
        if mode.ends_with("_stall") {
            handle.cancel();
        }
        until(|| observer.observation().terminal.is_some());
        until(|| observer.observation().retirement == RetirementState::Exited);
        worker.retire().unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
        assert_eq!(
            observer.observation().terminal,
            Some(if mode.ends_with("_stall") {
                DriverError::Cancelled
            } else {
                DriverError::Deadline
            })
        );
    } else if closed_commit {
        until(|| started.load(Ordering::SeqCst));
        if mode == "pressure" {
            handle.cancel_pairing().unwrap();
        }
        until(|| observer.observation().terminal.is_some());
        if mode == "pressure" {
            assert_eq!(observer.observation().terminal, Some(DriverError::Budget));
        }
        if mode == "commit_timeout" {
            assert_eq!(observer.observation().terminal, Some(DriverError::Deadline));
        }
        let completed = observer.observation().writes_completed;
        assert!(observer.observation().pending_commit);
        assert_eq!(worker.retire(), Err(DriverError::CleanupTimeout));
        assert!(!observer.observation().joined);
        assert_eq!(
            observer.observation().retirement,
            RetirementState::AwaitCommit
        );
        release.release();
        until(|| observer.observation().retirement == RetirementState::Exited);
        worker.retire().unwrap();
        assert_eq!(observer.observation().writes_completed, completed);
        assert_eq!(
            observer.observation().recovery_required,
            mode == "uncertain"
        );
        if mode == "uncertain" {
            let config = SessionConfig {
                private: [5; 32],
                pairing_psk: [7; 32],
                connection_generation: 1,
                hello: json!({"name":"fixture","supported_roles":[]}),
                catalog: store.read(),
                protected: vec![],
                limits: roleless_session_limits(),
            };
            let service = FixtureServices {
                directory: store.directory().to_owned(),
                started: started.clone(),
                release: None,
                threads: threads.clone(),
                outcome: None,
                stall: None,
                cancelled: cancelled.clone(),
                candidate_delay_ms: 0,
            };
            assert!(matches!(
                worker.start(p.clone(), &p, config, Box::new(service), limits),
                Err(DriverError::StorageUncertain)
            ));
            assert_eq!(
                FixtureStore::open(store.directory()).read().records().len(),
                1
            );
            worker.acknowledge_recovery().unwrap();
        }
    } else {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let state = observer.observation();
            if mode == "suppression"
                && !queued_during_rotation
                && state.phase == SessionPhase::AwaitNewActivation
                && state.handshake_generation == 2
            {
                handle
                    .try_send(ApplicationMessage::json(
                        "client/time",
                        json!({"client_transmitted":99}),
                    ))
                    .unwrap();
                queued_during_rotation = true;
            }
            while let Some(event) = observer.next_event() {
                match event.event() {
                    SessionEvent::Activated(n) => {
                        assert!(n.roles.is_empty());
                        generations.push(n.handshake_generation);
                    }
                    SessionEvent::Message(m) => {
                        assert_eq!(m.id, 0);
                        let value: Value = serde_json::from_slice(&m.body).unwrap();
                        assert_eq!(value["type"], "server/time");
                        if value["payload"]["client_transmitted"] == -1
                            && let Some(expected) = preserved_arrival
                        {
                            assert_eq!(m.arrival_ms, expected);
                            assert_eq!(value["payload"]["padding"].as_str().unwrap().len(), 70000);
                        }
                        arrivals.push(m.arrival_ms);
                        handle
                            .try_send(ApplicationMessage::json(
                                "client/time",
                                json!({"client_transmitted":value["payload"]["client_transmitted"]}),
                            ))
                            .unwrap();
                    }
                    SessionEvent::Unsent(message) => {
                        assert_eq!(mode, "suppression");
                        assert_eq!(
                            serde_json::from_slice::<Value>(&message.body).unwrap()["payload"],
                            json!({"client_transmitted":99})
                        );
                        suppressed = true;
                    }
                    _ => {}
                }
            }
            if matches!(mode, "write_stall" | "write_timeout")
                && arrivals.len() == 3
                && generations.last() == Some(&3)
            {
                while peer.read()["stage"] != "write_stall" {}
                until(|| {
                    !observer.observation().pending_write
                        && observer.observation().command_entries == 0
                });
                exercise_backpressured_write(&handle, &observer, mode == "write_timeout");
            }
            if observer.observation().terminal.is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{mode}: {:?}",
                observer.observation()
            );
            thread::sleep(Duration::from_millis(2));
        }
        if mode == "playback" || mode == "roles" {
            assert_eq!(
                observer.observation().terminal,
                Some(DriverError::Admission)
            );
            assert_eq!(generations, [1]);
        } else if mode == "stale_candidate" {
            assert_eq!(observer.observation().handshake_generation, 2);
            assert_eq!(arrivals.len(), 1);
            assert_eq!(generations, [1, 2]);
        } else {
            assert_eq!(observer.observation().handshake_generation, 3);
            assert_eq!(arrivals.len(), 3);
            assert!(arrivals.windows(2).all(|w| w[0] <= w[1]));
            assert_eq!(generations, [1, 2, 3]);
            if mode == "corrupt" {
                assert_eq!(
                    observer.observation().terminal,
                    Some(DriverError::Authentication)
                );
            }
        }
        worker.retire().unwrap();
        if matches!(mode, "write_stall" | "write_timeout") {
            fs::write(store.directory().join("release-transport"), b"release").unwrap();
        }
        if mode == "suppression" {
            assert!(queued_during_rotation && suppressed);
        }
    }
    for thread in threads.lock().unwrap().drain(..) {
        thread.join().unwrap();
    }
    peer.finish();
    assert!(observer.observation().joined);
    assert!(!observer.observation().pending_commit);
    while let Some(event) = observer.next_event() {
        drop(event);
    }
    let status = observer.observation();
    assert_eq!(status.event_bytes, 0);
    assert_eq!(status.event_entries, 0);
    assert_eq!(status.producer_bytes, 0);
    assert_eq!(status.command_entries, 0);
    assert!(!status.pending_admission && !status.pending_candidate && !status.pending_write);
    assert!(status.event_high_water <= 1048576 && status.producer_high_water <= 1048576);
    let reread = FixtureStore::open(store.directory()).read();
    assert_eq!(
        reread.records().len(),
        usize::from(mode != "unchanged" && !stalled && mode != "stale_candidate")
    );
    let expected = json!({"capacity":reread.capacity(),"superseded":reread.superseded_ids(),
        "records":reread.records().iter().map(|record|json!({"server":record.server_public.to_vec(),
            "order":record.last_used_order,"psk_id":record.psk_id().to_vec()})).collect::<Vec<_>>()});
    let mut restart = Peer::readback(store.directory());
    assert_eq!(restart.read(), expected);
    restart.finish();
    fs::write(
        store.directory().join("transport-receipt.txt"),
        format!(
            "{:#?}\nrecords={}\n",
            observer.observation(),
            reread.records().len()
        ),
    )
    .unwrap();
}
#[test]
#[ignore = "requires explicitly pinned offline Python driver"]
fn real_socket_pairing_two_rotations_corruption_and_role_denial() {
    for mode in [
        "happy",
        "arrival",
        "suppression",
        "write_stall",
        "write_timeout",
        "corrupt",
        "playback",
        "roles",
    ] {
        scenario(mode);
    }
}
#[test]
#[ignore = "requires explicitly pinned offline Python driver"]
fn real_socket_close_retains_commit_and_classifies_storage_results() {
    for mode in [
        "commit_close",
        "unchanged",
        "uncertain",
        "pressure",
        "commit_timeout",
    ] {
        scenario(mode);
    }
}
#[test]
#[ignore = "requires explicitly pinned offline Python driver"]
fn real_socket_services_cancel_expire_and_fence_stale_candidates() {
    for mode in [
        "admission_stall",
        "candidate_stall",
        "admission_timeout",
        "candidate_timeout",
        "stale_candidate",
    ] {
        scenario(mode);
    }
}
