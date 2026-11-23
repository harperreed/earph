// ABOUTME: Proves strict hash chaining, quiet writes, and new-key activation fencing.
// ABOUTME: Exercises real Snow epochs through the sole session owner.
#[path = "support/session_fixture.rs"]
mod fixture;
use earph_sendspin::{noise::SENTINEL, session::*};
use fixture::*;
use serde_json::json;

fn active() -> (ClientSession, Peer) {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    (client, peer)
}
fn rotate(client: &mut ClientSession, peer: &mut Peer, psk: [u8; 32], category: &str, now: u64) {
    let (handshake, payload) = peer.rotation(psk, category, None);
    peer.send_json(client, "noise/handshake", payload, now)
        .unwrap();
    assert_eq!(client.phase(), SessionPhase::Quiet);
    let write = client.next_write(now).unwrap().unwrap();
    let reply = peer.read_json(&write.bytes);
    assert_eq!(reply["type"], "noise/handshake");
    peer.install(handshake, &reply);
    client
        .complete_write(write.id, WriteResult::Complete, now)
        .unwrap();
    assert_eq!(client.phase(), SessionPhase::AwaitNewActivation);
    assert_eq!(client.handshake_hash(), Some(peer.hash));
}

#[test]
fn rotations_chain_immediate_hash_and_require_activation_each_time() {
    let (mut client, mut peer) = active();
    for (i, psk, category) in [(0, SENTINEL, "sn"), (1, [7; 32], "pr"), (2, [7; 32], "pr")] {
        let now = 10 + i * 5;
        rotate(&mut client, &mut peer, psk, category, now);
        let rejected = client
            .send(ApplicationMessage::json("client/goodbye", json!({})), now)
            .unwrap_err();
        assert_eq!(rejected.message.id, 0);
        let events = peer
            .send_json(
                &mut client,
                "server/activate",
                json!({"activities":[]}),
                now + 1,
            )
            .unwrap();
        client.admit(admission(&events), true, now + 2).unwrap();
        assert_eq!(client.handshake_generation(), 2 + i);
    }
}

#[test]
fn strict_lookup_miss_has_no_sentinel_reply() {
    for (psk, category) in [([99; 32], "lt"), ([7; 32], "lt"), (SENTINEL, "pr")] {
        let (mut client, mut peer) = active();
        let (_, payload) = peer.rotation(psk, category, None);
        assert!(
            peer.send_json(&mut client, "noise/handshake", payload, 10)
                .is_err()
        );
        assert_eq!(client.phase(), SessionPhase::Closed);
        assert!(client.next_write(11).unwrap().is_none());
    }
}

#[test]
fn new_epoch_rejects_every_nonactivation_even_unknown() {
    for kind in [
        "server/future",
        "server/hello",
        "noise/handshake",
        "server/time",
    ] {
        let (mut client, mut peer) = active();
        rotate(&mut client, &mut peer, [7; 32], "pr", 10);
        assert!(peer.send_json(&mut client, kind, json!({}), 11).is_err());
        assert_eq!(client.phase(), SessionPhase::Closed);
    }
}

#[test]
fn already_started_fragmented_send_drains_before_old_key_reply() {
    let (mut client, mut peer) = active();
    let body = vec![65; 140_000];
    client
        .send(
            ApplicationMessage {
                id: 12,
                body: body.clone(),
            },
            7,
        )
        .unwrap();
    let first = client.next_write(7).unwrap().unwrap();
    let plain = peer.read(&first.bytes);
    assert_eq!(&plain[..3], &[1, 2, 12]);
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 8)
        .unwrap();
    assert!(
        client
            .send(
                ApplicationMessage {
                    id: 12,
                    body: vec![1]
                },
                8
            )
            .is_err()
    );
    client
        .complete_write(first.id, WriteResult::Complete, 8)
        .unwrap();
    let mut reassembled = plain[3..].to_vec();
    loop {
        let write = client.next_write(8).unwrap().unwrap();
        let plain = peer.read(&write.bytes);
        assert_eq!(plain[0], 1);
        reassembled.extend_from_slice(&plain[2..]);
        let last = plain[1] & 1 != 0;
        client
            .complete_write(write.id, WriteResult::Complete, 8)
            .unwrap();
        if last {
            break;
        }
    }
    assert_eq!(reassembled, body);
    let reply = client.next_write(8).unwrap().unwrap();
    let plain = peer.read_json(&reply.bytes);
    peer.install(handshake, &plain);
    let early = peer.encrypt(b"\0{\"type\":\"server/activate\",\"payload\":{\"activities\":[]}}");
    assert!(
        client
            .receive(TransportKind::Binary, &early, fixture::stamp_ms(9))
            .unwrap()
            .is_empty()
    );
    let events = client
        .complete_write(reply.id, WriteResult::Complete, 9)
        .unwrap();
    client.admit(admission(&events), true, 9).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
}

#[test]
fn lost_activation_deadline_never_extends_and_uncertain_reply_cannot_resume() {
    let (mut client, mut peer) = active();
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    assert!(client.tick(30_010).is_err());
    let (mut client, mut peer) = active();
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 10)
        .unwrap();
    let write = client.next_write(10).unwrap().unwrap();
    assert!(
        client
            .complete_write(write.id, WriteResult::Uncertain, 11)
            .is_err()
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn unstarted_producer_is_returned_when_quiet_period_begins() {
    let (mut client, mut peer) = active();
    client
        .send(
            ApplicationMessage {
                id: 12,
                body: vec![5; 30],
            },
            7,
        )
        .unwrap();
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    let events = peer
        .send_json(&mut client, "noise/handshake", payload, 8)
        .unwrap();
    let unsent = events
        .into_iter()
        .find_map(|e| match e {
            SessionEvent::Unsent(message) => Some(message),
            _ => None,
        })
        .expect("producer ownership returned");
    assert_eq!(unsent.id, 12);
    assert_eq!(unsent.body, vec![5; 30]);
}

#[test]
fn bound_longterm_rotations_preserve_sticky_roles_and_remove_once_after_stream_end() {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let catalog = RecordCatalog::from_committed(
        5,
        vec![PairingRecord {
            server_public: earph_sendspin::noise::static_public_key(&[11; 32]).unwrap(),
            psk: [90; 32],
            last_used_order: 1,
        }],
        vec![],
    )
    .unwrap();
    let (mut client, mut peer) = initial_from(new_client_with_catalog(catalog));
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    rotate(&mut client, &mut peer, [90; 32], "lt", 10);
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["playback"],"active_roles":["player@v1"]}),
            11,
        )
        .unwrap();
    client.admit(admission(&events), true, 12).unwrap();
    let mut context = Box::new((vec![42; 32], json!({"codec":"pcm"}), vec![10u64]));
    let address = &*context as *const _;
    rotate(&mut client, &mut peer, [90; 32], "lt", 15);
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["playback"]}),
            16,
        )
        .unwrap();
    let events = client.admit(admission(&events), true, 17).unwrap();
    assert_eq!(client.roles(), ["player@v1"]);
    let notice = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::Activated(notice) => Some(notice),
            _ => None,
        })
        .unwrap();
    assert!(notice.removed_roles.is_empty());
    assert_eq!(&*context as *const _, address);
    assert_eq!(context.0, vec![42; 32]);
    let end = peer
        .send_json(&mut client, "stream/end", json!({"player":{}}), 18)
        .unwrap();
    assert!(end.iter().any(|e| matches!(e, SessionEvent::Message(_))));
    rotate(&mut client, &mut peer, SENTINEL, "sn", 20);
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 21)
        .unwrap();
    let effects = client.admit(admission(&events), true, 22).unwrap();
    let notice = effects
        .iter()
        .find_map(|e| match e {
            SessionEvent::Activated(notice) => Some(notice),
            _ => None,
        })
        .unwrap();
    assert_eq!(notice.removed_roles, ["player@v1"]);
    assert!(client.roles().is_empty());
    context.0.clear();
    assert!(context.0.is_empty());
    rotate(&mut client, &mut peer, [7; 32], "pr", 25);
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 26)
        .unwrap();
    client.admit(admission(&events), true, 27).unwrap();
}

#[test]
fn stale_prologue_and_wrong_new_tag_poison_without_epoch_retry() {
    let (mut client, mut peer) = active();
    let stale = peer.hash;
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 11)
        .unwrap();
    client.admit(admission(&events), true, 12).unwrap();
    let (_, payload) = peer.rotation([7; 32], "pr", Some(stale));
    assert!(
        peer.send_json(&mut client, "noise/handshake", payload, 15)
            .is_err()
    );
    let (mut client, mut peer) = active();
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    let mut bad = peer.encrypt(b"\0{\"type\":\"server/activate\",\"payload\":{\"activities\":[]}}");
    bad[0] ^= 1;
    assert_eq!(
        client
            .receive(TransportKind::Binary, &bad, fixture::stamp_ms(11))
            .unwrap_err(),
        SessionError::Authentication
    );
    assert!(client.next_write(12).unwrap().is_none());
}

#[test]
fn binding_mismatch_closes_silently_and_new_activation_can_be_fragmented() {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let catalog = RecordCatalog::from_committed(
        5,
        vec![PairingRecord {
            server_public: [88; 32],
            psk: [90; 32],
            last_used_order: 1,
        }],
        vec![],
    )
    .unwrap();
    let (mut client, mut peer) = initial_from(new_client_with_catalog(catalog));
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    let (_, payload) = peer.rotation([90; 32], "lt", None);
    assert!(
        peer.send_json(&mut client, "noise/handshake", payload, 10)
            .is_err()
    );
    assert!(client.next_write(11).unwrap().is_none());
    let (mut client, mut peer) = active();
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    let body = br#"{"type":"server/activate","payload":{"activities":[]}}"#;
    let first = peer.encrypt(&[&[1, 2, 0], &body[..20]].concat());
    assert!(
        client
            .receive(TransportKind::Binary, &first, fixture::stamp_ms(11))
            .unwrap()
            .is_empty()
    );
    let last = peer.encrypt(&[&[1, 1], &body[20..]].concat());
    let events = client
        .receive(TransportKind::Binary, &last, fixture::stamp_ms(12))
        .unwrap();
    client.admit(admission(&events), true, 13).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
}

#[test]
fn pairing_index_resets_only_after_completed_handshake_and_stale_candidate_cannot_cross() {
    use earph_sendspin::pairing::Candidate;
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            5,
        )
        .unwrap();
    let events = client.admit(admission(&events), true, 6).unwrap();
    let old = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::RequestCandidate(id) => Some(*id),
            _ => None,
        })
        .unwrap();
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            11,
        )
        .unwrap();
    let events = client.admit(admission(&events), true, 12).unwrap();
    let current = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::RequestCandidate(id) => Some(*id),
            _ => None,
        })
        .unwrap();
    assert_eq!(old.pairing_index, 1);
    assert_eq!(current.pairing_index, 1);
    assert_ne!(old.handshake_generation, current.handshake_generation);
    assert!(
        client
            .candidate(old, Candidate::new([88; 32]), 0)
            .unwrap()
            .is_empty()
    );
    client
        .candidate(current, Candidate::new([90; 32]), 13)
        .unwrap();
    let write = client.next_write(13).unwrap().unwrap();
    assert_eq!(peer.read_json(&write.bytes)["payload"]["pairing_index"], 1);
}

#[test]
fn longterm_to_pairing_reuses_permanent_psk_and_cancelled_rotation_never_resumes() {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let catalog = RecordCatalog::from_committed(
        5,
        vec![PairingRecord {
            server_public: earph_sendspin::noise::static_public_key(&[11; 32]).unwrap(),
            psk: [90; 32],
            last_used_order: 1,
        }],
        vec![],
    )
    .unwrap();
    let (mut client, mut peer) = initial_from(new_client_with_catalog(catalog));
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 5)
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    rotate(&mut client, &mut peer, [90; 32], "lt", 10);
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 11)
        .unwrap();
    client.admit(admission(&events), true, 12).unwrap();
    rotate(&mut client, &mut peer, [7; 32], "pr", 15);
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            16,
        )
        .unwrap();
    let events = client.admit(admission(&events), true, 17).unwrap();
    assert!(events.iter().any(|e|matches!(e,SessionEvent::RequestCandidate(id) if id.pairing_index==1&&id.handshake_generation==3)));
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 18)
        .unwrap();
    client.close();
    assert!(client.next_write(19).unwrap().is_none());
    assert!(
        peer.send_json(&mut client, "server/activate", json!({"activities":[]}), 20)
            .is_err()
    );
}

#[test]
fn unstarted_pairing_abort_does_not_begin_inside_the_quiet_period() {
    let (mut client, mut peer) = initial();
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            5,
        )
        .unwrap();
    client.admit(admission(&events), true, 6).unwrap();
    client.cancel_pairing(7).unwrap();
    let (_, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 8)
        .unwrap();
    let write = client.next_write(8).unwrap().unwrap();
    assert_eq!(peer.read_json(&write.bytes)["type"], "noise/handshake");
}

#[test]
fn unsupported_first_new_activation_aborts_then_admits_later_activation() {
    let (mut client, mut peer) = active();
    rotate(&mut client, &mut peer, [7; 32], "pr", 10);
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["pairing"],"pairing":{"method":"static_pairing_code"}}),
            11,
        )
        .unwrap();
    assert!(events.is_empty());
    let write = client.next_write(12).unwrap().unwrap();
    assert_eq!(
        peer.read_json(&write.bytes)["payload"]["reason"],
        "method_not_supported"
    );
    client
        .complete_write(write.id, WriteResult::Complete, 12)
        .unwrap();
    assert!(
        client
            .send(
                ApplicationMessage::json("client/goodbye", json!({"client_transmitted":1})),
                12
            )
            .is_err()
    );
    let events = peer
        .send_json(&mut client, "server/activate", json!({"activities":[]}), 13)
        .unwrap();
    client.admit(admission(&events), true, 14).unwrap();
    assert_eq!(client.phase(), SessionPhase::Active);
}

#[test]
fn captured_ciphertext_stamp_survives_rotation_flush_and_admission() {
    let (mut client, mut peer) = active();
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut client, "noise/handshake", payload, 8)
        .unwrap();
    let reply = client.next_write(8).unwrap().unwrap();
    let message = peer.read_json(&reply.bytes);
    peer.install(handshake, &message);
    let activation =
        peer.encrypt(b"\0{\"type\":\"server/activate\",\"payload\":{\"activities\":[]}}");
    assert!(
        client
            .receive(
                TransportKind::Binary,
                &activation,
                earph_sendspin::message::ReceiveStamp::new(9001).unwrap()
            )
            .unwrap()
            .is_empty()
    );
    let state = peer.encrypt(b"\0{\"type\":\"server/state\",\"payload\":{}}");
    assert!(
        client
            .receive(
                TransportKind::Binary,
                &state,
                earph_sendspin::message::ReceiveStamp::new(9037).unwrap()
            )
            .unwrap()
            .is_empty()
    );
    let events = client
        .complete_write(reply.id, WriteResult::Complete, 10)
        .unwrap();
    let events = client.admit(admission(&events), true, 11).unwrap();
    let message = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::Message(message) => Some(message),
            _ => None,
        })
        .unwrap();
    assert_eq!(message.arrival.microseconds(), 9037);
    assert_eq!(client.phase(), SessionPhase::Active);
}
