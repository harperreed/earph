// ABOUTME: Encodes one bounded committed Sendspin identity and pairing catalog.
// ABOUTME: Treats its bytes as secrets without selecting storage or generating identity.
use crate::{
    noise::SENTINEL,
    pairing::{PairingRecord, RecordCatalog},
};
use serde::{Deserialize, Serialize};
use std::fmt;

const DOCUMENT_BYTES: usize = 65536;
const CATALOG_CAPACITY: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialError {
    Budget,
    InvalidDocument,
    UnsupportedVersion,
}
impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Budget => "credential document exceeds its byte budget",
            Self::InvalidDocument => "invalid credential document",
            Self::UnsupportedVersion => "unsupported credential document version",
        })
    }
}
impl std::error::Error for CredentialError {}

pub struct EncodedCredentials(Vec<u8>);
impl EncodedCredentials {
    /// The caller must keep these plaintext bytes inside its credential-storage boundary.
    pub fn as_secret_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl fmt::Debug for EncodedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncodedCredentials([REDACTED])")
    }
}

pub struct CommittedCredentials {
    static_private: [u8; 32],
    pairing_psk: [u8; 32],
    catalog: RecordCatalog,
    last_playback_server: Option<[u8; 32]>,
}
impl fmt::Debug for CommittedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CommittedCredentials([REDACTED])")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    static_private: [u8; 32],
    pairing_psk: [u8; 32],
    catalog: Catalog,
    #[serde(deserialize_with = "Option::deserialize")]
    last_playback_server: Option<[u8; 32]>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    capacity: usize,
    records: Vec<Record>,
    superseded_ids: Vec<[u8; 32]>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    server_public: [u8; 32],
    psk: [u8; 32],
    last_used_order: u64,
}

impl CommittedCredentials {
    /// Accept already generated identity and already committed catalog state.
    pub fn new(
        static_private: [u8; 32],
        pairing_psk: [u8; 32],
        catalog: RecordCatalog,
        last_playback_server: Option<[u8; 32]>,
    ) -> Result<Self, CredentialError> {
        if catalog.capacity() != CATALOG_CAPACITY
            || pairing_psk == SENTINEL
            || catalog.records().iter().any(|r| r.psk == pairing_psk)
        {
            return Err(CredentialError::InvalidDocument);
        }
        Ok(Self {
            static_private,
            pairing_psk,
            catalog,
            last_playback_server,
        })
    }

    pub fn static_private(&self) -> &[u8; 32] {
        &self.static_private
    }
    pub fn pairing_psk(&self) -> &[u8; 32] {
        &self.pairing_psk
    }
    pub fn catalog(&self) -> &RecordCatalog {
        &self.catalog
    }
    pub fn last_playback_server(&self) -> Option<[u8; 32]> {
        self.last_playback_server
    }

    pub fn encode_secret(&self) -> Result<EncodedCredentials, CredentialError> {
        let document = Document {
            schema_version: 1,
            static_private: self.static_private,
            pairing_psk: self.pairing_psk,
            catalog: Catalog {
                capacity: self.catalog.capacity(),
                records: self
                    .catalog
                    .records()
                    .iter()
                    .map(|r| Record {
                        server_public: r.server_public,
                        psk: r.psk,
                        last_used_order: r.last_used_order,
                    })
                    .collect(),
                superseded_ids: self.catalog.superseded_ids().to_vec(),
            },
            last_playback_server: self.last_playback_server,
        };
        let bytes = serde_json::to_vec(&document).map_err(|_| CredentialError::InvalidDocument)?;
        if bytes.len() > DOCUMENT_BYTES {
            return Err(CredentialError::Budget);
        }
        Ok(EncodedCredentials(bytes))
    }

    /// Failure yields no replacement identity and performs no storage operation.
    pub fn decode_secret(bytes: &[u8]) -> Result<Self, CredentialError> {
        if bytes.len() > DOCUMENT_BYTES {
            return Err(CredentialError::Budget);
        }
        let document: Document =
            serde_json::from_slice(bytes).map_err(|_| CredentialError::InvalidDocument)?;
        if document.schema_version != 1 {
            return Err(CredentialError::UnsupportedVersion);
        }
        let catalog = RecordCatalog::from_committed(
            document.catalog.capacity,
            document
                .catalog
                .records
                .into_iter()
                .map(|r| PairingRecord {
                    server_public: r.server_public,
                    psk: r.psk,
                    last_used_order: r.last_used_order,
                })
                .collect(),
            document.catalog.superseded_ids,
        )
        .map_err(|_| CredentialError::InvalidDocument)?;
        Self::new(
            document.static_private,
            document.pairing_psk,
            catalog,
            document.last_playback_server,
        )
    }
}
