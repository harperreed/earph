// ABOUTME: Exercises the dormant Sendspin bridge through owned fixture processes.
// ABOUTME: Retains synthetic documents and private locks without native store access.
use earph_credentials::{NativeConfig, StoreError};
use earph_engine::sendspin_credentials::*;
use earph_sendspin::{credentials::CommittedCredentials, pairing::RecordCatalog};
use std::{
    future::Future,
    path::PathBuf,
    pin::pin,
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};
#[path = "support/sendspin_credential_helper.rs"]
mod helper;
fn wait<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                assert!(Instant::now() < deadline, "operation did not settle");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
fn document() -> CommittedCredentials {
    CommittedCredentials::new(
        [17; 32],
        [29; 32],
        RecordCatalog::new(5).unwrap(),
        Some([41; 32]),
    )
    .unwrap()
}
fn root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let p = std::env::temp_dir().join(format!(
            "earph-sendspin-services-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&p).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        p
    })
    .clone()
}
fn owner(case: &str) -> CredentialSupervisor {
    CredentialSupervisor::spawn(StoreConfiguration {
        helper: std::env::current_exe().unwrap(),
        native: NativeConfig::SecretService {
            address: format!("fixture:{case}:{}", root().display()),
        },
        lock_path: root().join(format!("{case}.lock")),
        operation_timeout: Duration::from_millis(600),
    })
    .unwrap()
}
fn request(generation: u64) -> SessionRequest {
    SessionRequest {
        connection_generation: generation,
        hello: serde_json::json!({"name":"fixture","supported_roles":[]}),
        protected: vec![],
        limits: earph_network::sendspin::roleless_session_limits(),
    }
}
fn checks() {
    for (case, want) in [
        ("missing", StoreError::Missing),
        ("locked", StoreError::Locked),
        ("unavailable", StoreError::Unavailable),
        ("ambiguous", StoreError::Ambiguous),
        ("invalid", StoreError::InvalidDocument),
    ] {
        let mut owner = owner(case);
        let result = wait(owner.load(request(1), AdmissionPolicy::deny()));
        assert!(
            matches!(result,Err(BridgeError::Store(e)) if e==want),
            "{case}"
        );
        assert!(!root().join(format!("{case}-commit")).exists());
        owner.retire(Duration::from_secs(1)).unwrap();
    }
    let mut owner = owner("good");
    let handoff = wait(owner.load(request(2), AdmissionPolicy::deny())).unwrap();
    assert_eq!(handoff.config.private, [17; 32]);
    assert_eq!(handoff.config.pairing_psk, [29; 32]);
    assert_eq!(handoff.config.connection_generation, 2);
    assert!(matches!(
        wait(owner.load(request(3), AdmissionPolicy::deny())),
        Err(BridgeError::Busy)
    ));
    drop(handoff);
    let handoff = wait(owner.load(request(3), AdmissionPolicy::deny())).unwrap();
    drop(handoff);
    owner.retire(Duration::from_secs(1)).unwrap();
}
fn main() {
    if std::env::args().nth(1).as_deref() == Some("credential-helper") {
        helper::run()
    } else if std::env::args().nth(1).as_deref() == Some("transport") {
        transport();
    } else {
        checks();
        commits();
        services();
        cancellation();
        println!("Sendspin credential process checks passed");
    }
}
#[track_caller]
fn until(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(8);
    while !condition() {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn attempt() -> earph_sendspin::pairing::AttemptId {
    earph_sendspin::pairing::AttemptId {
        connection_generation: 1,
        handshake_generation: 2,
        pairing_index: 1,
    }
}
fn replacement() -> earph_sendspin::pairing::RecordUpdate {
    use earph_sendspin::pairing::*;
    let old = PairingRecord {
        server_public: [2; 32],
        psk: [3; 32],
        last_used_order: 8,
    };
    let catalog = RecordCatalog::from_committed(5, vec![old.clone()], vec![]).unwrap();
    catalog
        .plan_replace(
            PairingRecord {
                server_public: [2; 32],
                psk: [4; 32],
                last_used_order: 9,
            },
            &[old.psk_id()],
        )
        .unwrap()
}
fn commits() {
    use earph_network::sendspin::{DriverError, SendspinServices};
    use earph_sendspin::pairing::CommitOutcome;
    let mut supervisor = owner("whole");
    let mut h = wait(supervisor.load(request(1), AdmissionPolicy::deny())).unwrap();
    let update = replacement();
    let wanted = update.replacement.clone();
    assert_eq!(wait(h.services.commit(attempt(), update)), Ok(()));
    assert_eq!(
        supervisor.observation().last_commit.unwrap().attempt,
        attempt()
    );
    assert!(!supervisor.observation().uncertain);
    drop(h);
    let h = wait(supervisor.load(request(1), AdmissionPolicy::deny())).unwrap();
    assert_eq!(h.config.catalog, wanted);
    let stored: Vec<u8> =
        serde_json::from_slice(&std::fs::read(root().join("whole-document")).unwrap()).unwrap();
    let stored = CommittedCredentials::decode_secret(&stored).unwrap();
    assert_eq!(stored.last_playback_server(), Some([41; 32]));
    assert_eq!(stored.static_private(), &[17; 32]);
    assert_eq!(stored.pairing_psk(), &[29; 32]);
    assert_eq!(stored.catalog(), &wanted);
    assert_eq!(h.config.private, [17; 32]);
    assert_eq!(h.config.pairing_psk, [29; 32]);
    drop(h);
    supervisor.retire(Duration::from_secs(1)).unwrap();
    for case in [
        "commit-missing",
        "commit-locked",
        "commit-unavailable",
        "commit-ambiguous",
        "commit-invalid-error",
        "commit-cancelled",
    ] {
        let mut s = owner(case);
        let mut h = wait(s.load(request(1), AdmissionPolicy::deny())).unwrap();
        assert_eq!(
            wait(h.services.commit(attempt(), replacement())),
            Err(CommitOutcome::Unchanged),
            "{case}"
        );
        assert!(!s.observation().uncertain);
        drop(h);
        s.retire(Duration::from_secs(1)).unwrap();
    }
    for case in ["uncertain", "mismatch", "uncertain-restart"] {
        let mut s = owner(case);
        let mut h = wait(s.load(request(1), AdmissionPolicy::deny())).unwrap();
        assert_eq!(
            wait(h.services.commit(attempt(), replacement())),
            Err(CommitOutcome::Uncertain)
        );
        let pending = s.observation().native_operation;
        assert!(pending.is_some());
        assert_eq!(
            wait(h.services.candidate(attempt())).err(),
            Some(DriverError::StorageUncertain)
        );
        drop(h);
        assert_eq!(wait(s.recover()), Ok(RecoveryRead::ValidDocument));
        assert_eq!(s.observation().native_operation, pending);
        assert!(s.observation().uncertain);
        assert!(matches!(
            wait(s.load(request(1), AdmissionPolicy::deny())),
            Err(BridgeError::Uncertain)
        ));
        s.retire(Duration::from_secs(1)).unwrap();
        let mut restarted = owner(case);
        assert!(matches!(
            wait(restarted.load(request(1), AdmissionPolicy::deny())),
            Err(BridgeError::Store(StoreError::Uncertain))
        ));
        assert_eq!(restarted.observation().native_operation, pending);
        restarted.retire(Duration::from_secs(1)).unwrap();
    }
    let mut s = owner("gated-commit");
    let mut h = wait(s.load(request(1), AdmissionPolicy::deny())).unwrap();
    let commit = h.services.commit(attempt(), replacement());
    until(|| root().join("gated-commit-entered").exists());
    assert_eq!(
        s.retire(Duration::from_millis(3)),
        Err(BridgeError::CleanupTimeout)
    );
    assert!(s.observation().pending.is_some());
    let mut other = owner("gated-commit");
    assert!(matches!(
        wait(other.load(request(2), AdmissionPolicy::deny())),
        Err(BridgeError::Store(StoreError::Busy))
    ));
    other.retire(Duration::from_secs(1)).unwrap();
    let competing = earph_credentials::CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: "fixture:good".into(),
        },
        &root().join("gated-commit.lock"),
        Duration::from_secs(1),
    );
    assert!(matches!(competing, Err(StoreError::Busy)));
    std::fs::write(root().join("gated-commit-release"), b"release").unwrap();
    assert_eq!(wait(commit), Ok(()));
    drop(h);
    s.retire(Duration::from_secs(1)).unwrap();
    let mut s = owner("gated-lost");
    let mut h = wait(s.load(request(1), AdmissionPolicy::deny())).unwrap();
    let commit = h.services.commit(attempt(), replacement());
    until(|| root().join("gated-lost-entered").exists());
    drop(commit);
    assert!(s.observation().uncertain);
    std::fs::write(root().join("gated-lost-release"), b"release").unwrap();
    until(|| s.observation().pending.is_none());
    assert_eq!(s.observation().last_commit.unwrap().result, Ok(()));
    assert!(s.observation().uncertain);
    drop(h);
    assert_eq!(wait(s.recover()), Ok(RecoveryRead::ValidDocument));
    assert!(s.observation().uncertain);
    s.retire(Duration::from_secs(1)).unwrap();
}
#[path = "../../earph-sendspin/tests/support/session_fixture.rs"]
mod session_peer;
fn proposal(pairing: bool) -> earph_sendspin::session::AdmissionProposal {
    let (mut session, mut peer) = session_peer::initial();
    let payload = if pairing {
        serde_json::json!({"activities":["pairing"],"active_roles":[],"pairing":{"method":"pairing_psk"}})
    } else {
        serde_json::json!({"activities":[],"active_roles":[]})
    };
    let events = peer
        .send_json(&mut session, "server/activate", payload, 4)
        .unwrap();
    events
        .into_iter()
        .find_map(|e| match e {
            earph_sendspin::session::SessionEvent::Admission(p) => Some(p),
            _ => None,
        })
        .unwrap()
}
fn services() {
    use earph_network::sendspin::{DriverError, SendspinServices};
    use earph_sendspin::{
        noise::{Category, Selection},
        pairing::*,
    };
    for (label, idle, pairing) in [
        ("deny", false, false),
        ("idle", true, false),
        ("pair", false, true),
        ("both", true, true),
    ] {
        let mut s = owner(label);
        let policy = AdmissionPolicy::new(idle, pairing, Instant::now() + Duration::from_secs(30));
        let mut h = wait(s.load(request(4), policy.clone())).unwrap();
        assert_eq!(wait(h.services.admission(proposal(false))), Ok(idle));
        assert_eq!(wait(h.services.admission(proposal(true))), Ok(pairing));
        let abandoned = h.services.admission(proposal(false));
        drop(abandoned);
        until(|| !s.observation().busy);
        let delayed = h.services.admission(proposal(false));
        until(|| s.observation().pending.is_none());
        policy.revoke();
        assert_eq!(wait(delayed), Ok(false));
        assert_eq!(wait(h.services.admission(proposal(false))), Ok(false));
        drop(h);
        s.retire(Duration::from_secs(1)).unwrap();
    }
    let mut s = owner("expired-policy");
    let policy = AdmissionPolicy::new(true, true, Instant::now());
    let mut h = wait(s.load(request(4), policy)).unwrap();
    assert_eq!(wait(h.services.admission(proposal(false))), Ok(false));
    drop(h);
    s.retire(Duration::from_secs(1)).unwrap();
    let mut s = owner("random");
    let mut h = wait(s.load(
        request(1),
        AdmissionPolicy::new(false, true, Instant::now() + Duration::from_secs(30)),
    ))
    .unwrap();
    let mut flights = vec![];
    for server in [40, 41] {
        let mut reducer = PairingClient::new(
            PairingContext {
                server_public: [server; 32],
                selection: Selection {
                    category: Category::Pairing,
                    sentinel_fallback: false,
                },
                connection_generation: 1,
                handshake_generation: 2,
                pairing_psk: [29; 32],
            },
            RecordCatalog::new(5).unwrap(),
            vec![],
        )
        .unwrap();
        let effects=reducer.handle(PairingEvent::Activate(serde_json::json!({"activities":["pairing"],"active_roles":[],"pairing":{"method":"pairing_psk"}})),0);
        let [PairingEffect::RequestCandidate(id)] = effects.as_slice() else {
            panic!("candidate missing")
        };
        let id = *id;
        let candidate = wait(h.services.candidate(id)).unwrap();
        let effects = reducer.handle(PairingEvent::CandidateReady(id, candidate), 1);
        let [PairingEffect::SendFlight(actual, flight)] = effects.as_slice() else {
            panic!("flight missing")
        };
        assert_eq!(*actual, id);
        flights.push(flight.messages()[1].clone());
        reducer.handle(PairingEvent::Cancel, 2);
        assert!(!root().join("random-commit").exists());
    }
    assert_ne!(flights[0], flights[1]);
    let mut wrong = attempt();
    wrong.connection_generation = 44;
    assert_eq!(
        wait(h.services.candidate(wrong)).err(),
        Some(DriverError::Admission)
    );
    drop(h);
    s.retire(Duration::from_secs(1)).unwrap();
}
fn cancellation() {
    use earph_network::sendspin::SendspinServices;
    let mut s = owner("gated-load");
    let pending = s.load(request(1), AdmissionPolicy::deny());
    until(|| root().join("gated-load-entered").exists());
    assert!(matches!(
        wait(s.load(request(2), AdmissionPolicy::deny())),
        Err(BridgeError::Busy)
    ));
    drop(pending);
    until(|| s.observation().pending.is_none());
    assert!(s.observation().cancelled);
    assert!(!root().join("gated-load-commit").exists());
    s.retire(Duration::from_secs(1)).unwrap();
    let mut s = owner("reservation");
    let mut h = wait(s.load(
        request(1),
        AdmissionPolicy::new(false, true, Instant::now() + Duration::from_secs(30)),
    ))
    .unwrap();
    let pending = h.services.candidate(attempt());
    until(|| s.observation().pending.is_none());
    assert!(matches!(
        wait(h.services.candidate(attempt())),
        Err(earph_network::sendspin::DriverError::Busy)
    ));
    drop(pending);
    until(|| !s.observation().busy);
    assert!(wait(h.services.candidate(attempt())).is_ok());
    drop(h);
    s.retire(Duration::from_secs(1)).unwrap();
    let mut s = CredentialSupervisor::spawn(StoreConfiguration {
        helper: PathBuf::from("relative"),
        native: NativeConfig::SecretService {
            address: "fixture:bad".into(),
        },
        lock_path: root().join("bad-open.lock"),
        operation_timeout: Duration::from_secs(1),
    })
    .unwrap();
    assert!(matches!(
        wait(s.load(request(1), AdmissionPolicy::deny())),
        Err(BridgeError::Store(StoreError::Unavailable))
    ));
    s.retire(Duration::from_secs(1)).unwrap();
}
#[path = "support/sendspin_transport_peer.rs"]
mod transport_peer;
fn transport() {
    use earph_network::sendspin::*;
    use std::net::{IpAddr, UdpSocket};
    for case in ["gated-transport", "gated-uncertain-transport"] {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:9").unwrap();
        let ip = socket.local_addr().unwrap().ip();
        assert!(matches!(ip,IpAddr::V4(a) if a.is_private()));
        drop(socket);
        let mut supervisor = owner(case);
        let handoff = wait(supervisor.load(
            request(1),
            AdmissionPolicy::new(true, true, Instant::now() + Duration::from_secs(30)),
        ))
        .unwrap();
        let peer_root = root().join(case);
        std::fs::create_dir(&peer_root).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&peer_root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut peer = transport_peer::Peer::new(&peer_root, ip);
        let ready = peer.read();
        assert_eq!(ready["stage"], "ready");
        assert_eq!(ready["source_hashes"].as_object().unwrap().len(), 13);
        std::fs::write(
            peer_root.join("peer-source-receipt.json"),
            serde_json::to_vec_pretty(&ready).unwrap(),
        )
        .unwrap();
        let permit = SendspinConnectPermit::new(
            earph_core::ids::OwnerId::new("bridge-process-fixture").unwrap(),
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
        let mut worker = SendspinWorker::default();
        let handle = worker
            .start(
                permit.clone(),
                &permit,
                handoff.config,
                Box::new(handoff.services),
                DriverLimits {
                    retire_ms: 10,
                    ..DriverLimits::default()
                },
            )
            .unwrap();
        let observer = handle.observer();
        assert_eq!(peer.read()["stage"], "paired");
        assert_eq!(peer.read()["stage"], "closed_during_commit");
        until(|| root().join(format!("{case}-entered")).exists());
        until(|| observer.observation().terminal.is_some());
        assert!(observer.observation().pending_commit);
        assert_eq!(worker.retire(), Err(DriverError::CleanupTimeout));
        assert_eq!(
            supervisor.retire(Duration::from_millis(3)),
            Err(BridgeError::CleanupTimeout)
        );
        assert_eq!(supervisor.observation().pending, Some(PendingKind::Commit));
        std::fs::write(root().join(format!("{case}-release")), b"release").unwrap();
        until(|| observer.observation().retirement == RetirementState::Exited);
        worker.retire().unwrap();
        peer.finish();
        assert!(!observer.observation().pending_commit);
        let uncertain = case.contains("uncertain");
        assert_eq!(observer.observation().recovery_required, uncertain);
        assert_eq!(supervisor.observation().uncertain, uncertain);
        let original = supervisor.observation().last_commit.unwrap();
        assert_eq!(original.attempt.connection_generation, 1);
        assert_eq!(original.attempt.handshake_generation, 1);
        assert_eq!(original.attempt.pairing_index, 1);
        assert_eq!(
            original.result,
            if uncertain {
                Err(earph_sendspin::pairing::CommitOutcome::Uncertain)
            } else {
                Ok(())
            }
        );
        supervisor.retire(Duration::from_secs(1)).unwrap();
        drop(handle);
        println!("real transport with synthetic helper: {case} passed");
    }
}
