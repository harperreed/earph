// ABOUTME: Restricts operational files to the current local user.
// ABOUTME: Writes durable metadata atomically and preserves abandoned artifacts.
use earph_core::{EngineError, ErrorCode};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Component, Path, PathBuf},
};

pub(crate) const IO: EngineError =
    EngineError::new(ErrorCode::InternalFault, "local operational storage failed");
const PRIVATE: EngineError = EngineError::new(
    ErrorCode::AuthDenied,
    "runtime paths must be private and owned by the current user",
);
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid runtime document or path");

pub(crate) fn directory(path: &Path, create: bool) -> Result<PathBuf, EngineError> {
    if !path.is_absolute()
        || path.as_os_str().as_bytes().len() > 60
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(INVALID);
    }
    let parent = path
        .parent()
        .ok_or(INVALID)?
        .canonicalize()
        .map_err(|_| IO)?;
    let uid = rustix::process::geteuid().as_raw();
    for ancestor in parent.ancestors() {
        let metadata = fs::metadata(ancestor).map_err(|_| IO)?;
        // Root-owned sticky directories prevent other users from replacing our child.
        let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if !metadata.is_dir()
            || ![0, uid].contains(&metadata.uid())
            || (metadata.mode() & 0o022 != 0 && !sticky_root)
        {
            return Err(PRIVATE);
        }
    }
    // Pin the resolved parent. Every lock, socket, and checkpoint must use this same path.
    let path = parent.join(path.file_name().ok_or(INVALID)?);
    if path.as_os_str().as_bytes().len() > 60 {
        return Err(INVALID);
    }
    if create {
        match fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(IO),
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|_| IO)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err(PRIVATE);
    }
    Ok(path)
}

fn validate_file(metadata: &fs::Metadata) -> Result<(), EngineError> {
    if !metadata.is_file()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        Err(PRIVATE)
    } else {
        Ok(())
    }
}

pub(crate) fn open_lock(path: &Path) -> Result<File, EngineError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(IO),
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|_| IO)?;
    validate_file(&file.metadata().map_err(|_| IO)?)?;
    Ok(file)
}

pub(crate) fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, EngineError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IO),
    };
    validate_file(&metadata)?;
    let file = File::open(path).map_err(|_| IO)?;
    validate_file(&file.metadata().map_err(|_| IO)?)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| IO)?;
    if bytes.len() > limit {
        return Err(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "runtime document exceeds byte limit",
        ));
    }
    Ok(Some(bytes))
}

pub(crate) fn write_atomic(
    directory: &Path,
    name: &str,
    token: &str,
    bytes: &[u8],
) -> Result<(), EngineError> {
    let destination = directory.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => validate_file(&metadata)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(IO),
    }
    let temporary = directory.join(format!(".{name}.{token}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| IO)?;
    validate_file(&file.metadata().map_err(|_| IO)?)?;
    file.write_all(bytes).map_err(|_| IO)?;
    file.sync_all().map_err(|_| IO)?;
    fs::rename(temporary, destination).map_err(|_| IO)?;
    File::open(directory)
        .and_then(|file| file.sync_all())
        .map_err(|_| IO)
}
