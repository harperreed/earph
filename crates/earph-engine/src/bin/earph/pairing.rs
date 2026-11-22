// ABOUTME: Prepares this player for a Sendspin server: creates its identity, prints its token, and pairs.
// ABOUTME: Keeps every secret out of messages and reports what the credential store holds afterward.
use crate::{
    Arguments,
    link::{Failure, Outcome},
    play::INTERRUPTED,
    sendspin::{address, encode_key, store},
};
use earph_credentials::{CredentialStore, StoreError};
use earph_engine::sendspin_credentials::{
    AdmissionPolicy, BridgeError, CredentialServices, CredentialSupervisor, SessionHandoff,
    SessionRequest, StoreConfiguration,
};
use earph_network::sendspin::{
    ClockConfiguration, DriverError, DriverLimits, MonotonicDomain, Operation,
    SendspinConnectPermit, SendspinServices, SendspinWorker, roleless_session_limits,
};
use earph_sendspin::{
    credentials::CommittedCredentials,
    noise,
    pairing::{AttemptId, Candidate, CommitOutcome, RecordCatalog, RecordUpdate},
    pairing_token::{PairingToken, encode_pairing_token},
    session::{AdmissionProposal, ApplicationMessage, SessionEvent},
    timing::TimePolicy,
};
use std::{
    os::unix::fs::DirBuilderExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    thread,
    time::{Duration, Instant},
};

/// The supervisor bounds each store operation. This bound covers a supervisor that never answers.
const ANSWER_DEADLINE: Duration = Duration::from_secs(30);
/// How long `pair` waits for the server unless `--timeout` says otherwise.
const PAIRING_WINDOW: Duration = Duration::from_secs(600);
/// A pending store write settles within the store timeout. This bound covers one that never does.
const RETIREMENT_DEADLINE: Duration = Duration::from_secs(40);
const RECOVERY: &str = "Earph refuses this store until it is recovered, and Earph has no recovery command yet; see PLAYER-CLI.md";

/// Says what a refused credential store needs. No message holds store content.
fn refusal(error: StoreError) -> Failure {
    Failure::playback(match error {
        StoreError::Uncertain => {
            return Failure::playback(format!(
                "an earlier write to the credential store never confirmed. {RECOVERY}"
            ));
        }
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

/// Prints the version-zero pairing token. Standard output is the only place that receives it.
pub fn token(arguments: &Arguments) -> Outcome<u8> {
    let mut supervisor = supervisor(configuration(arguments)?)?;
    let loaded = load(&mut supervisor, 1, AdmissionPolicy::deny())?;
    let client_public = noise::static_public_key(&loaded.config.private)
        .map_err(|_| Failure::engine("cannot derive the player ID"))?;
    let token = encode_pairing_token(&PairingToken {
        client_public,
        pairing_psk: loaded.config.pairing_psk,
    });
    println!("{token}");
    eprintln!(
        "This token is a permanent secret: anyone who holds it can pair a server with this player. Paste it only into the server's player setup."
    );
    Ok(0)
}

/// A server public key with the identifier of one long-term secret.
type Credential = ([u8; 32], [u8; 32]);

/// Passes every call to the credential services and notes the paired identity of each activation.
/// The identity proves which stored secret the server's latest handshake used.
struct Watched {
    services: CredentialServices,
    paired: Arc<Mutex<Option<Credential>>>,
}
impl SendspinServices for Watched {
    fn playback_authorized(&self) -> bool {
        self.services.playback_authorized()
    }
    fn admission(&mut self, proposal: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        let identity = proposal
            .paired_identity()
            .map(|identity| (identity.server_public(), identity.credential_id()));
        if let Ok(mut paired) = self.paired.lock() {
            *paired = identity;
        }
        self.services.admission(proposal)
    }
    fn candidate(&mut self, id: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        self.services.candidate(id)
    }
    fn commit(
        &mut self,
        id: AttemptId,
        update: RecordUpdate,
    ) -> Operation<Result<(), CommitOutcome>> {
        self.services.commit(id, update)
    }
}
fn credentials(catalog: &RecordCatalog) -> Vec<Credential> {
    let records = catalog.records().iter();
    records
        .map(|record| (record.server_public, record.psk_id()))
        .collect()
}
/// Why the wait for the server ended.
enum End {
    /// The server switched to the new secret and this player admitted its next activation.
    Switched,
    Closed(DriverError),
    TimedOut,
    Interrupted,
}
/// Says why a connection ended before pairing completed.
fn ending(error: DriverError) -> &'static str {
    match error {
        DriverError::Connect | DriverError::Upgrade => {
            "cannot connect to the server. Check the address, the port, and the path"
        }
        DriverError::Authentication => {
            "the server and this player could not authenticate each other. Check that the server holds this player's current token"
        }
        DriverError::PeerClosed => "the server closed the connection before pairing completed",
        DriverError::Admission => {
            "the server asked for something that a pairing connection refuses"
        }
        DriverError::Deadline => "the server stopped answering",
        DriverError::Service | DriverError::StorageUncertain => {
            "the credential store did not accept the new record"
        }
        _ => "the connection failed",
    }
}

/// Runs one pairing connection: wait for the server to pair, store its record, and confirm the switch.
pub fn pair(arguments: &Arguments, interrupted: &AtomicBool) -> Outcome<u8> {
    const USAGE: &str = "usage: earph sendspin pair --server ws://IP:PORT/PATH [--timeout SECONDS]";
    let Some(server) = &arguments.server else {
        return Err(Failure::usage(format!("--server is needed\n{USAGE}")));
    };
    let pinned_address = address(server)?;
    let window = arguments.timeout.unwrap_or(PAIRING_WINDOW);
    let expires = Instant::now() + window;
    // The permit and the pairing authority end together.
    let policy = AdmissionPolicy::new(true, true, expires);
    let mut supervisor = supervisor(configuration(arguments)?)?;
    let handoff = load(&mut supervisor, 1, policy.clone())?;
    let before = credentials(&handoff.config.catalog);
    let paired = Arc::new(Mutex::new(None));
    let refused = |_| Failure::engine("cannot start the pairing connection");
    let owner = earph_core::ids::OwnerId::new("earph-cli")
        .map_err(|_| Failure::engine("cannot start the pairing connection"))?;
    let permit = SendspinConnectPermit::new(owner, 1, 1, expires, server, vec![pinned_address])
        .map_err(refused)?;
    let clock = ClockConfiguration {
        domain: MonotonicDomain::new(1).map_err(refused)?,
        policy: TimePolicy::new(250_000, 10_000_000, 60_000_000)
            .map_err(|_| Failure::engine("cannot start the pairing connection"))?,
    };
    let mut network = SendspinWorker::default();
    let services = Watched {
        services: handoff.services,
        paired: paired.clone(),
    };
    let handle = network
        .start(
            permit.clone(),
            &permit,
            handoff.config,
            clock,
            Box::new(services),
            DriverLimits::default(),
        )
        .map_err(refused)?;
    let observer = handle.observer();
    eprintln!(
        "waiting up to {} s for the server to pair. Enter the token in the server's player setup. Control-C cancels",
        window.as_secs()
    );
    let mut activated = 0;
    let end = loop {
        while let Some(event) = observer.next_event() {
            if let SessionEvent::Activated(notice) = event.event() {
                activated = notice.handshake_generation;
            }
        }
        let stored = supervisor
            .observation()
            .last_commit
            .filter(|commit| commit.result.is_ok())
            .map(|commit| commit.attempt.handshake_generation);
        let identified = paired.lock().is_ok_and(|paired| paired.is_some());
        if stored.is_some_and(|generation| activated > generation) && identified {
            break End::Switched;
        }
        match observer.observation().terminal {
            // The permit ends with the window, so the transport may report the timeout first.
            Some(DriverError::Expired) => break End::TimedOut,
            Some(error) => break End::Closed(error),
            None => {}
        }
        if interrupted.load(Ordering::Relaxed) {
            break End::Interrupted;
        }
        if Instant::now() >= expires {
            break End::TimedOut;
        }
        thread::sleep(Duration::from_millis(20));
    };
    if matches!(end, End::Switched) {
        // Playback uses its own connection, so this one says that the player returns.
        let goodbye =
            ApplicationMessage::json("client/goodbye", serde_json::json!({"reason":"restart"}));
        if handle.try_send(goodbye).is_ok() {
            let sent = Instant::now() + Duration::from_secs(2);
            while observer.observation().terminal.is_none() && Instant::now() < sent {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
    policy.revoke();
    drop(handle);
    // Network joins first, so its session consumes the result of a store write in progress.
    // That write ignores cancellation: its result decides what the store holds.
    let deadline = Instant::now() + RETIREMENT_DEADLINE;
    let mut uncertain = loop {
        match network.retire() {
            Ok(()) => break false,
            Err(DriverError::CleanupTimeout) if Instant::now() < deadline => {}
            Err(_) => break true,
        }
    };
    let observed = supervisor.observation();
    uncertain |= observed.uncertain || observer.observation().recovery_required;
    if uncertain {
        return Err(Failure::playback(format!(
            "the credential store did not confirm the new pairing record, so the store may or may not hold it. {RECOVERY}"
        )));
    }
    // A fresh load reads what the store holds now.
    let after = credentials(
        &load(&mut supervisor, 2, AdmissionPolicy::deny())?
            .config
            .catalog,
    );
    let added: Vec<&Credential> = after
        .iter()
        .filter(|credential| !before.contains(credential))
        .collect();
    let confirmed = paired.lock().ok().and_then(|paired| *paired);
    if let Some(confirmed) = confirmed.filter(|confirmed| added.contains(&confirmed)) {
        println!("paired");
        println!("server key  {}", encode_key(&confirmed.0));
        return Ok(0);
    }
    let reason = match end {
        End::Switched | End::Closed(DriverError::Cancelled) => "the connection ended".to_owned(),
        End::Closed(error) => ending(error).to_owned(),
        End::TimedOut => format!(
            "timed out after {} s: the server did not complete pairing. Check that the server holds this player's token",
            window.as_secs()
        ),
        End::Interrupted => "interrupted".to_owned(),
    };
    let exit = if matches!(end, End::Interrupted) {
        INTERRUPTED
    } else {
        1
    };
    let message = match added.first() {
        Some(stored) => format!(
            "{reason}. A new record for the server with key {} was stored, but the server did not confirm that it switched to that record. Try `earph sendspin --server URL`; if the server denies it, pair again",
            encode_key(&stored.0)
        ),
        None if observed.last_commit.is_some() => {
            format!("{reason}. The credential store refused the new record. Nothing was stored")
        }
        None => format!("{reason}. Nothing was stored"),
    };
    Err(Failure { exit, message })
}

/// Returns the key of the one server that this player is paired with.
/// A prior version of a replaced record names the same server, so servers count once.
pub fn only_server(arguments: &Arguments) -> Outcome<[u8; 32]> {
    let mut supervisor = supervisor(configuration(arguments)?)?;
    let loaded = load(&mut supervisor, 1, AdmissionPolicy::deny())?;
    let mut servers: Vec<[u8; 32]> = Vec::new();
    for record in loaded.config.catalog.records() {
        if !servers.contains(&record.server_public) {
            servers.push(record.server_public);
        }
    }
    match servers.as_slice() {
        [] => Err(Failure::playback(
            "this player is paired with no server. Run `earph sendspin pair --server ws://IP:PORT/PATH` first",
        )),
        [server] => Ok(*server),
        // No message lists the stored servers.
        _ => Err(Failure::usage(
            "this player is paired with several servers. Pass --server-key KEY to choose one; `earph sendspin pair` prints the key of the server that it pairs",
        )),
    }
}
