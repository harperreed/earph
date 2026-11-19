// ABOUTME: Restricts operational files to the current local user.
// ABOUTME: Writes durable metadata atomically and preserves abandoned artifacts.
use earph_core::{EngineError, ErrorCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
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

pub(crate) const MAX_CHECKPOINT_BYTES: usize = 2 * crate::transport::MAX_BYTES;
const MAX_RECOVERY_BYTES: usize = 6 * MAX_CHECKPOINT_BYTES + 512;
const RECOVERY_FILE: &str = "checkpoint-recovery.json";
const UNCERTAIN: EngineError = EngineError::new(
    ErrorCode::InternalFault,
    "checkpoint outcome is uncertain; startup recovery is required",
);

#[derive(Debug, Clone, Copy)]
pub(crate) enum CheckpointError {
    Storage(EngineError),
    Rejected(EngineError),
}
impl CheckpointError {
    pub(crate) fn error(self) -> EngineError {
        match self {
            Self::Storage(error) | Self::Rejected(error) => error,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum RecoveryRecord {
    Pending {
        schema_version: u32,
        #[serde(deserialize_with = "deserialize_previous")]
        previous: Option<String>,
    },
    Settled {
        schema_version: u32,
        checkpoint_sha256: String,
    },
}
fn deserialize_previous<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

#[derive(Debug, Clone, Copy)]
enum ReplacementError {
    Unchanged(EngineError),
    Uncertain,
}
impl ReplacementError {
    fn error(self) -> EngineError {
        match self {
            Self::Unchanged(error) => error,
            Self::Uncertain => UNCERTAIN,
        }
    }
}

fn sync_directory(directory: &Path) -> std::io::Result<()> {
    File::open(directory)?.sync_all()
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn encode_record(record: &RecoveryRecord) -> Result<Vec<u8>, EngineError> {
    let bytes = serde_json::to_vec(record).map_err(|_| INVALID)?;
    if bytes.len() > MAX_RECOVERY_BYTES {
        return Err(INVALID);
    }
    Ok(bytes)
}
fn settle_checkpoint(
    directory: &Path,
    token: &str,
    bytes: &[u8],
    sync: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), EngineError> {
    let record = encode_record(&RecoveryRecord::Settled {
        schema_version: 1,
        checkpoint_sha256: digest(bytes),
    })?;
    replace_atomic(directory, RECOVERY_FILE, token, &record, sync).map_err(ReplacementError::error)
}

pub(crate) fn read_checkpoint(
    directory: &Path,
    token: &str,
) -> Result<Option<Vec<u8>>, EngineError> {
    read_checkpoint_with_sync(directory, token, &mut sync_directory)
}
fn read_checkpoint_with_sync(
    directory: &Path,
    token: &str,
    sync: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<Option<Vec<u8>>, EngineError> {
    let Some(record) = read(&directory.join(RECOVERY_FILE), MAX_RECOVERY_BYTES)? else {
        return read(&directory.join("queue.json"), MAX_CHECKPOINT_BYTES);
    };
    // Confirm the observed directory entries before interpreting an uncertain commit.
    sync(directory).map_err(|_| UNCERTAIN)?;
    match serde_json::from_slice::<RecoveryRecord>(&record).map_err(|_| INVALID)? {
        RecoveryRecord::Pending {
            schema_version: 1,
            previous,
        } => {
            let Some(previous) = previous else {
                // Keep the pending record and abandoned candidate until a later transaction.
                return Ok(None);
            };
            if previous.len() > MAX_CHECKPOINT_BYTES {
                return Err(INVALID);
            }
            let current = match read(&directory.join("queue.json"), MAX_CHECKPOINT_BYTES) {
                Ok(bytes) => bytes,
                Err(error) if error.code == ErrorCode::ResourceBudgetExceeded => None,
                Err(error) => return Err(error),
            };
            if current.as_deref() == Some(previous.as_bytes()) {
                return Ok(Some(previous.into_bytes()));
            }
            replace_atomic(
                directory,
                "queue.json",
                &format!("{token}-recover"),
                previous.as_bytes(),
                sync,
            )
            .map_err(ReplacementError::error)?;
            settle_checkpoint(
                directory,
                &format!("{token}-recovered"),
                previous.as_bytes(),
                sync,
            )?;
            Ok(Some(previous.into_bytes()))
        }
        RecoveryRecord::Settled {
            schema_version: 1,
            checkpoint_sha256,
        } => {
            let bytes =
                read(&directory.join("queue.json"), MAX_CHECKPOINT_BYTES)?.ok_or(INVALID)?;
            if digest(&bytes) != checkpoint_sha256 {
                return Err(INVALID);
            }
            Ok(Some(bytes))
        }
        _ => Err(INVALID),
    }
}

pub(crate) fn write_checkpoint(
    directory: &Path,
    token: &str,
    bytes: &[u8],
) -> Result<(), EngineError> {
    checkpoint_transaction(directory, token, bytes, || Ok(())).map_err(CheckpointError::error)
}
pub(crate) fn checkpoint_transaction(
    directory: &Path,
    token: &str,
    bytes: &[u8],
    validate: impl FnOnce() -> Result<(), EngineError>,
) -> Result<(), CheckpointError> {
    checkpoint_transaction_with_sync(directory, token, bytes, validate, &mut sync_directory)
}
fn checkpoint_transaction_with_sync(
    directory: &Path,
    token: &str,
    bytes: &[u8],
    validate: impl FnOnce() -> Result<(), EngineError>,
    sync: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), CheckpointError> {
    let storage = CheckpointError::Storage;
    if bytes.len() > MAX_CHECKPOINT_BYTES {
        return Err(storage(INVALID));
    }
    let previous =
        read_checkpoint_with_sync(directory, &format!("{token}-prior"), sync).map_err(storage)?;
    let previous = previous
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| storage(INVALID))?;
    let pending = encode_record(&RecoveryRecord::Pending {
        schema_version: 1,
        previous: previous.clone(),
    })
    .map_err(storage)?;
    replace_atomic(
        directory,
        RECOVERY_FILE,
        &format!("{token}-pending"),
        &pending,
        sync,
    )
    .map_err(|error| storage(error.error()))?;
    replace_atomic(directory, "queue.json", token, bytes, sync)
        .map_err(|error| storage(error.error()))?;
    if let Err(error) = validate() {
        if let Some(previous) = previous {
            replace_atomic(
                directory,
                "queue.json",
                &format!("{token}-rollback"),
                previous.as_bytes(),
                sync,
            )
            .map_err(|_| storage(UNCERTAIN))?;
            settle_checkpoint(
                directory,
                &format!("{token}-rolled-back"),
                previous.as_bytes(),
                sync,
            )
            .map_err(|_| storage(UNCERTAIN))?;
        }
        return Err(CheckpointError::Rejected(error));
    }
    settle_checkpoint(directory, &format!("{token}-settled"), bytes, sync)
        .map_err(|_| storage(UNCERTAIN))
}

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

pub(crate) struct OwnerLock(File);
impl OwnerLock {
    pub(crate) fn try_acquire(file: File) -> Result<Self, std::fs::TryLockError> {
        file.try_lock()?;
        Ok(Self(file))
    }
}
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // A concurrent pre-exec child can still hold this open file description.
        // Unlock before close so that child cannot retain the retired owner's lease.
        let _ = self.0.unlock();
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

pub(crate) fn open_private_file(path: &Path) -> Result<Option<File>, EngineError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(IO),
    };
    validate_file(&metadata)?;
    let file = File::open(path).map_err(|_| IO)?;
    validate_file(&file.metadata().map_err(|_| IO)?)?;
    Ok(Some(file))
}

pub(crate) fn read(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, EngineError> {
    let Some(file) = open_private_file(path)? else {
        return Ok(None);
    };
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

/// Verify and synchronize one admitted object handle without buffering a second object.
fn verify_immutable_handle(
    file: &File,
    expected: &[u8],
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
) -> Result<(), EngineError> {
    if file.metadata().map_err(|_| IO)?.len() != expected.len() as u64 {
        return Err(INVALID);
    }
    let mut reader = file;
    let mut buffer = [0_u8; 16 * 1024];
    for chunk in expected.chunks(buffer.len()) {
        let observed = &mut buffer[..chunk.len()];
        reader.read_exact(observed).map_err(|_| IO)?;
        if observed != chunk {
            return Err(INVALID);
        }
    }
    if reader.read(&mut [0_u8; 1]).map_err(|_| IO)? != 0 {
        return Err(INVALID);
    }
    sync_file(file).map_err(|_| IO)
}

/// Publish a complete immutable object without replacing an existing hash name.
pub(crate) fn publish_immutable_file(
    path: &Path,
    bytes: &[u8],
    check: &mut impl FnMut() -> Result<(), EngineError>,
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
) -> Result<(), EngineError> {
    publish_immutable_with_writer(path, bytes, check, sync_file, &mut |file, chunk| {
        file.write_all(chunk)
    })
}
fn publish_immutable_with_writer(
    path: &Path,
    bytes: &[u8],
    check: &mut impl FnMut() -> Result<(), EngineError>,
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
    write: &mut impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<(), EngineError> {
    let directory = path.parent().ok_or(INVALID)?;
    let parent = File::open(directory).map_err(|_| IO)?;
    publish_immutable_at_with_writer(
        &parent,
        path.file_name().ok_or(INVALID)?,
        bytes,
        check,
        sync_file,
        write,
    )
}

/// Share exclusive immutable publication with descriptor-relative package staging.
pub(crate) fn publish_immutable_file_at(
    directory: &File,
    name: &std::ffi::OsStr,
    bytes: &[u8],
    check: &mut impl FnMut() -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    publish_immutable_at_with_writer(
        directory,
        name,
        bytes,
        check,
        &mut File::sync_all,
        &mut |file, chunk| file.write_all(chunk),
    )
}
fn publish_immutable_at_with_writer(
    directory: &File,
    name: &std::ffi::OsStr,
    bytes: &[u8],
    check: &mut impl FnMut() -> Result<(), EngineError>,
    sync_file: &mut impl FnMut(&File) -> std::io::Result<()>,
    write: &mut impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<(), EngineError> {
    use rustix::fs::{Mode, OFlags, openat};
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(INVALID);
    }
    let open = |name: &std::ffi::OsStr| -> Result<Option<File>, EngineError> {
        match openat(
            directory,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let file = File::from(fd);
                validate_file(&file.metadata().map_err(|_| IO)?)?;
                Ok(Some(file))
            }
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(_) => Err(IO),
        }
    };
    check()?;
    if let Some(file) = open(name)? {
        verify_immutable_handle(&file, bytes, sync_file)?;
    } else {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| IO)?
            .as_nanos();
        let stage = format!(".immutable-{}-{time:x}-{sequence:x}", std::process::id());
        let mut file = File::from(
            openat(
                directory,
                &stage,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(|_| IO)?,
        );
        validate_file(&file.metadata().map_err(|_| IO)?)?;
        for chunk in bytes.chunks(16 * 1024) {
            check()?;
            write(&mut file, chunk).map_err(|_| IO)?;
        }
        file.rewind().map_err(|_| IO)?;
        verify_immutable_handle(&file, bytes, sync_file)?;
        check()?;
        match rustix::fs::renameat_with(
            directory,
            &stage,
            directory,
            name,
            rustix::fs::RenameFlags::NOREPLACE,
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::EXIST) => {
                verify_immutable_handle(&open(name)?.ok_or(INVALID)?, bytes, sync_file)?
            }
            Err(_) => return Err(IO),
        }
    }
    // Keep every abandoned stage. Only complete synced bytes can acquire the final hash name.
    directory.sync_all().map_err(|_| IO)?;
    check()
}

pub(crate) fn write_atomic(
    directory: &Path,
    name: &str,
    token: &str,
    bytes: &[u8],
) -> Result<(), EngineError> {
    replace_atomic(directory, name, token, bytes, &mut sync_directory)
        .map_err(ReplacementError::error)
}
fn replace_atomic(
    directory: &Path,
    name: &str,
    token: &str,
    bytes: &[u8],
    sync: &mut impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), ReplacementError> {
    let unchanged = ReplacementError::Unchanged;
    let destination = directory.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => validate_file(&metadata).map_err(unchanged)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(unchanged(IO)),
    }
    let temporary = directory.join(format!(".{name}.{token}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| unchanged(IO))?;
    validate_file(&file.metadata().map_err(|_| unchanged(IO))?).map_err(unchanged)?;
    file.write_all(bytes).map_err(|_| unchanged(IO))?;
    file.sync_all().map_err(|_| unchanged(IO))?;
    fs::rename(temporary, &destination).map_err(|_| unchanged(IO))?;
    if sync(directory).is_ok() {
        return Ok(());
    }
    // A failed directory sync does not undo rename. Read before retrying durability.
    if read(&destination, bytes.len()).ok().flatten().as_deref() == Some(bytes)
        && sync(directory).is_ok()
    {
        return Ok(());
    }
    Err(ReplacementError::Uncertain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/checkpoint-recovery")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn interrupted_immutable_write_preserves_prefix_and_retry_publishes() {
        let directory = fixture();
        let path = directory.join("content.hash");
        let bytes = b"the complete immutable object";
        let result = publish_immutable_with_writer(
            &path,
            bytes,
            &mut || Ok(()),
            &mut File::sync_all,
            &mut |file, chunk| {
                file.write_all(&chunk[..5])?;
                Err(std::io::Error::other(
                    "controlled interruption after prefix",
                ))
            },
        );
        assert!(result.is_err());
        assert!(!path.exists());
        let stages: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(stages.len(), 1);
        assert_eq!(fs::read(&stages[0]).unwrap(), &bytes[..5]);
        publish_immutable_file(&path, bytes, &mut || Ok(()), &mut File::sync_all).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::read(&stages[0]).unwrap(), &bytes[..5]);
    }
    #[test]
    fn exclusive_publication_preserves_racing_object_and_completed_stage() {
        let directory = fixture();
        let path = directory.join("content.hash");
        let bytes = b"racing complete object";
        let mut winner = None;
        publish_immutable_file(&path, bytes, &mut || Ok(()), &mut |file| {
            file.sync_all()?;
            if winner.is_none() {
                let mut competing = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)?;
                competing.write_all(bytes)?;
                winner = Some(competing.metadata()?);
            }
            Ok(())
        })
        .unwrap();
        let observed = fs::metadata(&path).unwrap();
        assert_eq!(observed.ino(), winner.unwrap().ino());
        assert_eq!(observed.nlink(), 1);
        let files: Vec<_> = fs::read_dir(directory)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(files.len(), 2);
        for file in files {
            assert_eq!(fs::read(file).unwrap(), bytes);
        }
    }
    #[test]
    fn immutable_publication_rejects_corrupt_and_hard_linked_destinations() {
        let directory = fixture();
        let path = directory.join("content.hash");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(b"prefix").unwrap();
        assert_eq!(
            publish_immutable_file(&path, b"complete", &mut || Ok(()), &mut File::sync_all)
                .unwrap_err()
                .code,
            ErrorCode::AssetInvalid
        );
        assert_eq!(fs::read(&path).unwrap(), b"prefix");
        fs::hard_link(&path, directory.join("linked")).unwrap();
        assert_eq!(
            publish_immutable_file(&path, b"prefix", &mut || Ok(()), &mut File::sync_all)
                .unwrap_err()
                .code,
            ErrorCode::AuthDenied
        );
        assert_eq!(fs::metadata(&path).unwrap().nlink(), 2);
    }

    fn fail_syncs(first: usize, count: usize) -> impl FnMut(&Path) -> std::io::Result<()> {
        let mut calls = 0;
        move |directory| {
            calls += 1;
            if (first..first + count).contains(&calls) {
                Err(std::io::Error::from(std::io::ErrorKind::Other))
            } else {
                sync_directory(directory)
            }
        }
    }
    #[test]
    fn unresolved_queue_sync_recovers_the_prior_checkpoint() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        let result = checkpoint_transaction_with_sync(
            &path,
            "new",
            b"new",
            || Ok(()),
            &mut fail_syncs(3, 2),
        );
        assert_eq!(result.unwrap_err().error().message, UNCERTAIN.message);
        assert_eq!(read(&path.join("queue.json"), 10).unwrap().unwrap(), b"new");
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"old");
        assert_eq!(read(&path.join("queue.json"), 10).unwrap().unwrap(), b"old");
    }
    #[test]
    fn unresolved_settlement_sync_reports_uncertainty_and_reconciles_the_commit() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        let result = checkpoint_transaction_with_sync(
            &path,
            "new",
            b"new",
            || Ok(()),
            &mut fail_syncs(4, 2),
        );
        assert_eq!(result.unwrap_err().error().message, UNCERTAIN.message);
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"new");
        assert_eq!(read_checkpoint(&path, "again").unwrap().unwrap(), b"new");
    }
    #[test]
    fn one_failed_directory_sync_is_confirmed_before_success() {
        let path = fixture();
        checkpoint_transaction_with_sync(&path, "new", b"new", || Ok(()), &mut fail_syncs(2, 1))
            .unwrap();
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"new");
    }
    #[test]
    fn first_uncertain_checkpoint_preserves_logical_absence_until_a_successful_write() {
        let path = fixture();
        let result = checkpoint_transaction_with_sync(
            &path,
            "new",
            b"new",
            || Ok(()),
            &mut fail_syncs(2, 2),
        );
        assert_eq!(result.unwrap_err().error().message, UNCERTAIN.message);
        assert!(read_checkpoint(&path, "restart").unwrap().is_none());
        assert_eq!(read(&path.join("queue.json"), 10).unwrap().unwrap(), b"new");
        write_checkpoint(&path, "accepted", b"accepted").unwrap();
        assert_eq!(
            read_checkpoint(&path, "again").unwrap().unwrap(),
            b"accepted"
        );
    }
    #[test]
    fn failed_edit_validation_rolls_back_with_the_original_baseline() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        let invalid =
            EngineError::new(ErrorCode::SafetyPolicyBlocked, "test edit became protected");
        let result = checkpoint_transaction_with_sync(
            &path,
            "rejected",
            b"rejected",
            || Err(invalid),
            &mut fail_syncs(4, 2),
        );
        assert_eq!(result.unwrap_err().error().message, UNCERTAIN.message);
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"old");
        assert_eq!(read(&path.join("queue.json"), 10).unwrap().unwrap(), b"old");
    }
    #[test]
    fn malformed_recovery_or_a_mismatched_settled_checkpoint_refuses_restore() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        write_atomic(&path, "queue.json", "tamper", b"other").unwrap();
        assert!(read_checkpoint(&path, "restart").is_err());
        write_atomic(&path, RECOVERY_FILE, "bad", b"{}").unwrap();
        assert!(read_checkpoint(&path, "restart").is_err());
        write_atomic(
            &path,
            RECOVERY_FILE,
            "missing",
            br#"{"state":"pending","schema_version":1}"#,
        )
        .unwrap();
        assert!(read_checkpoint(&path, "restart").is_err());
    }
    #[test]
    fn excessive_checkpoint_is_rejected_before_any_replacement() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        assert!(write_checkpoint(&path, "huge", &vec![b'x'; MAX_CHECKPOINT_BYTES + 1]).is_err());
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"old");
    }
    #[test]
    fn failure_before_queue_replacement_is_definite_and_preserves_the_prior_bytes() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        let collision = path.join(".queue.json.new");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&collision)
            .unwrap()
            .write_all(b"preserve collision")
            .unwrap();
        assert_eq!(write_checkpoint(&path, "new", b"new").unwrap_err(), IO);
        assert_eq!(read(&path.join("queue.json"), 10).unwrap().unwrap(), b"old");
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"old");
        assert_eq!(fs::read(collision).unwrap(), b"preserve collision");
    }
    #[test]
    fn pending_baseline_restores_without_requiring_another_checkpoint_write() {
        let path = fixture();
        write_checkpoint(&path, "old", b"old").unwrap();
        let record = encode_record(&RecoveryRecord::Pending {
            schema_version: 1,
            previous: Some("old".into()),
        })
        .unwrap();
        write_atomic(&path, RECOVERY_FILE, "pending", &record).unwrap();
        let collision = path.join(".queue.json.restart-recover");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&collision)
            .unwrap()
            .write_all(b"preserve collision")
            .unwrap();
        assert_eq!(read_checkpoint(&path, "restart").unwrap().unwrap(), b"old");
        assert_eq!(fs::read(collision).unwrap(), b"preserve collision");
        assert_eq!(
            read(&path.join(RECOVERY_FILE), MAX_RECOVERY_BYTES)
                .unwrap()
                .unwrap(),
            record
        );
    }
    #[test]
    fn private_oversized_candidate_cannot_block_a_valid_pending_baseline() {
        let path = fixture();
        let record = encode_record(&RecoveryRecord::Pending {
            schema_version: 1,
            previous: Some("baseline".into()),
        })
        .unwrap();
        write_atomic(&path, RECOVERY_FILE, "pending", &record).unwrap();
        write_atomic(
            &path,
            "queue.json",
            "oversized",
            &vec![b'x'; MAX_CHECKPOINT_BYTES + 1],
        )
        .unwrap();
        assert_eq!(
            read_checkpoint(&path, "restart").unwrap().unwrap(),
            b"baseline"
        );
        assert_eq!(fs::read(path.join("queue.json")).unwrap(), b"baseline");
    }
    #[test]
    fn oversized_candidate_with_unsafe_permissions_still_refuses_recovery() {
        use std::os::unix::fs::PermissionsExt;
        let path = fixture();
        let record = encode_record(&RecoveryRecord::Pending {
            schema_version: 1,
            previous: Some("baseline".into()),
        })
        .unwrap();
        write_atomic(&path, RECOVERY_FILE, "pending", &record).unwrap();
        write_atomic(
            &path,
            "queue.json",
            "oversized",
            &vec![b'x'; MAX_CHECKPOINT_BYTES + 1],
        )
        .unwrap();
        fs::set_permissions(path.join("queue.json"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            read_checkpoint(&path, "restart").unwrap_err().code,
            ErrorCode::AuthDenied
        );
        assert_eq!(
            fs::metadata(path.join("queue.json")).unwrap().len(),
            (MAX_CHECKPOINT_BYTES + 1) as u64
        );
        assert_eq!(fs::read(path.join(RECOVERY_FILE)).unwrap(), record);
    }
}
