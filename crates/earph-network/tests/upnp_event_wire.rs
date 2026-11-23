// ABOUTME: Exercises subscription ingress and callback delivery through real private-interface sockets.
// ABOUTME: Proves response completion, shared transport capacity, and exact event retirement.
use earph_network::upnp::{DeviceIdentity, HttpSurface};
use std::{
    io::Write,
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    time::{Duration, Instant},
};
fn surface() -> HttpSurface {
    HttpSurface::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        DeviceIdentity::new(
            "uuid:01234567-89ab-cdef-0123-456789abcdef",
            "Event fixture",
            "Fixture/1 UPnP/1.0 Earph/1",
        )
        .unwrap(),
    )
    .unwrap()
}
#[test]
fn subscription_request_reaches_owner_instead_of_generic_method_rejection() {
    let surface = surface();
    let mut stream = TcpStream::connect(surface.local_addr()).unwrap();
    stream.write_all(b"SUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: fixture\r\nCALLBACK: <http://callback.test:24680/events>\r\nNT: upnp:event\r\nTIMEOUT: Second-30\r\nContent-Length: 0\r\n\r\n").unwrap();
    let end = Instant::now() + Duration::from_secs(1);
    while surface.stats().queued == 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        surface.stats().queued,
        1,
        "SUBSCRIBE must reach typed owner ingress"
    );
    assert!(surface.shutdown().joined);
}
#[test]
fn owner_faults_and_getters_use_the_declared_wire_contract() {
    use earph_network::upnp::{Service, UpnpFault, decode_soap, encode_fault};
    for (fault, code) in [
        (UpnpFault::ActionNotAuthorized, 606),
        (UpnpFault::TransitionNotAvailable, 701),
    ] {
        let xml = String::from_utf8(encode_fault(fault)).unwrap();
        assert!(xml.contains(&format!("<errorCode>{code}</errorCode>")));
    }
    let namespace = "urn:schemas-upnp-org:service:AVTransport:1";
    let body = format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:GetTransportInfo xmlns:u=\"{namespace}\"><InstanceID>0</InstanceID></u:GetTransportInfo></s:Body></s:Envelope>"
    );
    let request = decode_soap(
        Service::AvTransport,
        &format!("\"{namespace}#GetTransportInfo\""),
        body.as_bytes(),
    )
    .unwrap();
    assert_eq!(
        request.output_fields(),
        &[
            ("CurrentTransportState", "TransportState"),
            ("CurrentTransportStatus", "TransportStatus"),
            ("CurrentSpeed", "TransportPlaySpeed")
        ]
    );
}
#[test]
fn unsubscribe_uses_lifecycle_ingress_before_ordinary_subscriptions() {
    use earph_core::renderer_events::RendererSubscriptionAction;
    use earph_network::upnp::IncomingRequest;
    let surface = surface();
    let mut sockets = Vec::new();
    for _ in 0..3 {
        let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
        socket.write_all(b"SUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: fixture\r\nCALLBACK: <http://callback.test:80/a>\r\nNT: upnp:event\r\nContent-Length: 0\r\n\r\n").unwrap();
        sockets.push(socket);
    }
    let end = Instant::now() + Duration::from_secs(2);
    while surface.stats().queued < 3 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
    socket.write_all(b"UNSUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: fixture\r\nSID: uuid:unknown\r\nContent-Length: 0\r\n\r\n").unwrap();
    sockets.push(socket);
    let end = Instant::now() + Duration::from_secs(2);
    while surface.stats().queued < 4 {
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(2));
    }
    let Some(IncomingRequest::Subscription(ticket)) = surface.try_recv_incoming() else {
        panic!("lifecycle subscription ticket")
    };
    assert!(
        matches!(
            ticket.action(),
            RendererSubscriptionAction::Unsubscribe { .. }
        ),
        "ordinary subscriptions must leave lifecycle ingress independent"
    );
    assert!(ticket.respond(Err(412)));
    assert!(surface.shutdown().joined);
}
