// ABOUTME: Serves one authenticated Sendspin player connection over a real blocking WebSocket.
// ABOUTME: Answers clock probes with its own clock and sends timestamped PCM for worker tests.
#[path = "../../../earph-sendspin/tests/support/session_fixture.rs"]
mod session_fixture;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::{
    message::{Complete, Envelope, MessageLimits, Outgoing, Reassembler},
    noise::{self, PATTERN},
    pairing::psk_id,
};
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    net::{TcpListener, TcpStream},
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{Error, Message, WebSocket, accept};

/// The fixture client identity that `session_fixture::Peer::rotation` expects.
pub const CLIENT_PRIVATE: [u8; 32] = [9; 32];
pub const PAIRING_PSK: [u8; 32] = [7; 32];
pub const SERVER_PRIVATE: [u8; 32] = [11; 32];
/// The long-term secret that the client catalog must already hold for this server.
pub const RECORD_PSK: [u8; 32] = [90; 32];
/// The server clock runs this far ahead of the peer's monotonic origin.
const SERVER_CLOCK_OFFSET_US: i64 = 5_000_000_000;
const STAGE: Duration = Duration::from_secs(10);

pub struct PlayerPeer {
    socket: WebSocket<TcpStream>,
    channel: session_fixture::Peer,
    reader: Reassembler,
    origin: Instant,
}
impl PlayerPeer {
    /// Accepts one client, authenticates on the pairing secret, then rotates to the record.
    pub fn accept(listener: &TcpListener) -> Self {
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(STAGE)).unwrap();
        let mut socket = accept(stream).unwrap();
        // A short timeout turns every later read into a poll.
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        let origin = Instant::now();
        let init = text(&mut socket, origin);
        let public = noise::static_public_key(&SERVER_PRIVATE).unwrap();
        let server = serde_json::to_string(&json!({"type":"server/init","payload":{"version":1,"server_id":URL_SAFE_NO_PAD.encode(public)}})).unwrap();
        socket.send(server.clone().into()).unwrap();
        let mut handshake = snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(&SERVER_PRIVATE)
            .unwrap()
            .remote_public_key(&noise::static_public_key(&CLIENT_PRIVATE).unwrap())
            .unwrap()
            .psk(2, &PAIRING_PSK)
            .unwrap()
            .prologue(&[init.as_bytes(), server.as_bytes()].concat())
            .unwrap()
            .build_initiator()
            .unwrap();
        let payload = serde_json::to_vec(
            &json!({"psk_id":URL_SAFE_NO_PAD.encode(psk_id(&PAIRING_PSK)),"psk_category":"pr"}),
        )
        .unwrap();
        let mut record = vec![0; payload.len() + 48];
        let n = handshake.write_message(&payload, &mut record).unwrap();
        let offer = json!({"type":"noise/handshake","payload":{"data":URL_SAFE_NO_PAD.encode(&record[..n])}});
        socket.send(offer.to_string().into()).unwrap();
        let reply: Value = serde_json::from_str(&text(&mut socket, origin)).unwrap();
        let record = URL_SAFE_NO_PAD
            .decode(reply["payload"]["data"].as_str().unwrap())
            .unwrap();
        handshake.read_message(&record, &mut [0; 64]).unwrap();
        let hash = handshake.get_handshake_hash().try_into().unwrap();
        let mut peer = Self {
            socket,
            channel: session_fixture::Peer {
                private: SERVER_PRIVATE,
                public,
                channel: handshake.into_transport_mode().unwrap(),
                hash,
            },
            reader: Reassembler::new(MessageLimits::default()),
            origin,
        };
        peer.send_json("server/hello", json!({"name":"Server"}));
        peer.await_json("client/hello", false);
        peer.send_json(
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
        );
        let (rotation, offer) = peer.channel.rotation(RECORD_PSK, "lt", None);
        peer.send_json("noise/handshake", offer);
        // Replies to earlier probes would use keys that this rotation retires.
        let payload = peer.await_json("noise/handshake", false);
        peer.channel.install(
            rotation,
            &json!({"type":"noise/handshake","payload":payload}),
        );
        peer
    }
    pub fn server_now_us(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_micros()).unwrap() + SERVER_CLOCK_OFFSET_US
    }
    /// Activates the player role, then serves clock probes until the client reports availability.
    pub fn activate_player(&mut self) {
        self.send_json(
            "server/activate",
            json!({"activities":["playback"],"active_roles":["player@v1"]}),
        );
        loop {
            let state = self.await_json("client/state", true);
            if state["player"]["available"] == json!(true) {
                return;
            }
        }
    }
    pub fn start_stream(&mut self) {
        let now = self.server_now_us();
        self.send_json(
            "stream/start",
            json!({"server_transmitted":now,"player":{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16}}),
        );
    }
    pub fn send_chunk(&mut self, timestamp_us: i64, pcm: &[u8]) {
        let body = [
            timestamp_us.to_be_bytes().as_slice(),
            &0u32.to_be_bytes(),
            pcm,
        ]
        .concat();
        self.send(4, body);
    }
    pub fn end_stream(&mut self) {
        self.send_json("stream/end", json!({}));
    }
    /// Serves clock probes until the condition holds.
    pub fn serve_until(&mut self, what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + STAGE;
        while !done() {
            assert!(Instant::now() < deadline, "peer stage deadline: {what}");
            if let Some(message) = self.poll() {
                self.application(&message, true);
            }
        }
    }
    pub fn close(mut self) {
        self.socket.close(None).unwrap();
        let deadline = Instant::now() + STAGE;
        loop {
            assert!(
                Instant::now() < deadline,
                "peer stage deadline: close reply"
            );
            match self.socket.read() {
                Ok(Message::Close(_)) | Err(Error::ConnectionClosed) => return,
                Ok(_) => {}
                Err(Error::Io(error)) if idle(&error) => {}
                Err(error) => panic!("peer close failed: {error}"),
            }
        }
    }
    fn send_json(&mut self, kind: &str, payload: Value) {
        self.send(
            0,
            serde_json::to_vec(&json!({"type":kind,"payload":payload})).unwrap(),
        );
    }
    fn send(&mut self, id: u8, body: Vec<u8>) {
        let mut outgoing = Outgoing::new(id, body, MessageLimits::default()).unwrap();
        while let Some(plain) = outgoing.next_record() {
            let record = self.channel.encrypt(&plain);
            self.socket.send(Message::Binary(record.into())).unwrap();
        }
    }
    /// Returns one complete decrypted message when the socket holds one.
    fn poll(&mut self) -> Option<Complete> {
        loop {
            match self.socket.read() {
                Ok(Message::Binary(record)) => {
                    let plain = self.channel.read(&record);
                    let stamp = stamp(self.origin);
                    if let Some(message) = self.reader.push(&plain, stamp).unwrap() {
                        return Some(message);
                    }
                }
                Ok(Message::Ping(_) | Message::Pong(_)) => {}
                Ok(other) => panic!("unexpected peer input: {other:?}"),
                Err(Error::Io(error)) if idle(&error) => return None,
                Err(error) => panic!("peer read failed: {error}"),
            }
        }
    }
    /// Answers a clock probe when allowed. Returns every other JSON message.
    fn application(&mut self, message: &Complete, answer_time: bool) -> Option<Envelope> {
        assert_eq!(message.id, 0, "the client sends JSON only");
        let envelope = Envelope::parse(&message.body).unwrap();
        if envelope.kind != "client/time" {
            return Some(envelope);
        }
        if answer_time {
            let received = self.server_now_us();
            let echo = envelope.payload["client_transmitted"].clone();
            let transmitted = self.server_now_us();
            self.send_json(
                "server/time",
                json!({"client_transmitted":echo,"server_received":received,"server_transmitted":transmitted}),
            );
        }
        None
    }
    fn await_json(&mut self, kind: &str, answer_time: bool) -> Value {
        let deadline = Instant::now() + STAGE;
        loop {
            assert!(Instant::now() < deadline, "peer stage deadline: {kind}");
            let Some(message) = self.poll() else {
                continue;
            };
            if let Some(envelope) = self.application(&message, answer_time)
                && envelope.kind == kind
            {
                return envelope.payload;
            }
        }
    }
}
fn idle(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}
fn stamp(origin: Instant) -> earph_sendspin::message::ReceiveStamp {
    session_fixture::stamp_ms(u64::try_from(origin.elapsed().as_millis()).unwrap())
}
/// Reads one cleartext handshake message.
fn text(socket: &mut WebSocket<TcpStream>, origin: Instant) -> String {
    loop {
        assert!(origin.elapsed() < STAGE, "peer stage deadline: handshake");
        match socket.read() {
            Ok(Message::Text(text)) => return text.to_string(),
            Ok(other) => panic!("unexpected handshake input: {other:?}"),
            Err(Error::Io(error)) if idle(&error) => {}
            Err(error) => panic!("handshake read failed: {error}"),
        }
    }
}
