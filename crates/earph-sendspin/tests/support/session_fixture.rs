// ABOUTME: Provides real synthetic Snow peers for ordered session tests.
// ABOUTME: Retains no production identity, network, vault, or output state.
#![allow(dead_code)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::{
    noise::{self, PATTERN},
    pairing::{RecordCatalog, psk_id},
    session::*,
};
use serde_json::{Value, json};

pub fn new_client() -> ClientSession {
    new_client_with_catalog(RecordCatalog::new(5).unwrap())
}
pub fn new_client_with_catalog(catalog: RecordCatalog) -> ClientSession {
    new_client_with_limits(catalog, SessionLimits::default())
}
pub fn new_client_with_limits(catalog: RecordCatalog, limits: SessionLimits) -> ClientSession {
    ClientSession::new(
        SessionConfig {
            private: [9; 32],
            pairing_psk: [7; 32],
            connection_generation: 4,
            hello: json!({"name":"Earph fixture","supported_roles":[]}),
            catalog,
            protected: vec![],
            limits,
        },
        0,
    )
    .unwrap()
}
pub struct Peer {
    pub private: [u8; 32],
    pub public: [u8; 32],
    pub channel: snow::TransportState,
    pub hash: [u8; 32],
}
impl Peer {
    pub fn encrypt(&mut self, plain: &[u8]) -> Vec<u8> {
        let mut out = vec![0; plain.len() + 16];
        let n = self.channel.write_message(plain, &mut out).unwrap();
        out.truncate(n);
        out
    }
    pub fn read(&mut self, record: &[u8]) -> Vec<u8> {
        let mut out = vec![0; record.len()];
        let n = self.channel.read_message(record, &mut out).unwrap();
        out.truncate(n);
        out
    }
    pub fn read_json(&mut self, record: &[u8]) -> Value {
        let out = self.read(record);
        assert_eq!(out[0], 0);
        serde_json::from_slice(&out[1..]).unwrap()
    }
    pub fn send_json(
        &mut self,
        client: &mut ClientSession,
        kind: &str,
        payload: Value,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        self.send_json_fragmented(client, kind, payload, now, 65519)
    }
    pub fn send_json_fragmented(
        &mut self,
        client: &mut ClientSession,
        kind: &str,
        payload: Value,
        now: u64,
        record_bytes: usize,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        use earph_sendspin::message::{MessageLimits, Outgoing};
        let body = serde_json::to_vec(&json!({"type":kind,"payload":payload})).unwrap();
        let mut outgoing = Outgoing::new(
            0,
            body,
            MessageLimits {
                record_plaintext_bytes: record_bytes,
                ..MessageLimits::default()
            },
        )
        .unwrap();
        let mut events = vec![];
        while let Some(plain) = outgoing.next_record() {
            let record = self.encrypt(&plain);
            events.extend(client.receive(TransportKind::Binary, &record, now)?);
        }
        Ok(events)
    }
    pub fn rotation(
        &self,
        psk: [u8; 32],
        category: &str,
        prologue: Option<[u8; 32]>,
    ) -> (snow::HandshakeState, Value) {
        let mut handshake = snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(&self.private)
            .unwrap()
            .remote_public_key(&noise::static_public_key(&[9; 32]).unwrap())
            .unwrap()
            .psk(2, &psk)
            .unwrap()
            .prologue(&prologue.unwrap_or(self.hash))
            .unwrap()
            .build_initiator()
            .unwrap();
        let payload = serde_json::to_vec(
            &json!({"psk_id":URL_SAFE_NO_PAD.encode(psk_id(&psk)),"psk_category":category}),
        )
        .unwrap();
        let mut record = vec![0; payload.len() + 48];
        let n = handshake.write_message(&payload, &mut record).unwrap();
        record.truncate(n);
        (handshake, json!({"data":URL_SAFE_NO_PAD.encode(record)}))
    }
    pub fn install(&mut self, mut handshake: snow::HandshakeState, reply: &Value) {
        let record = URL_SAFE_NO_PAD
            .decode(reply["payload"]["data"].as_str().unwrap())
            .unwrap();
        let mut out = [0; 64];
        let n = handshake.read_message(&record, &mut out).unwrap();
        assert_eq!(&out[..n], b"{}");
        self.hash = handshake.get_handshake_hash().try_into().unwrap();
        self.channel = handshake.into_transport_mode().unwrap();
    }
}
pub fn admission(events: &[SessionEvent]) -> AdmissionId {
    events
        .iter()
        .find_map(|e| match e {
            SessionEvent::Admission(a) => Some(a.id()),
            _ => None,
        })
        .expect("admission required")
}
pub fn initial() -> (ClientSession, Peer) {
    initial_from(new_client())
}
pub fn initial_from(client: ClientSession) -> (ClientSession, Peer) {
    initial_from_with_record_limit(client, 65519)
}
pub fn initial_from_with_record_limit(
    mut client: ClientSession,
    record_bytes: usize,
) -> (ClientSession, Peer) {
    let init = client.next_write(0).unwrap().unwrap();
    client
        .complete_write(init.id, WriteResult::Complete, 0)
        .unwrap();
    let private = [11; 32];
    let public = noise::static_public_key(&private).unwrap();
    let server=serde_json::to_vec(&json!({"type":"server/init","payload":{"version":1,"server_id":URL_SAFE_NO_PAD.encode(public)}})).unwrap();
    client.receive(TransportKind::Text, &server, 1).unwrap();
    let mut handshake = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&private)
        .unwrap()
        .remote_public_key(&noise::static_public_key(&[9; 32]).unwrap())
        .unwrap()
        .psk(2, &[7; 32])
        .unwrap()
        .prologue(&[init.bytes.as_slice(), &server].concat())
        .unwrap()
        .build_initiator()
        .unwrap();
    let payload = serde_json::to_vec(
        &json!({"psk_id":URL_SAFE_NO_PAD.encode(psk_id(&[7;32])),"psk_category":"pr"}),
    )
    .unwrap();
    let mut record = vec![0; payload.len() + 48];
    let n = handshake.write_message(&payload, &mut record).unwrap();
    let envelope = serde_json::to_vec(
        &json!({"type":"noise/handshake","payload":{"data":URL_SAFE_NO_PAD.encode(&record[..n])}}),
    )
    .unwrap();
    client.receive(TransportKind::Text, &envelope, 2).unwrap();
    let reply = client.next_write(2).unwrap().unwrap();
    let value: Value = serde_json::from_slice(&reply.bytes).unwrap();
    let record = URL_SAFE_NO_PAD
        .decode(value["payload"]["data"].as_str().unwrap())
        .unwrap();
    handshake.read_message(&record, &mut [0; 64]).unwrap();
    let hash = handshake.get_handshake_hash().try_into().unwrap();
    let mut peer = Peer {
        private,
        public,
        channel: handshake.into_transport_mode().unwrap(),
        hash,
    };
    client
        .complete_write(reply.id, WriteResult::Complete, 2)
        .unwrap();
    peer.send_json_fragmented(
        &mut client,
        "server/hello",
        json!({"name":"Server"}),
        3,
        record_bytes,
    )
    .unwrap();
    let mut reader = earph_sendspin::message::Reassembler::new(
        earph_sendspin::message::MessageLimits::default(),
    );
    loop {
        let hello = client.next_write(3).unwrap().unwrap();
        let plain = peer.read(&hello.bytes);
        let message = reader.push(&plain, 3).unwrap();
        client
            .complete_write(hello.id, WriteResult::Complete, 3)
            .unwrap();
        if let Some(message) = message {
            let hello: Value = serde_json::from_slice(&message.body).unwrap();
            assert_eq!(
                hello["payload"]["supported_pair_methods"],
                json!({"pairing_psk":{}})
            );
            break;
        }
    }
    (client, peer)
}
