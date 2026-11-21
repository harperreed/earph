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
fn observed<T>(result: Result<T, StoreError>) -> Result<T, StoreError> {
    if let Err(error) = &result {
        println!("earph-keychain-error={error:?}");
    }
    result
}
#[test]
#[ignore = "requires an explicitly prepared private keychain root on a disposable hosted Mac runner"]
fn isolated_keychain() {
    println!("earph-keychain-stage=test-started");
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
    println!("earph-keychain-stage=root-validated");
    let _interaction = SecKeychain::disable_user_interaction().unwrap();
    println!("earph-keychain-stage=interaction-disabled");
    let keychain = CreateOptions::new()
        .password("earph-synthetic-hosted-keychain-only")
        .prompt_user(false)
        .create(&path)
        .unwrap();
    println!("earph-keychain-stage=keychain-created");
    let mut store = observed(CredentialStore::open(
        PathBuf::from(env!("CARGO_BIN_EXE_earph-credential-helper")),
        NativeConfig::Keychain { path },
        &root.join("store.lock"),
        Duration::from_secs(8),
    ))
    .unwrap();
    println!("earph-keychain-stage=store-opened");
    let cancel = AtomicBool::new(false);
    assert_eq!(
        observed(store.load(&cancel)).unwrap_err(),
        StoreError::Missing
    );
    println!("earph-keychain-stage=missing-confirmed");
    observed(store.commit(&document(60), &cancel)).unwrap();
    println!("earph-keychain-stage=first-commit");
    observed(store.commit(&document(61), &cancel)).unwrap();
    println!("earph-keychain-stage=second-commit");
    assert!(
        observed(store.load(&cancel))
            .unwrap()
            .encode_secret()
            .unwrap()
            .as_secret_bytes()
            == document(61).encode_secret().unwrap().as_secret_bytes()
    );
    println!("earph-keychain-stage=readback-confirmed");
    let (_, mut item) = keychain
        .find_generic_password("org.earph.credentials", "sendspin-committed-v1")
        .unwrap();
    println!("earph-keychain-stage=native-item-opened");
    item.set_password(&[1, 2, 3]).unwrap();
    println!("earph-keychain-stage=corrupt-written");
    assert_eq!(
        observed(store.load(&cancel)).unwrap_err(),
        StoreError::InvalidDocument
    );
    assert_eq!(
        observed(store.commit(&document(62), &cancel)),
        Err(StoreError::InvalidDocument)
    );
    println!("earph-keychain-stage=corruption-refused");
    // SAFETY: This retained keychain was created above, and no other keychain reference enters this call.
    assert_eq!(
        unsafe { SecKeychainLock(keychain.as_concrete_TypeRef()) },
        0
    );
    println!("earph-keychain-stage=keychain-locked");
    assert_eq!(
        observed(store.load(&cancel)).unwrap_err(),
        StoreError::Locked
    );
    assert_eq!(
        observed(store.commit(&document(63), &cancel)),
        Err(StoreError::Locked)
    );
    println!("earph-keychain-stage=locked-refused");
}
