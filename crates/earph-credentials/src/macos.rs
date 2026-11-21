// ABOUTME: Stores one committed document in an explicitly selected macOS keychain.
// ABOUTME: Disables helper-local interaction and never unlocks, deletes, or repairs items.
use crate::{StoreError, decode, wire::Action};
use core_foundation::base::TCFType;
use security_framework::{
    base::Error,
    item::{ItemClass, ItemSearchOptions, Limit, SearchResult},
    os::macos::keychain::SecKeychain,
};
use security_framework_sys::base::{SecKeychainRef, errSecAuthFailed, errSecItemNotFound};
use std::path::Path;
// Verified against Apple SecBase.h; absent from the sys crate.
const INTERACTION_NOT_ALLOWED: i32 = -25308;
const SERVICE: &str = "org.earph.credentials";
const ACCOUNT: &str = "sendspin-committed-v1";
// Verified against Apple's SecKeychain.h. The sys crate does not expose this function.
#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecKeychainGetStatus(keychain: SecKeychainRef, status: *mut u32) -> i32;
}
fn error(error: Error) -> StoreError {
    match error.code() {
        errSecItemNotFound => StoreError::Missing,
        INTERACTION_NOT_ALLOWED | errSecAuthFailed => StoreError::Locked,
        _ => StoreError::Unavailable,
    }
}
fn unlocked(keychain: &SecKeychain) -> Result<(), StoreError> {
    let mut status = 0;
    // SAFETY: The retained keychain reference and writable status pointer remain live during this synchronous call.
    let code = unsafe { SecKeychainGetStatus(keychain.as_concrete_TypeRef(), &mut status) };
    if code != 0 {
        return Err(StoreError::Unavailable);
    }
    if status & 1 == 0 {
        return Err(StoreError::Locked);
    }
    Ok(())
}
fn read(keychain: &SecKeychain) -> Result<Vec<u8>, StoreError> {
    unlocked(keychain)?;
    let results = ItemSearchOptions::new()
        .keychains(std::slice::from_ref(keychain))
        .class(ItemClass::generic_password())
        .service(SERVICE)
        .account(ACCOUNT)
        .limit(Limit::Max(2))
        .load_data(true)
        .search()
        .map_err(error)?;
    if results.len() > 1 {
        return Err(StoreError::Ambiguous);
    }
    match results.into_iter().next() {
        Some(SearchResult::Data(bytes)) => {
            decode(&bytes)?;
            Ok(bytes)
        }
        None => Err(StoreError::Missing),
        _ => Err(StoreError::Unavailable),
    }
}
pub(crate) fn execute(path: &Path, action: &Action) -> Result<Vec<u8>, StoreError> {
    // The process owns this setting. No native calls run after the guard restores it.
    let _no_interaction = SecKeychain::disable_user_interaction().map_err(error)?;
    let keychain = SecKeychain::open(path).map_err(|_| StoreError::Unavailable)?;
    match action {
        Action::Read => read(&keychain),
        Action::Commit(bytes) => {
            match read(&keychain) {
                Ok(_) => {
                    let (_, mut item) = keychain
                        .find_generic_password(SERVICE, ACCOUNT)
                        .map_err(error)?;
                    item.set_password(bytes)
                        .map_err(|_| StoreError::Uncertain)?;
                }
                Err(StoreError::Missing) => keychain
                    .add_generic_password(SERVICE, ACCOUNT, bytes)
                    .map_err(|_| StoreError::Uncertain)?,
                Err(e) => return Err(e),
            }
            let observed = read(&keychain).map_err(|_| StoreError::Uncertain)?;
            if observed != *bytes {
                return Err(StoreError::Uncertain);
            }
            Ok(observed)
        }
    }
}
