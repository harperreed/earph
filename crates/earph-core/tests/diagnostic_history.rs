// ABOUTME: Exercises owner-scoped sparse event recovery through actual Session receipts.
// ABOUTME: Checks bounded pages, recovery gaps, immutable replay, and default redaction.
use earph_core::{control::*, ids::*, session::Session};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("secret-zone").unwrap(),
        OwnerId::new("secret-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn query(s: &mut Session, id: &str, cursor: Value) -> Receipt {
    let bytes = serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"secret-zone","owner_id":"secret-owner","session_id":null,"expected_revision":null,"payload":{"method":"events.get","cursor":cursor}})).unwrap();
    let c =
        decode_command(&bytes, &ControlLimits::default()).expect("events.get must be supported");
    s.submit(c)
}
fn page(s: &mut Session, id: &str, after: Option<u64>) -> Value {
    let cursor = after.map_or(
        Value::Null,
        |n| json!({"owner_id":"secret-owner","after_sequence":n}),
    );
    let r = query(s, id, cursor);
    assert_eq!(r.status, ReceiptStatus::Applied, "{:?}", r.error);
    serde_json::to_value(r).unwrap()["result"]["value"]["report"].clone()
}
fn mute(s: &mut Session, n: usize) {
    let state = s.snapshot();
    let r = s.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(format!("secret-event-{n}")).unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: None,
        expected_revision: None,
        payload: Command::Mute { muted: true },
    });
    assert_eq!(r.status, ReceiptStatus::Applied);
}
#[test]
fn events_initial_future_and_owner_recovery_are_explicit() {
    let mut s = owner();
    let empty = page(&mut s, "initial", None);
    assert_eq!(empty["next_sequence"], 0);
    assert_eq!(empty["events"], json!([]));
    assert_eq!(empty["scope"], "retained_state_events");
    assert_eq!(empty["complete_audit_log"], false);
    let future = page(&mut s, "future", Some(u64::MAX));
    assert_eq!(future["gap"], true);
    assert_eq!(future["next_sequence"], 0);
    assert_eq!(
        query(
            &mut s,
            "wrong-owner",
            json!({"owner_id":"old-owner","after_sequence":0})
        )
        .error
        .unwrap()
        .code,
        earph_core::ErrorCode::StaleSession
    );
}
#[test]
fn events_page_retention_gap_and_historical_replay_use_actual_entries() {
    let mut s = owner();
    for n in 0..129 {
        mute(&mut s, n);
    }
    let first = page(&mut s, "first", Some(0));
    assert_eq!(first["events"].as_array().unwrap().len(), 128);
    assert_eq!(first["next_sequence"], 128);
    assert_eq!(first["has_more"], true);
    assert_eq!(first["snapshot"]["cut"]["event_sequence"], 129);
    let second = page(&mut s, "second", Some(128));
    assert_eq!(second["events"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], false);
    for n in 129..1100 {
        mute(&mut s, n);
    }
    assert_eq!(page(&mut s, "first", Some(0)), first);
    let gap = page(&mut s, "gap", Some(0));
    assert_eq!(gap["gap"], true);
    assert_eq!(gap["snapshot"]["cut"]["event_sequence"], 1100);
    assert!(!gap.to_string().contains("secret"));
    assert_eq!(page(&mut s, "latest", Some(1100))["events"], json!([]));
    assert_eq!(page(&mut s, "oldest", Some(76))["gap"], false);
}
#[test]
fn sparse_sequence_advances_do_not_become_fabricated_events() {
    let mut s = owner();
    let state = s.snapshot();
    let r = s.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("secret-queue").unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: None,
        expected_revision: Some(state.revision),
        payload: Command::QueueAppend {
            path: "/secret/audio.wav".into(),
        },
    });
    assert_eq!(r.status, ReceiptStatus::Applied);
    let entry = s.queue_snapshot().entries[0].id.clone();
    s.set_queue_preparation(
        s.snapshot().epochs,
        &entry,
        earph_core::queue::PreparationStatus::Unprepared,
    )
    .unwrap();
    mute(&mut s, 3);
    let p = page(&mut s, "sparse", Some(0));
    let sequences = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["sequence"].as_u64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sequences, vec![1, 3]);
    assert_eq!(p["snapshot"]["cut"]["event_sequence"], 3);
    assert!(!p.to_string().contains("secret"));
}
#[test]
fn events_decoder_rejects_false_completeness_order_and_missing_nulls() {
    use earph_core::diagnostics::EventsRead;
    let mut s = owner();
    mute(&mut s, 1);
    mute(&mut s, 2);
    let r = query(
        &mut s,
        "decode",
        json!({"owner_id":"secret-owner","after_sequence":0}),
    );
    let original = serde_json::to_value(r).unwrap()["result"]["value"].clone();
    EventsRead::decode(&serde_json::to_vec(&original).unwrap()).unwrap();
    for field in ["complete_audit_log", "has_more"] {
        let mut bad = original.clone();
        bad["report"][field] = json!(true);
        assert!(EventsRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
    let mut bad = original.clone();
    bad["report"]["events"][1]["sequence"] = json!(1);
    assert!(EventsRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    bad = original.clone();
    bad["report"]["snapshot"]
        .as_object_mut()
        .unwrap()
        .remove("error");
    assert!(EventsRead::decode(&serde_json::to_vec(&bad).unwrap()).is_err());
    let mut typed = EventsRead::decode(&serde_json::to_vec(&original).unwrap()).unwrap();
    typed.report.events.reserve(256);
    assert!(typed.validate().is_err());
}
#[test]
fn restored_sparse_history_preserves_the_existing_gap_rule() {
    let mut s = owner();
    let state = s.snapshot();
    s.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("queue-before-restore").unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: None,
        expected_revision: Some(0),
        payload: Command::QueueAppend {
            path: "/secret/restore.wav".into(),
        },
    });
    let mut s = Session::restore_owner(
        ZoneId::new("secret-zone").unwrap(),
        OwnerId::new("secret-owner").unwrap(),
        ControlLimits::default(),
        s.checkpoint(),
    )
    .unwrap();
    let id = s.queue_snapshot().entries[0].id.clone();
    s.set_queue_preparation(
        s.snapshot().epochs,
        &id,
        earph_core::queue::PreparationStatus::Unprepared,
    )
    .unwrap();
    let expected = s.events_since(0);
    assert!(expected.gap);
    let p = page(&mut s, "restored-history", Some(0));
    assert_eq!(p["gap"], expected.gap);
    assert_eq!(p["next_sequence"], s.snapshot().event_sequence);
}

#[test]
fn consumer_matches_the_actual_page_to_its_requested_cursor() {
    use earph_core::diagnostics::{EventCursor, EventsRead};
    let mut s = owner();
    mute(&mut s, 1);
    mute(&mut s, 2);
    let r = query(
        &mut s,
        "cursor-read",
        json!({"owner_id":"secret-owner","after_sequence":0}),
    );
    let value = serde_json::to_value(r).unwrap()["result"]["value"].clone();
    let read = EventsRead::decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    let mut cursor = EventCursor {
        owner_id: OwnerId::new("secret-owner").unwrap(),
        after_sequence: 0,
    };
    read.validate_cursor(Some(&cursor)).unwrap();
    assert!(read.validate_cursor(None).is_err());
    cursor.after_sequence = 2;
    assert!(read.validate_cursor(Some(&cursor)).is_err());
    cursor.after_sequence = 0;
    cursor.owner_id = OwnerId::new("other-owner").unwrap();
    assert!(read.validate_cursor(Some(&cursor)).is_err());
}
