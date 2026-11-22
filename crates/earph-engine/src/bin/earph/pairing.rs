// ABOUTME: Prepares this player for a Sendspin server: creates its identity, prints its token, and pairs.
// ABOUTME: Keeps every secret out of messages and reports what the credential store holds afterward.
use crate::{
    Arguments,
    link::{Failure, Outcome},
    sendspin::{encode_key, store},
};
use earph_credentials::{CredentialStore, StoreError};
use earph_engine::sendspin_credentials::{
    AdmissionPolicy, BridgeError, CredentialSupervisor, SessionHandoff, SessionRequest,
    StoreConfiguration,
};
use earph_network::sendspin::{Operation, roleless_session_limits};
use earph_sendspin::{credentials::CommittedCredentials, noise, pairing::RecordCatalog};
use std::{
    os::unix::fs::DirBuilderExt,
    sync::atomic::AtomicBool,
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

/// The supervisor bounds each store operation. This bound covers a supervisor that never answers.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);
const RECOVERY: &str = "a write to the credential store never confirmed, so Earph refuses this store until it is recovered. Earph has no recovery command yet; see PLAYER-CLI.md";

/// Says what a refused credential store needs. No message holds store content.
fn refusal(error: StoreError) -> Failure {
    Failure::playback(match error {
        StoreError::Missing => "no Sendspin identity is stored. Run `earph sendspin init` first",
        StoreError::Locked => {
            "the credential store is locked. Unlock the keyring or keychain, then run this command again"
        }
        StoreError::Unavailable => {
            "the credential store is unavailable. Check --credential-store, --credential-helper, and --credential-lock"
        }
        StoreError::Ambiguous => {
            "the credential store holds several Sendspin identities, and Earph will not choose one. Remove the extra items, then run this command again"
        }
        StoreError::InvalidDocument => {
            "the stored Sendspin identity is unreadable. Earph will not replace it; remove that item yourself to start over"
        }
        StoreError::Busy => {
            "another Earph process uses this credential store. Stop it with `earph quit`, then run this command again"
        }
        StoreError::Cancelled => "the credential store operation was cancelled",
        StoreError::Uncertain => RECOVERY,
    })
}
fn bridge_refusal(error: BridgeError) -> Failure {
    match error {
        BridgeError::Store(error) => refusal(error),
        BridgeError::Busy => refusal(StoreError::Busy),
        BridgeError::Uncertain => refusal(StoreError::Uncertain),
        _ => Failure::playback("the credential store did not answer"),
    }
}
/// Names the store and makes the directory of its lock file when that directory is missing.
fn configuration(arguments: &Arguments) -> Outcome<StoreConfiguration> {
    let configuration = store(arguments)?;
    if let Some(directory) = configuration.lock_path.parent()
        && !directory.exists()
    {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|error| {
                Failure::engine(format!(
                    "cannot create {} for the credential lock: {error}",
                    directory.display()
                ))
            })?;
    }
    Ok(configuration)
}
/// Polls one supervisor operation to its end. The supervisor thread does the work.
fn settle<T>(mut operation: Operation<T>, limit: Duration) -> Option<T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Poll::Ready(value) = operation
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(5));
    }
}
fn request(connection_generation: u64) -> SessionRequest {
    SessionRequest {
        connection_generation,
        hello: serde_json::json!({"name":"Earph","supported_roles":[]}),
        protected: vec![],
        limits: roleless_session_limits(),
    }
}
/// Loads the stored identity as every Sendspin connection does.
fn load(
    supervisor: &mut CredentialSupervisor,
    connection_generation: u64,
    policy: AdmissionPolicy,
) -> Outcome<SessionHandoff> {
    settle(
        supervisor.load(request(connection_generation), policy),
        ANSWER_DEADLINE,
    )
    .ok_or_else(|| Failure::playback("the credential store did not answer"))?
    .map_err(bridge_refusal)
}
fn supervisor(configuration: StoreConfiguration) -> Outcome<CredentialSupervisor> {
    CredentialSupervisor::spawn(configuration).map_err(bridge_refusal)
}

/// Creates the player identity and the pairing secret in a store that holds neither.
pub fn init(arguments: &Arguments) -> Outcome<u8> {
    let configuration = configuration(arguments)?;
    let never = AtomicBool::new(false);
    let mut native = CredentialStore::open(
        configuration.helper.clone(),
        configuration.native.clone(),
        &configuration.lock_path,
        configuration.operation_timeout,
    )
    .map_err(refusal)?;
    // Only a definite `Missing` allows a new identity. Every other answer keeps what exists.
    match native.load(&never) {
        Err(StoreError::Missing) => {}
        Ok(_) => {
            return Err(Failure::playback(
                "a Sendspin identity already exists in this store, and nothing changed. Print its token with `earph sendspin token`",
            ));
        }
        Err(error) => return Err(refusal(error)),
    }
    let failed = || Failure::engine("cannot generate a Sendspin identity");
    let private = noise::generate_static_private_key().map_err(|_| failed())?;
    let public = noise::static_public_key(&private).map_err(|_| failed())?;
    let mut pairing_psk = [0; 32];
    getrandom::fill(&mut pairing_psk).map_err(|_| failed())?;
    let catalog = RecordCatalog::new(5).map_err(|_| failed())?;
    let document =
        CommittedCredentials::new(private, pairing_psk, catalog, None).map_err(|_| failed())?;
    // The store reports success only after it read the same bytes back.
    native
        .commit(&document, &never)
        .map_err(|error| match error {
            StoreError::Uncertain => refusal(error),
            error => Failure::playback(format!("nothing was stored: {}", refusal(error).message)),
        })?;
    // Release the store lock before the supervisor opens the same store.
    drop(native);
    let mut supervisor = supervisor(configuration)?;
    let loaded = load(&mut supervisor, 1, AdmissionPolicy::deny())?;
    if loaded.config.private != private || loaded.config.pairing_psk != pairing_psk {
        return Err(Failure::playback(
            "the credential store returned a different identity than the one just stored. Another program changed it",
        ));
    }
    println!("created a Sendspin identity");
    println!("player ID  {}", encode_key(&public));
    println!("next       earph sendspin token");
    Ok(0)
}
