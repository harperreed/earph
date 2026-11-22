// ABOUTME: Checks the `earph sendspin` setup commands: init, token, pair, and play on the stored record.
// ABOUTME: Uses the synthetic credential helper and local test peers, and never prints a secret.
use super::{
    linux::{Bench, Pending, Player, text},
    peer::{self, PlayerPeer},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use std::process::Output;

/// Every text form in which a 32-byte secret could leak into output.
fn forms(secret: &[u8; 32]) -> Vec<String> {
    let hex: String = secret.iter().map(|byte| format!("{byte:02x}")).collect();
    vec![
        URL_SAFE_NO_PAD.encode(secret),
        base64::engine::general_purpose::STANDARD.encode(secret),
        hex.to_uppercase(),
        hex,
    ]
}
/// Fails without printing the output when the output holds a secret.
fn assert_holds_no(output: &Output, secrets: &[&[u8; 32]], what: &str) {
    let printed = [text(&output.stdout), text(&output.stderr)].concat();
    for secret in secrets {
        let leaked = forms(secret).iter().any(|form| printed.contains(form));
        assert!(!leaked, "{what} printed a secret");
    }
}

pub fn init_creates_one_identity_and_refuses_every_other_store_state() {
    let bench = Bench::new();
    assert!(bench.stored_bytes("empty").is_none());
    let created = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    let document = bench.stored("empty").expect("init stores a valid document");
    assert!(document.catalog().records().is_empty());
    assert_eq!(document.last_playback_server(), None);
    assert_ne!(document.static_private(), &[0; 32]);
    assert_ne!(document.pairing_psk(), &[0; 32]);
    assert_ne!(document.pairing_psk(), document.static_private());
    assert_holds_no(
        &created,
        &[document.static_private(), document.pairing_psk()],
        "init",
    );
    // The player ID is public. It names this player on the server.
    let public = earph_sendspin::noise::static_public_key(document.static_private()).unwrap();
    assert!(text(&created.stdout).contains(&URL_SAFE_NO_PAD.encode(public)));
    assert!(
        !bench.directory.join("rt").exists(),
        "init started an Engine"
    );

    // The default lock file lives in the runtime directory, which no Engine has made yet.
    let helper = std::env::current_exe().unwrap();
    let store = bench.directory.join("store");
    let address = format!("fixture:empty-default:{}", store.display());
    let default_lock = [
        "sendspin",
        "init",
        "--credential-helper",
        helper.to_str().unwrap(),
        "--credential-store",
        &address,
    ];
    let defaulted = bench.earph(&default_lock).finish();
    assert_eq!(defaulted.status.code(), Some(0));
    let runtime = bench.directory.join("rt");
    assert!(runtime.join("sendspin-credentials.lock").is_file());
    let mode = std::os::unix::fs::MetadataExt::mode(&runtime.metadata().unwrap());
    assert_eq!(mode & 0o777, 0o700, "the runtime directory is private");
    assert!(bench.stored("empty-default").is_some());

    let before = bench.stored_bytes("empty").unwrap();
    let second = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(second.status.code(), Some(1));
    let refusal = text(&second.stderr);
    assert!(refusal.contains("already"), "{refusal}");
    assert!(refusal.contains("earph sendspin token"), "{refusal}");
    assert!(
        bench.stored_bytes("empty").unwrap() == before,
        "a second init changed the store"
    );
    assert_holds_no(
        &second,
        &[document.static_private(), document.pairing_psk()],
        "a second init",
    );

    for (case, problem) in [
        ("owner", "already"),
        ("locked", "locked"),
        ("unavailable", "unavailable"),
        ("ambiguous", "several"),
        ("invalid", "unreadable"),
    ] {
        let refused = bench.sendspin_with(case, &["init"]).finish();
        let refusal = text(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{case}: {refusal}");
        assert!(refusal.contains(problem), "{case}: {refusal}");
        assert!(!bench.write_issued(case), "{case}: init wrote to the store");
    }
}

/// The reference token of the pinned specification. It fixes the length and the alphabet.
const SPEC_TOKEN: &str = "SP:0AAAQEAYEAUDAOCAJBIFQYDIOB4IBCEQTCQKRMFYYDENBWHA5DYP6BYPC4PSOLZXH5DU6V97M5XXO74HR6LZ7J5PW674PT6X37T6757Y";

pub fn token_prints_the_stored_pairing_token_to_standard_output_only() {
    use earph_sendspin::pairing_token::{PairingToken, decode_pairing_token, encode_pairing_token};
    let bench = Bench::new();
    let created = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(created.status.code(), Some(0));
    let before = bench.stored_bytes("empty").unwrap();
    let document = bench.stored("empty").unwrap();
    // This output holds the token, so the test log never shows it.
    let output = bench.sendspin_with("empty", &["token"]).finish_unprinted();
    assert_eq!(output.status.code(), Some(0));
    let stdout = text(&output.stdout);
    assert_eq!(stdout.lines().count(), 1, "standard output holds one line");
    let token = stdout.trim_end_matches('\n');
    assert!(token.starts_with("SP:0"), "the token has version zero");
    assert_eq!(token.len(), SPEC_TOKEN.len());
    let alphabet = |text: &str| {
        text[4..]
            .chars()
            .all(|symbol| matches!(symbol, 'A'..='Z' | '3'..='7' | '9'))
    };
    assert!(alphabet(SPEC_TOKEN) && alphabet(token), "token alphabet");
    let expected = PairingToken {
        client_public: earph_sendspin::noise::static_public_key(document.static_private()).unwrap(),
        pairing_psk: *document.pairing_psk(),
    };
    assert!(
        decode_pairing_token(token).is_ok_and(|decoded| decoded == expected),
        "the token decodes to the stored public key and pairing secret"
    );
    assert!(
        encode_pairing_token(&expected) == token,
        "the token is canonical"
    );
    let stderr = text(&output.stderr);
    assert_eq!(stderr.lines().count(), 1, "standard error holds one line");
    assert!(stderr.contains("permanent") && stderr.contains("secret"));
    assert!(!stderr.contains(token), "standard error repeats the token");
    assert!(
        bench.stored_bytes("empty").unwrap() == before,
        "token changed the store"
    );
    assert!(
        !bench.directory.join("rt").exists(),
        "token started an Engine"
    );

    for (case, problem) in [("empty-other", "earph sendspin init"), ("locked", "locked")] {
        let refused = bench.sendspin_with(case, &["token"]).finish();
        let refusal = text(&refused.stderr);
        assert_eq!(refused.status.code(), Some(1), "{case}: {refusal}");
        assert!(refusal.contains(problem), "{case}: {refusal}");
        assert!(refused.stdout.is_empty(), "{case}: printed a token");
        assert!(
            !bench.write_issued(case),
            "{case}: token wrote to the store"
        );
    }
}

/// The public key and the pairing secret that a server reads from the printed token.
fn token_of(bench: &Bench, case: &str) -> ([u8; 32], [u8; 32]) {
    let output = bench.sendspin_with(case, &["token"]).finish_unprinted();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let token = earph_sendspin::pairing_token::decode_pairing_token(&text(&output.stdout));
    // The decoder's error names a class only, never the input.
    let token = token.expect("the printed token decodes");
    (token.client_public, token.pairing_psk)
}
/// Creates an identity in the `empty` case and returns what its token holds.
fn provisioned(bench: &Bench) -> ([u8; 32], [u8; 32]) {
    let created = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    token_of(bench, "empty")
}
/// Starts `earph sendspin pair` and accepts its connection as a server that holds the token.
fn pairing(bench: &Bench, case: &str, extra: &[&str]) -> (Player, PlayerPeer) {
    let (client, secret) = token_of(bench, case);
    let url = bench.url();
    let mut player = bench.sendspin_with(case, &[&["pair", "--server", &url], extra].concat());
    bench.await_connection(&mut player);
    (
        player,
        PlayerPeer::connect(&bench.listener, client, secret, "pr"),
    )
}
/// Completes the Pairing PSK flow as a server does, then waits for the client's goodbye.
/// Returns the long-term secret that the client delivered.
fn complete(peer: &mut PlayerPeer) -> [u8; 32] {
    let long_term = peer.receive_pairing();
    peer.acknowledge_pairing();
    peer.rotate(long_term, "lt");
    peer.activate_idle();
    long_term
}

pub fn pair_stores_one_record_and_prints_the_server_key() {
    let bench = Bench::new();
    provisioned(&bench);
    let identity = bench.stored("empty").unwrap();
    let (player, mut peer) = pairing(&bench, "empty", &[]);
    let long_term = complete(&mut peer);
    assert_eq!(peer.await_goodbye()["reason"], "restart");
    let output = player.finish();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    let mut lines = stdout.lines();
    assert_eq!(lines.next(), Some("paired"));
    let key = lines
        .next()
        .and_then(|line| line.strip_prefix("server key  "));
    assert_eq!(key, Some(bench.key.as_str()), "{stdout}");
    let stored = bench.stored("empty").unwrap();
    let server = earph_sendspin::noise::static_public_key(&peer::SERVER_PRIVATE).unwrap();
    let records = stored.catalog().records();
    assert_eq!(records.len(), 1, "the store holds one record");
    assert_eq!(records[0].server_public, server);
    assert!(records[0].psk == long_term, "the stored secret differs");
    assert!(
        stored.static_private() == identity.static_private()
            && stored.pairing_psk() == identity.pairing_psk(),
        "pairing changed the identity"
    );
    assert_holds_no(
        &output,
        &[stored.static_private(), stored.pairing_psk(), &long_term],
        "pair",
    );
    assert!(
        !bench.directory.join("rt").exists(),
        "pair started an Engine"
    );
}

pub fn an_incomplete_pairing_fails_and_leaves_the_store_unchanged() {
    let bench = Bench::new();
    provisioned(&bench);
    let before = bench.stored_bytes("empty").unwrap();
    let unchanged = |what: &str| {
        assert!(
            bench.stored_bytes("empty").unwrap() == before,
            "{what} changed the store"
        );
    };

    // The server takes the new secret and never acknowledges it.
    let (player, mut peer) = pairing(&bench, "empty", &["--timeout", "3"]);
    let started = std::time::Instant::now();
    let unacknowledged = peer.receive_pairing();
    peer.serve_to_close("the command gives up");
    let output = player.finish();
    let failure = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{failure}");
    assert!(failure.contains("timed out"), "{failure}");
    assert!(failure.contains("Nothing was stored"), "{failure}");
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
    assert!(!text(&output.stdout).contains("paired"));
    assert_holds_no(&output, &[&unacknowledged], "a timed out pair");
    unchanged("a timed out pair");

    // The server stores its record, then the acknowledgement is lost with the connection.
    let (player, mut peer) = pairing(&bench, "empty", &[]);
    peer.receive_pairing();
    peer.abandon();
    let output = player.finish();
    let failure = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{failure}");
    assert!(failure.contains("Nothing was stored"), "{failure}");
    unchanged("a lost acknowledgement");

    // Control-C while the server holds the attempt open.
    let (player, mut peer) = pairing(&bench, "empty", &[]);
    peer.receive_pairing();
    rustix::process::kill_process(
        rustix::process::Pid::from_child(player.0.as_ref().unwrap()),
        rustix::process::Signal::INT,
    )
    .unwrap();
    peer.serve_to_close("the interrupted command exits");
    let output = player.finish();
    assert_eq!(output.status.code(), Some(130), "{}", text(&output.stderr));
    assert!(text(&output.stderr).contains("Nothing was stored"));
    unchanged("an interrupted pair");
}

/// The synthetic helper answers a write in this case with an unknown outcome.
pub fn an_unconfirmed_store_write_reports_that_recovery_is_required() {
    let bench = Bench::new();
    let case = "uncertain-pair";
    let (player, mut peer) = pairing(&bench, case, &[]);
    peer.receive_pairing();
    peer.acknowledge_pairing();
    let output = player.finish();
    let failure = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{failure}");
    assert!(failure.contains("recovered"), "{failure}");
    assert!(!failure.contains("Nothing was stored"), "{failure}");
    assert!(!text(&output.stdout).contains("paired"));
    // The store lock keeps the unknown outcome, so a later command refuses the store.
    let later = bench.sendspin_with(case, &["token"]).finish_unprinted();
    assert_eq!(later.status.code(), Some(1));
    assert!(later.stdout.is_empty(), "a refused store printed a token");
    assert!(text(&later.stderr).contains("recovered"));
    // A play on that store names the store, not the server key.
    let url = bench.url();
    let play = bench
        .sendspin_with(case, &["--server", &url, "--server-key", &bench.key])
        .finish();
    let failure = text(&play.stderr);
    assert_eq!(play.status.code(), Some(1), "{failure}");
    assert_eq!(failure.lines().count(), 1, "{failure}");
    assert!(
        failure.contains("credential store") && failure.contains("run it again"),
        "{failure}"
    );
    assert!(!failure.contains("--server-key"), "{failure}");
    assert!(bench.listener.peek_pending().is_none());
    bench.no_engine_remains();
}

/// The whole path a person takes: init, token, pair, then play with no `--server-key`.
pub fn a_paired_player_plays_without_a_server_key() {
    let bench = Bench::new();
    let (client, _) = provisioned(&bench);
    let (player, mut peer) = pairing(&bench, "empty", &[]);
    let long_term = complete(&mut peer);
    peer.await_goodbye();
    let paired = player.finish();
    assert_eq!(paired.status.code(), Some(0), "{}", text(&paired.stderr));
    drop(peer);

    let url = bench.url();
    let mut player = bench.sendspin_with("empty", &["--server", &url]);
    bench.await_connection(&mut player);
    // A paired server authenticates on the stored long-term secret at once.
    let mut peer = PlayerPeer::connect(&bench.listener, client, long_term, "lt");
    bench.stream_pattern(&mut peer);
    peer.end_stream();
    bench.serve(&mut peer, "the stream end returns to buffering", |state| {
        state["transport"] == "buffering"
    });
    peer.close();
    let output = player.finish();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains("result   ended"));
    bench.capture_is_exact();
    bench.no_engine_remains();
}

pub fn play_without_a_server_key_needs_exactly_one_stored_record() {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let bench = Bench::new();
    let url = bench.url();
    let refused = |case: &str, code: i32, problem: &str| {
        let output = bench.sendspin_with(case, &["--server", &url]).finish();
        let refusal = text(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{case}: {refusal}");
        assert!(refusal.contains(problem), "{case}: {refusal}");
        assert!(bench.listener.peek_pending().is_none(), "{case} connected");
        assert!(!bench.directory.join("rt").exists(), "{case} started");
        output
    };
    refused("empty-absent", 1, "earph sendspin init");
    // An identity with no pairing record.
    let created = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(created.status.code(), Some(0));
    refused("empty", 1, "earph sendspin pair");
    // An identity paired with two servers.
    let servers = [[31; 32], [32; 32]];
    let records = servers.iter().zip([[71; 32], [72; 32]]).zip(1..);
    let records = records
        .map(|((server, psk), order)| PairingRecord {
            server_public: *server,
            psk,
            last_used_order: order,
        })
        .collect();
    let document = earph_sendspin::credentials::CommittedCredentials::new(
        peer::CLIENT_PRIVATE,
        peer::PAIRING_PSK,
        RecordCatalog::from_committed(5, records, vec![]).unwrap(),
        None,
    )
    .unwrap();
    let bytes = document.encode_secret().unwrap();
    std::fs::write(
        bench.directory.join("store").join("two-document"),
        serde_json::to_vec(bytes.as_secret_bytes()).unwrap(),
    )
    .unwrap();
    let output = refused("two", 2, "--server-key");
    let refusal = text(&output.stderr);
    for server in servers {
        assert!(
            !refusal.contains(&URL_SAFE_NO_PAD.encode(server)),
            "{refusal}"
        );
    }
    assert_holds_no(&output, &[&[71; 32], &[72; 32]], "a refused play");
}

pub fn pair_refuses_bad_arguments_and_a_missing_identity() {
    let bench = Bench::new();
    let url = bench.url();
    let cases: [(&str, &[&str], i32, &str); 6] = [
        ("owner", &["pair"], 2, "--server is needed"),
        (
            "owner",
            &["pair", "--server", "wss://10.1.2.3:8927/sendspin"],
            2,
            "wss",
        ),
        (
            "owner",
            &["pair", "--server", "ws://127.0.0.1:8927/sendspin"],
            2,
            "loopback",
        ),
        (
            "owner",
            &["pair", "--server", &url, "--timeout", "0"],
            2,
            "--timeout",
        ),
        ("owner", &["pair", "extra", "--server", &url], 2, "usage"),
        (
            "empty",
            &["pair", "--server", &url],
            1,
            "earph sendspin init",
        ),
    ];
    for (case, arguments, code, problem) in cases {
        let output = bench.sendspin_with(case, arguments).finish();
        let refusal = text(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{arguments:?}: {refusal}");
        assert!(refusal.contains(problem), "{arguments:?}: {refusal}");
        assert!(
            bench.listener.peek_pending().is_none(),
            "{arguments:?} opened a connection"
        );
        assert!(
            !bench.write_issued(case),
            "{arguments:?} wrote to the store"
        );
    }
}

/// Ends the Python peer if its test fails first.
struct Reaped(std::process::Child);
impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs only by name, with `EARPH_SENDSPIN_PEER` set to the launcher of the pinned Python peer.
pub fn pair_completes_against_the_pinned_python_server() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    let launcher = std::env::var_os("EARPH_SENDSPIN_PEER")
        .expect("set EARPH_SENDSPIN_PEER to the pinned Python peer launcher");
    let bench = Bench::new();
    let created = bench.sendspin_with("empty", &["init"]).finish();
    assert_eq!(created.status.code(), Some(0), "{}", text(&created.stderr));
    let token = bench.sendspin_with("empty", &["token"]).finish_unprinted();
    assert_eq!(token.status.code(), Some(0));
    let interface = bench.listener.local_addr().unwrap().ip();
    let mut server = Reaped(
        Command::new(launcher)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/sendspin_pairing_server.py"
            ))
            .arg(interface.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(bench.directory.join("python-stderr.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    // The pinned Python decoder reads the token exactly as the command printed it.
    server
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(&token.stdout)
        .unwrap();
    let mut lines = BufReader::new(server.0.stdout.take().unwrap()).lines();
    let mut stage = |name: &str| {
        let line = lines.next().expect("the Python peer ended early").unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["stage"], name, "inspect python-stderr.log");
        value
    };
    let ready = stage("ready");
    assert_eq!(ready["source_hashes"].as_object().unwrap().len(), 9);
    let url = format!("ws://{interface}:{}/sendspin", ready["port"]);
    let player = bench.sendspin_with("empty", &["pair", "--server", &url, "--timeout", "20"]);
    let paired = stage("paired");
    assert_eq!(stage("goodbye")["reason"], "restart");
    let output = player.finish();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let server_id = ready["server_id"].as_str().unwrap();
    let printed = text(&output.stdout);
    assert!(
        printed
            .lines()
            .eq(["paired", &format!("server key  {server_id}")]),
        "{printed}"
    );
    let stored = bench.stored("empty").unwrap();
    let records = stored.catalog().records();
    assert_eq!(records.len(), 1);
    assert_eq!(URL_SAFE_NO_PAD.encode(records[0].server_public), server_id);
    // Both sides hold the same long-term secret.
    assert_eq!(
        URL_SAFE_NO_PAD.encode(records[0].psk_id()),
        paired["psk_id"]
    );
    assert!(
        server.0.wait().unwrap().success(),
        "inspect python-stderr.log"
    );
}
