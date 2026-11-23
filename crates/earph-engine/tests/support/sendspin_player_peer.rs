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
    client_public: [u8; 32],
}
/// Builds the server's first Noise message for one client and one secret.
fn offer(
    client_public: &[u8; 32],
    psk: &[u8; 32],
    category: &str,
    prologue: &[u8],
) -> (snow::HandshakeState, Value) {
    let mut handshake = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&SERVER_PRIVATE)
        .unwrap()
        .remote_public_key(client_public)
        .unwrap()
        .psk(2, psk)
        .unwrap()
        .prologue(prologue)
        .unwrap()
        .build_initiator()
        .unwrap();
    let payload = serde_json::to_vec(
        &json!({"psk_id":URL_SAFE_NO_PAD.encode(psk_id(psk)),"psk_category":category}),
    )
    .unwrap();
    let mut record = vec![0; payload.len() + 48];
    let n = handshake.write_message(&payload, &mut record).unwrap();
    (
        handshake,
        json!({"data":URL_SAFE_NO_PAD.encode(&record[..n])}),
    )
}
impl PlayerPeer {
    /// Accepts one client, authenticates on the pairing secret, then rotates to the record.
    pub fn accept(listener: &TcpListener) -> Self {
        let client = noise::static_public_key(&CLIENT_PRIVATE).unwrap();
        let mut peer = Self::connect(listener, client, PAIRING_PSK, "pr");
        peer.activate_idle();
        peer.rotate(RECORD_PSK, "lt");
        peer
    }
    /// Accepts one client, authenticates it on one secret, and exchanges hellos. Sends no activation.
    /// The client must present the expected identity, as a server checks a pairing token.
    pub fn connect(
        listener: &TcpListener,
        client_public: [u8; 32],
        psk: [u8; 32],
        category: &str,
    ) -> Self {
        let (mut socket, origin, mut handshake) =
            offered(listener, &client_public, &client_public, &psk, category);
        let public = noise::static_public_key(&SERVER_PRIVATE).unwrap();
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
            client_public,
        };
        peer.send_json("server/hello", json!({"name":"Server"}));
        peer.await_json("client/hello", false);
        peer
    }
    /// Accepts one client and sends a Noise message that was made for another client key.
    /// The client cannot authenticate that message. Returns after the client ended the connection.
    pub fn fail_authentication(listener: &TcpListener) {
        let client = noise::static_public_key(&CLIENT_PRIVATE).unwrap();
        let other = noise::static_public_key(&[3; 32]).unwrap();
        let (mut socket, origin, _) = offered(listener, &client, &other, &PAIRING_PSK, "pr");
        loop {
            assert!(origin.elapsed() < STAGE, "peer stage deadline: refusal");
            match socket.read() {
                Err(Error::Io(error)) if idle(&error) => {}
                Ok(Message::Close(_)) | Err(_) => return,
                Ok(_) => {}
            }
        }
    }
    /// Re-handshakes inside the current channel onto another secret.
    pub fn rotate(&mut self, psk: [u8; 32], category: &str) {
        let (rotation, offer) = offer(&self.client_public, &psk, category, &self.channel.hash);
        self.send_json("noise/handshake", offer);
        // Replies to earlier probes would use keys that this rotation retires.
        let payload = self.await_json("noise/handshake", false);
        self.channel.install(
            rotation,
            &json!({"type":"noise/handshake","payload":payload}),
        );
    }
    pub fn activate_idle(&mut self) {
        self.send_json(
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
        );
    }
    /// Runs the server side of the Pairing PSK flow up to the client's finalize.
    /// Returns the long-term secret that the client delivered. The caller acknowledges it.
    pub fn receive_pairing(&mut self) -> [u8; 32] {
        self.send_json(
            "server/activate",
            json!({"activities":["pairing"],"active_roles":[],"pairing":{"method":"pairing_psk"}}),
        );
        let init = self.await_json("client/pair-init", true);
        assert_eq!(init["pairing_index"], 1);
        let finalize = self.await_json("client/pair-finalize", true);
        URL_SAFE_NO_PAD
            .decode(finalize["long_term_psk"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap()
    }
    /// Tells the client that the server stored the pairing record.
    pub fn acknowledge_pairing(&mut self) {
        self.send_json("server/pair-finalize", json!({}));
    }
    /// Returns the payload of the client's goodbye.
    pub fn await_goodbye(&mut self) -> Value {
        self.await_json("client/goodbye", true)
    }
    /// Serves clock probes until the client ends the connection.
    pub fn serve_to_close(mut self, what: &str) {
        let deadline = Instant::now() + STAGE;
        loop {
            assert!(Instant::now() < deadline, "peer stage deadline: {what}");
            match self.socket.read() {
                Ok(Message::Binary(record)) => {
                    let plain = self.channel.read(&record);
                    if let Some(message) = self.reader.push(&plain, stamp(self.origin)).unwrap() {
                        self.application(&message, true);
                    }
                }
                Ok(Message::Close(_)) | Err(Error::ConnectionClosed | Error::AlreadyClosed) => {
                    return;
                }
                Ok(_) => {}
                Err(Error::Io(error)) if idle(&error) => {}
                // A cancelled client may reset the socket.
                Err(Error::Io(_) | Error::Protocol(_)) => return,
                Err(error) => panic!("peer read failed: {error}"),
            }
        }
    }
    /// Drops the connection without a closing handshake.
    pub fn abandon(self) {
        let _ = self.socket.get_ref().shutdown(std::net::Shutdown::Both);
    }
    pub fn server_now_us(&self) -> i64 {
        i64::try_from(self.origin.elapsed().as_micros()).unwrap() + SERVER_CLOCK_OFFSET_US
    }
    /// Activates the player role, then serves clock probes until the client reports availability.
    /// Returns the state that the client declared: its availability and its player object.
    pub fn activate_player(&mut self) -> Value {
        self.send_json(
            "server/activate",
            json!({"activities":["playback"],"active_roles":["player@v1"]}),
        );
        loop {
            let state = self.await_json("client/state", true);
            if state["available"] == json!(true) {
                return state;
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
    /// Clears the player's buffer. A timestamp clears only the audio from that server time on.
    pub fn clear_stream(&mut self, from_timestamp_us: Option<i64>) {
        let mut payload = json!({"server_transmitted":self.server_now_us()});
        if let Some(from) = from_timestamp_us {
            payload["from_timestamp"] = json!(from);
        }
        self.send_json("stream/clear", payload);
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
/// Accepts one client, exchanges the init messages, and sends the server's first Noise message.
/// The client must present `client_public`. The Noise message is made for `noise_public`.
fn offered(
    listener: &TcpListener,
    client_public: &[u8; 32],
    noise_public: &[u8; 32],
    psk: &[u8; 32],
    category: &str,
) -> (WebSocket<TcpStream>, Instant, snow::HandshakeState) {
    let (stream, _) = listener.accept().unwrap();
    // Timed chunks must leave at once. Batching would add tens of milliseconds of transit.
    stream.set_nodelay(true).unwrap();
    stream.set_read_timeout(Some(STAGE)).unwrap();
    let mut socket = accept(stream).unwrap();
    // A short timeout turns every later read into a poll.
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(1)))
        .unwrap();
    let origin = Instant::now();
    let init = text(&mut socket, origin);
    let presented: Value = serde_json::from_str(&init).unwrap();
    assert!(
        presented["payload"]["client_id"] == URL_SAFE_NO_PAD.encode(client_public),
        "the client presented another identity"
    );
    let public = noise::static_public_key(&SERVER_PRIVATE).unwrap();
    let server = serde_json::to_string(&json!({"type":"server/init","payload":{"version":1,"server_id":URL_SAFE_NO_PAD.encode(public)}})).unwrap();
    socket.send(server.clone().into()).unwrap();
    let prologue = [init.as_bytes(), server.as_bytes()].concat();
    let (handshake, payload) = offer(noise_public, psk, category, &prologue);
    let offer = json!({"type":"noise/handshake","payload":payload});
    socket.send(offer.to_string().into()).unwrap();
    (socket, origin, handshake)
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
