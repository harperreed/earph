// ABOUTME: Rejects malformed event subscription framing before owner admission.
// ABOUTME: Exercises real socket limits without granting any callback or media authority.
use earph_network::upnp::{DeviceIdentity, HttpSurface};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    time::Duration,
};
#[test]
fn subscription_header_combinations_duplicates_and_body_fail_closed() {
    let surface = HttpSurface::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        DeviceIdentity::new(
            "uuid:01234567-89ab-cdef-0123-456789abcdef",
            "Header fixture",
            "Fixture/1 UPnP/1.0 Earph/1",
        )
        .unwrap(),
    )
    .unwrap();
    for (method, headers, body, status) in [
        (
            "SUBSCRIBE",
            "SID: uuid:a\r\nCALLBACK: <http://callback.test:80/a>\r\n",
            "",
            400,
        ),
        ("SUBSCRIBE", "SID: uuid:a\r\nNT: upnp:event\r\n", "", 400),
        (
            "SUBSCRIBE",
            "CALLBACK: <http://callback.test:80/a>\r\nNT: wrong\r\n",
            "",
            412,
        ),
        (
            "SUBSCRIBE",
            "CALLBACK: <http://callback.test:80/a>\r\nNT: upnp:event\r\nTIMEOUT: Second-0\r\n",
            "",
            400,
        ),
        (
            "SUBSCRIBE",
            "CALLBACK: <http://callback.test:80/a>\r\nCALLBACK: <http://callback.test:80/b>\r\nNT: upnp:event\r\n",
            "",
            400,
        ),
        (
            "SUBSCRIBE",
            "CALLBACK: <https://callback.test:443/a>\r\nNT: upnp:event\r\n",
            "",
            412,
        ),
        (
            "SUBSCRIBE",
            "CALLBACK: <http://callback.test:80/a>\r\nNT: upnp:event\r\n",
            "x",
            400,
        ),
        (
            "UNSUBSCRIBE",
            "SID: uuid:a\r\nTIMEOUT: Second-30\r\n",
            "",
            400,
        ),
        ("UNSUBSCRIBE", "", "", 412),
    ] {
        let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(socket,"{method} /upnp/avt/events HTTP/1.1\r\nHost: fixture\r\n{headers}Content-Length: {}\r\n\r\n{body}",body.len()).unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.starts_with(&format!("HTTP/1.1 {status}")),
            "{headers}: {text}"
        );
        assert_eq!(surface.stats().queued, 0);
    }
    assert!(surface.shutdown().joined);
}
