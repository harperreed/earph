// ABOUTME: Parses Core's generated event bodies through both XML layers.
// ABOUTME: Checks exact payload admission and direct writes into final bounded slots.
use earph_core::{
    control::*, ids::*, remote_volume::RemoteVolume, renderer_events::*, session::Session,
};
use quick_xml::{Reader, events::Event};
use std::{collections::BTreeMap, sync::Arc};

fn projection(metadata: &str) -> RendererProjection {
    let mut owner = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("encoder-fixture").unwrap(),
        ControlLimits::default(),
    );
    let request = CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("descriptor").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: owner.snapshot().owner_id,
        session_id: None,
        expected_revision: Some(0),
        payload: Command::SourceStageUri {
            uri: "http://private.example:80/media?a=1&b=2".into(),
            metadata: metadata.into(),
        },
    };
    assert_eq!(owner.submit(request).status, ReceiptStatus::Applied);
    let descriptor = owner
        .staged_uri_descriptor(&StagedMediaId::new("staged-1").unwrap(), 1)
        .unwrap();
    RendererProjection {
        service: RendererService::AvTransport,
        transport_state: "TRANSITIONING",
        transport_status: "ERROR_OCCURRED",
        transport_actions: "Play,Stop,Pause",
        presets: "FactoryDefaults",
        duration_seconds: Some(u64::MAX),
        position_seconds: None,
        muted: true,
        volume: RemoteVolume::from_volume(0).unwrap(),
        connected: true,
        descriptor: Some(descriptor),
    }
}
fn body(projection: &RendererProjection) -> String {
    let mut buffer = String::with_capacity(projection.service.event_slot_bytes());
    let pointer = buffer.as_ptr();
    projection.encode_into(&mut buffer).unwrap();
    assert_eq!(
        buffer.as_ptr(),
        pointer,
        "encoding must use the final reserved slot"
    );
    assert_eq!(buffer.len(), projection.encoded_size().unwrap());
    buffer
}
fn last_change(xml: &str) -> String {
    let mut reader = Reader::from_str(xml);
    let mut text = String::new();
    let mut inside = false;
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) if e.local_name().as_ref() == b"LastChange" => inside = true,
            Event::End(e) if e.local_name().as_ref() == b"LastChange" => inside = false,
            Event::Text(e) if inside => text.push_str(&e.xml10_content().unwrap()),
            Event::GeneralRef(e) if inside => text.push_str(
                &quick_xml::escape::unescape(&format!("&{};", e.decode().unwrap())).unwrap(),
            ),
            Event::Eof => break,
            _ => {}
        }
    }
    assert!(!text.is_empty());
    text
}
fn values(inner: &str) -> BTreeMap<String, String> {
    let mut reader = Reader::from_str(inner);
    let mut values = BTreeMap::new();
    loop {
        match reader.read_event().unwrap() {
            Event::Empty(e) => {
                let name = String::from_utf8(e.local_name().as_ref().to_vec()).unwrap();
                for attr in e.attributes() {
                    let attr = attr.unwrap();
                    if attr.key.as_ref() == b"val" {
                        assert!(
                            !attr
                                .value
                                .iter()
                                .any(|c| matches!(c, b'\t' | b'\r' | b'\n')),
                            "literal attribute whitespace loses opaque metadata under XML normalization"
                        );
                        values.insert(
                            name.clone(),
                            attr.decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .unwrap()
                            .into_owned(),
                        );
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    values
}
#[test]
fn event_metadata_survives_both_xml_layers_and_attribute_normalization() {
    let metadata = "artist & <track> \"quoted\" 'apostrophe' 🦀\tline\r\nnext";
    let projection = projection(metadata);
    let values = values(&last_change(&body(&projection)));
    assert_eq!(values["AVTransportURIMetaData"], metadata);
    assert_eq!(values["CurrentTrackMetaData"], metadata);
    assert_eq!(
        values["AVTransportURI"],
        "http://private.example:80/media?a=1&b=2"
    );
    assert_eq!(values["CurrentTrackURI"], values["AVTransportURI"]);
}
#[test]
fn exact_event_cap_is_checked_before_writing_or_growing_the_slot() {
    let seed = projection("");
    let base = body(&seed).len();
    // URI metadata appears twice. Each literal quote expands to ten bytes in each copy.
    let mut count = (MAX_RENDERER_EVENT_BYTES - base) / 20;
    let mut candidate = projection(&"\"".repeat(count));
    while candidate.encoded_size().is_err() {
        count -= 1;
        candidate = projection(&"\"".repeat(count));
    }
    let bytes = body(&candidate).len();
    let padding = (MAX_RENDERER_EVENT_BYTES - bytes) / 2;
    let mut metadata = "\"".repeat(count);
    metadata.push_str(&"x".repeat(padding));
    candidate = projection(&metadata);
    if candidate.encoded_size().unwrap() < MAX_RENDERER_EVENT_BYTES {
        // This encoder test varies scalar width independently from canonical transport-state semantics.
        candidate.transport_state = "TRANSITIONINGX";
    }
    let exact = body(&candidate);
    assert_eq!(exact.len(), MAX_RENDERER_EVENT_BYTES);
    values(&last_change(&exact));
    candidate.transport_state = if candidate.transport_state == "TRANSITIONING" {
        "TRANSITIONINGX"
    } else {
        "TRANSITIONINGXX"
    };
    let mut slot = String::with_capacity(MAX_RENDERER_EVENT_BYTES);
    slot.push_str("untouched");
    let pointer = slot.as_ptr();
    assert!(candidate.encode_into(&mut slot).is_err());
    assert_eq!(slot, "untouched");
    assert_eq!(slot.as_ptr(), pointer);
    let excessive = projection(&"\"".repeat(65536));
    assert!(excessive.encoded_size().is_err());
}
#[test]
fn small_service_tables_fit_their_fixed_slots_and_do_not_emit_soap_only_fields() {
    let mut projection = projection("");
    projection.service = RendererService::RenderingControl;
    projection.volume = RemoteVolume::from_volume(20467).unwrap();
    let rcs = body(&projection);
    let values = values(&last_change(&rcs));
    assert_eq!(values["Volume"], "20467");
    assert_eq!(values["VolumeDB"], "-10240");
    assert_eq!(values["Mute"], "1");
    assert!(rcs.len() < 2048);
    projection.service = RendererService::ConnectionManager;
    let cm = body(&projection);
    assert!(cm.len() < 1024);
    assert!(!cm.contains("A_ARG_TYPE"));
    assert!(!cm.contains("LastChange"));
    let mut reader = Reader::from_str(&cm);
    loop {
        if matches!(reader.read_event().unwrap(), Event::Eof) {
            break;
        }
    }
    let initial = RendererEventSnapshot {
        controller_id: RendererControllerId::new("controller").unwrap(),
        controller_generation: 1,
        projection,
    };
    let shared = Arc::new(initial);
    assert!(!format!("{shared:?}").contains("private.example"));
}
