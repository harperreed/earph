// ABOUTME: Verifies exact origin admission without opening a socket.
// ABOUTME: Catches unsafe destinations and stale source or grant bindings.
use earph_core::ids::{OwnerId, SessionId, StagedMediaId};
use earph_network::*;
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};

fn binding(generation: u64) -> SourceBinding {
    SourceBinding::new(
        OwnerId::new("owner").unwrap(),
        SessionId::new("session").unwrap(),
        StagedMediaId::new("media").unwrap(),
        generation,
        1,
    )
    .unwrap()
}
fn grant(url: &str, pins: &[&str], expiry: Instant) -> Result<OriginGrant, PolicyError> {
    OriginGrant::new(
        CheckedUrl::new(url)?.origin(),
        OwnerId::new("owner").unwrap(),
        1,
        pins.iter().map(|s| s.parse::<IpAddr>().unwrap()).collect(),
        expiry,
        vec![],
    )
}
#[test]
fn canonical_origins_and_explicit_lan_http_are_admitted() {
    let a = CheckedUrl::new("HTTP://EXAMPLE.test:80/a?q=secret").unwrap();
    let b = CheckedUrl::new("http://example.test/b").unwrap();
    assert_eq!(a.origin(), b.origin());
    let now = Instant::now();
    let grants = vec![
        grant(
            "http://example.test",
            &["192.168.23.153"],
            now + Duration::from_secs(10),
        )
        .unwrap(),
    ];
    assert!(FetchPermit::new(a, binding(1), &binding(1), grants, now).is_ok());
}
#[test]
fn unsafe_urls_are_rejected_without_echoing_input() {
    for text in [
        "http://user:secret@example.test",
        "http://@example.test",
        "file:///private",
        "https://example.test/#private",
        "http://example.test/\nsecret",
        "https:example.test",
        "http:///example.test",
    ] {
        let error = CheckedUrl::new(text).unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret"));
    }
    assert!(CheckedUrl::new(&format!("http://example.test/{}", "x".repeat(8192))).is_err());
    let url = CheckedUrl::new("https://example.test/private?secret=yes").unwrap();
    assert!(!format!("{url:?}").contains("secret"));
}
#[test]
fn prohibited_and_mapped_destinations_never_form_grants() {
    for ip in [
        "0.0.0.0",
        "0.1.2.3",
        "127.0.0.1",
        "169.254.1.1",
        "224.0.0.1",
        "255.255.255.255",
        "192.0.2.1",
        "198.18.0.1",
        "::",
        "::1",
        "ff02::1",
        "fe80::1",
        "::ffff:127.0.0.1",
        "::ffff:169.254.1.1",
        "2001:db8::1",
        "64:ff9b::7f00:1",
        "2002:7f00:1::",
    ] {
        assert!(
            grant(
                "http://example.test",
                &[ip],
                Instant::now() + Duration::from_secs(10)
            )
            .is_err(),
            "{ip}"
        );
    }
    for ip in [
        "192.168.23.153",
        "10.1.2.3",
        "172.17.0.1",
        "fd12:3456::1",
        "::ffff:192.168.23.153",
    ] {
        assert!(
            grant(
                "http://example.test",
                &[ip],
                Instant::now() + Duration::from_secs(10)
            )
            .is_ok(),
            "{ip}"
        );
    }
}
#[test]
fn missing_pins_expiry_and_stale_bindings_fail_closed() {
    let now = Instant::now();
    assert!(grant("http://example.test", &[], now + Duration::from_secs(10)).is_err());
    assert!(
        grant(
            "http://192.168.23.153",
            &["192.168.23.49"],
            now + Duration::from_secs(10)
        )
        .is_err()
    );
    let url = || CheckedUrl::new("http://example.test/private").unwrap();
    assert!(
        FetchPermit::new(
            url(),
            binding(1),
            &binding(1),
            vec![grant("http://example.test", &["192.168.23.153"], now).unwrap()],
            now
        )
        .is_err()
    );
    assert!(FetchPermit::new(url(), binding(2), &binding(1), vec![], now).is_err());
    assert!(
        SourceBinding::new(
            OwnerId::new("owner").unwrap(),
            SessionId::new("session").unwrap(),
            StagedMediaId::new("media").unwrap(),
            0,
            1
        )
        .is_err()
    );
    let mut wrong = binding(1);
    wrong = SourceBinding::new(
        OwnerId::new("different").unwrap(),
        wrong.session().clone(),
        wrong.media().clone(),
        1,
        1,
    )
    .unwrap();
    assert!(
        FetchPermit::new(
            url(),
            wrong.clone(),
            &wrong,
            vec![
                grant(
                    "http://example.test",
                    &["192.168.23.153"],
                    now + Duration::from_secs(10)
                )
                .unwrap()
            ],
            now
        )
        .is_err()
    );
}
#[test]
fn grant_and_certificate_budgets_are_checked_before_transport() {
    let now = Instant::now();
    assert!(grant("http://example.test", &["10.0.0.1"; 9], now).is_err());
    let grants = vec![
        grant(
            "http://example.test",
            &["10.0.0.1"],
            now + Duration::from_secs(10)
        )
        .unwrap();
        9
    ];
    assert!(
        FetchPermit::new(
            CheckedUrl::new("http://example.test").unwrap(),
            binding(1),
            &binding(1),
            grants,
            now
        )
        .is_err()
    );
    assert!(
        OriginGrant::new(
            CheckedUrl::new("https://example.test").unwrap().origin(),
            OwnerId::new("owner").unwrap(),
            1,
            vec!["10.0.0.1".parse().unwrap()],
            now,
            vec![vec![0; 1024 * 1024 + 1]]
        )
        .is_err()
    );
}
#[test]
fn ordinary_public_addresses_next_to_special_ranges_remain_admissible() {
    for ip in [
        "192.0.8.1",
        "198.17.0.1",
        "198.20.0.1",
        "2001:4860:4860::8888",
    ] {
        assert!(
            grant(
                "http://example.test",
                &[ip],
                Instant::now() + Duration::from_secs(10)
            )
            .is_ok(),
            "{ip}"
        );
    }
}
#[test]
fn grant_generation_and_duplicate_origins_cannot_change_authority() {
    let now = Instant::now();
    let current = SourceBinding::new(
        OwnerId::new("owner").unwrap(),
        SessionId::new("session").unwrap(),
        StagedMediaId::new("media").unwrap(),
        1,
        2,
    )
    .unwrap();
    let url = || CheckedUrl::new("http://example.test/").unwrap();
    let origin = grant(
        "http://example.test/",
        &["10.0.0.1"],
        now + Duration::from_secs(10),
    )
    .unwrap();
    assert_eq!(
        FetchPermit::new(url(), current.clone(), &current, vec![origin.clone()], now).unwrap_err(),
        PolicyError::InvalidGrant
    );
    assert_eq!(
        FetchPermit::new(
            url(),
            binding(1),
            &binding(1),
            vec![origin.clone(), origin],
            now
        )
        .unwrap_err(),
        PolicyError::InvalidGrant
    );
}
