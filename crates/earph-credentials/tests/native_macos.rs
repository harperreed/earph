// ABOUTME: Qualifies explicit keychain storage only on an isolated hosted Mac runner.
// ABOUTME: Retains its synthetic keychain and never selects a default keychain.
#![cfg(target_os = "macos")]
use core_foundation::base::TCFType;
use earph_credentials::{CredentialStore, NativeConfig, StoreError};
use earph_sendspin::{credentials::CommittedCredentials, pairing::RecordCatalog};
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use security_framework_sys::base::SecKeychainRef;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::Duration,
};
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainLock(keychain: SecKeychainRef) -> i32;
}
fn document(seed: u8) -> CommittedCredentials {
    CommittedCredentials::new(
        [seed; 32],
        [seed + 1; 32],
        RecordCatalog::new(5).unwrap(),
        None,
    )
    .unwrap()
}
#[test]
#[ignore = "requires an explicitly prepared private keychain root on a disposable hosted Mac runner"]
fn isolated_keychain() {
    assert_eq!(std::env::var("GITHUB_ACTIONS").as_deref(), Ok("true"));
    let runner =
        PathBuf::from(std::env::var_os("RUNNER_TEMP").expect("hosted runner temporary root"));
    let root = PathBuf::from(
        std::env::var_os("EARPH_CREDENTIAL_TEST_KEYCHAIN_ROOT")
            .expect("private qualification root"),
    );
    assert!(root.is_absolute() && root.starts_with(&runner));
    let meta = fs::symlink_metadata(&root).unwrap();
    assert!(
        meta.is_dir()
            && meta.permissions().mode() & 0o077 == 0
            && meta.uid() == rustix::process::geteuid().as_raw()
    );
    assert_eq!(
        fs::read_to_string(root.join("owner-marker")).unwrap(),
        std::env::var("EARPH_CREDENTIAL_TEST_KEYCHAIN_TOKEN").unwrap()
    );
    let path = root.join("synthetic.keychain-db");
    assert!(!path.exists());
    let _interaction = SecKeychain::disable_user_interaction().unwrap();
    let keychain = CreateOptions::new()
        .password("earph-synthetic-hosted-keychain-only")
        .prompt_user(false)
        .create(&path)
        .unwrap();
    let mut store = CredentialStore::open(
        PathBuf::from(env!("CARGO_BIN_EXE_earph-credential-helper")),
        NativeConfig::Keychain { path },
        &root.join("store.lock"),
        Duration::from_secs(8),
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    assert_eq!(store.load(&cancel).unwrap_err(), StoreError::Missing);
    store.commit(&document(60), &cancel).unwrap();
    store.commit(&document(61), &cancel).unwrap();
    assert!(
        store
            .load(&cancel)
            .unwrap()
            .encode_secret()
            .unwrap()
            .as_secret_bytes()
            == document(61).encode_secret().unwrap().as_secret_bytes()
    );
    let (_, mut item) = keychain
        .find_generic_password("org.earph.credentials", "sendspin-committed-v1")
        .unwrap();
    item.set_password(&[1, 2, 3]).unwrap();
    assert_eq!(
        store.load(&cancel).unwrap_err(),
        StoreError::InvalidDocument
    );
    assert_eq!(
        store.commit(&document(62), &cancel),
        Err(StoreError::InvalidDocument)
    );
    // SAFETY: This retained keychain was created above, and no other keychain reference enters this call.
    assert_eq!(
        unsafe { SecKeychainLock(keychain.as_concrete_TypeRef()) },
        0
    );
    assert_eq!(store.load(&cancel).unwrap_err(), StoreError::Locked);
    assert_eq!(
        store.commit(&document(63), &cancel),
        Err(StoreError::Locked)
    );
}
