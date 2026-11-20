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
