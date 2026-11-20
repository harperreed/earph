// ABOUTME: Proves durable pairing, deferred rotation, and acknowledged commit recovery.
// ABOUTME: Uses synthetic real filesystem storage behind the session's completion seam.
#[path = "support/session_fixture.rs"]
mod fixture;
#[path = "support/pairing_fixture.rs"]
mod storage;
use earph_sendspin::{pairing::*, session::*};
use fixture::*;
use serde_json::json;

fn begin(client: &mut ClientSession, peer: &mut Peer) -> AttemptId {
    let events = peer
        .send_json(
            client,
            "server/activate",
            json!({"activities":["pairing"],"active_roles":[],"pairing":{"method":"pairing_psk"}}),
            5,
        )
        .unwrap();
    let effects = client.admit(admission(&events), true, 6).unwrap();
    effects
        .iter()
        .find_map(|event| match event {
            SessionEvent::RequestCandidate(id) => Some(*id),
            _ => None,
        })
        .unwrap()
}
#[test]
fn acknowledged_commit_survives_close_and_rotating_cannot_commit_staging() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    let init = client.next_write(7).unwrap().unwrap();
    peer.read_json(&init.bytes);
    client
        .complete_write(init.id, WriteResult::Complete, 7)
        .unwrap();
    let finalize = client.next_write(8).unwrap().unwrap();
    peer.read_json(&finalize.bytes);
    client
        .complete_write(finalize.id, WriteResult::Complete, 8)
        .unwrap();
    let events = peer
        .send_json(&mut client, "server/pair-finalize", json!({}), 9)
        .unwrap();
    let update = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::CommitRecord(actual, update) => {
                assert_eq!(*actual, id);
                Some(update.clone())
            }
            _ => None,
        })
        .unwrap();
    client.close();
    let store = storage::FixtureStore::allocate("session-close-commit");
    store.commit(&update);
    client.commit_succeeded(id, 10).unwrap();
    assert_eq!(
        client.catalog(),
        &storage::FixtureStore::open(store.directory()).read()
    );
    assert_eq!(client.phase(), SessionPhase::Closed);
}

#[test]
fn commit_delays_rotation_but_send_suppression_starts_at_authenticated_message_one() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    for _ in 0..2 {
        let write = client.next_write(8).unwrap().unwrap();
        peer.read_json(&write.bytes);
        client
            .complete_write(write.id, WriteResult::Complete, 8)
            .unwrap();
    }
    let events = peer
        .send_json(&mut client, "server/pair-finalize", json!({}), 9)
        .unwrap();
    let update = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::CommitRecord(_, update) => Some(update.clone()),
            _ => None,
        })
        .unwrap();
    let (handshake, payload) = peer.rotation([90; 32], "lt", None);
    assert!(
        peer.send_json(&mut client, "noise/handshake", payload, 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        client
            .send(ApplicationMessage::json("client/time", json!({})), 11)
            .is_err()
    );
    assert!(client.next_write(11).unwrap().is_none());
    let store = storage::FixtureStore::allocate("session-deferred-rotation");
    store.commit(&update);
    client.commit_succeeded(id, 12).unwrap();
    let reply = client.next_write(12).unwrap().unwrap();
    let value = peer.read_json(&reply.bytes);
    peer.install(handshake, &value);
    client
        .complete_write(reply.id, WriteResult::Complete, 12)
        .unwrap();
    let events = peer
        .send_json(
            &mut client,
            "server/activate",
            json!({"activities":["playback"],"active_roles":["player@v1"]}),
            13,
        )
        .unwrap();
    let evidence = events
        .iter()
        .find_map(|e| match e {
            SessionEvent::Admission(proposal) => proposal.paired_identity(),
            _ => None,
        })
        .unwrap();
    assert_eq!(evidence.server_public(), peer.public);
    assert_eq!(evidence.handshake_generation(), 2);
    assert_eq!(evidence.credential_id(), psk_id(&[90; 32]));
    client.admit(admission(&events), true, 14).unwrap();
    assert_eq!(client.roles(), ["player@v1"]);
    assert_eq!(client.catalog(), &store.read());
}

#[test]
fn cancel_before_finalize_cannot_emit_secret_and_stale_owner_receipts_do_nothing() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    let init = client.next_write(7).unwrap().unwrap();
    assert_eq!(peer.read_json(&init.bytes)["type"], "client/pair-init");
    client
        .complete_write(init.id, WriteResult::Complete, 7)
        .unwrap();
    client.cancel_pairing(8).unwrap();
    let abort = client.next_write(8).unwrap().unwrap();
    assert_eq!(peer.read_json(&abort.bytes)["type"], "pair/abort");
    client
        .complete_write(abort.id, WriteResult::Complete, 8)
        .unwrap();
    let mut other = new_client();
    let foreign = other.next_write(0).unwrap().unwrap();
    assert!(
        client
            .complete_write(foreign.id, WriteResult::Uncertain, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(client.phase(), SessionPhase::Active);
    assert!(client.catalog().records().is_empty());
}

#[test]
fn stale_candidates_and_commit_callbacks_have_no_clock_authority() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    let stale = AttemptId {
        pairing_index: 99,
        ..id
    };
    assert!(
        client
            .candidate(stale, Candidate::new([90; 32]), 0)
            .unwrap()
            .is_empty()
    );
    assert!(client.commit_succeeded(stale, 0).unwrap().is_empty());
    assert_eq!(client.phase(), SessionPhase::Active);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    assert!(client.next_write(8).unwrap().is_some());
}

#[test]
fn commit_preserves_earlier_authenticated_message_order_before_deferred_rotation() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    for _ in 0..2 {
        let write = client.next_write(8).unwrap().unwrap();
        peer.read_json(&write.bytes);
        client
            .complete_write(write.id, WriteResult::Complete, 8)
            .unwrap();
    }
    peer.send_json(&mut client, "server/pair-finalize", json!({}), 9)
        .unwrap();
    assert!(
        peer.send_json(&mut client, "server/state", json!({}), 10)
            .unwrap()
            .is_empty()
    );
    let (_, payload) = peer.rotation([90; 32], "lt", None);
    peer.send_json(&mut client, "noise/handshake", payload, 11)
        .unwrap();
    assert!(
        client
            .send(ApplicationMessage::json("client/time", json!({})), 11)
            .is_err()
    );
    let events = client.commit_succeeded(id, 12).unwrap();
    let messages = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::Message(message) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].arrival_ms, 10);
    assert_eq!(client.phase(), SessionPhase::Quiet);
}

#[test]
fn activation_queued_before_rotation_cannot_release_the_later_quiet_period() {
    let (mut client, mut peer) = initial();
    let id = begin(&mut client, &mut peer);
    client.candidate(id, Candidate::new([90; 32]), 7).unwrap();
    for _ in 0..2 {
        let write = client.next_write(8).unwrap().unwrap();
        peer.read_json(&write.bytes);
        client
            .complete_write(write.id, WriteResult::Complete, 8)
            .unwrap();
    }
    peer.send_json(&mut client, "server/pair-finalize", json!({}), 9)
        .unwrap();
    peer.send_json(&mut client, "server/activate", json!({"activities":[]}), 10)
        .unwrap();
    let (_, payload) = peer.rotation([90; 32], "lt", None);
    peer.send_json(&mut client, "noise/handshake", payload, 11)
        .unwrap();
    let events = client.commit_succeeded(id, 12).unwrap();
    client.admit(admission(&events), true, 13).unwrap();
    assert_eq!(client.phase(), SessionPhase::Quiet);
    let reply = client.next_write(13).unwrap().unwrap();
    assert_eq!(peer.read_json(&reply.bytes)["type"], "noise/handshake");
}
