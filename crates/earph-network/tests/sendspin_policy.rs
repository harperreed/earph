// ABOUTME: Exercises exact outbound grants and unchanged destination admission.
// ABOUTME: Keeps endpoint components and synthetic identities out of diagnostics.
use earph_core::ids::OwnerId;
use earph_network::sendspin::*;
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};
fn permit(endpoint: &str, pins: Vec<IpAddr>) -> Result<SendspinConnectPermit, DriverError> {
    SendspinConnectPermit::new(
        OwnerId::new("SECRET-owner").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(2),
        endpoint,
        pins,
    )
}
#[test]
fn admitted_private_pin_and_explicit_ws_endpoint() {
    let p = permit(
        "ws://SECRET-host:43210/sendspin",
        vec!["10.1.2.3".parse().unwrap()],
    )
    .unwrap();
    assert_eq!(format!("{p:?}"), "SendspinConnectPermit([REDACTED])");
}
#[test]
fn rejects_ambiguous_endpoints_and_disallowed_destinations() {
    for endpoint in [
        "ws://host/path",
        "ws://host:0/path",
        "ws://user@host:42/",
        "ws://host:42/?secret",
        "ws://host:42/#secret",
        "ws://host:42/ bad",
        "ws://host:42/\\bad",
        "https://host:42/",
        "ws://host:42/%0a",
    ] {
        assert!(
            permit(endpoint, vec!["10.1.2.3".parse().unwrap()]).is_err(),
            "{endpoint}"
        );
    }
    assert_eq!(
        permit("wss://host:42/", vec!["10.1.2.3".parse().unwrap()]).unwrap_err(),
        DriverError::TlsUnsupported
    );
    for ip in [
        "127.0.0.1",
        "0.0.0.0",
        "169.254.1.1",
        "224.0.0.1",
        "100.64.0.1",
        "::1",
        "fe80::1",
        "::ffff:127.0.0.1",
    ] {
        assert!(permit("ws://host:42/", vec![ip.parse().unwrap()]).is_err());
    }
    assert!(permit("ws://10.1.2.4:42/", vec!["10.1.2.3".parse().unwrap()]).is_err());
    assert!(permit("ws://host:42/", vec![]).is_err());
    assert!(permit("ws://host:42/", vec!["10.1.2.3".parse().unwrap(); 9]).is_err());
}
#[test]
fn exact_expiry_and_generations_are_required() {
    let now = Instant::now();
    assert_eq!(
        SendspinConnectPermit::new(
            OwnerId::new("SECRET-owner").unwrap(),
            1,
            1,
            now,
            "ws://host:42/",
            vec!["10.1.2.3".parse().unwrap()]
        )
        .unwrap_err(),
        DriverError::Expired
    );
    for (connection, grant) in [(0, 1), (1, 0)] {
        assert!(
            SendspinConnectPermit::new(
                OwnerId::new("SECRET-owner").unwrap(),
                connection,
                grant,
                now + Duration::from_secs(1),
                "ws://host:42/",
                vec!["10.1.2.3".parse().unwrap()]
            )
            .is_err()
        );
    }
}
