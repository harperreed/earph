// ABOUTME: Parses generated device and service descriptions as a control point would.
// ABOUTME: Checks that descriptions and SOAP admission share one action contract.
use earph_network::upnp::{Service, device_description, service_description};
use quick_xml::{events::Event, reader::Reader};

#[derive(Default, Debug)]
struct Element {
    name: String,
    text: String,
    children: Vec<Element>,
}

impl Element {
    fn child(&self, name: &str) -> &Self {
        self.children.iter().find(|c| c.name == name).unwrap()
    }
}

fn parse(xml: &str) -> Element {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().expand_empty_elements = true;
    let mut stack = vec![Element::default()];
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) => stack.push(Element {
                name: String::from_utf8(e.local_name().as_ref().to_vec()).unwrap(),
                ..Element::default()
            }),
            Event::End(_) => {
                let node = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(node);
            }
            Event::Text(e) => stack
                .last_mut()
                .unwrap()
                .text
                .push_str(&e.xml10_content().unwrap()),
            Event::GeneralRef(e) => stack.last_mut().unwrap().text.push_str(
                &quick_xml::escape::unescape(&format!("&{};", e.decode().unwrap())).unwrap(),
            ),
            Event::Decl(_) => {}
            Event::Eof => break,
            other => panic!("unexpected generated XML: {other:?}"),
        }
    }
    assert_eq!(stack.len(), 1);
    assert_eq!(stack[0].children.len(), 1);
    stack.pop().unwrap().children.pop().unwrap()
}

#[test]
fn device_identity_is_escaped_and_service_urls_remain_local() {
    let xml = device_description(
        "uuid:01234567-89ab-cdef-0123-456789abcdef",
        "Earph & <room>",
    )
    .unwrap();
    let root = parse(&xml);
    let device = root.child("device");
    assert_eq!(
        device.child("deviceType").text,
        "urn:schemas-upnp-org:device:MediaRenderer:1"
    );
    assert_eq!(device.child("friendlyName").text, "Earph & <room>");
    assert_eq!(
        device.child("UDN").text,
        "uuid:01234567-89ab-cdef-0123-456789abcdef"
    );
    let services = &device.child("serviceList").children;
    assert_eq!(services.len(), 3);
    for service in services {
        for field in ["controlURL", "eventSubURL", "SCPDURL"] {
            let value = &service.child(field).text;
            assert!(value.starts_with("/upnp/"));
            assert!(!value.contains("://"));
        }
    }
    for identity in [
        "uuid:bad",
        "uuid:01234567-89ab-cdef-0123-456789abcde\r\n",
        "https://fixture.invalid/",
    ] {
        assert!(device_description(identity, "Earph").is_err());
    }
}

#[test]
fn rendering_control_describes_the_accepted_volume_grid() {
    let root = parse(&service_description(Service::RenderingControl));
    let variables = &root.child("serviceStateTable").children;
    for (name, minimum, maximum) in [("Volume", 0, 30700), ("VolumeDB", -32767, 0)] {
        let variable = variables
            .iter()
            .find(|v| v.child("name").text == name)
            .unwrap();
        let range = variable.child("allowedValueRange");
        assert_eq!(range.child("minimum").text.parse::<i32>().unwrap(), minimum);
        assert_eq!(range.child("maximum").text.parse::<i32>().unwrap(), maximum);
        assert_eq!(range.child("step").text.parse::<i32>().unwrap(), 1);
    }
}

#[test]
fn downloaded_action_and_state_tables_form_consistent_soap_requests() {
    for service in [
        Service::AvTransport,
        Service::RenderingControl,
        Service::ConnectionManager,
    ] {
        let root = parse(&service_description(service));
        let variables = &root.child("serviceStateTable").children;
        let actions = &root.child("actionList").children;
        let mut names = std::collections::BTreeSet::new();
        for action in actions {
            let name = &action.child("name").text;
            assert!(names.insert(name));
            assert_ne!(name, "SetNextAVTransportURI");
            let mut input = String::new();
            for arg in &action.child("argumentList").children {
                let variable = &arg.child("relatedStateVariable").text;
                assert!(variables.iter().any(|v| v.child("name").text == *variable));
                if arg.child("direction").text == "in" {
                    let name = &arg.child("name").text;
                    input.push_str(&format!("<{name}>0</{name}>"));
                }
            }
            let namespace = service.namespace();
            let xml = format!(
                r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{name} xmlns:u="{namespace}">{input}</u:{name}></s:Body></s:Envelope>"#
            );
            let parsed = earph_network::upnp::decode_soap(
                service,
                &format!("\"{namespace}#{name}\""),
                xml.as_bytes(),
            )
            .unwrap();
            assert_eq!(parsed.name(), name);
        }
        for required in match service {
            Service::AvTransport => &[
                "SetAVTransportURI",
                "Stop",
                "Play",
                "Seek",
                "Next",
                "Previous",
                "GetMediaInfo",
                "GetTransportInfo",
                "GetPositionInfo",
                "GetDeviceCapabilities",
                "GetTransportSettings",
            ][..],
            Service::RenderingControl => &[
                "ListPresets",
                "SelectPreset",
                "GetMute",
                "SetMute",
                "GetVolume",
                "SetVolume",
                "GetVolumeDB",
                "SetVolumeDB",
                "GetVolumeDBRange",
            ][..],
            Service::ConnectionManager => &[
                "GetProtocolInfo",
                "GetCurrentConnectionIDs",
                "GetCurrentConnectionInfo",
            ][..],
        } {
            assert!(names.iter().any(|name| name.as_str() == *required));
        }
    }
}

#[test]
fn advertised_event_subscription_urls_reach_typed_listener_for_every_service() {
    use earph_core::renderer_events::RendererService;
    use earph_network::upnp::{DeviceIdentity, HttpSurface, IncomingRequest};
    use std::{
        io::{Read, Write},
        net::{Ipv4Addr, SocketAddrV4, TcpStream},
        sync::mpsc,
        time::{Duration, Instant},
    };
    let surface = HttpSurface::bind(
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        DeviceIdentity::new(
            "uuid:01234567-89ab-cdef-0123-456789abcdef",
            "Description fixture",
            "Fixture/1 UPnP/1.0 Earph/1",
        )
        .unwrap(),
    )
    .unwrap();
    let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .write_all(b"GET /upnp/device.xml HTTP/1.1\r\nHost: fixture\r\n\r\n")
        .unwrap();
    let mut wire = String::new();
    socket.read_to_string(&mut wire).unwrap();
    assert!(wire.starts_with("HTTP/1.1 200"));
    let description = parse(wire.split_once("\r\n\r\n").unwrap().1);
    let mut failures = Vec::new();
    let services = &description.child("device").child("serviceList").children;
    assert_eq!(services.len(), 3);
    for service in services {
        let event_url = &service.child("eventSubURL").text;
        let namespace = &service.child("serviceType").text;
        let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        write!(socket, "SUBSCRIBE {event_url} HTTP/1.1\r\nHost: fixture\r\nCALLBACK: <http://callback.test:80/notify>\r\nNT: upnp:event\r\nContent-Length: 0\r\n\r\n").unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            tx.send(response).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let response = loop {
            if let Some(IncomingRequest::Subscription(pending)) = surface.try_recv_incoming() {
                let actual_namespace = match pending.service() {
                    RendererService::AvTransport => Service::AvTransport.namespace(),
                    RendererService::RenderingControl => Service::RenderingControl.namespace(),
                    RendererService::ConnectionManager => Service::ConnectionManager.namespace(),
                };
                assert_eq!(actual_namespace, namespace);
                assert!(pending.respond(Err(412)));
                break rx.recv_timeout(Duration::from_secs(2)).unwrap();
            }
            if let Ok(response) = rx.try_recv() {
                break response;
            }
            assert!(
                Instant::now() < deadline,
                "advertised URL {event_url} produced no ticket or response"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        reader.join().unwrap();
        if !response.starts_with("HTTP/1.1 412") {
            failures.push(format!("{event_url}: {}", response.lines().next().unwrap()));
        }
    }
    let singular = services[0]
        .child("eventSubURL")
        .text
        .strip_suffix('s')
        .unwrap();
    let mut socket = TcpStream::connect(surface.local_addr()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        socket,
        "SUBSCRIBE {singular} HTTP/1.1\r\nHost: fixture\r\nNT: invalid\r\nContent-Length: 0\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    socket.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 404"),
        "the singular event path must remain unavailable"
    );
    assert!(surface.shutdown().joined);
    assert!(
        failures.is_empty(),
        "advertised event URLs bypassed typed subscription ingress: {failures:?}"
    );
}
