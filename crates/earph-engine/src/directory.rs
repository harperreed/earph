// ABOUTME: Expands one local directory into a deterministic lossless file list.
// ABOUTME: Bounds scans and rejects oversized imports before any queue mutation.
use earph_core::{EngineError, ErrorCode};
use std::{fs, path::Path};

const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "directory is unavailable or contains an invalid UTF-8 path",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "directory exceeds scan or queue bounds",
);

pub(crate) fn paths(path: &str) -> Result<Vec<String>, EngineError> {
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        return Err(INVALID);
    }
    let directory = fs::read_dir(Path::new(path)).map_err(|_| INVALID)?;
    let mut paths = Vec::new();
    for (index, entry) in directory.enumerate() {
        if index >= 4096 {
            return Err(BUDGET);
        }
        let entry = entry.map_err(|_| INVALID)?;
        if !entry.file_type().map_err(|_| INVALID)?.is_file() {
            continue;
        }
        let path = entry.path();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(extension.as_str(), "wav" | "flac" | "aiff" | "aif") {
            continue;
        }
        if paths.len() == 256 {
            return Err(BUDGET);
        }
        paths.push(path.to_str().ok_or(INVALID)?.to_owned());
    }
    paths.sort_unstable();
    Ok(paths)
}
