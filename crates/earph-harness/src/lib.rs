// ABOUTME: Provides deterministic offline tools for engine qualification.
// ABOUTME: Keeps filesystem access and evidence work outside the audio core.
#![forbid(unsafe_code)]
pub mod catalog;
pub mod evidence;
pub mod fixture;
pub mod inventory;
pub mod render;
pub mod virtual_output;

use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn build_provenance() -> serde_json::Value {
    serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/build-provenance.json"
    )))
    .expect("Cargo-generated provenance is valid JSON")
}

pub fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut file = File::open(path).map_err(|_| "cannot open input".to_string())?;
    let metadata = file.metadata().map_err(|_| "cannot inspect input")?;
    let length = usize::try_from(metadata.len()).map_err(|_| "input exceeds addressable size")?;
    if !metadata.is_file() || length > max_bytes {
        return Err("input is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| "cannot reserve input buffer")?;
    bytes.resize(length, 0);
    file.read_exact(&mut bytes)
        .map_err(|_| "cannot read complete input")?;
    let mut extra = [0];
    if file
        .read(&mut extra)
        .map_err(|_| "cannot verify input length")?
        != 0
    {
        return Err("input changed while reading".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hash_matches_published_sha256_vector() {
        assert_eq!(
            super::sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
