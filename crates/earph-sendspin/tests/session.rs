// ABOUTME: Exercises initial session sequencing, real write receipts, and admission.
// ABOUTME: Uses the same real Snow peer helper as re-handshake scenarios.
#[path = "support/session_fixture.rs"]
mod fixture;
use earph_sendspin::session::*;
use fixture::*;
use serde_json::json;

#[test]
fn initial_sequence_requires_hello_then_fenced_admission() {
    let (mut client, mut peer) = initial();
    assert!(
        client
            .send(
                ApplicationMessage::json("client/time", json!({"client_transmitted":1})),
                4
            )
            .is_err()
    );
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
            5,
        )
        .unwrap();
    let id = admission(&events);
    assert_eq!(client.phase(), SessionPhase::AwaitAdmission);
    client.admit(id, true, 6).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
    client
        .send(
            ApplicationMessage::json("client/time", json!({"client_transmitted":2})),
            7,
        )
        .unwrap();
    let write = client.next_write(7).unwrap().unwrap();
    assert_eq!(peer.read_json(&write.bytes)["type"], "client/time");
    client
        .complete_write(write.id, WriteResult::Complete, 7)
        .unwrap();
}

#[test]
fn unknown_and_wrong_direction_before_activation_close() {
    for kind in [
        "client/future",
        "client/time",
        "server/hello",
        "server/init",
    ] {
        let (mut client, mut peer) = initial();
        assert!(peer.send_json(&mut client, kind, json!({}), 5).is_err());
        assert_eq!(client.phase(), SessionPhase::Closed);
    }
}

#[test]
fn stale_admission_cannot_release_sends_and_text_after_encryption_closes() {
    let (mut client, mut peer) = initial();
    let first = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    let id = admission(&first);
    client.admit(id, true, 6).unwrap();
    let second = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 7)
        .unwrap();
    let second_id = admission(&second);
    client.admit(id, true, 8).unwrap();
    assert_eq!(client.phase(), SessionPhase::AwaitAdmission);
    client.admit(second_id, true, 9).unwrap();
    assert!(client.receive(TransportKind::Text, b"{}", 10).is_err());
}

#[test]
fn partial_write_and_fixed_initial_deadline_poison() {
    let mut client = new_client();
    let write = client.next_write(0).unwrap().unwrap();
    assert!(
        client
            .complete_write(write.id, WriteResult::Uncertain, 1)
            .is_err()
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
    let mut client = new_client();
    assert!(client.tick(30_000).is_err());
    assert!(client.next_write(30_001).unwrap().is_none());
}

#[test]
fn method_rejection_stays_suppressed_until_later_admitted_activation() {
    let (mut client, mut peer) = initial();
    let events=peer.send_json(&mut client,"server/activate",json!({"activities":["pairing"],"pairing":{"method":"dynamic_pairing_code","format":"digits"}}),5).unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::Admission(_)))
    );
    let write = client.next_write(6).unwrap().unwrap();
    assert_eq!(
        peer.read_json(&write.bytes)["payload"]["reason"],
        "method_not_supported"
    );
    client
        .complete_write(write.id, WriteResult::Complete, 6)
        .unwrap();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 7)
        .unwrap();
    client.admit(admission(&events), true, 8).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
}

#[test]
fn rehandshake_queued_during_admission_stays_quiet_and_runs_after_admission() {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    let id = admission(&events);
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 6)
        .unwrap();
    client.admit(id, true, 7).unwrap();
    assert_eq!(client.phase(), SessionPhase::Quiet);
    assert!(client.next_write(7).unwrap().is_some());
}

#[test]
fn activation_rejection_precedence_and_ordered_goodbye_remain_suppressed() {
    for (activities, method, expected) in [
        (json!(["playback"]), None, "pairing_required"),
        (
            json!(["playback", "pairing"]),
            Some("static_pairing_code"),
            "unauthorized",
        ),
    ] {
        let (mut client, mut peer) = initial();
        let mut payload = json!({"activities":activities,"active_roles":["player@v1"]});
        if let Some(method) = method {
            payload["pairing"] = json!({"method":method});
        }
        let events = peer
            .send_json(&mut client, "server/activate", payload, 5)
            .unwrap();
        assert!(events.is_empty());
        assert_eq!(client.phase(), SessionPhase::Closing);
        assert!(
            client
                .send(ApplicationMessage::json("client/time", json!({})), 6)
                .is_err()
        );
        let write = client.next_write(6).unwrap().unwrap();
        assert_eq!(peer.read_json(&write.bytes)["payload"]["reason"], expected);
        assert_eq!(client.phase(), SessionPhase::Closing);
        client
            .complete_write(write.id, WriteResult::Complete, 7)
            .unwrap();
        assert_eq!(client.phase(), SessionPhase::Closed);
    }
}

#[test]
fn admission_rejection_never_applies_roles_and_closes_after_complete_goodbye() {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), false, 6).unwrap();
    assert!(client.roles().is_empty());
    assert_eq!(client.phase(), SessionPhase::Closing);
    let write = client.next_write(6).unwrap().unwrap();
    assert_eq!(peer.read_json(&write.bytes)["type"], "client/goodbye");
    client
        .complete_write(write.id, WriteResult::Complete, 6)
        .unwrap();
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn encrypted_unknown_ids_are_not_legacy_fragments_and_unknown_types_ignore_only_when_active() {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    for id in [2, 3, 250] {
        let record = peer.encrypt(&[id, 0, 1]);
        assert!(
            client
                .receive(TransportKind::Binary, &record, 7)
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        peer.send_json(&mut client, "client/future", json!({"future":42}), 8)
            .unwrap()
            .is_empty()
    );
    assert!(
        peer.send_json(
            &mut client,
            "client/time",
            json!({"client_transmitted":1}),
            9
        )
        .is_err()
    );
}

#[test]
fn discarded_unknown_fragments_must_authenticate_every_record() {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    let first = peer.encrypt(&[1, 2, 250, 8]);
    client.receive(TransportKind::Binary, &first, 7).unwrap();
    let mut last = peer.encrypt(&[1, 1, 9]);
    last[0] ^= 1;
    assert_eq!(
        client.receive(TransportKind::Binary, &last, 8).unwrap_err(),
        SessionError::Authentication
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn later_activation_cannot_revive_close_rejected_session() {
    let (mut client, mut peer) = initial();
    peer.send_json(
        &mut client,
        "server/activate",
        json!({"activities":["playback"],"active_roles":["player@v1"]}),
        5,
    )
    .unwrap();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 6)
        .unwrap();
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SessionEvent::Admission(_)))
    );
    assert_eq!(client.phase(), SessionPhase::Closing);
    let write = client.next_write(7).unwrap().unwrap();
    assert_eq!(peer.read_json(&write.bytes)["type"], "client/goodbye");
}

#[test]
fn delayed_admission_counts_its_retained_activation_against_pending_input_budget() {
    use earph_sendspin::pairing::RecordCatalog;
    let limits = SessionLimits {
        pending_input_bytes: 1024,
        ..SessionLimits::default()
    };
    let (mut client, mut peer) = initial_from(new_client_with_limits(
        RecordCatalog::new(5).unwrap(),
        limits,
    ));
    peer.send_json(
        &mut client,
        "server/activate",
        json!({"activities":[],"future":"a".repeat(500)}),
        5,
    )
    .unwrap();
    assert_eq!(
        peer.send_json(
            &mut client,
            "server/future",
            json!({"future":"b".repeat(500)}),
            6
        )
        .unwrap_err(),
        SessionError::Budget
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn queued_rotation_cannot_extend_the_initial_admission_deadline() {
    let (mut client, mut peer) = initial();
    peer.send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 29_999)
        .unwrap();
    assert_eq!(client.tick(30_000).unwrap_err(), SessionError::Deadline);
}

#[test]
fn debug_omits_wire_bodies_and_sensitive_role_names() {
    let notice = ActivationNotice {
        handshake_generation: 1,
        roles: vec!["_private_customer_role".into()],
        removed_roles: vec![],
        activities: vec!["playback".into()],
    };
    assert!(!format!("{notice:?}").contains("_private_customer_role"));
    let message = ApplicationMessage::json("client/state", json!({"private":"never-log-this"}));
    assert!(!format!("{message:?}").contains("never-log-this"));
    assert!(!format!("{:?}", SessionEvent::Unsent(message)).contains("never-log-this"));
}

#[test]
fn producer_budget_failure_returns_the_exact_unsent_data_and_closes() {
    use earph_sendspin::pairing::RecordCatalog;
    let limits = SessionLimits {
        pending_output_bytes: 1024,
        ..SessionLimits::default()
    };
    let (mut client, mut peer) = initial_from(new_client_with_limits(
        RecordCatalog::new(5).unwrap(),
        limits,
    ));
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    let message = ApplicationMessage {
        id: 12,
        body: vec![42; 1025],
    };
    let rejected = client.send(message, 7).unwrap_err();
    assert_eq!(rejected.error, SessionError::Budget);
    assert_eq!(rejected.message.body, vec![42; 1025]);
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn producer_reserves_its_first_record_before_taking_ownership() {
    use earph_sendspin::pairing::RecordCatalog;
    let limits = SessionLimits {
        pending_output_bytes: 1024,
        ..SessionLimits::default()
    };
    let (mut client, mut peer) = initial_from(new_client_with_limits(
        RecordCatalog::new(5).unwrap(),
        limits,
    ));
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    let rejected = client
        .send(
            ApplicationMessage {
                id: 12,
                body: vec![42; 800],
            },
            7,
        )
        .unwrap_err();
    assert_eq!(rejected.error, SessionError::Budget);
    assert_eq!(rejected.message.body, vec![42; 800]);
}
