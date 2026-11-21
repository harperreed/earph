// ABOUTME: Qualifies live clock exchanges over the pinned authenticated Python peer.
// ABOUTME: Keeps real transport timestamps separate from output or accuracy authority.
use commit_services as fixture;
#[path = "support/sendspin_transport_peer.rs"]
mod real_peer;
use earph_core::ids::OwnerId;
use earph_network::sendspin::*;
use earph_sendspin::{pairing::*, session::*};
use serde_json::json;
use std::{
    fs,
    net::{IpAddr, UdpSocket},
    thread,
    time::{Duration, Instant},
};
struct Services;
impl SendspinServices for Services {
    fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        Box::pin(async { Ok(true) })
    }
    fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        panic!("clock-only peer requests no pairing")
    }
    fn commit(&mut self, _: AttemptId, _: RecordUpdate) -> Operation<Result<(), CommitOutcome>> {
        panic!("clock-only peer requests no commit")
    }
}
fn interface() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    let ip = socket.local_addr().unwrap().ip();
    assert!(matches!(ip,IpAddr::V4(v) if v.is_private()));
    ip
}
fn burst(mode: &str) {
    let store = fixture::FixtureStore::allocate(mode);
    let ip = interface();
    let mut peer = real_peer::Peer::new(ip, mode, store.directory());
    let ready = peer.read();
    assert_eq!(ready["stage"], "ready");
    let permit = SendspinConnectPermit::new(
        OwnerId::new("clock-fixture").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(30),
        &format!(
            "ws://clock.invalid:{}/sendspin",
            ready["port"].as_u64().unwrap()
        ),
        vec![ip],
    )
    .unwrap();
    let mut worker = SendspinWorker::default();
    let handle = worker
        .start(
            permit.clone(),
            &permit,
            SessionConfig {
                private: [5; 32],
                pairing_psk: [7; 32],
                connection_generation: 1,
                hello: json!({"name":"roleless clock fixture","supported_roles":[]}),
                catalog: RecordCatalog::new(5).unwrap(),
                protected: vec![],
                limits: roleless_session_limits(),
            },
            clock_configuration(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    let observer = handle.observer();
    let burst = peer.read();
    assert_eq!(burst["stage"], "clock_burst");
    let echoes = burst["echoes"].as_array().unwrap();
    assert_eq!(echoes.len(), 8);
    assert!(
        echoes
            .windows(2)
            .all(|w| w[0].as_i64().unwrap() < w[1].as_i64().unwrap())
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while observer
        .observation()
        .clock
        .is_none_or(|clock| clock.snapshot.count != 1)
    {
        assert!(
            observer.observation().terminal.is_none(),
            "{:?}",
            observer.observation()
        );
        assert!(
            Instant::now() < deadline,
            "mapping deadline: {:?}",
            observer.observation()
        );
        thread::sleep(Duration::from_millis(2));
    }
    let mapped = observer.observation().clock.unwrap();
    assert_eq!(mapped.domain_generation, 1);
    assert_eq!(mapped.snapshot.mapping_revision, 1);
    assert_eq!(mapped.snapshot.diagnostics.accepted_measurements, 1);
    assert_eq!(mapped.snapshot.diagnostics.completed_bursts, 1);
    assert_eq!(mapped.snapshot.diagnostics.emitted_requests, 8);
    assert_eq!(
        mapped.snapshot.diagnostics.timeouts,
        u64::from(mode == "clock_timeout")
    );
    let estimate = mapped.snapshot.estimate.unwrap();
    // One actual wire exchange plus the captured selected T4 must explain the first integer estimate.
    assert!(
        burst["exchanges"]
            .as_array()
            .unwrap()
            .iter()
            .any(|exchange| {
                let t1 = exchange[0].as_i64().unwrap();
                let t2 = exchange[1].as_i64().unwrap();
                let t3 = exchange[2].as_i64().unwrap();
                (((t2 - t1) + (t3 - estimate.last_update_us)) / 2) as f64 == estimate.offset_us
            })
    );
    fs::write(
        store.directory().join("clock-burst-receipt.json"),
        serde_json::to_vec_pretty(&burst).unwrap(),
    )
    .unwrap();
    if mode == "clock_rotation" {
        assert_eq!(peer.read()["stage"], "clock_quiet");
        let deadline = Instant::now() + Duration::from_secs(5);
        while observer.observation().phase != SessionPhase::AwaitNewActivation {
            assert!(Instant::now() < deadline, "{:?}", observer.observation());
            thread::sleep(Duration::from_millis(2));
        }
        let quiet = observer.observation().clock.unwrap();
        assert_eq!(quiet.snapshot.estimate, mapped.snapshot.estimate);
        assert_eq!(quiet.snapshot.mapping_revision, 1);
        assert_eq!(quiet.snapshot.diagnostics.emitted_requests, 9);
        assert_eq!(quiet.snapshot.diagnostics.incomplete_bursts, 1);
        thread::sleep(Duration::from_millis(20));
        assert_eq!(
            observer
                .observation()
                .clock
                .unwrap()
                .snapshot
                .diagnostics
                .emitted_requests,
            9
        );
        fs::write(store.directory().join("reactivate-clock"), b"reactivate").unwrap();
        assert_eq!(peer.read()["stage"], "clock_resumed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while observer.observation().clock.unwrap().snapshot.count != 2 {
            assert!(Instant::now() < deadline, "{:?}", observer.observation());
            thread::sleep(Duration::from_millis(2));
        }
        let resumed = observer.observation().clock.unwrap();
        assert_eq!(resumed.snapshot.mapping_revision, 2);
        assert_eq!(resumed.snapshot.diagnostics.emitted_requests, 17);
        assert_eq!(resumed.snapshot.diagnostics.incomplete_bursts, 1);
    }
    fs::write(store.directory().join("release-clock"), b"release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while observer.observation().terminal.is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    worker.retire().unwrap();
    peer.finish();
    assert!(observer.observation().joined);
}
#[test]
#[ignore = "requires the pinned offline Python peer"]
fn eight_authenticated_exchanges_establish_one_clock_mapping() {
    burst("clock");
}
#[test]
#[ignore = "requires the pinned offline Python peer"]
fn fragmented_authenticated_time_replies_establish_one_clock_mapping() {
    burst("clock_fragmented");
}

fn clock_configuration() -> earph_network::sendspin::ClockConfiguration {
    earph_network::sendspin::ClockConfiguration {
        domain: earph_network::sendspin::MonotonicDomain::new(1).unwrap(),
        policy: earph_sendspin::timing::TimePolicy::new(100_000, 1_000_000, 20_000_000).unwrap(),
    }
}

#[test]
#[ignore = "requires the pinned offline Python peer"]
fn invalid_and_duplicate_authenticated_replies_do_not_create_measurements() {
    burst("clock_invalid");
}
#[test]
#[ignore = "requires the pinned offline Python peer"]
fn actual_reply_timeout_keeps_the_burst_sequential_and_bounded() {
    burst("clock_timeout");
}
#[test]
#[ignore = "requires the pinned offline Python peer"]
fn actual_rehandshake_abandons_pending_probe_and_preserves_mapping() {
    burst("clock_rotation");
}

#[allow(dead_code)]
#[path = "support/sendspin_fixture.rs"]
mod commit_services;
struct ReleaseCommit(Option<std::sync::mpsc::Sender<()>>);
impl ReleaseCommit {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for ReleaseCommit {
    fn drop(&mut self) {
        self.release();
    }
}
struct ReleaseClockFiles(std::path::PathBuf);
impl Drop for ReleaseClockFiles {
    fn drop(&mut self) {
        use std::io::Write;
        for (name, bytes) in [
            ("release-held-clock", b"{\"after_us\":0}".as_slice()),
            ("release-clock", b"release".as_slice()),
        ] {
            if let Ok(mut file) = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.0.join(name))
            {
                let _ = file.write_all(bytes);
            }
        }
    }
}
struct CommitThreads(std::sync::Arc<std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>>);
impl Drop for CommitThreads {
    fn drop(&mut self) {
        for owned in self.0.lock().unwrap().drain(..) {
            owned.join().unwrap();
        }
    }
}
#[test]
#[ignore = "requires the pinned offline Python peer"]
fn actual_fragmented_clock_reply_held_behind_commit_cannot_update_the_abandoned_burst() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let store = fixture::FixtureStore::allocate("clock-commit");
    let ip = interface();
    let mut peer = real_peer::Peer::new(ip, "clock_commit", store.directory());
    let ready = peer.read();
    let permit = SendspinConnectPermit::new(
        OwnerId::new("clock-commit").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(30),
        &format!(
            "ws://clock.invalid:{}/sendspin",
            ready["port"].as_u64().unwrap()
        ),
        vec![ip],
    )
    .unwrap();
    let threads = Arc::new(Mutex::new(Vec::new()));
    let _threads = CommitThreads(threads.clone());
    let started = Arc::new(AtomicBool::new(false));
    let (release, gate) = mpsc::channel();
    let mut worker = SendspinWorker::default();
    let mut release = ReleaseCommit(Some(release));
    let _release_clock = ReleaseClockFiles(store.directory().to_owned());
    let services = commit_services::FixtureServices {
        directory: store.directory().to_owned(),
        started: started.clone(),
        release: Some(gate),
        threads,
        outcome: None,
        stall: None,
        cancelled: Arc::new(AtomicBool::new(false)),
        candidate_delay_ms: 20,
    };
    let handle = worker
        .start(
            permit.clone(),
            &permit,
            SessionConfig {
                private: [5; 32],
                pairing_psk: [7; 32],
                connection_generation: 1,
                hello: json!({"name":"roleless held clock fixture","supported_roles":[]}),
                catalog: RecordCatalog::new(5).unwrap(),
                protected: vec![],
                limits: roleless_session_limits(),
            },
            clock_configuration(),
            Box::new(services),
            DriverLimits::default(),
        )
        .unwrap();
    let observer = handle.observer();
    assert_eq!(peer.read()["stage"], "paired");
    let held = peer.read();
    assert_eq!(held["stage"], "clock_held");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.load(Ordering::SeqCst)
        || observer.observation().received_messages != held["messages"].as_u64().unwrap()
    {
        assert!(
            observer.observation().terminal.is_none(),
            "{:?}",
            observer.observation()
        );
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let blocked = observer.observation();
    assert!(blocked.pending_commit);
    let captured = blocked.last_arrival.unwrap();
    let clock = blocked.clock.unwrap();
    assert_eq!(clock.snapshot.count, 0);
    let prior_emissions = clock.snapshot.diagnostics.emitted_requests;
    let prior_incomplete = clock.snapshot.diagnostics.incomplete_bursts;
    assert!(prior_emissions >= 1);
    assert!(prior_incomplete >= 1);
    // Advance the driver while the final-fragment capture stays held by the real session.
    thread::sleep(Duration::from_millis(20));
    release.release();
    fs::write(
        store.directory().join("release-held-clock"),
        serde_json::to_vec(&json!({"after_us":captured.microseconds()})).unwrap(),
    )
    .unwrap();
    assert_eq!(peer.read()["stage"], "clock_burst");
    let deadline = Instant::now() + Duration::from_secs(5);
    while observer.observation().clock.unwrap().snapshot.count != 1 {
        assert!(
            observer.observation().terminal.is_none(),
            "{:?}",
            observer.observation()
        );
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let clock = observer.observation().clock.unwrap();
    assert_eq!(
        clock.snapshot.diagnostics.emitted_requests,
        prior_emissions + 8
    );
    assert_eq!(clock.snapshot.diagnostics.accepted_measurements, 1);
    assert_eq!(
        clock.snapshot.diagnostics.incomplete_bursts,
        prior_incomplete
    );
    assert!(clock.snapshot.estimate.unwrap().last_update_us > captured.microseconds());
    assert!(!observer.observation().pending_commit);
    assert_eq!(
        fixture::FixtureStore::open(store.directory())
            .read()
            .records()
            .len(),
        1
    );
    fs::write(store.directory().join("release-clock"), b"release").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while observer.observation().terminal.is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    worker.retire().unwrap();
    peer.finish();
    assert!(observer.observation().joined);
}
