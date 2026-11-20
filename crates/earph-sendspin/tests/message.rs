// ABOUTME: Tests authenticated message boundaries and bounded fragment assembly.
// ABOUTME: Uses real records in session tests and exact plaintext format vectors here.
use earph_sendspin::message::*;

#[test]
fn fragment_roundtrip_keeps_last_arrival_and_exact_body() {
    let limits = MessageLimits {
        record_plaintext_bytes: 12,
        ..MessageLimits::default()
    };
    let body = vec![42; 30];
    let mut writer = Outgoing::new(4, body.clone(), limits).unwrap();
    let mut reader = Reassembler::new(limits);
    let mut complete = None;
    let mut count = 0;
    while let Some(record) = writer.next_record() {
        count += 1;
        complete = reader.push(&record, count).unwrap();
    }
    let complete = complete.unwrap();
    assert_eq!(complete.id, 4);
    assert_eq!(complete.body, body);
    assert_eq!(complete.arrival_ms, count);
}

#[test]
fn malformed_fragment_sequences_poison_assembly() {
    for records in [
        vec![vec![1]],
        vec![vec![1, 2]],
        vec![vec![1, 2, 1]],
        vec![vec![1, 4, 0]],
        vec![vec![1, 0]],
        vec![vec![1, 2, 0], vec![1, 2, 0]],
        vec![vec![1, 2, 0], vec![0]],
    ] {
        let mut reader = Reassembler::new(MessageLimits::default());
        let mut failed = false;
        for record in records {
            if reader.push(&record, 0).is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed);
        assert!(reader.push(&[0, b'{', b'}'], 0).is_err());
    }
}

#[test]
fn unknown_fragment_discard_is_still_bounded() {
    let limits = MessageLimits {
        body_bytes: 2,
        fragment_count: 2,
        assembly_ms: 5,
        ..MessageLimits::default()
    };
    let mut reader = Reassembler::new(limits);
    assert!(reader.push(&[1, 2, 2, 9], 0).unwrap().is_none());
    assert!(reader.push(&[1, 0, 9], 1).unwrap().is_none());
    assert_eq!(reader.push(&[1, 1], 2), Err(MessageError::Budget));
    let mut reader = Reassembler::new(limits);
    reader.push(&[1, 2, 3], 0).unwrap();
    assert_eq!(reader.push(&[1, 1], 5), Err(MessageError::Deadline));
}

#[test]
fn envelope_validates_shape_without_guessing_unknown_direction() {
    assert!(
        Envelope::parse(br#"{"type":"client/future","payload":{}}"#)
            .unwrap()
            .direction()
            .is_none()
    );
    assert_eq!(
        Envelope::parse(br#"{"type":"client/hello","payload":{}}"#)
            .unwrap()
            .direction(),
        Some(Direction::Client)
    );
    for raw in [
        b"[]".as_slice(),
        b"{}",
        br#"{"type":0,"payload":{}}"#,
        br#"{"type":"x","payload":[]}"#,
    ] {
        assert!(Envelope::parse(raw).is_err());
    }
}

#[test]
fn first_last_empty_and_limit_boundaries_are_valid() {
    let limits = MessageLimits {
        body_bytes: 2,
        ..MessageLimits::default()
    };
    let mut reader = Reassembler::new(limits);
    assert_eq!(
        reader.push(&[1, 3, 4], 7).unwrap().unwrap().body,
        Vec::<u8>::new()
    );
    assert_eq!(
        reader.push(&[4, 1, 2], 8).unwrap().unwrap().body,
        vec![1, 2]
    );
    assert_eq!(reader.push(&[4, 1, 2, 3], 9), Err(MessageError::Budget));
}

#[test]
fn exact_wire_record_boundaries_fragment_only_when_needed() {
    let limits = MessageLimits::default();
    let mut exact = Outgoing::new(4, vec![8; 65518], limits).unwrap();
    let record = exact.next_record().unwrap();
    assert_eq!(record.len(), 65519);
    assert_eq!(record[0], 4);
    assert!(exact.next_record().is_none());
    let mut larger = Outgoing::new(4, vec![8; 65519], limits).unwrap();
    let first = larger.next_record().unwrap();
    assert_eq!(first.len(), 65519);
    assert_eq!(&first[..3], &[1, 2, 4]);
    let last = larger.next_record().unwrap();
    assert_eq!(&last[..2], &[1, 1]);
    assert_eq!(last.len(), 5);
    assert!(larger.next_record().is_none());
    assert_eq!(
        Outgoing::validate(
            4,
            usize::MAX,
            MessageLimits {
                body_bytes: usize::MAX,
                ..limits
            }
        ),
        Err(MessageError::Budget)
    );
}
