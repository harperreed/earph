// ABOUTME: Runs full pinned driver promotion, two rotations, and authenticated old-key discard.
// ABOUTME: Keeps durable synthetic storage and caller stream/clock context outside the session.
#[path = "support/session_bridge.rs"]
mod bridge;
#[path = "support/pairing_fixture.rs"]
mod fixture;
use bridge::*;
use earph_sendspin::{noise, pairing::Candidate, session::*};
use fixture::FixtureStore;
use serde_json::{Value, json};
use std::collections::VecDeque;

fn peer_scenario(mode: &str) {
    let store = FixtureStore::allocate(mode);
    let mut peer = Bridge::new();
    peer.send(json!({"mode":mode}));
    let mut client=ClientSession::new(SessionConfig{private:noise::generate_static_private_key().unwrap(),pairing_psk:[7;32],connection_generation:1,catalog:store.read(),protected:vec![],hello:json!({"name":"Earph","supported_roles":["player@v1","_fixture@v1"],"player@v1_support":{"supported_formats":[{"codec":"pcm","channels":2,"sample_rate":48000,"bit_depth":16}],"buffer_capacity":1_000_000}}),limits:SessionLimits::default()},0).unwrap();
    let mut now = 1;
    let mut events = VecDeque::new();
    let mut committed = None;
    let mut clock_context = Box::new(vec![19u64]);
    let clock_address = clock_context.as_ptr();
    let mut stream = None;
    let mut audio = None;
    let mut old_queued = false;
    let mut corrupt_next = false;
    let mut rotations = 0;
    let mut await_m1 = false;
    let mut fault_sent = false;
    let mut preserved_arrival = None;
    loop {
        while let Some(event) = events.pop_front() {
            match event {
                SessionEvent::Admission(proposal) => {
                    events.extend(client.admit(proposal.id(), true, now).unwrap())
                }
                SessionEvent::RequestCandidate(id) => {
                    events.extend(client.candidate(id, Candidate::new([90; 32]), now).unwrap())
                }
                SessionEvent::CommitRecord(id, update) => {
                    store.commit(&update);
                    assert_eq!(
                        FixtureStore::open(store.directory()).read(),
                        update.replacement
                    );
                    committed = Some(id);
                    events.extend(client.commit_succeeded(id, now).unwrap());
                }
                SessionEvent::Message(message) => {
                    if message.id == 4 {
                        assert_eq!(message.body.len(), 70_012);
                        assert_eq!(&message.body[12..], vec![42; 70_000]);
                        preserved_arrival = Some(message.arrival_ms);
                        audio = Some(message);
                    } else if message.id == 0 {
                        let value: Value = serde_json::from_slice(&message.body).unwrap();
                        if value["type"] == "stream/start" {
                            stream = Some(value);
                        }
                    }
                }
                SessionEvent::Unsent(_) => panic!("unexpected unstarted producer"),
                SessionEvent::Activated(_) => {}
                SessionEvent::Finishing(_) => {}
                SessionEvent::Closed => panic!("session closed"),
            }
        }
        while !fault_sent {
            let Some(write) = client.next_write(now).unwrap() else {
                break;
            };
            let mut bytes = write.bytes;
            if corrupt_next {
                bytes[0] ^= 1;
                corrupt_next = false;
                fault_sent = true;
            }
            peer.send(json!({"wire":{"kind":if write.kind==TransportKind::Text{"text"}else{"binary"},"data":if write.kind==TransportKind::Text{String::from_utf8(bytes).unwrap()}else{hex(&bytes)}}}));
            events.extend(
                client
                    .complete_write(write.id, WriteResult::Complete, now)
                    .unwrap(),
            );
            if await_m1 {
                break;
            }
        }
        let response = peer.read();
        now += 1;
        if let Some(wire) = response.get("wire") {
            let kind = if wire["kind"] == "text" {
                TransportKind::Text
            } else {
                TransportKind::Binary
            };
            let bytes = if kind == TransportKind::Text {
                wire["data"].as_str().unwrap().as_bytes().to_vec()
            } else {
                unhex(wire["data"].as_str().unwrap())
            };
            events.extend(client.receive(kind, &bytes, now).unwrap());
            await_m1 = false;
            continue;
        }
        match response["stage"].as_str().unwrap() {
            "paired" => {
                assert!(committed.is_some());
                assert_eq!(response["source_hashes"].as_object().unwrap().len(), 9);
                assert_eq!(
                    unhex(response["hash"].as_str().unwrap()),
                    client.handshake_hash().unwrap()
                );
                peer.send(json!({"committed":true}));
            }
            "rotated" => {
                rotations += 1;
                assert_eq!(
                    unhex(response["hash"].as_str().unwrap()),
                    client.handshake_hash().unwrap()
                );
                client
                    .send(
                        ApplicationMessage::json(
                            "client/time",
                            json!({"client_transmitted":rotations-1}),
                        ),
                        now,
                    )
                    .unwrap();
                assert_eq!(client.roles(), ["player@v1", "_fixture@v1"]);
                if rotations == 2 {
                    assert!(stream.is_some());
                    assert_eq!(
                        audio.as_ref().unwrap().arrival_ms,
                        preserved_arrival.unwrap()
                    );
                }
            }
            "queue_old" => {
                old_queued = true;
                await_m1 = true;
                assert!(stream.is_some());
                assert!(audio.is_some());
                peer.send(json!({"old_started":true}));
                client
                    .send(
                        ApplicationMessage::json(
                            "client/state",
                            json!({"_fixture": {"value":"a".repeat(140_000)}}),
                        ),
                        now,
                    )
                    .unwrap();
                corrupt_next = mode == "corrupt_old";
            }
            "corrupt_old_rejected" => {
                assert_eq!(mode, "corrupt_old");
                assert!(old_queued);
                client.close();
                break;
            }
            "done" => {
                assert_eq!(mode, "success");
                assert_eq!(rotations, 2);
                assert!(old_queued);
                assert_eq!(client.handshake_generation(), 3);
                assert_eq!(clock_context.as_ptr(), clock_address);
                assert_eq!(*clock_context, vec![19]);
                clock_context.push(20);
                break;
            }
            _ => panic!("unknown fixture stage"),
        }
    }
    peer.finish();
}
#[test]
#[ignore = "requires explicitly pinned offline Python driver"]
fn pinned_driver_pairing_promotion_rotations_and_context_preservation() {
    peer_scenario("success");
}
#[test]
#[ignore = "requires explicitly pinned offline Python driver"]
fn pinned_server_rejects_corrupt_old_key_traffic_instead_of_discarding_unauthenticated() {
    peer_scenario("corrupt_old");
}
