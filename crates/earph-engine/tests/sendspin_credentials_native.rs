// ABOUTME: Qualifies the production bridge through isolated native storage and authenticated transport.
// ABOUTME: Requires a reviewed private operator and retains every synthetic fixture.
#![cfg(target_os = "linux")]
use earph_credentials::{CredentialStore, NativeConfig, StoreError};
use earph_engine::sendspin_credentials::*;
use earph_network::sendspin::*;
use earph_sendspin::{credentials::CommittedCredentials, pairing::RecordCatalog};
use std::{
    fs,
    future::Future,
    net::{IpAddr, UdpSocket},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    pin::pin,
    sync::atomic::AtomicBool,
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Proxy, connection::Builder},
    zvariant::OwnedObjectPath,
};
fn wait<T>(f: impl Future<Output = T>) -> T {
    let mut f = pin!(f);
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => return v,
            Poll::Pending => {
                assert!(Instant::now() < end);
                thread::sleep(Duration::from_millis(2));
            }
        }
    }
}
#[path = "support/sendspin_transport_peer.rs"]
mod transport_peer;
use transport_peer::Peer;
fn request(generation: u64) -> SessionRequest {
    SessionRequest {
        connection_generation: generation,
        hello: serde_json::json!({"name":"native fixture","supported_roles":[]}),
        protected: vec![],
        limits: roleless_session_limits(),
    }
}
#[test]
#[ignore = "requires reviewed private native-service operator; never use the workstation keyring"]
fn isolated_secret_service() {
    let root = PathBuf::from(
        std::env::var_os("EARPH_CREDENTIAL_NATIVE_TEST_ROOT").expect("private operator root"),
    );
    let token =
        std::env::var("EARPH_CREDENTIAL_NATIVE_TEST_TOKEN").expect("private operator token");
    let metadata = fs::symlink_metadata(&root).unwrap();
    assert!(
        metadata.is_dir()
            && metadata.permissions().mode() & 0o077 == 0
            && metadata.uid() == rustix::process::geteuid().as_raw()
    );
    assert_eq!(
        fs::read_to_string(root.join("owner-marker")).unwrap(),
        token
    );
    let address = format!("unix:path={}", root.join("bus").display());
    assert_eq!(std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap(), address);
    let end = Instant::now() + Duration::from_secs(8);
    let connection = loop {
        if let Ok(c) = Builder::address(address.as_str())
            .unwrap()
            .method_timeout(Duration::from_millis(500))
            .build()
            && Proxy::new(
                &c,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .unwrap()
            .call::<_, _, String>("GetNameOwner", &("org.freedesktop.secrets",))
            .is_ok()
        {
            break c;
        }
        assert!(Instant::now() < end, "private Secret Service readiness");
        thread::sleep(Duration::from_millis(25));
    };
    let helper = PathBuf::from(std::env::var_os("EARPH_CREDENTIAL_HELPER").unwrap());
    let config = || StoreConfiguration {
        helper: helper.clone(),
        native: NativeConfig::SecretService {
            address: address.clone(),
        },
        lock_path: root.join("bridge.lock"),
        operation_timeout: Duration::from_secs(4),
    };
    let mut absent = CredentialSupervisor::spawn(config()).unwrap();
    assert!(matches!(
        wait(absent.load(request(1), AdmissionPolicy::deny())),
        Err(BridgeError::Store(StoreError::Missing))
    ));
    absent.retire(Duration::from_secs(1)).unwrap();
    let no = AtomicBool::new(false);
    let original = CommittedCredentials::new(
        [5; 32],
        [7; 32],
        RecordCatalog::new(5).unwrap(),
        Some([41; 32]),
    )
    .unwrap();
    let mut preload = CredentialStore::open(
        helper.clone(),
        NativeConfig::SecretService {
            address: address.clone(),
        },
        &root.join("bridge.lock"),
        Duration::from_secs(4),
    )
    .unwrap();
    preload.commit(&original, &no).unwrap();
    drop(preload);
    let mut supervisor = CredentialSupervisor::spawn(config()).unwrap();
    let handoff = wait(supervisor.load(
        request(1),
        AdmissionPolicy::new(true, true, Instant::now() + Duration::from_secs(30)),
    ))
    .unwrap();
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    let ip = socket.local_addr().unwrap().ip();
    assert!(matches!(ip,IpAddr::V4(a) if a.is_private()));
    drop(socket);
    let mut peer = Peer::new(&root, ip);
    let ready = peer.read();
    assert_eq!(ready["stage"], "ready");
    assert_eq!(ready["source_hashes"].as_object().unwrap().len(), 13);
    fs::write(
        root.join("sendspin-peer-source-receipt.json"),
        serde_json::to_vec_pretty(&ready).unwrap(),
    )
    .unwrap();
    let permit = SendspinConnectPermit::new(
        earph_core::ids::OwnerId::new("native-fixture").unwrap(),
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
            DriverLimits::default(),
        )
        .unwrap();
    let observer = handle.observer();
    assert_eq!(peer.read()["stage"], "paired");
    assert_eq!(peer.read()["stage"], "closed_during_commit");
    peer.finish();
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let state = observer.observation();
        if state.retirement == RetirementState::Exited {
            break;
        }
        assert!(Instant::now() < end);
        thread::sleep(Duration::from_millis(2));
    }
    worker.retire().unwrap();
    assert!(!observer.observation().pending_commit);
    assert!(!observer.observation().recovery_required);
    let committed = supervisor.observation().last_commit.unwrap();
    assert_eq!(committed.result, Ok(()));
    assert_eq!(committed.attempt.connection_generation, 1);
    drop(handle);
    supervisor.retire(Duration::from_secs(1)).unwrap();
    let mut store = CredentialStore::open(
        helper.clone(),
        NativeConfig::SecretService {
            address: address.clone(),
        },
        &root.join("bridge.lock"),
        Duration::from_secs(4),
    )
    .unwrap();
    let document = store.load(&no).unwrap();
    assert_eq!(document.static_private(), original.static_private());
    assert_eq!(document.pairing_psk(), original.pairing_psk());
    assert_eq!(document.last_playback_server(), Some([41; 32]));
    assert_eq!(document.catalog().records().len(), 1);
    assert_eq!(document.catalog().capacity(), 5);
    assert!(document.catalog().superseded_ids().is_empty());
    let catalog = document.catalog().clone();
    let exact = document.encode_secret().unwrap();
    assert_eq!(
        store
            .load(&no)
            .unwrap()
            .encode_secret()
            .unwrap()
            .as_secret_bytes(),
        exact.as_secret_bytes()
    );
    drop(store);
    let mut restarted = CredentialSupervisor::spawn(config()).unwrap();
    let h = wait(restarted.load(request(2), AdmissionPolicy::deny())).unwrap();
    assert_eq!(h.config.private, [5; 32]);
    assert_eq!(h.config.pairing_psk, [7; 32]);
    assert_eq!(h.config.catalog, catalog);
    drop(h);
    restarted.retire(Duration::from_secs(1)).unwrap();
    let service = Proxy::new(
        &connection,
        "org.freedesktop.secrets",
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    )
    .unwrap();
    let collection: OwnedObjectPath = service.call("ReadAlias", &("default",)).unwrap();
    let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
        service.call("Lock", &(vec![collection],)).unwrap();
    assert_eq!(prompt.as_str(), "/");
    let mut locked = CredentialSupervisor::spawn(config()).unwrap();
    assert!(matches!(
        wait(locked.load(request(3), AdmissionPolicy::deny())),
        Err(BridgeError::Store(StoreError::Locked))
    ));
    locked.retire(Duration::from_secs(1)).unwrap();
    let mut unavailable_config = config();
    unavailable_config.native = NativeConfig::SecretService {
        address: format!("unix:path={}", root.join("absent-bus").display()),
    };
    unavailable_config.lock_path = root.join("unavailable.lock");
    let mut unavailable = CredentialSupervisor::spawn(unavailable_config).unwrap();
    assert!(matches!(
        wait(unavailable.load(request(4), AdmissionPolicy::deny())),
        Err(BridgeError::Store(StoreError::Unavailable))
    ));
    unavailable.retire(Duration::from_secs(1)).unwrap();
    fs::write(root.join("sendspin-native-results.json"),b"{\"native_exact_readback\":true,\"restart_load\":true,\"authenticated_transport\":true,\"locked_refused\":true,\"unavailable_refused\":true,\"playback\":false,\"music_assistant\":false}\n").unwrap();
}
