// ABOUTME: Provides deterministic offline tools for engine qualification.
// ABOUTME: Keeps filesystem access and evidence work outside the audio core.
#![forbid(unsafe_code)]
pub mod catalog;
pub mod inventory;

use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn read_bounded(path: &Path, max_bytes: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "cannot open input".to_string())?;
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read input".to_string())?;
    if bytes.len() > max_bytes {
        return Err("input exceeds byte limit".into());
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
