// ABOUTME: Verifies the pinned thirteen-byte player header with independent literal frames.
// ABOUTME: Keeps header fields, borrowed payload, and admission bounds distinct.
use earph_sendspin::{ProtocolError, decode_player_audio};

const FRAME: [u8; 15] = [
    4, 255, 255, 255, 255, 255, 255, 255, 249, 1, 2, 3, 4, 0x55, 0xaa,
];

#[test]
fn player_payload_starts_after_timestamp_and_send_ahead() {
    let bytes = FRAME;
    let frame = decode_player_audio(&bytes, 15).unwrap();
    assert_eq!(frame.server_timestamp_us, -7);
    assert_eq!(frame.send_ahead_us, 0x01020304);
    assert_eq!(frame.payload, &[0x55, 0xaa]);
    assert!(std::ptr::eq(frame.payload.as_ptr(), bytes[13..].as_ptr()));
    assert!(!format!("{frame:?}").contains("[85, 170]"));
}

#[test]
fn every_truncated_header_and_short_legacy_frame_are_rejected() {
    for length in 0..13 {
        assert_eq!(
            decode_player_audio(&FRAME[..length], 15),
            Err(ProtocolError::TruncatedHeader)
        );
    }
    let legacy = [4, 255, 255, 255, 255, 255, 255, 255, 249, 0x55, 0xaa];
    assert_eq!(
        decode_player_audio(&legacy, 15),
        Err(ProtocolError::TruncatedHeader)
    );
}

#[test]
fn the_actual_pinned_python_producer_preserves_the_player_payload_boundary() {
    let bytes = include_bytes!("fixtures/python-player-frame.bin");
    let frame = decode_player_audio(bytes, bytes.len()).unwrap();
    assert_eq!(frame.server_timestamp_us, -7);
    assert_eq!(frame.send_ahead_us, 16909060);
    assert_eq!(frame.payload, &[0x55, 0xaa]);
    assert!(std::ptr::eq(frame.payload.as_ptr(), bytes[13..].as_ptr()));
}

#[test]
fn the_byte_limit_counts_the_entire_message_including_the_header() {
    assert_eq!(
        decode_player_audio(&FRAME, 14),
        Err(ProtocolError::FrameBudgetExceeded)
    );
    assert!(
        decode_player_audio(&FRAME[..13], 13)
            .unwrap()
            .payload
            .is_empty()
    );
    let mut wrong_type = FRAME;
    wrong_type[0] = 1;
    assert_eq!(
        decode_player_audio(&wrong_type, 15),
        Err(ProtocolError::WrongMessageType)
    );
}

#[test]
fn signed_timestamp_extremes_and_saturated_lead_survive_without_normalization() {
    for (timestamp, lead) in [(i64::MIN, 0), (i64::MAX, u32::MAX), (0x0102030405060708, 1)] {
        let mut bytes = [4; 13];
        bytes[1..9].copy_from_slice(&timestamp.to_be_bytes());
        bytes[9..13].copy_from_slice(&lead.to_be_bytes());
        let frame = decode_player_audio(&bytes, 13).unwrap();
        assert_eq!(frame.server_timestamp_us, timestamp);
        assert_eq!(frame.send_ahead_us, lead);
    }
}
