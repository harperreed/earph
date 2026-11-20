// ABOUTME: Checks service faults and strict escaped SOAP responses at the wire boundary.
// ABOUTME: Keeps shape admission separate from unavailable owner execution.
use earph_network::upnp::{Service, UpnpFault, decode_soap, encode_fault, encode_response};

fn request(service: Service, action: &str, arguments: &str) -> earph_network::upnp::SoapRequest {
    let ns = service.namespace();
    let xml = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{ns}">{arguments}</u:{action}></s:Body></s:Envelope>"#
    );
    decode_soap(service, &format!("\"{ns}#{action}\""), xml.as_bytes()).unwrap()
}

#[test]
fn semantic_types_and_service_faults_precede_any_owner_dispatch() {
    for (service, action, args, code) in [
        (
            Service::AvTransport,
            "Stop",
            "<InstanceID>x</InstanceID>",
            402,
        ),
        (
            Service::AvTransport,
            "Stop",
            "<InstanceID>4294967296</InstanceID>",
            402,
        ),
        (
            Service::AvTransport,
            "Stop",
            "<InstanceID>1</InstanceID>",
            718,
        ),
        (
            Service::RenderingControl,
            "GetMute",
            "<InstanceID>1</InstanceID><Channel>Master</Channel>",
            702,
        ),
        (
            Service::AvTransport,
            "Play",
            "<InstanceID>0</InstanceID><Speed>2</Speed>",
            717,
        ),
        (
            Service::RenderingControl,
            "SetMute",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredMute>yes!</DesiredMute>",
            402,
        ),
        (
            Service::RenderingControl,
            "SetVolume",
            "<InstanceID>0</InstanceID><Channel>Master</Channel><DesiredVolume>65536</DesiredVolume>",
            402,
        ),
        (
            Service::ConnectionManager,
            "GetCurrentConnectionInfo",
            "<ConnectionID>nan</ConnectionID>",
            402,
        ),
    ] {
        assert_eq!(
            request(service, action, args)
                .validate_arguments()
                .unwrap_err()
                .code(),
            code
        );
    }
    for action in ["Next", "Previous"] {
        assert_eq!(
            request(Service::AvTransport, action, "<InstanceID>0</InstanceID>")
                .nonseekable_fault()
                .unwrap()
                .code(),
            711
        );
    }
    assert_eq!(
        request(
            Service::AvTransport,
            "Seek",
            "<InstanceID>0</InstanceID><Unit>REL_TIME</Unit><Target>00:01:00</Target>"
        )
        .nonseekable_fault()
        .unwrap()
        .code(),
        710
    );
    assert!(
        request(Service::AvTransport, "Stop", "<InstanceID>0</InstanceID>")
            .validate_arguments()
            .is_ok()
    );
}

#[test]
fn responses_use_declared_order_and_escape_opaque_values_without_double_decoding() {
    let req = request(
        Service::AvTransport,
        "GetTransportInfo",
        "<InstanceID>0</InstanceID>",
    );
    let bytes = encode_response(
        &req,
        &[
            ("CurrentTransportState", "STOPPED"),
            ("CurrentTransportStatus", "OK"),
            ("CurrentSpeed", "1"),
        ],
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("GetTransportInfoResponse"));
    assert!(text.contains(Service::AvTransport.namespace()));
    assert!(
        encode_response(
            &req,
            &[
                ("CurrentSpeed", "1"),
                ("CurrentTransportStatus", "OK"),
                ("CurrentTransportState", "STOPPED")
            ]
        )
        .is_err()
    );
    assert!(encode_response(&req, &[]).is_err());
    let req = request(Service::ConnectionManager, "GetProtocolInfo", "");
    let bytes = encode_response(
        &req,
        &[
            ("Source", "<&private>"),
            ("Sink", "http-get:*:audio/flac:*"),
        ],
    )
    .unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("&lt;&amp;private&gt;"));
    assert!(!text.contains("<Source><&private>"));
    assert!(encode_response(&req, &[("Source", "\u{0000}"), ("Sink", "")]).is_err());
}

#[test]
fn faults_use_the_uda_control_namespace_and_carry_no_private_error_text() {
    let text = String::from_utf8(encode_fault(UpnpFault::ActionFailed)).unwrap();
    assert!(text.contains("urn:schemas-upnp-org:control-1-0"));
    assert!(text.contains("<errorCode>501</errorCode>"));
    assert!(text.contains("<faultcode>s:Client</faultcode>"));
    assert!(text.contains("<errorDescription>Action Failed</errorDescription>"));
}

#[test]
fn response_budget_counts_actual_escaped_bytes_before_allocating() {
    let req = request(Service::ConnectionManager, "GetProtocolInfo", "");
    let value = "&".repeat(100000);
    let bytes = encode_response(&req, &[("Source", &value), ("Sink", "")])
        .expect("500000 escaped bytes fit the response budget");
    assert!(bytes.len() < 524288);
    assert!(encode_response(&req, &[("Source", &"&".repeat(110000)), ("Sink", "")]).is_err());
}
