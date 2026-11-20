// ABOUTME: Reproduces revoked pairing flights and old-epoch timeout effects through real Snow records.
// ABOUTME: Distinguishes unstarted messages, permitted fragment drain, and uncertain transport writes.
#[path = "support/session_fixture.rs"]
mod fixture;
use earph_sendspin::{
    message::{MessageLimits, Reassembler},
    pairing::{AttemptId, Candidate, RecordCatalog},
    session::*,
};
use fixture::*;
use serde_json::{Value, json};

fn pairing(record_bytes: usize) -> (ClientSession, Peer, AttemptId) {
    let limits = SessionLimits {
        messages: MessageLimits {
            record_plaintext_bytes: record_bytes,
            ..MessageLimits::default()
        },
        ..SessionLimits::default()
    };
    let (mut client, mut peer) = initial_from_with_record_limit(
        new_client_with_limits(RecordCatalog::new(5).unwrap(), limits),
        record_bytes,
    );
    let events = peer
        .send_json_fragmented(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            5,
            record_bytes,
        )
        .unwrap();
    let events = client.admit(admission(&events), true, 6).unwrap();
    let id = events
        .into_iter()
        .find_map(|event| match event {
            SessionEvent::RequestCandidate(id) => Some(id),
            _ => None,
        })
        .unwrap();
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    (client, peer, id)
}
fn read_json(client: &mut ClientSession, peer: &mut Peer, now: u64) -> Option<Value> {
    let first = client.next_write(now).unwrap()?;
    let mut reader = Reassembler::new(MessageLimits::default());
    let plain = peer.read(&first.bytes);
    let message = reader.push(&plain, now).unwrap();
    client
        .complete_write(first.id, WriteResult::Complete, now)
        .unwrap();
    let complete = if let Some(message) = message {
        message
    } else {
        finish_fragments(client, peer, reader, now)
    };
    assert_eq!(complete.id, 0);
    Some(serde_json::from_slice(&complete.body).unwrap())
}
fn finish_fragments(
    client: &mut ClientSession,
    peer: &mut Peer,
    mut reader: Reassembler,
    now: u64,
) -> earph_sendspin::message::Complete {
    loop {
        let write = client
            .next_write(now)
            .unwrap()
            .expect("started message must drain");
        let plain = peer.read(&write.bytes);
        let complete = reader.push(&plain, now).unwrap();
        client
            .complete_write(write.id, WriteResult::Complete, now)
            .unwrap();
        if let Some(complete) = complete {
            return complete;
        }
    }
}
fn init_sent(client: &mut ClientSession, peer: &mut Peer) {
    assert_eq!(
        read_json(client, peer, 7).unwrap()["type"],
        "client/pair-init"
    );
}

#[test]
fn expired_unstarted_finalize_is_removed_before_the_timeout_abort() {
    let (mut client, mut peer, _) = pairing(65519);
    init_sent(&mut client, &mut peer);
    client.tick(120_007).unwrap();
    let message = read_json(&mut client, &mut peer, 120_007).unwrap();
    assert_eq!(message["type"], "pair/abort");
    assert_eq!(message["payload"]["reason"], "attempt_timeout");
    assert!(read_json(&mut client, &mut peer, 120_007).is_none());
    assert!(
        peer.send_json(&mut client, "server/pair-finalize", json!({}), 120_008)
            .unwrap()
            .is_empty()
    );
    assert!(client.catalog().records().is_empty());
}
#[test]
fn requesting_finalize_at_deadline_processes_abort_without_a_prior_tick() {
    let (mut client, mut peer, _) = pairing(65519);
    init_sent(&mut client, &mut peer);
    let message = read_json(&mut client, &mut peer, 120_007).unwrap();
    assert_eq!(message["type"], "pair/abort");
    assert_eq!(message["payload"]["reason"], "attempt_timeout");
    assert!(read_json(&mut client, &mut peer, 120_007).is_none());
    assert!(client.catalog().records().is_empty());
}
#[test]
fn pending_activation_revokes_unstarted_init_and_finalize() {
    for sent_init in [false, true] {
        let (mut client, mut peer, id) = pairing(65519);
        if sent_init {
            init_sent(&mut client, &mut peer);
        }
        let events = peer
            .send_json(&mut client, "server/activate", json!({"activities":[]}), 8)
            .unwrap();
        assert!(client.next_write(8).unwrap().is_none());
        assert!(
            client
                .candidate(id, Candidate::new([91; 32]), 0)
                .unwrap()
                .is_empty()
        );
        client.admit(admission(&events), true, 9).unwrap();
        assert!(read_json(&mut client, &mut peer, 9).is_none());
        assert!(client.catalog().records().is_empty());
    }
}
#[test]
fn rejected_activation_never_writes_an_old_flight_before_goodbye() {
    for sent_init in [false, true] {
        let (mut client, mut peer, _) = pairing(65519);
        if sent_init {
            init_sent(&mut client, &mut peer);
        }
        let events = peer
            .send_json(&mut client, "server/activate", json!({"activities":[]}), 8)
            .unwrap();
        client.admit(admission(&events), false, 9).unwrap();
        assert_eq!(
            read_json(&mut client, &mut peer, 9).unwrap()["type"],
            "client/goodbye"
        );
        assert_eq!(client.phase(), SessionPhase::Closed);
        assert!(client.catalog().records().is_empty());
    }
}
#[test]
fn admitted_replacement_pairing_can_write_only_the_current_candidate() {
    for sent_init in [false, true] {
        let (mut client, mut peer, old) = pairing(65519);
        if sent_init {
            init_sent(&mut client, &mut peer);
        }
        let events = peer
            .send_json(
                &mut client,
                "server/activate",
                json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
                8,
            )
            .unwrap();
        let events = client.admit(admission(&events), true, 9).unwrap();
        let current = events
            .into_iter()
            .find_map(|e| match e {
                SessionEvent::RequestCandidate(id) => Some(id),
                _ => None,
            })
            .unwrap();
        assert_eq!(current.pairing_index, 2);
        assert!(
            client
                .candidate(old, Candidate::new([92; 32]), 0)
                .unwrap()
                .is_empty()
        );
        client
            .candidate(current, Candidate::new([91; 32]), 10)
            .unwrap();
        let init = read_json(&mut client, &mut peer, 10).unwrap();
        assert_eq!(init["type"], "client/pair-init");
        assert_eq!(init["payload"]["pairing_index"], 2);
        let finalize = read_json(&mut client, &mut peer, 10).unwrap();
        assert_eq!(finalize["type"], "client/pair-finalize");
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let psk = URL_SAFE_NO_PAD
            .decode(finalize["payload"]["long_term_psk"].as_str().unwrap())
            .unwrap();
        assert!(
            psk == [91; 32],
            "only the current synthetic candidate may be emitted"
        );
        assert!(client.catalog().records().is_empty());
    }
}
#[test]
fn already_started_pair_init_drains_without_starting_its_revoked_finalize() {
    let (mut client, mut peer, _) = pairing(40);
    let first = client.next_write(7).unwrap().unwrap();
    let plain = peer.read(&first.bytes);
    assert_eq!(&plain[..3], &[1, 2, 0]);
    let mut reader = Reassembler::new(MessageLimits::default());
    assert!(reader.push(&plain, 7).unwrap().is_none());
    let events = peer
        .send_json_fragmented(
            &mut client,
            "server/activate",
            json!({"activities":[]}),
            8,
            40,
        )
        .unwrap();
    client.admit(admission(&events), true, 9).unwrap();
    client
        .complete_write(first.id, WriteResult::Complete, 9)
        .unwrap();
    let complete = finish_fragments(&mut client, &mut peer, reader, 9);
    let value: Value = serde_json::from_slice(&complete.body).unwrap();
    assert_eq!(value["type"], "client/pair-init");
    assert!(client.next_write(9).unwrap().is_none());
    assert!(client.catalog().records().is_empty());
}
#[test]
fn already_started_finalize_drains_but_its_late_ack_cannot_commit() {
    let (mut client, mut peer, _) = pairing(64);
    init_sent(&mut client, &mut peer);
    let first = client.next_write(8).unwrap().unwrap();
    let plain = peer.read(&first.bytes);
    assert_eq!(&plain[..3], &[1, 2, 0]);
    let mut reader = Reassembler::new(MessageLimits::default());
    assert!(reader.push(&plain, 8).unwrap().is_none());
    let events = peer
        .send_json_fragmented(
            &mut client,
            "server/activate",
            json!({"activities":[]}),
            9,
            64,
        )
        .unwrap();
    assert!(events.is_empty());
    let events = client
        .complete_write(first.id, WriteResult::Complete, 10)
        .unwrap();
    client.admit(admission(&events), true, 11).unwrap();
    let complete = finish_fragments(&mut client, &mut peer, reader, 11);
    let value: Value = serde_json::from_slice(&complete.body).unwrap();
    assert_eq!(value["type"], "client/pair-finalize");
    assert!(client.next_write(11).unwrap().is_none());
    assert!(
        peer.send_json_fragmented(&mut client, "server/pair-finalize", json!({}), 12, 64)
            .unwrap()
            .is_empty()
    );
    assert!(client.catalog().records().is_empty());
}
#[test]
fn uncertainty_in_a_started_finalize_poisons_the_transport() {
    let (mut client, mut peer, _) = pairing(64);
    init_sent(&mut client, &mut peer);
    let first = client.next_write(8).unwrap().unwrap();
    peer.read(&first.bytes);
    let events = peer
        .send_json_fragmented(
            &mut client,
            "server/activate",
            json!({"activities":[]}),
            9,
            64,
        )
        .unwrap();
    assert!(events.is_empty());
    assert_eq!(
        client
            .complete_write(first.id, WriteResult::Uncertain, 10)
            .unwrap_err(),
        SessionError::Write
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
    assert!(client.next_write(10).unwrap().is_none());
    assert!(client.catalog().records().is_empty());
}
#[test]
fn timeout_during_started_finalize_drains_before_abort_without_committing() {
    let (mut client, mut peer, _) = pairing(64);
    init_sent(&mut client, &mut peer);
    let first = client.next_write(120_006).unwrap().unwrap();
    let plain = peer.read(&first.bytes);
    let mut reader = Reassembler::new(MessageLimits::default());
    assert!(reader.push(&plain, 120_006).unwrap().is_none());
    client.tick(120_007).unwrap();
    client
        .complete_write(first.id, WriteResult::Complete, 120_007)
        .unwrap();
    let complete = finish_fragments(&mut client, &mut peer, reader, 120_007);
    let value: Value = serde_json::from_slice(&complete.body).unwrap();
    assert_eq!(value["type"], "client/pair-finalize");
    assert_eq!(
        read_json(&mut client, &mut peer, 120_007).unwrap()["payload"]["reason"],
        "attempt_timeout"
    );
    assert!(client.catalog().records().is_empty());
}
fn rotation_crossing(reply_outstanding: bool) {
    let (mut client, mut peer, _) = pairing(65519);
    init_sent(&mut client, &mut peer);
    assert_eq!(
        read_json(&mut client, &mut peer, 7).unwrap()["type"],
        "client/pair-finalize"
    );
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 120_006)
        .unwrap();
    let write = if reply_outstanding {
        let write = client.next_write(120_006).unwrap().unwrap();
        client.tick(120_007).unwrap();
        write
    } else {
        client.tick(120_007).unwrap();
        client.next_write(120_007).unwrap().unwrap()
    };
    let reply = peer.read_json(&write.bytes);
    assert_eq!(reply["type"], "noise/handshake");
    peer.install(handshake, &reply);
    client
        .complete_write(write.id, WriteResult::Complete, 120_007)
        .unwrap();
    assert!(client.next_write(120_007).unwrap().is_none());
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":[]}),
            120_008,
        )
        .unwrap();
    client.admit(admission(&events), true, 120_009).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
    assert!(client.next_write(120_009).unwrap().is_none());
    assert!(client.catalog().records().is_empty());
    client
        .send(
            ApplicationMessage::json("client/time", json!({"client_transmitted":42})),
            120_010,
        )
        .unwrap();
    assert_eq!(
        read_json(&mut client, &mut peer, 120_010).unwrap()["payload"]["client_transmitted"],
        42
    );
}
#[test]
fn old_attempt_deadline_before_reply_cannot_create_quiet_period_abort() {
    rotation_crossing(false);
}
#[test]
fn old_attempt_deadline_during_reply_cannot_cross_into_new_keys() {
    rotation_crossing(true);
}

#[test]
fn authenticated_ack_before_message_one_survives_delayed_local_write_completion() {
    let (mut client, mut peer, id) = pairing(65519);
    init_sent(&mut client, &mut peer);
    let finalize = client.next_write(7).unwrap().unwrap();
    assert_eq!(
        peer.read_json(&finalize.bytes)["type"],
        "client/pair-finalize"
    );
    assert!(
        peer.send_json(&mut client, "server/pair-finalize", json!({}), 8)
            .unwrap()
            .is_empty()
    );
    let (handshake, payload) = peer.rotation([90; 32], "lt", None);
    peer.send_json(&mut client, "noise/handshake", payload, 9)
        .unwrap();
    let events = client
        .complete_write(finalize.id, WriteResult::Complete, 10)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event,SessionEvent::CommitRecord(actual,_) if *actual==id))
    );
    assert!(client.next_write(10).unwrap().is_none());
    client.commit_succeeded(id, 11).unwrap();
    let reply = read_json(&mut client, &mut peer, 11).unwrap();
    assert_eq!(reply["type"], "noise/handshake");
    peer.install(handshake, &reply);
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 12)
        .unwrap();
    client.admit(admission(&events), true, 13).unwrap();
    assert_eq!(client.catalog().records().len(), 1);
    assert_eq!(client.phase(), SessionPhase::Active);
}
#[test]
fn old_activation_method_abort_cannot_follow_the_rotation_reply() {
    let (mut client, mut peer, id) = pairing(65519);
    init_sent(&mut client, &mut peer);
    read_json(&mut client, &mut peer, 7).unwrap();
    let events = peer
        .send_json(&mut client, "server/pair-finalize", json!({}), 8)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, SessionEvent::CommitRecord(..)))
    );
    peer.send_json(
        &mut client,
        "server/activate",
        json!({"activities":["pairing"],"pairing":{"method":"static_pairing_code"}}),
        9,
    )
    .unwrap();
    let (handshake, payload) = peer.rotation([90; 32], "lt", None);
    peer.send_json(&mut client, "noise/handshake", payload, 10)
        .unwrap();
    client.commit_succeeded(id, 11).unwrap();
    let reply = read_json(&mut client, &mut peer, 11).unwrap();
    assert_eq!(reply["type"], "noise/handshake");
    peer.install(handshake, &reply);
    assert!(client.next_write(11).unwrap().is_none());
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 12)
        .unwrap();
    client.admit(admission(&events), true, 13).unwrap();
    assert!(client.next_write(13).unwrap().is_none());
    assert_eq!(client.phase(), SessionPhase::Active);
}
#[test]
fn started_old_finalize_drains_before_reply_without_reviving_its_timeout() {
    let (mut client, mut peer, _) = pairing(64);
    init_sent(&mut client, &mut peer);
    let first = client.next_write(120_006).unwrap().unwrap();
    let plain = peer.read(&first.bytes);
    let mut reader = Reassembler::new(MessageLimits::default());
    assert!(reader.push(&plain, 120_006).unwrap().is_none());
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json_fragmented(&mut client, "noise/handshake", payload, 120_006, 64)
        .unwrap();
    client.tick(120_007).unwrap();
    client
        .complete_write(first.id, WriteResult::Complete, 120_007)
        .unwrap();
    let complete = finish_fragments(&mut client, &mut peer, reader, 120_007);
    let finalize: Value = serde_json::from_slice(&complete.body).unwrap();
    assert_eq!(finalize["type"], "client/pair-finalize");
    let reply = read_json(&mut client, &mut peer, 120_007).unwrap();
    assert_eq!(reply["type"], "noise/handshake");
    peer.install(handshake, &reply);
    assert!(client.next_write(120_007).unwrap().is_none());
    let events = peer
        .send_json_fragmented(
            &mut client,
            "server/activate",
            json!({"activities":[]}),
            120_008,
            64,
        )
        .unwrap();
    client.admit(admission(&events), true, 120_009).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
    assert!(client.catalog().records().is_empty());
}
