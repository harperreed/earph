// ABOUTME: Runs the bounded pairing unit through real Noise records and atomic fixture storage.
// ABOUTME: Covers restart and uncertain replacement with synthetic credentials and no sockets.
#[path = "support/pairing_fixture.rs"]
mod fixture;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::{noise::*, pairing::*};
use fixture::FixtureStore;
use serde_json::{Value, json};

fn record(server: u8) -> PairingRecord {
    PairingRecord {
        server_public: [server; 32],
        psk: [server + 30; 32],
        last_used_order: u64::from(server),
    }
}
fn context(server: [u8; 32]) -> PairingContext {
    PairingContext {
        server_public: server,
        selection: Selection {
            category: Category::Pairing,
            sentinel_fallback: false,
        },
        connection_generation: 1,
        handshake_generation: 1,
        pairing_psk: [7; 32],
    }
}
fn encode(value: &Value) -> Vec<u8> {
    let mut out = vec![0];
    out.extend(serde_json::to_vec(value).unwrap());
    out
}
fn decode(value: &[u8]) -> Value {
    assert_eq!(value[0], 0);
    serde_json::from_slice(&value[1..]).unwrap()
}
fn send(server: &mut snow::TransportState, value: &Value) -> Vec<u8> {
    let mut output = vec![0; 4096];
    let n = server.write_message(&encode(value), &mut output).unwrap();
    output.truncate(n);
    output
}
fn receive(server: &mut snow::TransportState, record: &[u8]) -> Value {
    let mut output = vec![0; 4096];
    let n = server.read_message(record, &mut output).unwrap();
    decode(&output[..n])
}
fn connected() -> (Responder, snow::TransportState, [u8; 32]) {
    let builder = snow::Builder::new(PATTERN.parse().unwrap());
    let client = builder.generate_keypair().unwrap();
    let server = builder.generate_keypair().unwrap();
    let client_init=serde_json::to_vec(&json!({"type":"client/init","payload":{"version":1,"suite":"25519_ChaChaPoly_SHA256","client_id":URL_SAFE_NO_PAD.encode(&client.public)}})).unwrap();
    let server_init=serde_json::to_vec(&json!({"type":"server/init","payload":{"version":1,"server_id":URL_SAFE_NO_PAD.encode(&server.public)}})).unwrap();
    let mut initiator = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&server.private)
        .unwrap()
        .remote_public_key(&client.public)
        .unwrap()
        .prologue(&[client_init.as_slice(), &server_init].concat())
        .unwrap()
        .psk(2, &[7; 32])
        .unwrap()
        .build_initiator()
        .unwrap();
    let mut responder = Responder::new(
        client.private.as_slice().try_into().unwrap(),
        &client_init,
        &server_init,
        Limits::default(),
    )
    .unwrap();
    let mut first = vec![0; 4096];
    let n = initiator
        .write_message(
            &serde_json::to_vec(
                &json!({"psk_id":URL_SAFE_NO_PAD.encode(psk_id(&[7;32])),"psk_category":"pr"}),
            )
            .unwrap(),
            &mut first,
        )
        .unwrap();
    let second = responder
        .receive_handshake(&first[..n], &[Credential::Pairing { psk: [7; 32] }])
        .unwrap();
    initiator.read_message(&second, &mut [0; 64]).unwrap();
    (
        responder,
        initiator.into_transport_mode().unwrap(),
        server.public.try_into().unwrap(),
    )
}

#[test]
fn five_records_survive_restart_and_the_sixth_replaces_the_unprotected_lru_atomically() {
    let store = FixtureStore::allocate("retention");
    for i in 1..=5 {
        store.commit(&store.read().plan_replace(record(i), &[]).unwrap());
    }
    let reopened = FixtureStore::open(store.directory());
    assert_eq!(reopened.read().records().len(), 5);
    let protected = [record(1).psk_id(), record(2).psk_id(), record(3).psk_id()];
    let sixth = reopened.read().plan_replace(record(6), &protected).unwrap();
    assert!(reopened.read().records().contains(&record(4)));
    let staged = reopened.stage(&sixth);
    assert!(staged.is_file());
    assert!(
        FixtureStore::open(store.directory())
            .read()
            .records()
            .contains(&record(4))
    );
    assert!(
        !FixtureStore::open(store.directory())
            .read()
            .records()
            .contains(&record(6))
    );
    reopened.commit(&sixth);
    assert_eq!(
        FixtureStore::open(store.directory()).read(),
        sixth.replacement
    );
    assert!(!reopened.read().records().contains(&record(4)));
    assert!(reopened.read().records().contains(&record(6)));
}

#[test]
fn real_noise_records_drive_the_ordered_flight_and_durable_ack_boundary() {
    let store = FixtureStore::allocate("noise");
    let (mut channel, mut server, server_public) = connected();
    let activation = json!({"type":"server/activate","payload":{"activities":["pairing"],"active_roles":[],"pairing":{"method":"pairing_psk"}}});
    let plain = channel.decrypt(&send(&mut server, &activation)).unwrap();
    assert_eq!(channel.phase(), Phase::Established);
    let mut client = PairingClient::new(context(server_public), store.read(), vec![]).unwrap();
    assert_eq!(client.selection(), channel.selection().unwrap());
    let effects = client.handle(
        PairingEvent::Activate(decode(&plain)["payload"].clone()),
        10,
    );
    let [PairingEffect::RequestCandidate(id)] = effects.as_slice() else {
        panic!("missing request")
    };
    let id = *id;
    let effects = client.handle(
        PairingEvent::CandidateReady(id, Candidate::new([90; 32])),
        11,
    );
    let [PairingEffect::SendFlight(_, flight)] = effects.as_slice() else {
        panic!("missing flight")
    };
    let messages = flight.messages();
    for (index, expected) in messages.iter().enumerate() {
        if index == 1 {
            client.handle(PairingEvent::PairingFinalizeStarted(id), 12);
        }
        let decoded = receive(&mut server, &channel.encrypt(&encode(expected)).unwrap());
        assert_eq!(&decoded, expected);
        if index == 0 {
            client.handle(PairingEvent::PairingInitSent(id, 12), 12);
        }
    }
    assert_eq!(messages[0]["type"], "client/pair-init");
    assert_eq!(messages[1]["type"], "client/pair-finalize");
    client.handle(PairingEvent::PairingFlightSent(id, 12), 13);
    assert!(store.read().records().is_empty());
    let finalize = json!({"type":"server/pair-finalize","payload":{"future":42}});
    let plain = channel.decrypt(&send(&mut server, &finalize)).unwrap();
    let effects = client.handle(
        PairingEvent::ServerFinalize(decode(&plain)["payload"].clone()),
        14,
    );
    let [PairingEffect::CommitRecord(actual, update)] = effects.as_slice() else {
        panic!("missing commit")
    };
    assert_eq!(*actual, id);
    assert!(client.catalog().records().is_empty());
    store.commit(update);
    client.handle(PairingEvent::CommitSucceeded(id), 15);
    assert_eq!(
        client.catalog(),
        &FixtureStore::open(store.directory()).read()
    );
    assert_eq!(client.phase(), PairingPhase::RecordStored);
    assert!(!client.is_paired());
    assert_eq!(channel.selection().unwrap().category, Category::Pairing);
    assert_eq!(client.catalog().records()[0].server_public, server_public);
}

#[test]
fn restarts_before_ack_never_restore_staging_and_uncertain_replacement_requires_readback() {
    for boundary in [
        "candidate",
        "flight",
        "sent",
        "ack",
        "replaced",
        "committed",
    ] {
        let store = FixtureStore::allocate(boundary);
        store.commit(&store.read().plan_replace(record(1), &[]).unwrap());
        let old = store.read();
        let mut client = PairingClient::new(context([20; 32]), old.clone(), vec![]).unwrap();
        let effects = client.handle(
            PairingEvent::Activate(
                json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}}),
            ),
            0,
        );
        let [PairingEffect::RequestCandidate(id)] = effects.as_slice() else {
            panic!("missing request")
        };
        let id = *id;
        if boundary != "candidate" {
            client.handle(
                PairingEvent::CandidateReady(id, Candidate::new([90; 32])),
                1,
            );
            if boundary != "flight" {
                client.handle(PairingEvent::PairingInitSent(id, 2), 2);
                client.handle(PairingEvent::PairingFinalizeStarted(id), 2);
                client.handle(PairingEvent::PairingFlightSent(id, 2), 2);
                if boundary != "sent" {
                    let effects = client.handle(PairingEvent::ServerFinalize(json!({})), 3);
                    let [PairingEffect::CommitRecord(_, update)] = effects.as_slice() else {
                        panic!("missing commit")
                    };
                    if boundary == "replaced" {
                        store.commit_with_boundary(update, true);
                        client.handle(PairingEvent::CommitFailed(id, CommitOutcome::Uncertain), 4);
                        assert!(client.recovery_required());
                        assert_eq!(client.catalog(), &old);
                    } else if boundary == "committed" {
                        store.commit(update);
                        client.handle(PairingEvent::CommitSucceeded(id), 4);
                    }
                }
            }
        }
        drop(client);
        let recovered = FixtureStore::open(store.directory()).read();
        if ["replaced", "committed"].contains(&boundary) {
            assert_eq!(recovered.records().len(), 2);
        } else {
            assert_eq!(recovered, old);
        }
        let restarted = PairingClient::new(context([20; 32]), recovered, vec![]).unwrap();
        assert_eq!(restarted.phase(), PairingPhase::Idle);
        assert!(!restarted.is_paired());
    }
}
