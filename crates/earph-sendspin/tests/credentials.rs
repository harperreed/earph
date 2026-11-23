// ABOUTME: Checks committed Sendspin secret documents through their public byte boundary.
// ABOUTME: Uses synthetic keys and real catalog validation without an OS credential store.
use earph_sendspin::{
    credentials::{CommittedCredentials, CredentialError},
    pairing::{PairingRecord, RecordCatalog},
};
use serde_json::Value;

#[test]
fn secret_document_restores_identity_catalog_and_last_playback_server() {
    let catalog = RecordCatalog::from_committed(
        5,
        vec![PairingRecord {
            server_public: [0x22; 32],
            psk: [0x33; 32],
            last_used_order: 41,
        }],
        vec![],
    )
    .unwrap();
    let credentials =
        CommittedCredentials::new([0x44; 32], [0x55; 32], catalog, Some([0x66; 32])).unwrap();
    let encoded = credentials.encode_secret().unwrap();
    let json: Value = serde_json::from_slice(encoded.as_secret_bytes()).unwrap();
    assert_eq!(json["schema_version"], 1);
    assert_eq!(json["catalog"]["records"][0]["last_used_order"], 41);
    let restored = CommittedCredentials::decode_secret(encoded.as_secret_bytes()).unwrap();
    assert_eq!(restored.static_private(), &[0x44; 32]);
    assert_eq!(restored.pairing_psk(), &[0x55; 32]);
    assert_eq!(restored.last_playback_server(), Some([0x66; 32]));
    assert_eq!(restored.catalog().capacity(), 5);
    assert_eq!(restored.catalog().records()[0].server_public, [0x22; 32]);
    assert_eq!(restored.catalog().records()[0].psk, [0x33; 32]);
    assert_eq!(restored.catalog().next_order().unwrap(), 42);
    assert_eq!(
        format!("{credentials:?}"),
        "CommittedCredentials([REDACTED])"
    );
    assert_eq!(format!("{encoded:?}"), "EncodedCredentials([REDACTED])");
}

fn document() -> Value {
    serde_json::json!({
        "schema_version": 1,
        "static_private": vec![0x44_u8; 32],
        "pairing_psk": vec![0x55_u8; 32],
        "catalog": { "capacity": 5, "records": [], "superseded_ids": [] },
        "last_playback_server": null,
    })
}

#[test]
fn missing_nullable_field_is_corrupt_instead_of_silently_resetting_history() {
    let mut value = document();
    value
        .as_object_mut()
        .unwrap()
        .remove("last_playback_server");
    assert_eq!(
        CommittedCredentials::decode_secret(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        CredentialError::InvalidDocument
    );
}

#[test]
fn corrupt_and_unsupported_documents_cannot_produce_credentials() {
    let mut cases = vec![];
    let mut value = document();
    value["schema_version"] = 2.into();
    cases.push((value, CredentialError::UnsupportedVersion));
    let mut value = document();
    value["unreviewed_field"] = true.into();
    cases.push((value, CredentialError::InvalidDocument));
    let mut value = document();
    value["catalog"]["unreviewed_field"] = true.into();
    cases.push((value, CredentialError::InvalidDocument));
    let mut value = document();
    value["static_private"] = serde_json::json!(vec![0x44_u8; 31]);
    cases.push((value, CredentialError::InvalidDocument));
    let mut value = document();
    value["catalog"]["capacity"] = 6.into();
    cases.push((value, CredentialError::InvalidDocument));
    let mut value = document();
    value["catalog"]["superseded_ids"] = serde_json::json!(vec![[0x66_u8; 32]]);
    cases.push((value, CredentialError::InvalidDocument));
    for (value, error) in cases {
        assert_eq!(
            CommittedCredentials::decode_secret(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
            error
        );
    }
    let duplicate = serde_json::to_string(&document()).unwrap().replace(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
    );
    assert_eq!(
        CommittedCredentials::decode_secret(duplicate.as_bytes()).unwrap_err(),
        CredentialError::InvalidDocument
    );
    for invalid in [b"{}".as_slice(), b"null", b"[", b"\xff", b"{}{}"] {
        assert_eq!(
            CommittedCredentials::decode_secret(invalid).unwrap_err(),
            CredentialError::InvalidDocument
        );
    }
}

#[test]
fn document_budget_includes_whitespace_and_accepts_its_exact_boundary() {
    let mut bytes = serde_json::to_vec(&document()).unwrap();
    bytes.resize(65536, b' ');
    assert!(CommittedCredentials::decode_secret(&bytes).is_ok());
    bytes.push(b' ');
    assert_eq!(
        CommittedCredentials::decode_secret(&bytes).unwrap_err(),
        CredentialError::Budget
    );
}

#[test]
fn restored_catalog_preserves_supersession_and_counter_exhaustion() {
    let old_id = earph_sendspin::pairing::psk_id(&[0x33; 32]);
    let catalog = RecordCatalog::from_committed(
        5,
        vec![
            PairingRecord {
                server_public: [0x22; 32],
                psk: [0x33; 32],
                last_used_order: 41,
            },
            PairingRecord {
                server_public: [0x22; 32],
                psk: [0x34; 32],
                last_used_order: u64::MAX,
            },
        ],
        vec![old_id],
    )
    .unwrap();
    let original = CommittedCredentials::new([0x44; 32], [0x55; 32], catalog, None).unwrap();
    let encoded = original.encode_secret().unwrap();
    let restored = CommittedCredentials::decode_secret(encoded.as_secret_bytes()).unwrap();
    assert_eq!(restored.catalog().superseded_ids(), &[old_id]);
    assert_eq!(restored.catalog().records().len(), 2);
    assert_eq!(
        restored.catalog().next_order(),
        Err(earph_sendspin::pairing::PairingError::CounterExhausted)
    );
    let released = restored.catalog().plan_release(&[]).unwrap();
    assert_eq!(released.replacement.records().len(), 1);
    assert_eq!(released.replacement.records()[0].psk, [0x34; 32]);
    assert_eq!(restored.last_playback_server(), None);
}

#[test]
fn public_constructor_and_decoder_reject_credential_category_aliases() {
    let catalog = RecordCatalog::new(5).unwrap();
    assert_eq!(
        CommittedCredentials::new([0x44; 32], earph_sendspin::noise::SENTINEL, catalog, None)
            .unwrap_err(),
        CredentialError::InvalidDocument
    );
    let catalog = RecordCatalog::from_committed(
        5,
        vec![PairingRecord {
            server_public: [0x22; 32],
            psk: [0x55; 32],
            last_used_order: 1,
        }],
        vec![],
    )
    .unwrap();
    assert_eq!(
        CommittedCredentials::new([0x44; 32], [0x55; 32], catalog, None).unwrap_err(),
        CredentialError::InvalidDocument
    );
    let mut value = document();
    value["pairing_psk"] = serde_json::json!(earph_sendspin::noise::SENTINEL);
    assert_eq!(
        CommittedCredentials::decode_secret(&serde_json::to_vec(&value).unwrap()).unwrap_err(),
        CredentialError::InvalidDocument
    );
}
