// ABOUTME: Owns a bounded Sendspin client Noise responder and its failure lifecycle.
// ABOUTME: Keeps cryptographic confirmation separate from pairing and playback authority.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use sha2::{Digest, Sha256};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use std::fmt;

pub const PATTERN: &str = "Noise_KKpsk2_25519_ChaChaPoly_SHA256";
pub const SENTINEL: [u8; 32] = [
    0x1b, 0x5e, 0x24, 0xdb, 0xc1, 0xae, 0xd9, 0x5f, 0xc2, 0xa5, 0xa3, 0x38, 0xa9, 0x0c, 0x05, 0xdf,
    0x44, 0xbd, 0x10, 0xf5, 0xec, 0x1f, 0x4c, 0xd6, 0x6c, 0xbf, 0x86, 0x27, 0x27, 0x67, 0xb9, 0xd3,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelError {
    Budget,
    Phase,
    Authentication,
    Malformed,
    CredentialBinding,
    Crypto,
}
impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sendspin channel {:?}", self)
    }
}
impl std::error::Error for ChannelError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Handshake,
    PendingConfirmation,
    Established,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    LongTerm,
    Pairing,
    Sentinel,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub category: Category,
    pub sentinel_fallback: bool,
}

pub enum Credential {
    LongTerm {
        psk: [u8; 32],
        server_public: [u8; 32],
    },
    Pairing {
        psk: [u8; 32],
    },
}
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub init_bytes: usize,
    pub handshake_payload_bytes: usize,
    pub record_bytes: usize,
    pub plaintext_bytes: usize,
    pub output_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            init_bytes: 4096,
            handshake_payload_bytes: 4096,
            record_bytes: 65535,
            plaintext_bytes: 65519,
            output_bytes: 65535,
        }
    }
}

enum State {
    Handshake(Box<snow::HandshakeState>),
    Transport {
        channel: Box<snow::TransportState>,
        confirmed: bool,
    },
    Closed,
}

pub struct Responder {
    state: State,
    server_public: [u8; 32],
    limits: Limits,
    selection: Option<Selection>,
    handshake_hash: Option<[u8; 32]>,
    matched_id: Option<[u8; 32]>,
    allow_fallback: bool,
}
impl fmt::Debug for Responder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Responder")
            .field("phase", &self.phase())
            .finish_non_exhaustive()
    }
}
impl Responder {
    /// Pass exact UTF-8 JSON message bodies in wire order, without WebSocket framing.
    /// The caller retains identity ownership and must enforce deadlines and socket closure.
    pub fn new(
        private: &[u8; 32],
        client_init: &[u8],
        server_init: &[u8],
        limits: Limits,
    ) -> Result<Self, ChannelError> {
        if client_init.len() > limits.init_bytes.min(65535)
            || server_init.len() > limits.init_bytes.min(65535)
        {
            return Err(ChannelError::Budget);
        }
        let client = init_payload(client_init, "client/init")?;
        let server = init_payload(server_init, "server/init")?;
        if client.get("suite").and_then(Value::as_str) != Some("25519_ChaChaPoly_SHA256") {
            return Err(ChannelError::Malformed);
        }
        let client_public = decode_key(client.get("client_id"))?;
        let server_public = decode_key(server.get("server_id"))?;
        if static_public_key(private)? != client_public {
            return Err(ChannelError::CredentialBinding);
        }
        let mut prologue = Vec::with_capacity(client_init.len() + server_init.len());
        prologue.extend_from_slice(client_init);
        prologue.extend_from_slice(server_init);
        let handshake = snow::Builder::new(PATTERN.parse().map_err(|_| ChannelError::Crypto)?)
            .local_private_key(private)
            .map_err(|_| ChannelError::Crypto)?
            .remote_public_key(&server_public)
            .map_err(|_| ChannelError::Crypto)?
            .prologue(&prologue)
            .map_err(|_| ChannelError::Crypto)?
            .build_responder()
            .map_err(|_| ChannelError::Crypto)?;
        Ok(Self {
            state: State::Handshake(Box::new(handshake)),
            server_public,
            limits,
            selection: None,
            handshake_hash: None,
            matched_id: None,
            allow_fallback: true,
        })
    }
    /// Strict rotation carries identities and uses only the immediately preceding hash.
    pub(crate) fn rehandshake(
        private: &[u8; 32],
        server_public: [u8; 32],
        prior_hash: [u8; 32],
        limits: Limits,
    ) -> Result<Self, ChannelError> {
        let handshake = snow::Builder::new(PATTERN.parse().map_err(|_| ChannelError::Crypto)?)
            .local_private_key(private)
            .map_err(|_| ChannelError::Crypto)?
            .remote_public_key(&server_public)
            .map_err(|_| ChannelError::Crypto)?
            .prologue(&prior_hash)
            .map_err(|_| ChannelError::Crypto)?
            .build_responder()
            .map_err(|_| ChannelError::Crypto)?;
        Ok(Self {
            state: State::Handshake(Box::new(handshake)),
            server_public,
            limits,
            selection: None,
            handshake_hash: None,
            matched_id: None,
            allow_fallback: false,
        })
    }
    pub(crate) fn server_public(&self) -> [u8; 32] {
        self.server_public
    }
    pub(crate) fn matched_id(&self) -> Option<[u8; 32]> {
        self.matched_id
    }
    pub fn phase(&self) -> Phase {
        match &self.state {
            State::Handshake(_) => Phase::Handshake,
            State::Transport {
                confirmed: false, ..
            } => Phase::PendingConfirmation,
            State::Transport {
                confirmed: true, ..
            } => Phase::Established,
            State::Closed => Phase::Closed,
        }
    }
    /// Reads raw Noise message 1 and returns raw Noise message 2, whose plaintext is exactly `{}`.
    /// Wrap the result in the specified base64url noise/handshake envelope at the transport boundary.
    pub fn receive_handshake(
        &mut self,
        record: &[u8],
        credentials: &[Credential],
    ) -> Result<Vec<u8>, ChannelError> {
        let state = std::mem::replace(&mut self.state, State::Closed);
        let result = (|| {
            let State::Handshake(mut handshake) = state else {
                return Err(ChannelError::Phase);
            };
            self.check_record(record.len())?;
            let payload_bytes = record
                .len()
                .checked_sub(48)
                .ok_or(ChannelError::Malformed)?;
            if payload_bytes > self.limits.handshake_payload_bytes.min(65487)
                || 50 > self.limits.output_bytes
                || 50 > self.limits.record_bytes
            {
                return Err(ChannelError::Budget);
            }
            let mut payload = vec![0; payload_bytes];
            let n = handshake
                .read_message(record, &mut payload)
                .map_err(|_| ChannelError::Authentication)?;
            let ResolvedCredential {
                psk,
                selection,
                matched_id,
            } = select_credential(
                &payload[..n],
                credentials,
                &self.server_public,
                self.allow_fallback,
            )?;
            handshake
                .set_psk(2, &psk)
                .map_err(|_| ChannelError::Crypto)?;
            let mut reply = vec![0; 50];
            let n = handshake
                .write_message(b"{}", &mut reply)
                .map_err(|_| ChannelError::Crypto)?;
            reply.truncate(n);
            let hash = handshake
                .get_handshake_hash()
                .try_into()
                .map_err(|_| ChannelError::Crypto)?;
            let channel = handshake
                .into_transport_mode()
                .map_err(|_| ChannelError::Crypto)?;
            self.state = State::Transport {
                channel: Box::new(channel),
                confirmed: false,
            };
            self.selection = Some(selection);
            self.matched_id = matched_id;
            self.handshake_hash = Some(hash);
            Ok(reply)
        })();
        if result.is_err() {
            self.close();
        }
        result
    }
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }
    /// Available only while transport state remains live. This is no re-handshake operation.
    pub fn handshake_hash(&self) -> Option<[u8; 32]> {
        self.handshake_hash
    }
    /// Returns one authenticated plaintext record; the caller must validate application sequencing and framing.
    /// The first successful read confirms peer PSK possession, without granting pairing or playback authority.
    pub fn decrypt(&mut self, record: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let state = std::mem::replace(&mut self.state, State::Closed);
        let result = (|| {
            let State::Transport { mut channel, .. } = state else {
                return Err(ChannelError::Phase);
            };
            self.check_record(record.len())?;
            let plaintext_bytes = record
                .len()
                .checked_sub(16)
                .ok_or(ChannelError::Malformed)?;
            self.check_plaintext(plaintext_bytes)?;
            if plaintext_bytes > self.limits.output_bytes {
                return Err(ChannelError::Budget);
            }
            let mut plaintext = vec![0; plaintext_bytes];
            let n = channel
                .read_message(record, &mut plaintext)
                .map_err(|_| ChannelError::Authentication)?;
            if n == 0 {
                return Err(ChannelError::Malformed);
            }
            plaintext.truncate(n);
            self.state = State::Transport {
                channel,
                confirmed: true,
            };
            Ok(plaintext)
        })();
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Application writes require authenticated server traffic first.
    /// The caller must authorize each message and finish any application fragmentation sequence.
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let state = std::mem::replace(&mut self.state, State::Closed);
        let result = (|| {
            let State::Transport {
                mut channel,
                confirmed: true,
            } = state
            else {
                return Err(ChannelError::Phase);
            };
            self.check_plaintext(plaintext.len())?;
            if plaintext.is_empty() {
                return Err(ChannelError::Malformed);
            }
            let bytes = plaintext.len() + 16;
            self.check_record(bytes)?;
            if bytes > self.limits.output_bytes {
                return Err(ChannelError::Budget);
            }
            let mut record = vec![0; bytes];
            let n = channel
                .write_message(plaintext, &mut record)
                .map_err(|_| ChannelError::Authentication)?;
            record.truncate(n);
            self.state = State::Transport {
                channel,
                confirmed: true,
            };
            Ok(record)
        })();
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Cancels any phase and drops all owned cryptographic state. No secret-erasure claim is made.
    pub fn close(&mut self) {
        self.state = State::Closed;
        self.selection = None;
        self.handshake_hash = None;
        self.matched_id = None;
    }
    fn check_record(&self, bytes: usize) -> Result<(), ChannelError> {
        if bytes > self.limits.record_bytes.min(65535) {
            Err(ChannelError::Budget)
        } else {
            Ok(())
        }
    }
    fn check_plaintext(&self, bytes: usize) -> Result<(), ChannelError> {
        if bytes > self.limits.plaintext_bytes.min(65519) {
            Err(ChannelError::Budget)
        } else {
            Ok(())
        }
    }
}

/// Uses Snow's selected OS CSPRNG. Persisting or sharing the returned key is the caller's responsibility.
pub fn generate_static_private_key() -> Result<[u8; 32], ChannelError> {
    snow::Builder::new(PATTERN.parse().map_err(|_| ChannelError::Crypto)?)
        .generate_keypair()
        .map_err(|_| ChannelError::Crypto)?
        .private
        .try_into()
        .map_err(|_| ChannelError::Crypto)
}

/// Derives the public identity through the selected library primitive.
pub fn static_public_key(private: &[u8; 32]) -> Result<[u8; 32], ChannelError> {
    let mut dh = DefaultResolver
        .resolve_dh(&snow::params::DHChoice::Curve25519)
        .ok_or(ChannelError::Crypto)?;
    dh.set(private);
    dh.pubkey().try_into().map_err(|_| ChannelError::Crypto)
}

fn init_payload(raw: &[u8], kind: &str) -> Result<serde_json::Map<String, Value>, ChannelError> {
    let mut value: Value = serde_json::from_slice(raw).map_err(|_| ChannelError::Malformed)?;
    if value.get("type").and_then(Value::as_str) != Some(kind) {
        return Err(ChannelError::Malformed);
    }
    let payload = value
        .get_mut("payload")
        .and_then(Value::as_object_mut)
        .ok_or(ChannelError::Malformed)?;
    if payload.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(ChannelError::Malformed);
    }
    Ok(std::mem::take(payload))
}

fn decode_key(value: Option<&Value>) -> Result<[u8; 32], ChannelError> {
    let text = value
        .and_then(Value::as_str)
        .ok_or(ChannelError::Malformed)?;
    if text.len() != 43 {
        return Err(ChannelError::Malformed);
    }
    let mut decoded = [0; 32];
    let n = URL_SAFE_NO_PAD
        .decode_slice(text, &mut decoded)
        .map_err(|_| ChannelError::Malformed)?;
    if n != 32 {
        return Err(ChannelError::Malformed);
    }
    Ok(decoded)
}

struct ResolvedCredential {
    psk: [u8; 32],
    selection: Selection,
    matched_id: Option<[u8; 32]>,
}

fn select_credential(
    payload: &[u8],
    credentials: &[Credential],
    server_public: &[u8; 32],
    allow_fallback: bool,
) -> Result<ResolvedCredential, ChannelError> {
    let value: Value = serde_json::from_slice(payload).map_err(|_| ChannelError::Malformed)?;
    let requested = decode_key(value.get("psk_id"))?;
    let category = match value.get("psk_category").and_then(Value::as_str) {
        Some("lt") => Category::LongTerm,
        Some("pr") => Category::Pairing,
        Some("sn") => Category::Sentinel,
        _ => return Err(ChannelError::Malformed),
    };
    if category == Category::Sentinel && requested == psk_id(&SENTINEL) {
        return Ok(ResolvedCredential {
            psk: SENTINEL,
            selection: Selection {
                category,
                sentinel_fallback: false,
            },
            matched_id: None,
        });
    }
    for credential in credentials {
        let (psk, candidate, binding) = match credential {
            Credential::LongTerm { psk, server_public } => {
                (psk, Category::LongTerm, Some(server_public))
            }
            Credential::Pairing { psk } => (psk, Category::Pairing, None),
        };
        if candidate == category && psk_id(psk) == requested {
            if binding.is_some_and(|key| key != server_public) {
                return Err(ChannelError::CredentialBinding);
            }
            return Ok(ResolvedCredential {
                psk: *psk,
                selection: Selection {
                    category,
                    sentinel_fallback: false,
                },
                matched_id: Some(requested),
            });
        }
    }
    if !allow_fallback {
        return Err(ChannelError::CredentialBinding);
    }
    Ok(ResolvedCredential {
        psk: SENTINEL,
        selection: Selection {
            category: Category::Sentinel,
            sentinel_fallback: true,
        },
        matched_id: None,
    })
}

pub(crate) fn psk_id(psk: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"sendspin-psk-id-v1");
    hash.update(psk);
    hash.finalize().into()
}
