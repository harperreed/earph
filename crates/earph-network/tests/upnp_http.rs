// ABOUTME: Exercises the real bounded HTTP listener using owned raw loopback sockets.
// ABOUTME: Checks framing, deadlines, typed queue pressure, cancellation, and joined shutdown.
use earph_network::upnp::{DeviceIdentity, HttpSurface, UpnpFault};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    time::{Duration, Instant},
};

fn server() -> HttpSurface {
    HttpSurface::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        DeviceIdentity::new(
            "uuid:01234567-89ab-cdef-0123-456789abcdef",
            "Earph & fixture",
            "FixtureOS/1 UPnP/1.0 Earph/0.1",
        )
        .unwrap(),
    )
    .unwrap()
}
fn shutdown(server: HttpSurface) {
    let stats = server.shutdown();
    assert!(stats.joined);
    assert_eq!(stats.service_error, None);
}
fn connect(server: &HttpSurface) -> TcpStream {
    let s = TcpStream::connect(server.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    s
}
fn read(mut stream: TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = stream
            .read(&mut buffer)
            .expect("complete framed HTTP response");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = headers.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
            });
            if length.is_some_and(|n| bytes.len() >= end + 4 + n) {
                break;
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
fn post(action: &str, args: &str) -> Vec<u8> {
    let ns = "urn:schemas-upnp-org:service:AVTransport:1";
    let body = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{ns}">{args}</u:{action}></s:Body></s:Envelope>"#
    );
    format!("POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nSOAPACTION: \"{ns}#{action}\"\r\nContent-Length: {}\r\n\r\n{body}",body.len()).into_bytes()
}
fn pending(server: &HttpSurface) -> earph_network::upnp::PendingAction {
    let end = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(p) = server.try_recv() {
            return p;
        }
        assert!(Instant::now() < end, "no typed action was delivered");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fragmented_description_and_soap_cross_real_sockets_without_owner_success() {
    let server = server();
    let mut stream = connect(&server);
    for chunk in b"GET /upnp/device.xml HTTP/1.1\r\nHost: fixture\r\n\r\n".chunks(3) {
        stream.write_all(chunk).unwrap();
    }
    let reply = read(stream);
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.contains("Earph &amp; fixture"));
    assert!(reply.to_ascii_lowercase().contains("connection: close"));
    let mut stream = connect(&server);
    stream
        .write_all(&post("Stop", "<InstanceID>0</InstanceID>"))
        .unwrap();
    let action = pending(&server);
    assert_eq!(action.request().name(), "Stop");
    assert!(action.respond(Err(UpnpFault::ActionFailed)));
    let reply = read(stream);
    assert!(reply.starts_with("HTTP/1.1 500"));
    assert!(reply.contains("<errorCode>501</errorCode>"));
    shutdown(server);
}

#[test]
fn malformed_framing_and_xml_fail_before_the_typed_queue() {
    let server = server();
    for bytes in [
        b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec(),
        b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]{let mut stream=connect(&server);stream.write_all(&bytes).unwrap();assert!(read(stream).starts_with("HTTP/1.1 400"));assert!(server.try_recv().is_none());}
    for (action, args, code) in [
        ("Play", "<InstanceID>0</InstanceID><Speed>2</Speed>", 717),
        ("Stop", "<InstanceID>1</InstanceID>", 718),
        ("Stop", "<InstanceID>abc</InstanceID>", 402),
    ] {
        let mut stream = connect(&server);
        stream.write_all(&post(action, args)).unwrap();
        let reply = read(stream);
        assert!(reply.contains(&format!("<errorCode>{code}</errorCode>")));
        assert!(server.try_recv().is_none());
    }
    shutdown(server);
}

#[test]
fn invalid_host_never_enters_the_typed_queue() {
    let server = server();
    let original = String::from_utf8(post("Stop", "<InstanceID>0</InstanceID>")).unwrap();
    for host in [
        "",
        "Host: fixture\r\nHost: fixture\r\n",
        "Host: \r\n",
        "Host: user@fixture\r\n",
        "Host: fixture:wrong\r\n",
        "Host: fixture/path\r\n",
        "Host: [wrong]\r\n",
        "Host: fixture%xx\r\n",
        "Host: bad host\r\n",
        "Host: \u{00a0}fixture\r\n",
    ] {
        let mut stream = connect(&server);
        stream
            .write_all(original.replace("Host: fixture\r\n", host).as_bytes())
            .unwrap();
        let reply = read(stream);
        assert!(
            reply.starts_with("HTTP/1.1 400"),
            "invalid Host entered admission: {host:?}: {reply}"
        );
        assert!(server.try_recv().is_none());
    }
    for host in [
        "fixture",
        "127.0.0.1:24680",
        "[::1]:24680",
        "fixture.local.",
        "f%69xture",
        "[v1.fixture]:24680",
    ] {
        let mut stream = connect(&server);
        stream
            .write_all(
                original
                    .replace("Host: fixture\r\n", &format!("Host: {host}\r\n"))
                    .as_bytes(),
            )
            .unwrap();
        assert!(pending(&server).respond(Err(UpnpFault::ActionFailed)));
        assert!(read(stream).starts_with("HTTP/1.1 500"));
    }
    shutdown(server);
}

#[test]
fn chunked_body_is_decoded_once_and_capped_before_xml_admission() {
    let server = server();
    let original = post("Stop", "<InstanceID>0</InstanceID>");
    let offset = original.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let body = &original[offset..];
    let mut stream = connect(&server);
    stream.write_all(b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Type: text/xml\r\nSOAPACTION: \"urn:schemas-upnp-org:service:AVTransport:1#Stop\"\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
    for chunk in body.chunks(7) {
        write!(stream, "{:x}\r\n", chunk.len()).unwrap();
        stream.write_all(chunk).unwrap();
        stream.write_all(b"\r\n").unwrap();
    }
    stream.write_all(b"0\r\n\r\n").unwrap();
    pending(&server).respond(Err(UpnpFault::ActionFailed));
    assert!(read(stream).contains("<errorCode>501</errorCode>"));
    let mut stream = connect(&server);
    stream.write_all(b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nTransfer-Encoding: chunked\r\n\r\n40001\r\n").unwrap();
    let _ = stream.write_all(&vec![b'x'; 262145]);
    assert!(read(stream).starts_with("HTTP/1.1 413"));
    assert!(server.try_recv().is_none());
    shutdown(server);
}

#[test]
fn headers_body_deadlines_memory_and_shutdown_are_bounded() {
    let server = server();
    let mut stream = connect(&server);
    stream
        .write_all(
            format!(
                "GET /upnp/device.xml HTTP/1.1\r\nHost: fixture\r\nX: {}\r\n\r\n",
                "x".repeat(32768)
            )
            .as_bytes(),
        )
        .unwrap();
    assert!(read(stream).starts_with("HTTP/1.1 431"));
    let mut stream = connect(&server);
    stream
        .write_all(
            b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Length: 262145\r\n\r\n",
        )
        .unwrap();
    assert!(read(stream).starts_with("HTTP/1.1 413"));
    let mut slow = connect(&server);
    slow.write_all(b"GET /upnp/").unwrap();
    let start = Instant::now();
    assert!(read(slow).starts_with("HTTP/1.1 408"));
    assert!(start.elapsed() >= Duration::from_secs(4));
    assert!(start.elapsed() < Duration::from_secs(7));
    let held: Vec<_> = (0..4).map(|_| connect(&server)).collect();
    std::thread::sleep(Duration::from_millis(50));
    assert!(server.stats().reserved_bytes <= 8 * 1024 * 1024);
    let start = Instant::now();
    shutdown(server);
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(held);
}

#[test]
fn lifecycle_tickets_keep_reserved_capacity_and_precede_ordinary_work() {
    let server = server();
    let mut ordinary = Vec::new();
    for _ in 0..3 {
        let mut stream = connect(&server);
        stream
            .write_all(&post("Play", "<InstanceID>0</InstanceID><Speed>1</Speed>"))
            .unwrap();
        ordinary.push(stream);
    }
    let end = Instant::now() + Duration::from_secs(2);
    while server.stats().queued < 3 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut excess = connect(&server);
    excess
        .write_all(&post("Play", "<InstanceID>0</InstanceID><Speed>1</Speed>"))
        .unwrap();
    assert!(read(excess).starts_with("HTTP/1.1 503"));
    let mut stop = connect(&server);
    stop.write_all(&post("Stop", "<InstanceID>0</InstanceID>"))
        .unwrap();
    let end = Instant::now() + Duration::from_secs(2);
    while server.stats().queued < 4 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(5));
    }
    let urgent = pending(&server);
    assert_eq!(urgent.request().name(), "Stop");
    assert!(urgent.respond(Err(UpnpFault::ActionFailed)));
    assert!(read(stop).contains("<errorCode>501</errorCode>"));
    for stream in ordinary {
        let next = pending(&server);
        assert_eq!(next.request().name(), "Play");
        assert!(next.respond(Err(UpnpFault::ActionFailed)));
        assert!(read(stream).contains("<errorCode>501</errorCode>"));
    }
    shutdown(server);
}

#[test]
fn body_deadline_expires_and_queued_tickets_cannot_reply_after_shutdown() {
    let server = server();
    let mut slow = connect(&server);
    slow.write_all(b"POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Type: text/xml\r\nContent-Length: 32\r\n\r\n<").unwrap();
    let start = Instant::now();
    assert!(read(slow).starts_with("HTTP/1.1 408"));
    assert!(start.elapsed() >= Duration::from_secs(4));
    assert!(server.try_recv().is_none());
    let mut stream = connect(&server);
    stream
        .write_all(&post("Stop", "<InstanceID>0</InstanceID>"))
        .unwrap();
    let pending = pending(&server);
    let final_stats = server.shutdown();
    assert!(final_stats.joined);
    assert_eq!(final_stats.service_error, None);
    assert!(!pending.is_live());
    assert!(!pending.respond(Ok(&[])));
}
