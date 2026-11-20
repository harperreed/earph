// ABOUTME: Exercises bounded SSDP generation, rate scheduling, and real loopback discovery.
// ABOUTME: Never joins multicast or emits advertisements onto a real network.
use earph_network::upnp::{
    Advertisement, DeviceIdentity, HttpSurface, SsdpEndpoint, SsdpScheduler, parse_search,
};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket},
    time::{Duration, Instant},
};
fn identity() -> DeviceIdentity {
    DeviceIdentity::new(
        "uuid:01234567-89ab-cdef-0123-456789abcdef",
        "Earph fixture",
        "FixtureOS/1 UPnP/1.0 Earph/0.1",
    )
    .unwrap()
}
fn shutdown(server: HttpSurface) {
    let stats = server.shutdown();
    assert!(stats.joined);
    assert_eq!(stats.service_error, None);
}
fn advertisement() -> Advertisement {
    Advertisement::new(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 24680), identity()).unwrap()
}
fn search(target: &str, mx: &str) -> Vec<u8> {
    format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: {mx}\r\nST: {target}\r\n\r\n").into_bytes()
}
#[test]
fn standard_targets_usns_and_selected_location_agree() {
    let advertisement = advertisement();
    let messages = advertisement.alive();
    assert_eq!(messages.len(), 6);
    for message in messages {
        let text = std::str::from_utf8(&message).unwrap();
        assert!(message.len() <= 2048);
        assert!(text.contains("LOCATION: http://127.0.0.1:24680/upnp/device.xml\r\n"));
        assert!(text.contains("NTS: ssdp:alive\r\n"));
        assert!(text.ends_with("\r\n\r\n"));
    }
    for message in advertisement.byebye() {
        let text = std::str::from_utf8(&message).unwrap();
        assert!(text.contains("NTS: ssdp:byebye\r\n"));
        assert!(!text.contains("LOCATION:"));
    }
    for (target, count) in [
        ("ssdp:all", 6),
        ("upnp:rootdevice", 1),
        ("uuid:01234567-89ab-cdef-0123-456789abcdef", 1),
        ("urn:schemas-upnp-org:service:AVTransport:1", 1),
        ("urn:schemas-upnp-org:service:AVTransport:2", 0),
    ] {
        let parsed = parse_search(&search(target, "999")).unwrap();
        assert_eq!(parsed.mx_seconds(), 5);
        assert_eq!(advertisement.responses(&parsed).len(), count);
    }
    for invalid in [
        search("ssdp:all", "0"),
        search("ssdp:all", "x"),
        search("ssdp:all", "1")
            .into_iter()
            .chain(b"x".iter().copied())
            .collect(),
        vec![b'x'; 2049],
        String::from_utf8(search("ssdp:all", "1"))
            .unwrap()
            .replace("MAN: \"ssdp:discover\"\r\n", "")
            .into_bytes(),
        String::from_utf8(search("ssdp:all", "1"))
            .unwrap()
            .replace("MX: 1", "MX: 1\r\nMX: 2")
            .into_bytes(),
    ] {
        assert!(parse_search(&invalid).is_err());
    }
}
#[test]
fn search_queue_is_atomic_bounded_and_rate_limited() {
    let mut scheduler = SsdpScheduler::new(advertisement());
    let peer = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 12345);
    for _ in 0..5 {
        assert_eq!(
            scheduler
                .schedule(&search("ssdp:all", "1"), peer, Duration::ZERO)
                .unwrap(),
            6
        );
    }
    assert_eq!(scheduler.queued(), 30);
    assert!(
        scheduler
            .schedule(&search("ssdp:all", "1"), peer, Duration::ZERO)
            .is_err()
    );
    assert_eq!(scheduler.queued(), 30);
    for _ in 0..10 {
        assert!(scheduler.next_due(Duration::from_secs(1)).is_some());
    }
    assert!(scheduler.next_due(Duration::from_secs(1)).is_none());
    for _ in 0..2 {
        assert_eq!(
            scheduler
                .schedule(&search("ssdp:all", "1"), peer, Duration::from_secs(1))
                .unwrap(),
            6
        );
    }
    for _ in 0..10 {
        assert!(scheduler.next_due(Duration::from_secs(2)).is_some());
    }
    assert!(scheduler.next_due(Duration::from_secs(2)).is_none());
    assert_eq!(scheduler.queued(), 2);
    assert!(scheduler.next_due(Duration::from_secs(3)).is_none());
    assert_eq!(scheduler.queued(), 0);
}
#[test]
fn unusable_reply_peers_cannot_enter_the_schedule() {
    let mut scheduler = SsdpScheduler::new(advertisement());
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let zero = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
    #[cfg(target_os = "linux")]
    assert_eq!(
        socket.send_to(b"probe", zero).unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput
    );
    for peer in [
        zero,
        SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 12345),
        SocketAddrV4::new(Ipv4Addr::BROADCAST, 12345),
        SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 12345),
    ] {
        assert!(
            scheduler
                .schedule(&search("upnp:rootdevice", "1"), peer, Duration::ZERO)
                .is_err(),
            "unusable peer entered schedule: {peer}"
        );
        assert_eq!(scheduler.queued(), 0);
    }
    drop(socket);
}

#[cfg(target_os = "linux")]
#[test]
fn refused_reply_peer_keeps_http_and_discovery_running() {
    let server = HttpSurface::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), identity()).unwrap();
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let receiver_addr = receiver.local_addr().unwrap();
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let closed = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    sender.connect(closed.local_addr().unwrap()).unwrap();
    drop(closed);
    sender.send(b"probe").unwrap();
    assert_eq!(
        sender.take_error().unwrap().unwrap().kind(),
        std::io::ErrorKind::ConnectionRefused
    );
    sender.send(b"probe").unwrap();
    server
        .attach_discovery(
            SsdpEndpoint::from_bound(
                receiver,
                sender,
                Advertisement::new(server.local_addr(), identity()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let controller = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    controller
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    controller
        .send_to(&search("upnp:rootdevice", "1"), receiver_addr)
        .unwrap();
    let mut data = [0; 2049];
    assert_eq!(
        controller.recv_from(&mut data).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let mut socket = TcpStream::connect(server.local_addr())
        .expect("one refused reply stopped the HTTP listener");
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .write_all(b"GET /upnp/device.xml HTTP/1.1\r\nHost: fixture\r\n\r\n")
        .unwrap();
    let mut body = String::new();
    socket.read_to_string(&mut body).unwrap();
    assert!(body.starts_with("HTTP/1.1 200"));
    controller
        .send_to(&search("upnp:rootdevice", "1"), receiver_addr)
        .unwrap();
    let (length, _) = controller
        .recv_from(&mut data)
        .expect("discovery stopped after a refused peer");
    assert!(data[..length].starts_with(b"HTTP/1.1 200"));
    shutdown(server);
}

#[cfg(target_os = "linux")]
#[test]
fn local_discovery_socket_failure_is_reported_after_join() {
    let server = HttpSurface::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), identity()).unwrap();
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let receiver_addr = receiver.local_addr().unwrap();
    let controller = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    controller
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    sender.connect(controller.local_addr().unwrap()).unwrap();
    socket2::SockRef::from(&sender)
        .shutdown(std::net::Shutdown::Write)
        .unwrap();
    assert_eq!(
        sender.send(b"probe").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    server
        .attach_discovery(
            SsdpEndpoint::from_bound(
                receiver,
                sender,
                Advertisement::new(server.local_addr(), identity()).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    controller
        .send_to(&search("upnp:rootdevice", "1"), receiver_addr)
        .unwrap();
    let mut bytes = [0; 2049];
    assert_eq!(
        controller.recv_from(&mut bytes).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    let stats = server.shutdown();
    assert!(stats.joined, "the failed runtime thread must still join");
    assert_eq!(stats.service_error, Some(std::io::ErrorKind::BrokenPipe));
}
#[test]
fn raw_udp_discovery_points_to_the_actual_selected_http_listener() {
    let server = HttpSurface::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), identity()).unwrap();
    let ad = Advertisement::new(server.local_addr(), identity()).unwrap();
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint_addr = receiver.local_addr().unwrap();
    let endpoint = SsdpEndpoint::from_bound(receiver, sender, ad).unwrap();
    server.attach_discovery(endpoint).unwrap();
    let controller = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    controller.set_nonblocking(true).unwrap();
    controller
        .send_to(&search("upnp:rootdevice", "1"), endpoint_addr)
        .unwrap();
    let start = Instant::now();
    let mut data = [0; 2049];
    let (length, peer) = loop {
        match controller.recv_from(&mut data) {
            Ok(reply) => break reply,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("{error}"),
        };
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(peer.ip(), Ipv4Addr::LOCALHOST);
    let reply = std::str::from_utf8(&data[..length]).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(reply.contains(&format!(
        "LOCATION: http://{}/upnp/device.xml\r\n",
        server.local_addr()
    )));
    let mut socket = TcpStream::connect(server.local_addr()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .write_all(b"GET /upnp/device.xml HTTP/1.1\r\nHost: fixture\r\n\r\n")
        .unwrap();
    let mut body = String::new();
    socket.read_to_string(&mut body).unwrap();
    assert!(body.contains("<UDN>uuid:01234567-89ab-cdef-0123-456789abcdef</UDN>"));
    shutdown(server);
}
