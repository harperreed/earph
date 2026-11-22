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
