// ABOUTME: Exercises the public supervisor through real owned fixture processes.
// ABOUTME: Uses synthetic documents in anonymous pipes and retained private lock files.
use earph_credentials::{CredentialStore, NativeConfig, StoreError};
use earph_sendspin::{credentials::CommittedCredentials, pairing::RecordCatalog};
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn document() -> CommittedCredentials {
    CommittedCredentials::new([17; 32], [29; 32], RecordCatalog::new(5).unwrap(), None).unwrap()
}
fn root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        if let Some(path) = std::env::var_os("EARPH_CREDENTIAL_TEST_ROOT") {
            return PathBuf::from(path);
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/credential-process-tests")
            .join(format!("{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    })
    .clone()
}
fn open(name: &str) -> CredentialStore {
    CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: if name == "cancel" {
                format!("fixture:cancel:{}", root().join("cancel.pid").display())
            } else {
                format!("fixture:{name}")
            },
        },
        &root().join(format!("{name}.lock")),
        Duration::from_millis(400),
    )
    .unwrap()
}
fn fixture() {
    let mut head = [0; 4];
    std::io::stdin().read_exact(&mut head).unwrap();
    let mut body = vec![0; u32::from_be_bytes(head) as usize];
    std::io::stdin().read_exact(&mut body).unwrap();
    let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let case = req["config"]["SecretService"]["address"]
        .as_str()
        .unwrap()
        .strip_prefix("fixture:")
        .unwrap();
    let (case, pid_path) = case
        .split_once(':')
        .map_or((case, None), |(name, path)| (name, Some(path)));
    if let Some(path) = pid_path {
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        write!(file, "{}", std::process::id()).unwrap();
    }
    let write = req["action"].get("Commit").is_some();
    if case == "timeout" || case == "cancel" {
        std::thread::sleep(Duration::from_secs(3));
        return;
    }
    if case == "crash" {
        std::process::exit(9);
    }
    let result = match case {
        "missing" => serde_json::json!({"Err":"Missing"}),
        "locked" => serde_json::json!({"Err":"Locked"}),
        "unavailable" => serde_json::json!({"Err":"Unavailable"}),
        "ambiguous" => serde_json::json!({"Err":"Ambiguous"}),
        "invalid" => serde_json::json!({"Ok":[1,2,3]}),
        "recovery-locks" if write => serde_json::json!({"Err":"Uncertain"}),
        "recovery-locks" if req["operation"].as_u64().unwrap() >= 3 => {
            serde_json::json!({"Err":"Locked"})
        }
        "mismatch" if write => {
            serde_json::json!({"Ok":document().encode_secret().unwrap().as_secret_bytes()})
        }
        "uncertain" | "uncertain-restart" if write => serde_json::json!({"Err":"Uncertain"}),
        _ if write => serde_json::json!({"Ok":req["action"]["Commit"]}),
        _ => serde_json::json!({"Ok":document().encode_secret().unwrap().as_secret_bytes()}),
    };
    let mut reply = serde_json::json!({"version":1,"operation":req["operation"],"result":result});
    if case == "wrong-operation" {
        reply["operation"] = serde_json::json!(999);
    }
    let bytes = serde_json::to_vec(&reply).unwrap();
    std::io::stdout()
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    std::io::stdout().write_all(&bytes).unwrap();
}
fn checks() {
    let no = AtomicBool::new(false);
    let foreign_path = root().join("foreign.lock");
    std::fs::write(&foreign_path, b"foreign-operation-data").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&foreign_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let foreign = CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: "fixture:good".into(),
        },
        &foreign_path,
        Duration::from_secs(1),
    );
    assert!(matches!(foreign, Err(StoreError::Uncertain)));
    assert_eq!(
        std::fs::read(&foreign_path).unwrap(),
        b"foreign-operation-data"
    );
    for (name, want) in [
        ("missing", StoreError::Missing),
        ("locked", StoreError::Locked),
        ("unavailable", StoreError::Unavailable),
        ("ambiguous", StoreError::Ambiguous),
        ("invalid", StoreError::InvalidDocument),
    ] {
        assert_eq!(open(name).load(&no).unwrap_err(), want, "{name}");
    }
    let mut store = open("good");
    assert!(store.load(&no).unwrap().static_private() == &[17; 32]);
    let op = store.commit(&document(), &no).unwrap();
    assert_eq!(op, 2);
    let mut other = CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: "fixture:good".into(),
        },
        &root().join("good.lock"),
        Duration::from_secs(1),
    );
    assert!(matches!(other, Err(StoreError::Busy)));
    drop(store);
    other = CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: "fixture:good".into(),
        },
        &root().join("good.lock"),
        Duration::from_secs(1),
    );
    assert!(other.is_ok());
    for case in [
        "timeout",
        "crash",
        "wrong-operation",
        "uncertain",
        "mismatch",
    ] {
        let mut store = open(case);
        let changed =
            CommittedCredentials::new([18; 32], [30; 32], RecordCatalog::new(5).unwrap(), None)
                .unwrap();
        assert_eq!(
            store.commit(&changed, &no),
            Err(StoreError::Uncertain),
            "{case}"
        );
        let pending = store.uncertain_operation().unwrap();
        assert_eq!(store.commit(&changed, &no), Err(StoreError::Uncertain));
        assert_eq!(store.load(&no).unwrap_err(), StoreError::Uncertain);
        assert_eq!(store.uncertain_operation(), Some(pending));
        if case == "uncertain" || case == "mismatch" {
            assert!(store.recover(&no).is_ok());
            assert_eq!(store.uncertain_operation(), Some(pending));
            assert_eq!(
                store.acknowledge_native_completion(pending, &AtomicBool::new(true)),
                Err(StoreError::Cancelled)
            );
            assert_eq!(store.uncertain_operation(), Some(pending));
            assert_eq!(
                store.acknowledge_native_completion(pending + 1, &no),
                Err(StoreError::Uncertain)
            );
            store.acknowledge_native_completion(pending, &no).unwrap();
            assert_eq!(store.uncertain_operation(), None);
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let send = cancel.clone();
    let mut store = open("cancel");
    let join = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !root().join("cancel.pid").is_file() {
            assert!(
                Instant::now() < deadline,
                "owned fixture never entered execution"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        send.store(true, Ordering::Release);
    });
    let start = Instant::now();
    assert_eq!(
        store.commit(&document(), &cancel),
        Err(StoreError::Uncertain)
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(store.uncertain_operation().is_some());
    join.join().unwrap();
    let pid = std::fs::read_to_string(root().join("cancel.pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert_eq!(
        rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap()),
        Err(rustix::io::Errno::SRCH)
    );
    let mut persisted = open("persisted");
    let op = persisted.commit(&document(), &no).unwrap();
    assert_eq!(op, 1);
    drop(persisted);
    let mut persisted = open("persisted");
    assert_eq!(persisted.commit(&document(), &no).unwrap(), 2);
    drop(persisted);
    let pending = {
        let mut uncertain = open("uncertain-restart");
        assert_eq!(
            uncertain.commit(&document(), &no),
            Err(StoreError::Uncertain)
        );
        uncertain.uncertain_operation().unwrap()
    };
    let mut uncertain = open("uncertain-restart");
    assert_eq!(uncertain.uncertain_operation(), Some(pending));
    assert_eq!(uncertain.load(&no).unwrap_err(), StoreError::Uncertain);
    assert_eq!(
        uncertain.acknowledge_native_completion(pending, &no),
        Err(StoreError::Uncertain)
    );
    let mut locked_recovery = open("recovery-locks");
    assert_eq!(
        locked_recovery.commit(&document(), &no),
        Err(StoreError::Uncertain)
    );
    let pending = locked_recovery.uncertain_operation().unwrap();
    assert!(locked_recovery.recover(&no).is_ok());
    assert_eq!(
        locked_recovery.acknowledge_native_completion(pending, &no),
        Err(StoreError::Locked)
    );
    assert_eq!(locked_recovery.uncertain_operation(), Some(pending));
    let mut store = open("precancel");
    assert_eq!(
        store.commit(&document(), &AtomicBool::new(true)),
        Err(StoreError::Cancelled)
    );
    assert_eq!(store.uncertain_operation(), None);
    let mut absent = CredentialStore::open(
        PathBuf::from("/definitely/missing/earph-credentials"),
        NativeConfig::SecretService {
            address: "unix:path=/missing/bus".into(),
        },
        &root().join("absent.lock"),
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(absent.load(&no).unwrap_err(), StoreError::Unavailable);
    assert!(!format!("{absent:?}").contains("missing/bus"));
    println!("process contract: all assertions passed");
}
fn directory_sync_probe(mode: &str) {
    let store = CredentialStore::open(
        std::env::current_exe().unwrap(),
        NativeConfig::SecretService {
            address: format!(
                "fixture:{}",
                if mode == "pending" {
                    "uncertain"
                } else {
                    "good"
                }
            ),
        },
        &root().join("durability.lock"),
        Duration::from_secs(2),
    );
    if mode == "failure" {
        assert!(
            matches!(store, Err(StoreError::Uncertain)),
            "directory-sync failure must refuse opening before a helper can run"
        );
        return;
    }
    let mut store = store.unwrap();
    let result = store.commit(&document(), &AtomicBool::new(false));
    if mode == "pending" {
        assert_eq!(result, Err(StoreError::Uncertain));
    } else {
        assert!(result.is_ok());
    }
}
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("credential-helper") => fixture(),
        Some("directory-sync-probe") => directory_sync_probe(&std::env::args().nth(2).unwrap()),
        _ => checks(),
    }
}
