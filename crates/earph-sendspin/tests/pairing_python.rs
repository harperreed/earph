// ABOUTME: Qualifies actual pinned Python token and Pairing PSK producer and consumer calls.
// ABOUTME: Connects real Noise sessions through pipes and commits only synthetic filesystem fixtures.
#[path = "support/pairing_fixture.rs"]
mod fixture;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::{noise::*, pairing::*, pairing_token::*};
use fixture::FixtureStore;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

struct Peer {
    child: std::process::Child,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
}
impl Peer {
    fn new() -> Self {
        let launcher =
            std::env::var_os("EARPH_SENDSPIN_PEER").expect("set the pinned uv offline launcher");
        let mut child = Command::new(launcher)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/pairing_peer.py"
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            input,
            output,
        }
    }
    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }
    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }
    fn finish(&mut self) {
        assert!(self.child.wait().unwrap().success());
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn unhex(value: &Value) -> Vec<u8> {
    let (pairs, remainder) = value.as_str().unwrap().as_bytes().as_chunks::<2>();
    assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|bytes| u8::from_str_radix(std::str::from_utf8(bytes).unwrap(), 16).unwrap())
        .collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn wire(value: &Value) -> Vec<u8> {
    let mut plain = vec![0];
    plain.extend(serde_json::to_vec(value).unwrap());
    plain
}
fn parse(plain: &[u8]) -> Value {
    assert_eq!(plain[0], 0);
    serde_json::from_slice(&plain[1..]).unwrap()
}

#[test]
#[ignore = "requires the explicitly pinned offline Python peer launcher"]
fn real_python_token_producer_and_consumer_match_both_directions() {
    let token = PairingToken {
        client_public: std::array::from_fn(|i| i as u8),
        pairing_psk: std::array::from_fn(|i| 224 + i as u8),
    };
    let mut peer = Peer::new();
    peer.send(json!({"mode":"tokens","rust_token":encode_pairing_token(&token)}));
    let response = peer.read();
    peer.finish();
    let decoded = decode_pairing_token(response["token"].as_str().unwrap()).unwrap();
    assert_eq!(decoded, token);
    assert_eq!(unhex(&response["client_public"]), token.client_public);
    assert_eq!(unhex(&response["psk"]), token.pairing_psk);
    // The server must compare this binding before starting an attempt.
    assert_eq!(decoded.client_public, token.client_public);
    assert_ne!(decoded.client_public, [99; 32]);
    assert_eq!(
        response["provenance"]["source_hashes"]
            .as_object()
            .unwrap()
            .len(),
        4
    );
}

#[test]
#[ignore = "requires the explicitly pinned offline Python peer launcher"]
fn real_python_pairing_client_produces_indexed_immediate_flight_and_stores_after_ack() {
    let mut peer = Peer::new();
    peer.send(json!({"mode":"client_reference"}));
    let result = peer.read();
    peer.finish();
    assert_eq!(
        result["flight"][0],
        json!({"type":"client/pair-init","payload":{"pairing_index":9}})
    );
    assert_eq!(result["flight"][1]["type"], "client/pair-finalize");
    let payload = result["flight"][1]["payload"].as_object().unwrap();
    assert_eq!(payload.len(), 1);
    let psk = URL_SAFE_NO_PAD
        .decode(payload["long_term_psk"].as_str().unwrap())
        .unwrap();
    assert_eq!(psk.len(), 32);
    assert_eq!(psk, unhex(&result["stored_psk"]));
    assert_eq!(
        result["events"],
        json!([
            "client/pair-init",
            "on_finalize",
            "client/pair-finalize",
            "server/pair-finalize"
        ])
    );
}

#[test]
#[ignore = "requires the explicitly pinned offline Python peer launcher"]
fn real_python_consumer_stores_rust_flight_before_ack_and_lost_ack_cannot_commit() {
    for mode in ["server_consumer", "lost_ack"] {
        let store = FixtureStore::allocate(mode);
        let mut peer = Peer::new();
        let private = generate_static_private_key().unwrap();
        let public = static_public_key(&private).unwrap();
        let init=serde_json::to_string(&json!({"type":"client/init","payload":{"version":1,"suite":"25519_ChaChaPoly_SHA256","client_id":URL_SAFE_NO_PAD.encode(public)}})).unwrap();
        peer.send(
            json!({"mode":mode,"client_public":URL_SAFE_NO_PAD.encode(public),"client_init":init}),
        );
        let first = peer.read();
        let server_init = first["server_init"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(server_init).unwrap();
        let server_public = URL_SAFE_NO_PAD
            .decode(parsed["payload"]["server_id"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let mut channel = Responder::new(
            &private,
            init.as_bytes(),
            server_init.as_bytes(),
            Limits::default(),
        )
        .unwrap();
        let reply = channel
            .receive_handshake(
                &unhex(&first["handshake"]),
                &[Credential::Pairing { psk: [7; 32] }],
            )
            .unwrap();
        peer.send(json!({"handshake":hex(&reply)}));
        let activate = peer.read();
        let activate = parse(&channel.decrypt(&unhex(&activate["record"])).unwrap());
        let mut client = PairingClient::new(
            PairingContext {
                server_public,
                selection: channel.selection().unwrap(),
                connection_generation: 1,
                handshake_generation: 1,
                pairing_psk: [7; 32],
            },
            store.read(),
            vec![],
        )
        .unwrap();
        let effects = client.handle(PairingEvent::Activate(activate["payload"].clone()), 1);
        let [PairingEffect::RequestCandidate(id)] = effects.as_slice() else {
            panic!("missing request")
        };
        let id = *id;
        let effects = client.handle(
            PairingEvent::CandidateReady(id, Candidate::new([90; 32])),
            2,
        );
        let [PairingEffect::SendFlight(_, flight)] = effects.as_slice() else {
            panic!("missing flight")
        };
        for (index, message) in flight.messages().iter().enumerate() {
            if index == 1 {
                client.handle(PairingEvent::PairingFinalizeStarted(id), 3);
            }
            peer.send(json!({"record":hex(&channel.encrypt(&wire(message)).unwrap())}));
            if index == 0 {
                client.handle(PairingEvent::PairingInitSent(id, 3), 3);
            }
        }
        client.handle(PairingEvent::PairingFlightSent(id, 3), 4);
        assert!(store.read().records().is_empty());
        let ack = peer.read();
        assert_eq!(ack["server_stored"], true);
        let outcome = peer.read();
        assert_eq!(outcome["outcome"], "stored_before_ack");
        assert_eq!(unhex(&outcome["stored_psk"]), [90; 32]);
        peer.finish();
        if mode == "lost_ack" {
            client.handle(PairingEvent::Disconnected, 5);
            assert!(store.read().records().is_empty());
        } else {
            let ack = parse(&channel.decrypt(&unhex(&ack["record"])).unwrap());
            let effects = client.handle(PairingEvent::ServerFinalize(ack["payload"].clone()), 5);
            let [PairingEffect::CommitRecord(actual, update)] = effects.as_slice() else {
                panic!("missing commit")
            };
            assert_eq!(*actual, id);
            store.commit(update);
            client.handle(PairingEvent::CommitSucceeded(id), 6);
            assert_eq!(
                client.catalog(),
                &FixtureStore::open(store.directory()).read()
            );
            assert_eq!(client.catalog().records()[0].psk, [90; 32]);
            assert!(!client.is_paired());
        }
        eprintln!("real pinned Python pairing mode {mode}: passed");
    }
}
