// ABOUTME: Checks the `earph sendspin` setup commands: init, token, pair, and play on the stored record.
// ABOUTME: Uses the synthetic credential helper and local test peers, and never prints a secret.
use super::linux::{Bench, text};
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
