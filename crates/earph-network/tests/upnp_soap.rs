// ABOUTME: Exercises actual SOAP decoding at the UPnP control boundary.
// ABOUTME: Verifies namespace identity, exact argument text, and private payload redaction.
use earph_network::upnp::{Service, SoapError, decode_soap};

const AVT: &str = "urn:schemas-upnp-org:service:AVTransport:1";

fn message(namespace: &str, action: &str, arguments: &str) -> Vec<u8> {
    format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
      s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body>
      <u:{action} xmlns:u="{namespace}">{arguments}</u:{action}>
      </s:Body></s:Envelope>"#
    )
    .into_bytes()
}

fn decode(action: &str, arguments: &str) -> Result<earph_network::upnp::SoapRequest, SoapError> {
    decode_soap(
        Service::AvTransport,
        &format!("\"{AVT}#{action}\""),
        &message(AVT, action, arguments),
    )
}

#[test]
fn stage_decodes_escaped_uri_and_opaque_metadata_without_disclosing_payloads() {
    let body = br#"<?xml version="1.0" encoding="utf-8"?>
      <s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
        s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
        <s:Body><u:SetAVTransportURI xmlns:u="urn:schemas-upnp-org:service:AVTransport:1">
          <InstanceID>0</InstanceID><CurrentURI>https://fixture.invalid/private.flac?a=1&amp;b=2</CurrentURI>
          <CurrentURIMetaData><![CDATA[<DIDL-Lite>private track</DIDL-Lite>]]></CurrentURIMetaData>
        </u:SetAVTransportURI></s:Body>
      </s:Envelope>"#;
    let request = decode_soap(
        Service::AvTransport,
        "\"urn:schemas-upnp-org:service:AVTransport:1#SetAVTransportURI\"",
        body,
    )
    .unwrap();
    assert_eq!(request.name(), "SetAVTransportURI");
    assert_eq!(request.argument("InstanceID"), Some("0"));
    assert_eq!(
        request.argument("CurrentURI"),
        Some("https://fixture.invalid/private.flac?a=1&b=2")
    );
    assert_eq!(
        request.argument("CurrentURIMetaData"),
        Some("<DIDL-Lite>private track</DIDL-Lite>")
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("private"));
    assert!(!debug.contains("fixture.invalid"));
}

#[test]
fn transport_actions_require_their_declared_ordered_arguments() {
    for action in [
        "Stop",
        "Pause",
        "Next",
        "Previous",
        "GetMediaInfo",
        "GetTransportInfo",
        "GetPositionInfo",
        "GetDeviceCapabilities",
        "GetTransportSettings",
        "GetCurrentTransportActions",
    ] {
        assert_eq!(
            decode(action, "<InstanceID>0</InstanceID>").unwrap().name(),
            action
        );
    }
    assert_eq!(
        decode("Play", "<InstanceID>0</InstanceID><Speed>1</Speed>")
            .unwrap()
            .argument("Speed"),
        Some("1")
    );
    assert_eq!(
        decode(
            "Seek",
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>00:01:00</Target>"
        )
        .unwrap()
        .argument("Target"),
        Some("00:01:00")
    );
    for arguments in [
        "",
        "<Speed>1</Speed><InstanceID>0</InstanceID>",
        "<InstanceID>0</InstanceID><InstanceID>0</InstanceID><Speed>1</Speed>",
        "<InstanceID>0</InstanceID><Speed>1</Speed><Extra>bad</Extra>",
    ] {
        assert_eq!(
            decode("Play", arguments).unwrap_err(),
            SoapError::InvalidArguments
        );
    }
    assert_eq!(
        decode("SetNextAVTransportURI", "").unwrap_err(),
        SoapError::InvalidAction
    );
}

#[test]
fn header_action_and_xml_namespace_must_agree() {
    let body = message(AVT, "Stop", "<InstanceID>0</InstanceID>");
    for header in [
        format!("{AVT}#Stop"),
        format!("\"{AVT}#Play\""),
        "\"urn:schemas-upnp-org:service:RenderingControl:1#Stop\"".into(),
    ] {
        assert_eq!(
            decode_soap(Service::AvTransport, &header, &body).unwrap_err(),
            SoapError::InvalidAction
        );
    }
    assert_eq!(
        decode_soap(
            Service::AvTransport,
            &format!("\"{AVT}#Stop\""),
            &message("urn:wrong", "Stop", "<InstanceID>0</InstanceID>")
        )
        .unwrap_err(),
        SoapError::InvalidAction
    );
}

#[test]
fn malformed_xml_entities_and_encoding_never_become_commands() {
    let valid = String::from_utf8(message(AVT, "Stop", "<InstanceID>0</InstanceID>")).unwrap();
    let examples = [
        format!("<!DOCTYPE x [<!ENTITY ext SYSTEM 'file:///etc/passwd'>]>{valid}"),
        valid.replace(">0<", ">&unknown;<"),
        valid.replace("</s:Body>", "</s:Wrong>"),
        format!("{valid}{valid}"),
        format!("<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>{valid}"),
        valid.replace(">0<", ">\u{0000}<"),
        format!("<?xml version=\"1.0\"?><?xml version=\"1.0\"?>{valid}"),
        format!(" <!--before declaration--><?xml version=\"1.0\"?>{valid}"),
        valid.replace("<InstanceID>", "<InstanceID extra=\"&#0;\">"),
    ];
    for body in examples {
        assert_eq!(
            decode_soap(
                Service::AvTransport,
                &format!("\"{AVT}#Stop\""),
                body.as_bytes()
            )
            .unwrap_err(),
            SoapError::MalformedXml
        );
    }
}

#[test]
fn bounded_foreign_extensions_do_not_change_known_action_arguments() {
    let body = message(
        AVT,
        "Play",
        "<x:extra xmlns:x=\"urn:extension\"><nested>ignored</nested></x:extra><InstanceID>0</InstanceID><!-- gap --><?extra ignored?><Speed custom=\"ignored\">1</Speed>",
    );
    let request = decode_soap(Service::AvTransport, &format!("\"{AVT}#Play\""), &body).unwrap();
    assert_eq!(request.argument("Speed"), Some("1"));
    assert!(request.argument("extra").is_none());
    let nested = "<x:n>".repeat(16) + &"</x:n>".repeat(16);
    let body = message(
        AVT,
        "Stop",
        &format!("<x:extra xmlns:x=\"urn:extension\">{nested}</x:extra><InstanceID>0</InstanceID>"),
    );
    assert_eq!(
        decode_soap(Service::AvTransport, &format!("\"{AVT}#Stop\""), &body).unwrap_err(),
        SoapError::ResourceBudgetExceeded
    );
    let elements = "<x:n/>".repeat(8192);
    let body = message(
        AVT,
        "Stop",
        &format!(
            "<x:extra xmlns:x=\"urn:extension\">{elements}</x:extra><InstanceID>0</InstanceID>"
        ),
    );
    assert_eq!(
        decode_soap(Service::AvTransport, &format!("\"{AVT}#Stop\""), &body).unwrap_err(),
        SoapError::ResourceBudgetExceeded
    );
}

#[test]
fn rendering_and_connection_queries_keep_their_service_contracts() {
    for (service, namespace, action, arguments) in [
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "ListPresets",
            "<InstanceID>0</InstanceID>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SelectPreset",
            "<InstanceID>0</InstanceID><PresetName>FactoryDefaults</PresetName>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "GetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredMute>1</DesiredMute>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "GetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>0</DesiredVolume>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "GetVolumeDB",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "SetVolumeDB",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>-32767</DesiredVolume>",
        ),
        (
            Service::RenderingControl,
            "urn:schemas-upnp-org:service:RenderingControl:1",
            "GetVolumeDBRange",
            "<InstanceID>0</InstanceID><Channel>Master</Channel>",
        ),
        (
            Service::ConnectionManager,
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetProtocolInfo",
            "",
        ),
        (
            Service::ConnectionManager,
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetCurrentConnectionIDs",
            "",
        ),
        (
            Service::ConnectionManager,
            "urn:schemas-upnp-org:service:ConnectionManager:1",
            "GetCurrentConnectionInfo",
            "<ConnectionID>0</ConnectionID>",
        ),
    ] {
        let parsed = decode_soap(
            service,
            &format!("\"{namespace}#{action}\""),
            &message(namespace, action, arguments),
        )
        .unwrap();
        assert_eq!(parsed.service, service);
        assert_eq!(parsed.name(), action);
        assert!(
            decode_soap(
                Service::AvTransport,
                &format!("\"{namespace}#{action}\""),
                &message(namespace, action, arguments)
            )
            .is_err()
        );
    }
}

#[test]
fn decoded_stage_fields_and_total_xml_work_have_bounded_sizes() {
    let uri = "x".repeat(8193);
    assert_eq!(
        decode(
            "SetAVTransportURI",
            &format!(
                "<InstanceID>0</InstanceID><CurrentURI>{uri}</CurrentURI><CurrentURIMetaData/>"
            )
        )
        .unwrap_err(),
        SoapError::ResourceBudgetExceeded
    );
    let metadata = "x".repeat(65537);
    assert_eq!(decode("SetAVTransportURI", &format!("<InstanceID>0</InstanceID><CurrentURI/><CurrentURIMetaData>{metadata}</CurrentURIMetaData>" )).unwrap_err(), SoapError::ResourceBudgetExceeded);
    let attributes = (0..33).map(|i| format!("a{i}=\"x\" ")).collect::<String>();
    let body = message(
        AVT,
        "Stop",
        &format!("<InstanceID {attributes}>0</InstanceID>"),
    );
    assert_eq!(
        decode_soap(Service::AvTransport, &format!("\"{AVT}#Stop\""), &body).unwrap_err(),
        SoapError::ResourceBudgetExceeded
    );
    assert_eq!(
        decode_soap(
            Service::AvTransport,
            &format!("\"{AVT}#Stop\""),
            &vec![b' '; 262145]
        )
        .unwrap_err(),
        SoapError::ResourceBudgetExceeded
    );
}

#[test]
fn duplicate_expanded_attribute_names_are_not_valid_namespace_xml() {
    let body = message(
        AVT,
        "Stop",
        "<InstanceID xmlns:a=\"urn:same\" xmlns:b=\"urn:same\" a:field=\"one\" b:field=\"two\">0</InstanceID>",
    );
    assert_eq!(
        decode_soap(Service::AvTransport, &format!("\"{AVT}#Stop\""), &body).unwrap_err(),
        SoapError::MalformedXml
    );
}

#[test]
fn escaped_namespace_identifiers_keep_their_actual_expanded_identity() {
    let body = String::from_utf8(message(AVT, "Stop", "<InstanceID>0</InstanceID>"))
        .unwrap()
        .replace("AVTransport:1", "AVTransport:&#49;");
    assert_eq!(
        decode_soap(
            Service::AvTransport,
            &format!("\"{AVT}#Stop\""),
            body.as_bytes()
        )
        .unwrap()
        .name(),
        "Stop"
    );
    let body = message(
        AVT,
        "Stop",
        "<InstanceID xmlns:a=\"urn:sam&#101;\" xmlns:b=\"urn:same\" a:field=\"one\" b:field=\"two\">0</InstanceID>",
    );
    assert_eq!(
        decode_soap(Service::AvTransport, &format!("\"{AVT}#Stop\""), &body).unwrap_err(),
        SoapError::MalformedXml
    );
}
