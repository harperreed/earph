// ABOUTME: Qualifies the native helper against an explicitly owned private Secret Service.
// ABOUTME: Refuses ordinary test execution and never reads the workstation session bus.
#![cfg(target_os = "linux")]
use earph_credentials::{CredentialStore, NativeConfig, StoreError};
use earph_sendspin::{credentials::CommittedCredentials, pairing::RecordCatalog};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicBool,
    thread,
    time::{Duration, Instant},
};
use zbus::{
    blocking::{Connection, Proxy, connection::Builder},
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};
fn document(seed: u8) -> CommittedCredentials {
    CommittedCredentials::new(
        [seed; 32],
        [seed + 1; 32],
        RecordCatalog::new(5).unwrap(),
        None,
    )
    .unwrap()
}
fn attributes() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("application", "earph"),
        ("document", "sendspin-committed-v1"),
    ])
}
fn proxy<'a>(c: &'a Connection, path: &'a str, interface: &'a str) -> Proxy<'a> {
    Proxy::new(c, "org.freedesktop.secrets", path, interface).unwrap()
}
#[test]
#[ignore = "requires the reviewed private-daemon operator; never run against an existing keyring"]
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
    let deadline = Instant::now() + Duration::from_secs(8);
    let connection = loop {
        if let Ok(c) = Builder::address(address.as_str())
            .unwrap()
            .method_timeout(Duration::from_millis(500))
            .build()
        {
            let ready = Proxy::new(
                &c,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            )
            .unwrap()
            .call::<_, _, String>("GetNameOwner", &("org.freedesktop.secrets",))
            .is_ok();
            if ready {
                break c;
            }
        }
        assert!(
            Instant::now() < deadline,
            "private Secret Service never became ready"
        );
        thread::sleep(Duration::from_millis(25));
    };
    let service = proxy(
        &connection,
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
    );
    let collection: OwnedObjectPath = service.call("ReadAlias", &("default",)).unwrap();
    assert_ne!(
        collection.as_str(),
        "/",
        "private default collection missing"
    );
    let helper = PathBuf::from(std::env::var_os("EARPH_CREDENTIAL_HELPER").unwrap());
    let mut store = CredentialStore::open(
        helper.clone(),
        NativeConfig::SecretService {
            address: address.clone(),
        },
        &root.join("store.lock"),
        Duration::from_secs(4),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(store.load(&cancel).unwrap_err(), StoreError::Missing);
    store.commit(&document(40), &cancel).unwrap();
    assert!(store.load(&cancel).unwrap().static_private() == &[40; 32]);
    store.commit(&document(41), &cancel).unwrap();
    let readback = store.load(&cancel).unwrap().encode_secret().unwrap();
    assert!(readback.as_secret_bytes() == document(41).encode_secret().unwrap().as_secret_bytes());
    let (_, session): (OwnedValue, OwnedObjectPath) = service
        .call("OpenSession", &("plain", Value::from("")))
        .unwrap();
    let (items, _): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) =
        service.call("SearchItems", &(attributes(),)).unwrap();
    assert_eq!(items.len(), 1);
    // Native corruption is retained. The public helper must refuse to overwrite it.
    proxy(
        &connection,
        items[0].as_str(),
        "org.freedesktop.Secret.Item",
    )
    .call::<_, _, ()>(
        "SetSecret",
        &((
            session.clone(),
            Vec::<u8>::new(),
            vec![1u8, 2, 3],
            "application/octet-stream",
        ),),
    )
    .unwrap();
    assert_eq!(
        store.load(&cancel).unwrap_err(),
        StoreError::InvalidDocument
    );
    assert_eq!(
        store.commit(&document(42), &cancel),
        Err(StoreError::InvalidDocument)
    );
    assert_eq!(store.uncertain_operation(), None);
    // Add a separate synthetic record. Never repair or remove the corrupt record.
    let mut properties = HashMap::new();
    properties.insert(
        "org.freedesktop.Secret.Item.Label",
        Value::from("synthetic duplicate"),
    );
    properties.insert(
        "org.freedesktop.Secret.Item.Attributes",
        Value::from(attributes()),
    );
    let (_, prompt): (OwnedObjectPath, OwnedObjectPath) = proxy(
        &connection,
        collection.as_str(),
        "org.freedesktop.Secret.Collection",
    )
    .call(
        "CreateItem",
        &(
            properties,
            (
                session,
                Vec::<u8>::new(),
                document(43)
                    .encode_secret()
                    .unwrap()
                    .as_secret_bytes()
                    .to_vec(),
                "application/octet-stream",
            ),
            false,
        ),
    )
    .unwrap();
    assert_eq!(prompt.as_str(), "/");
    assert_eq!(store.load(&cancel).unwrap_err(), StoreError::Ambiguous);
    assert_eq!(
        store.commit(&document(44), &cancel),
        Err(StoreError::Ambiguous)
    );
    // Lock only the newly created private collection. Do not unlock it again.
    let (_, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) =
        service.call("Lock", &(vec![collection],)).unwrap();
    assert_eq!(prompt.as_str(), "/");
    assert_eq!(store.load(&cancel).unwrap_err(), StoreError::Locked);
    assert_eq!(
        store.commit(&document(45), &cancel),
        Err(StoreError::Locked)
    );
    drop(store);
    let mut absent = CredentialStore::open(
        helper,
        NativeConfig::SecretService {
            address: format!("unix:path={}", root.join("absent-bus").display()),
        },
        &root.join("unavailable.lock"),
        Duration::from_secs(2),
    )
    .unwrap();
    assert_eq!(absent.load(&cancel).unwrap_err(), StoreError::Unavailable);
}
