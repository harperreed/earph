// ABOUTME: Stores immutable correction WAV bytes in private owner storage.
// ABOUTME: Verifies content identity and geometry before freezing preparation inputs.
use crate::runtime;
use earph_core::{EngineError, ErrorCode, dsp_nodes::FirAssetReference};
use earph_media::{FirCorrection, decode_fir_wav, read_fir_file, read_fir_source};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "correction store is missing, corrupt, or differs from its reference",
);

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
    let directory = directory(runtime_path, true)?;
    let bytes = read_fir_source(source)?;
    let reference = decode_fir_wav(&bytes)?.reference;
    let path = directory.join(format!("{}.wav", reference.sha256.as_str()));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| runtime::IO)?;
            File::open(&directory)
                .and_then(|f| f.sync_all())
                .map_err(|_| runtime::IO)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let observed = read_fir_file(runtime::open_private_file(&path)?.ok_or(INVALID)?)?;
            if observed != bytes {
                return Err(INVALID);
            }
        }
        Err(_) => return Err(runtime::IO),
    }
    let observed = read_fir_file(runtime::open_private_file(&path)?.ok_or(INVALID)?)?;
    if observed != bytes {
        return Err(INVALID);
    }
    Ok(reference)
}

/// Resolve immutable bytes on the preparation worker, never during processing or activation.
pub fn resolve(
    runtime_path: &Path,
    reference: &FirAssetReference,
) -> Result<FirCorrection, EngineError> {
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
    Ok(correction)
}
