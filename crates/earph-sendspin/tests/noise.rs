// ABOUTME: Exercises the real Noise responder against a library initiator.
// ABOUTME: Covers raw transcript binding, credential choice, record limits, and permanent failure.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::noise::*;
use sha2::{Digest, Sha256};

fn id(psk: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(
        [b"sendspin-psk-id-v1".as_slice(), psk].concat(),
    ))
}
struct Peer {
    client: snow::Keypair,
    server: snow::Keypair,
    client_init: Vec<u8>,
    server_init: Vec<u8>,
}
impl Peer {
    fn new() -> Self {
        let builder = snow::Builder::new(PATTERN.parse().unwrap());
        let client = builder.generate_keypair().unwrap();
        let server = builder.generate_keypair().unwrap();
        let client_init = format!("{{\"type\":\"client/init\",\"payload\":{{\"version\":1,\"suite\":\"25519_ChaChaPoly_SHA256\",\"client_id\":\"{}\",\"future\":true}}}}", URL_SAFE_NO_PAD.encode(&client.public)).into_bytes();
        let server_init = format!(
            "{{\"type\":\"server/init\",\"payload\":{{\"version\":1,\"server_id\":\"{}\"}}}}",
            URL_SAFE_NO_PAD.encode(&server.public)
        )
        .into_bytes();
        Self {
            client,
            server,
            client_init,
            server_init,
        }
    }
    fn responder(&self, limits: Limits) -> Responder {
        Responder::new(
            self.client.private.as_slice().try_into().unwrap(),
            &self.client_init,
            &self.server_init,
            limits,
        )
        .unwrap()
    }
    fn initiator(&self, psk: &[u8; 32]) -> snow::HandshakeState {
        snow::Builder::new(PATTERN.parse().unwrap())
            .local_private_key(&self.server.private)
            .unwrap()
            .remote_public_key(&self.client.public)
            .unwrap()
            .prologue(&[self.client_init.as_slice(), &self.server_init].concat())
            .unwrap()
            .psk(2, psk)
            .unwrap()
            .build_initiator()
            .unwrap()
    }
    fn credential(&self, psk: [u8; 32]) -> Credential {
        Credential::LongTerm {
            psk,
            server_public: self.server.public.as_slice().try_into().unwrap(),
        }
    }
}
fn message(peer: &mut snow::HandshakeState, psk: &[u8; 32], category: &str) -> Vec<u8> {
    let payload = format!(
        "{{\"psk_id\":\"{}\",\"psk_category\":\"{category}\",\"future\":true}}",
        id(psk)
    );
    let mut out = vec![0; 65535];
    let n = peer.write_message(payload.as_bytes(), &mut out).unwrap();
    out.truncate(n);
    out
}
fn encrypt(peer: &mut snow::TransportState, plaintext: &[u8]) -> Vec<u8> {
    let mut out = vec![0; 65535];
    let n = peer.write_message(plaintext, &mut out).unwrap();
    out.truncate(n);
    out
}

#[test]
fn responder_selects_psk_after_authenticated_first_message() {
    let p = Peer::new();
    let psk = [7; 32];
    let mut server = p.initiator(&psk);
    let mut client = p.responder(Limits::default());
    let reply = client
        .receive_handshake(&message(&mut server, &psk, "lt"), &[p.credential(psk)])
        .unwrap();
    assert_eq!(client.phase(), Phase::PendingConfirmation);
    assert_eq!(
        client.selection(),
        Some(Selection {
            category: Category::LongTerm,
            sentinel_fallback: false
        })
    );
    let mut body = [0; 64];
    let n = server.read_message(&reply, &mut body).unwrap();
    assert_eq!(&body[..n], b"{}");
    let mut server = server.into_transport_mode().unwrap();
    assert_eq!(
        client
            .decrypt(&encrypt(&mut server, b"\x04server"))
            .unwrap(),
        b"\x04server"
    );
    assert_eq!(client.phase(), Phase::Established);
    let out = client.encrypt(b"\x04client").unwrap();
    let n = server.read_message(&out, &mut body).unwrap();
    assert_eq!(&body[..n], b"\x04client");
}

fn connected(limits: Limits) -> (Responder, snow::TransportState) {
    let p = Peer::new();
    let mut server = p.initiator(&SENTINEL);
    let mut client = p.responder(limits);
    let reply = client
        .receive_handshake(&message(&mut server, &SENTINEL, "sn"), &[])
        .unwrap();
    server.read_message(&reply, &mut [0; 64]).unwrap();
    (client, server.into_transport_mode().unwrap())
}

#[test]
fn malformed_authenticated_selection_closes_and_cannot_retry() {
    for payload in [
        b"{}".as_slice(),
        b"{\"psk_id\":\"x\",\"psk_category\":\"sn\"}",
        b"{\"psk_id\":42,\"psk_category\":\"sn\"}",
        b"[]",
        b"null",
        b"{\"psk_id\":\"GFsV9tLaSQm9HcFWpKsgYQOr7wFTvNUtkmFwuVz3zoo\",\"psk_category\":\"xx\"}",
    ] {
        let p = Peer::new();
        let mut server = p.initiator(&SENTINEL);
        let mut client = p.responder(Limits::default());
        let mut record = [0; 512];
        let n = server.write_message(payload, &mut record).unwrap();
        assert_eq!(
            client.receive_handshake(&record[..n], &[]),
            Err(ChannelError::Malformed)
        );
        assert_eq!(client.phase(), Phase::Closed);
        assert_eq!(
            client.receive_handshake(&record[..n], &[]),
            Err(ChannelError::Phase)
        );
    }
}

#[test]
fn transcript_tampering_and_wrong_static_key_fail_authentication() {
    for wrong_key in [false, true] {
        let mut p = Peer::new();
        let mut server = p.initiator(&SENTINEL);
        if wrong_key {
            p.client = snow::Builder::new(PATTERN.parse().unwrap())
                .generate_keypair()
                .unwrap();
            p.client_init=format!("{{\"type\":\"client/init\",\"payload\":{{\"version\":1,\"suite\":\"25519_ChaChaPoly_SHA256\",\"client_id\":\"{}\"}}}}",URL_SAFE_NO_PAD.encode(&p.client.public)).into_bytes();
        } else {
            p.server_init.push(b' ');
        }
        let mut client = p.responder(Limits::default());
        assert_eq!(
            client.receive_handshake(&message(&mut server, &SENTINEL, "sn"), &[]),
            Err(ChannelError::Authentication)
        );
        assert_eq!(client.phase(), Phase::Closed);
    }
}

#[test]
fn matched_long_term_credential_must_bind_the_server_key() {
    let p = Peer::new();
    let psk = [7; 32];
    let mut server = p.initiator(&psk);
    let mut client = p.responder(Limits::default());
    assert_eq!(
        client.receive_handshake(
            &message(&mut server, &psk, "lt"),
            &[Credential::LongTerm {
                psk,
                server_public: [0; 32]
            }]
        ),
        Err(ChannelError::CredentialBinding)
    );
    assert_eq!(client.phase(), Phase::Closed);
}

#[test]
fn category_miss_falls_back_to_sentinel_without_paired_authority() {
    let p = Peer::new();
    let psk = [7; 32];
    let mut server = p.initiator(&SENTINEL);
    let mut client = p.responder(Limits::default());
    let reply = client
        .receive_handshake(
            &message(&mut server, &psk, "lt"),
            &[Credential::Pairing { psk }],
        )
        .unwrap();
    assert_eq!(
        client.selection(),
        Some(Selection {
            category: Category::Sentinel,
            sentinel_fallback: true
        })
    );
    assert_eq!(client.phase(), Phase::PendingConfirmation);
    let mut payload = [0; 64];
    assert_eq!(server.read_message(&reply, &mut payload).unwrap(), 2);
}

#[test]
fn pairing_credentials_keep_their_distinct_category() {
    let p = Peer::new();
    let psk = [9; 32];
    let mut server = p.initiator(&psk);
    let mut client = p.responder(Limits::default());
    let reply = client
        .receive_handshake(
            &message(&mut server, &psk, "pr"),
            &[Credential::Pairing { psk }],
        )
        .unwrap();
    assert_eq!(
        client.selection(),
        Some(Selection {
            category: Category::Pairing,
            sentinel_fallback: false
        })
    );
    server.read_message(&reply, &mut [0; 64]).unwrap();
}

#[test]
fn a_wrong_psk_is_unconfirmed_until_incoming_authentication_fails() {
    let p = Peer::new();
    let referenced = [7; 32];
    let mut server = p.initiator(&[8; 32]);
    let mut client = p.responder(Limits::default());
    let reply = client
        .receive_handshake(
            &message(&mut server, &referenced, "lt"),
            &[p.credential(referenced)],
        )
        .unwrap();
    assert_eq!(client.phase(), Phase::PendingConfirmation);
    assert!(server.read_message(&reply, &mut [0; 64]).is_err());
    assert_eq!(client.decrypt(&[0; 17]), Err(ChannelError::Authentication));
    assert_eq!(client.phase(), Phase::Closed);
}

#[test]
fn corruption_replay_and_out_of_order_records_close_permanently() {
    for mode in 0..3 {
        let (mut client, mut server) = connected(Limits::default());
        let first = encrypt(&mut server, b"\x04first");
        let second = encrypt(&mut server, b"\x04second");
        let bad = match mode {
            0 => {
                let mut bad = first.clone();
                bad[0] ^= 1;
                bad
            }
            1 => {
                assert!(client.decrypt(&first).is_ok());
                first
            }
            _ => second,
        };
        assert_eq!(client.decrypt(&bad), Err(ChannelError::Authentication));
        assert_eq!(client.phase(), Phase::Closed);
        assert_eq!(client.decrypt(&bad), Err(ChannelError::Phase));
        assert_eq!(client.encrypt(b"\x04later"), Err(ChannelError::Phase));
        assert_eq!(client.selection(), None);
    }
}

#[test]
fn record_limit_includes_tag_and_accepts_exact_maximum() {
    let (mut client, mut server) = connected(Limits::default());
    let plaintext = vec![4; 65519];
    let record = encrypt(&mut server, &plaintext);
    assert_eq!(record.len(), 65535);
    assert_eq!(client.decrypt(&record).unwrap(), plaintext);
    assert_eq!(client.encrypt(&plaintext).unwrap().len(), 65535);
    assert_eq!(client.encrypt(&vec![4; 65520]), Err(ChannelError::Budget));
    assert_eq!(client.phase(), Phase::Closed);
    let (mut client, _) = connected(Limits::default());
    assert_eq!(client.decrypt(&vec![0; 65536]), Err(ChannelError::Budget));
    assert_eq!(client.phase(), Phase::Closed);
}

#[test]
fn caller_plaintext_and_output_budgets_close_before_decrypt_or_encrypt() {
    for outbound in [false, true] {
        let (mut client, mut server) = connected(Limits {
            plaintext_bytes: 4,
            ..Limits::default()
        });
        assert!(client.decrypt(&encrypt(&mut server, b"\x04ok")).is_ok());
        let result = if outbound {
            client.encrypt(b"\x04long")
        } else {
            client.decrypt(&encrypt(&mut server, b"\x04long"))
        };
        assert_eq!(result, Err(ChannelError::Budget));
        assert_eq!(client.phase(), Phase::Closed);
    }
    let p = Peer::new();
    let mut server = p.initiator(&SENTINEL);
    let mut client = p.responder(Limits {
        output_bytes: 49,
        ..Limits::default()
    });
    assert_eq!(
        client.receive_handshake(&message(&mut server, &SENTINEL, "sn"), &[]),
        Err(ChannelError::Budget)
    );
    assert_eq!(client.phase(), Phase::Closed);
}

#[test]
fn empty_plaintext_wrong_phase_and_explicit_cancel_drop_channel_state() {
    let (mut client, mut server) = connected(Limits::default());
    assert_eq!(
        client.decrypt(&encrypt(&mut server, b"")),
        Err(ChannelError::Malformed)
    );
    assert_eq!(client.phase(), Phase::Closed);
    let (mut client, _) = connected(Limits::default());
    assert_eq!(client.encrypt(b"\x04early"), Err(ChannelError::Phase));
    assert_eq!(client.phase(), Phase::Closed);
    let (mut client, _) = connected(Limits::default());
    client.close();
    client.close();
    assert_eq!(client.phase(), Phase::Closed);
    assert_eq!(client.selection(), None);
    assert_eq!(client.decrypt(&[0; 17]), Err(ChannelError::Phase));
}

#[test]
fn init_shapes_identity_and_byte_budget_are_checked_before_noise() {
    let p = Peer::new();
    let private = p.client.private.as_slice().try_into().unwrap();
    for raw in [b"{}".as_slice(),b"[]",b"{\"type\":\"server/init\",\"payload\":{\"version\":true,\"server_id\":\"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"}}"] {
        assert!(matches!(Responder::new(private,&p.client_init,raw,Limits::default()),Err(ChannelError::Malformed)));
    }
    assert!(matches!(
        Responder::new(
            private,
            &p.client_init,
            &p.server_init,
            Limits {
                init_bytes: 1,
                ..Limits::default()
            }
        ),
        Err(ChannelError::Budget)
    ));
    assert!(matches!(
        Responder::new(&[0; 32], &p.client_init, &p.server_init, Limits::default()),
        Err(ChannelError::CredentialBinding)
    ));
}

#[test]
fn debug_and_errors_do_not_print_secrets_or_records() {
    let p = Peer::new();
    let client = p.responder(Limits::default());
    assert_eq!(
        format!("{:?}", p.credential([7; 32])),
        "Credential([REDACTED])"
    );
    assert_eq!(format!("{client:?}"), "Responder { phase: Handshake, .. }");
    assert_eq!(
        ChannelError::Authentication.to_string(),
        "Sendspin channel Authentication"
    );
}

#[test]
fn generated_identities_use_fresh_library_randomness() {
    let first = generate_static_private_key().unwrap();
    let second = generate_static_private_key().unwrap();
    assert!(first != second);
    assert_ne!(
        static_public_key(&first).unwrap(),
        static_public_key(&second).unwrap()
    );
}

#[test]
fn wrong_phase_and_short_records_close_without_retry_state() {
    let p = Peer::new();
    let mut client = p.responder(Limits::default());
    assert_eq!(client.decrypt(&[0; 17]), Err(ChannelError::Phase));
    assert_eq!(client.phase(), Phase::Closed);
    let (mut client, _) = connected(Limits::default());
    assert_eq!(
        client.receive_handshake(&[0; 50], &[]),
        Err(ChannelError::Phase)
    );
    assert_eq!(client.phase(), Phase::Closed);
    for length in 0..16 {
        let (mut client, _) = connected(Limits::default());
        assert_eq!(
            client.decrypt(&vec![0; length]),
            Err(ChannelError::Malformed)
        );
        assert_eq!(client.phase(), Phase::Closed);
    }
    let (mut client, mut server) = connected(Limits::default());
    client.decrypt(&encrypt(&mut server, b"\x04ready")).unwrap();
    assert_eq!(client.encrypt(b""), Err(ChannelError::Malformed));
    assert_eq!(client.phase(), Phase::Closed);
}

#[test]
fn handshake_plaintext_record_and_incoming_output_budgets_close() {
    for limits in [
        Limits {
            handshake_payload_bytes: 1,
            ..Limits::default()
        },
        Limits {
            record_bytes: 50,
            ..Limits::default()
        },
    ] {
        let p = Peer::new();
        let mut server = p.initiator(&SENTINEL);
        let mut client = p.responder(limits);
        assert_eq!(
            client.receive_handshake(&message(&mut server, &SENTINEL, "sn"), &[]),
            Err(ChannelError::Budget)
        );
        assert_eq!(client.phase(), Phase::Closed);
    }
    let (mut client, mut server) = connected(Limits {
        output_bytes: 50,
        ..Limits::default()
    });
    assert_eq!(
        client.decrypt(&encrypt(&mut server, &[4; 51])),
        Err(ChannelError::Budget)
    );
    assert_eq!(client.phase(), Phase::Closed);
    let (mut client, mut server) = connected(Limits {
        output_bytes: 50,
        ..Limits::default()
    });
    client.decrypt(&encrypt(&mut server, b"\x04ready")).unwrap();
    assert_eq!(client.encrypt(&[4; 35]), Err(ChannelError::Budget));
    assert_eq!(client.phase(), Phase::Closed);
}
