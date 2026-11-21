// ABOUTME: Restores synthetic credentials from a real file into an authenticated Noise exchange.
// ABOUTME: Retains its private fixture and makes no OS-store or power-loss durability claim.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_sendspin::{
    credentials::CommittedCredentials,
    noise::{Category, Limits, PATTERN, Responder, static_public_key},
    pairing::{PairingRecord, RecordCatalog, psk_id},
};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn restored_file_drives_authenticated_exchange_with_original_identity_and_psk() {
    let server_private = [0x11; 32];
    let server_public = static_public_key(&server_private).unwrap();
    let original = CommittedCredentials::new(
        [0x44; 32],
        [0x55; 32],
        RecordCatalog::from_committed(
            5,
            vec![PairingRecord {
                server_public,
                psk: [0x33; 32],
                last_used_order: 41,
            }],
            vec![],
        )
        .unwrap(),
        Some(server_public),
    )
    .unwrap();
    let encoded = original.encode_secret().unwrap();
    let name = format!(
        "earph-synthetic-credentials-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let file = std::env::temp_dir().join(name);
    let mut writer = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&file)
        .unwrap();
    writer.write_all(encoded.as_secret_bytes()).unwrap();
    writer.sync_all().unwrap();
    drop(writer);
    drop(original);
    drop(encoded);
    let mut bytes = vec![];
    std::fs::File::open(&file)
        .unwrap()
        .take(65537)
        .read_to_end(&mut bytes)
        .unwrap();
    let restored = CommittedCredentials::decode_secret(&bytes).unwrap();
    assert_eq!(restored.static_private(), &[0x44; 32]);
    assert_eq!(restored.pairing_psk(), &[0x55; 32]);
    assert_eq!(restored.catalog().next_order().unwrap(), 42);
    assert_eq!(restored.last_playback_server(), Some(server_public));

    let client_public = static_public_key(restored.static_private()).unwrap();
    let client_init = serde_json::to_vec(&serde_json::json!({
        "type":"client/init", "payload":{
            "version":1, "suite":"25519_ChaChaPoly_SHA256",
            "client_id":URL_SAFE_NO_PAD.encode(client_public)
        }
    }))
    .unwrap();
    let server_init = serde_json::to_vec(&serde_json::json!({
        "type":"server/init", "payload":{
            "version":1,"server_id":URL_SAFE_NO_PAD.encode(server_public)
        }
    }))
    .unwrap();
    let mut client = Responder::new(
        restored.static_private(),
        &client_init,
        &server_init,
        Limits::default(),
    )
    .unwrap();
    let mut server = snow::Builder::new(PATTERN.parse().unwrap())
        .local_private_key(&server_private)
        .unwrap()
        .remote_public_key(&client_public)
        .unwrap()
        .prologue(&[client_init.as_slice(), server_init.as_slice()].concat())
        .unwrap()
        .psk(2, &[0x33; 32])
        .unwrap()
        .build_initiator()
        .unwrap();
    let payload = serde_json::to_vec(&serde_json::json!({
        "psk_category":"lt", "psk_id":URL_SAFE_NO_PAD.encode(psk_id(&[0x33;32]))
    }))
    .unwrap();
    let mut buffer = [0; 1024];
    let n = server.write_message(&payload, &mut buffer).unwrap();
    let record = &restored.catalog().records()[0];
    let reply = client
        .receive_handshake(
            &buffer[..n],
            &[earph_sendspin::noise::Credential::LongTerm {
                psk: record.psk,
                server_public: record.server_public,
            }],
        )
        .unwrap();
    assert_eq!(client.selection().unwrap().category, Category::LongTerm);
    assert!(!client.selection().unwrap().sentinel_fallback);
    assert_eq!(server.read_message(&reply, &mut buffer).unwrap(), 2);
    let mut server = server.into_transport_mode().unwrap();
    let n = server
        .write_message(b"restored credential exchange", &mut buffer)
        .unwrap();
    assert_eq!(
        client.decrypt(&buffer[..n]).unwrap(),
        b"restored credential exchange"
    );
}
