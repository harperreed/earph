// ABOUTME: Exercises typed player messages through real framing and reassembly.
// ABOUTME: Checks fixed PCM contracts without sockets, native output, or admission claims.
use earph_sendspin::{decode_player_audio_body, message::*, player::*, timing::MAX_TIME_US};
use serde_json::{Value, json};

fn wire(id: u8, body: Vec<u8>) -> Complete {
    let limits = MessageLimits {
        record_plaintext_bytes: 17,
        ..MessageLimits::default()
    };
    let mut outgoing = Outgoing::new(id, body, limits).unwrap();
    let mut reader = Reassembler::new(limits);
    let mut complete = None;
    let mut stamp = 100;
    while let Some(record) = outgoing.next_record() {
        complete = reader
            .push(&record, ReceiveStamp::new(stamp).unwrap())
            .unwrap();
        stamp += 1;
    }
    complete.unwrap()
}
fn message(kind: &str, payload: Value) -> Complete {
    wire(
        0,
        serde_json::to_vec(&json!({"type":kind,"payload":payload})).unwrap(),
    )
}
fn start() -> Value {
    json!({"server_transmitted":1,"player":{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16,"unknown":true},"unknown":true})
}

#[test]
fn support_and_state_encode_exact_fixed_format_and_empty_commands() {
    let support = PlayerSupport::new(1048576).unwrap().to_value();
    assert_eq!(
        support,
        json!({"supported_formats":[{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16}],"buffer_capacity":1048576})
    );
    assert!(PlayerSupport::new(1048577).is_err());
    assert!(PlayerSupport::new(0).is_err());
    let state = PlayerState::new(100, 7, 9).unwrap().to_value();
    assert_eq!(
        state,
        json!({"output_delay_ms":100,"required_lead_time_ms":7,"min_buffer_ms":9,"supported_commands":[]})
    );
    assert!(PlayerState::new(5001, 0, 0).is_err());
    assert!(PlayerState::new(0, 0, 5001).is_err());
}

#[test]
fn lifecycle_roundtrip_validates_active_known_fields_and_ignores_inactive_objects() {
    let complete = message("stream/start", start());
    assert!(matches!(
        PlayerEvent::decode(&complete, true).unwrap(),
        Some(PlayerEvent::Start {
            format: StreamFormat::PCM,
            server_transmitted_us: 1,
            ..
        })
    ));
    for key in ["codec", "sample_rate", "channels", "bit_depth"] {
        let mut payload = start();
        payload["player"].as_object_mut().unwrap().remove(key);
        assert_eq!(
            PlayerEvent::decode(&message("stream/start", payload), true).unwrap_err(),
            PlayerError::Malformed
        );
    }
    for field in [
        json!({"codec":"flac","sample_rate":48000,"channels":2,"bit_depth":16}),
        json!({"codec":"pcm","sample_rate":48000.0,"channels":2,"bit_depth":16}),
        json!({"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16,"codec_header":null}),
    ] {
        let complete = message(
            "stream/start",
            json!({"server_transmitted":1,"player":field}),
        );
        assert!(PlayerEvent::decode(&complete, true).is_err());
        assert!(PlayerEvent::decode(&complete, false).unwrap().is_none());
    }
    for bad in [json!(1.0), json!(MAX_TIME_US + 1), json!(null)] {
        assert!(
            PlayerEvent::decode(
                &message(
                    "stream/start",
                    json!({"server_transmitted":bad,"player":false})
                ),
                false
            )
            .is_err()
        );
    }
    let partial = message(
        "stream/clear",
        json!({"server_transmitted":2,"from_timestamp":3,"roles":["player"],"extra":42}),
    );
    assert!(matches!(
        PlayerEvent::decode(&partial, true).unwrap(),
        Some(PlayerEvent::Clear {
            from_timestamp_us: Some(3),
            ..
        })
    ));
    let full = message("stream/clear", json!({"server_transmitted":2}));
    assert!(matches!(
        PlayerEvent::decode(&full, true).unwrap(),
        Some(PlayerEvent::Clear {
            from_timestamp_us: None,
            ..
        })
    ));
    let end = message("stream/end", json!({}));
    assert!(matches!(
        PlayerEvent::decode(&end, true).unwrap(),
        Some(PlayerEvent::End { .. })
    ));
    assert!(
        PlayerEvent::decode(&message("server/command", json!({"player":false})), true)
            .unwrap()
            .is_none()
    );
}

#[test]
fn audio_body_uses_one_borrowed_header_and_rejects_geometry_before_allocation() {
    for frames in [0, 1, 7200] {
        let mut body = 123_i64.to_be_bytes().to_vec();
        body.extend_from_slice(&u32::MAX.to_be_bytes());
        body.extend(std::iter::repeat_n([0, 128, 255, 127], frames).flatten());
        let complete = wire(4, body);
        let borrowed =
            decode_player_audio_body(complete.id, &complete.body, MAX_CHUNK_WIRE_BYTES).unwrap();
        assert!(std::ptr::eq(
            borrowed.payload.as_ptr(),
            complete.body[12..].as_ptr()
        ));
        let event = PlayerEvent::decode(&complete, true).unwrap().unwrap();
        let PlayerEvent::Audio { frame, arrival } = event else {
            panic!("audio expected")
        };
        assert_eq!(frame.payload.len(), frames * 4);
        assert_eq!(arrival, complete.arrival);
    }
    for bytes in [1, 2, 3, 28804] {
        let mut body = vec![0; 12];
        body.extend(vec![0; bytes]);
        assert!(PlayerEvent::decode(&wire(4, body), true).is_err());
    }
    for bytes in 0..12 {
        let complete = wire(4, vec![0; bytes]);
        assert!(PlayerEvent::decode(&complete, true).is_err());
        assert!(PlayerEvent::decode(&complete, false).unwrap().is_none());
    }
    let mut body = (MAX_TIME_US + 1).to_be_bytes().to_vec();
    body.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        PlayerEvent::decode(&wire(4, body), true).unwrap_err(),
        PlayerError::NumericRange
    );
}
