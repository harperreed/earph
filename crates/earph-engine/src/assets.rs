// ABOUTME: Stores immutable correction WAV bytes in private owner storage.
// ABOUTME: Verifies content identity and geometry before freezing preparation inputs.
use crate::runtime;
use earph_core::{
    EngineError, ErrorCode,
    dsp_nodes::{FirAssetReference, FirSha256},
    graph::{GraphDocument, NodeKind},
};
use earph_media::{FirCorrection, decode_fir_wav, read_fir_file, read_fir_source};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
};
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "correction store is missing, corrupt, or differs from its reference",
);

/// Owns the original WAV and the coefficients verified from those exact bytes.
#[derive(Debug)]
pub(crate) struct PreparedFirSource {
    bytes: Vec<u8>,
    coefficients: earph_dsp::FirCoefficients,
}
impl PreparedFirSource {
    pub(crate) fn read(path: &Path) -> Result<Self, EngineError> {
        let bytes = read_fir_source(path)?;
        Self::from_bytes(bytes)
    }
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Result<Self, EngineError> {
        let correction = decode_fir_wav(&bytes)?;
        let coefficients = earph_dsp::FirCoefficients::new(correction.reference, correction.taps)
            .map_err(|error| EngineError::new(error.code, error.message))?;
        Ok(Self {
            bytes,
            coefficients,
        })
    }
    pub(crate) fn coefficients(&self) -> &earph_dsp::FirCoefficients {
        &self.coefficients
    }
    pub(crate) fn source_bytes(&self) -> usize {
        self.bytes.len()
    }
    pub(crate) fn original_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn retained_bytes(&self) -> Result<u64, EngineError> {
        (self.bytes.capacity() as u64)
            .checked_add(self.coefficients.resident_bytes())
            .and_then(|bytes| bytes.checked_add(size_of::<Self>() as u64))
            .ok_or(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "correction storage overflow",
            ))
    }
    pub(crate) fn publish(
        &self,
        runtime: &Path,
        check: &mut impl FnMut() -> Result<(), EngineError>,
    ) -> Result<FirAssetReference, EngineError> {
        self.publish_with_sync(runtime, check, &mut File::sync_all)
    }
    fn publish_with_sync(
        &self,
        runtime: &Path,
        check: &mut impl FnMut() -> Result<(), EngineError>,
        sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
    ) -> Result<FirAssetReference, EngineError> {
        check()?;
        let directory = directory(runtime, true)?;
        let reference = self.coefficients.reference();
        let path = directory.join(format!("{}.wav", reference.sha256.as_str()));
        runtime::publish_immutable_file(&path, &self.bytes, check, sync_file)?;
        check()?;
        Ok(reference.clone())
    }
}

/// Shared asset admission for live preparation and offline profile validation.
pub struct GraphAssets<'a> {
    references: BTreeMap<FirSha256, &'a FirAssetReference>,
    declared_bytes: u64,
}
impl<'a> GraphAssets<'a> {
    pub fn inspect(graph: &'a GraphDocument) -> Result<Self, EngineError> {
        let mut references = BTreeMap::new();
        for node in &graph.nodes {
            if let NodeKind::Convolution { asset } = &node.kind {
                asset.validate()?;
                if let Some(prior) = references.insert(asset.sha256.clone(), asset)
                    && prior != asset
                {
                    return Err(EngineError::new(
                        ErrorCode::AssetInvalid,
                        "correction identity has conflicting geometry",
                    ));
                }
            }
        }
        let declared_bytes = references.values().try_fold(0_u64, |n, a| {
            n.checked_add(Self::coefficient_bytes(a)?)
                .ok_or(EngineError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "correction storage exceeds admission",
                ))
        })?;
        Ok(Self {
            references,
            declared_bytes,
        })
    }
    pub(crate) fn coefficient_bytes(reference: &FirAssetReference) -> Result<u64, EngineError> {
        reference.validate()?;
        (reference.taps_per_channel as u64)
            .checked_mul(reference.channels.count() as u64)
            .and_then(|n| n.checked_mul(8))
            .and_then(|n| n.checked_add(1024))
            .ok_or(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "correction storage exceeds admission",
            ))
    }
    pub(crate) fn declared_bytes(&self) -> u64 {
        self.declared_bytes
    }
    pub(crate) fn references(&self) -> impl Iterator<Item = &'a FirAssetReference> + '_ {
        self.references.values().copied()
    }
    /// One bounded source, packet, decoder buffer, and metadata during sequential resolution.
    pub fn scratch_bytes(&self) -> u64 {
        if self.references.is_empty() {
            0
        } else {
            8 * 1024 * 1024
        }
    }
    pub fn resolve(
        &self,
        runtime_path: Option<&Path>,
        allowance: u64,
        check: impl FnMut() -> Result<(), EngineError>,
    ) -> Result<Vec<earph_dsp::FirCoefficients>, EngineError> {
        self.resolve_with(allowance, check, |reference| {
            coefficients(runtime_path.ok_or(INVALID)?, reference)
        })
    }
    pub(crate) fn resolve_with(
        &self,
        allowance: u64,
        mut check: impl FnMut() -> Result<(), EngineError>,
        mut resolve_asset: impl FnMut(
            &FirAssetReference,
        ) -> Result<earph_dsp::FirCoefficients, EngineError>,
    ) -> Result<Vec<earph_dsp::FirCoefficients>, EngineError> {
        if self.declared_bytes > allowance {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "correction storage exceeds admission",
            ));
        }
        let mut resolved = Vec::with_capacity(self.references.len());
        for reference in self.references.values() {
            check()?;
            let coefficients = resolve_asset(reference)?;
            if coefficients.reference() != *reference {
                return Err(INVALID);
            }
            resolved.push(coefficients);
        }
        check()?;
        Ok(resolved)
    }
}

pub(crate) fn coefficients(
    runtime: &Path,
    reference: &FirAssetReference,
) -> Result<earph_dsp::FirCoefficients, EngineError> {
    let correction = resolve(runtime, reference)?;
    earph_dsp::FirCoefficients::new(correction.reference, correction.taps)
        .map_err(|error| EngineError::new(error.code, error.message))
}

fn directory(runtime_path: &Path, create: bool) -> Result<PathBuf, EngineError> {
    let runtime = runtime::directory(runtime_path, create)?;
    let path = runtime.join("assets");
    if create {
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(runtime::IO),
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| INVALID)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "correction storage must be private and owned by the current user",
        ));
    }
    Ok(path)
}

/// Import without replacing any source or existing object. Interrupted files remain inspectable.
pub fn import(runtime_path: &Path, source: &Path) -> Result<FirAssetReference, EngineError> {
    let runtime_path = runtime::directory(runtime_path, true)?;
    let _owner_lock =
        runtime::OwnerLock::try_acquire(runtime::open_lock(&runtime_path.join("owner.lock"))?)
            .map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => EngineError::new(
                    ErrorCode::SourceBusy,
                    "the owner must import corrections while its runtime is locked",
                ),
                std::fs::TryLockError::Error(_) => runtime::IO,
            })?;
    import_with_sync(&runtime_path, source, &mut File::sync_all)
}
pub(crate) fn import_owned(
    runtime_path: &Path,
    source: &Path,
) -> Result<FirAssetReference, EngineError> {
    import_with_sync(runtime_path, source, &mut File::sync_all)
}

/// Forward a legacy FIR CLI request to the locked owner without opening a second writer.
pub fn import_via_owner(
    runtime_path: &Path,
    source: &Path,
) -> Result<FirAssetReference, EngineError> {
    use earph_core::{
        control::{Command, CommandEnvelope},
        dsp_nodes::FirPurpose,
        ids::{OwnerId, RequestId, SessionId, ZoneId},
    };
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    let selected = source.canonicalize().map_err(|_| INVALID)?;
    let path = selected
        .to_str()
        .ok_or(EngineError::new(
            ErrorCode::AssetInvalid,
            "owner FIR forwarding requires UTF-8 paths",
        ))?
        .to_owned();
    let generation = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| runtime::IO)?
        .as_nanos();
    let request_id = format!("fir-{}-{generation:x}", std::process::id());
    let query = CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(format!("{request_id}-state"))?,
        zone_id: ZoneId::new("main")?,
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::State,
    };
    let bytes = crate::request(
        runtime_path,
        &serde_json::to_vec(&query).map_err(|_| runtime::IO)?,
    )?;
    let reply: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    let state = &reply["receipt"]["result"]["value"];
    let owner_id: OwnerId =
        serde_json::from_value(state["owner_id"].clone()).map_err(|_| INVALID)?;
    let session_id: Option<SessionId> =
        serde_json::from_value(state["session_id"].clone()).map_err(|_| INVALID)?;
    let command = CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(request_id)?,
        zone_id: ZoneId::new("main")?,
        owner_id: Some(owner_id),
        session_id,
        expected_revision: Some(state["revision"].as_u64().ok_or(INVALID)?),
        payload: Command::FirImport {
            path,
            purpose: FirPurpose::Correction,
        },
    };
    command.validate()?;
    let bytes = serde_json::to_vec(&command).map_err(|_| runtime::IO)?;
    let deadline = Instant::now() + Duration::from_secs(7);
    loop {
        let response = crate::request(runtime_path, &bytes)?;
        let reply: serde_json::Value = serde_json::from_slice(&response).map_err(|_| INVALID)?;
        let receipt = &reply["receipt"];
        match receipt["status"].as_str() {
            Some("applied") => {
                return serde_json::from_value(receipt["result"]["value"].clone())
                    .map_err(|_| INVALID);
            }
            Some("accepted") if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(2))
            }
            Some("accepted") => {
                return Err(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "owner correction result exceeded the reconciliation deadline",
                ));
            }
            _ => {
                return Err(EngineError::new(
                    serde_json::from_value(receipt["error"]["code"].clone())
                        .unwrap_or(ErrorCode::InternalFault),
                    "the owner rejected the FIR correction import",
                ));
            }
        }
    }
}
fn import_with_sync(
    runtime_path: &Path,
    source: &Path,
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
) -> Result<FirAssetReference, EngineError> {
    directory(runtime_path, true)?;
    PreparedFirSource::read(source)?.publish_with_sync(runtime_path, &mut || Ok(()), sync_file)
}

/// Resolve immutable bytes on the preparation worker, never during processing or activation.
pub fn resolve(
    runtime_path: &Path,
    reference: &FirAssetReference,
) -> Result<FirCorrection, EngineError> {
    resolve_with_size(runtime_path, reference).map(|(correction, _)| correction)
}
pub(crate) fn resolve_with_size(
    runtime_path: &Path,
    reference: &FirAssetReference,
) -> Result<(FirCorrection, usize), EngineError> {
    reference.validate()?;
    let directory = directory(runtime_path, false)?;
    let file =
        runtime::open_private_file(&directory.join(format!("{}.wav", reference.sha256.as_str())))?
            .ok_or(INVALID)?;
    let bytes = read_fir_file(file)?;
    let correction = decode_fir_wav(&bytes)?;
    if &correction.reference != reference {
        return Err(INVALID);
    }
    Ok((correction, bytes.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

    #[test]
    fn existing_complete_fir_is_file_synced_before_import_succeeds() {
        let runtime = PathBuf::from("/tmp").join(format!(
            "efd-{:x}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(44_u32.to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(3_u16.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(48000_u32.to_le_bytes());
        bytes.extend(384000_u32.to_le_bytes());
        bytes.extend(8_u16.to_le_bytes());
        bytes.extend(64_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(8_u32.to_le_bytes());
        bytes.extend(0.5_f64.to_le_bytes());
        let source = runtime.join("authored.wav");
        fs::write(&source, &bytes).unwrap();
        let expected = decode_fir_wav(&bytes).unwrap().reference;
        let directory = directory(&runtime, true).unwrap();
        let path = directory.join(format!("{}.wav", expected.sha256.as_str()));
        let mut interrupted = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        interrupted.write_all(&bytes).unwrap();
        let metadata = interrupted.metadata().unwrap();
        drop(interrupted);
        let mut synced = 0;
        let observed = import_with_sync(&runtime, &source, &mut |file| {
            let observed = file.metadata()?;
            assert_eq!(
                (observed.dev(), observed.ino()),
                (metadata.dev(), metadata.ino())
            );
            file.sync_all()?;
            synced += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(observed, expected);
        assert_eq!(synced, 1, "retry did not synchronize the verified FIR file");
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
