// ABOUTME: Pins bounded artifact directories and regular files through nofollow Unix descriptors.
// ABOUTME: Retains staging identities and records installation progress without deleting artifacts.
use super::*;
use rustix::fs::{self as unix, AtFlags, Dir, FileType, Mode, OFlags};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::Read,
    path::Component,
};
pub(crate) const MISSING: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "required package object is missing",
);
pub(crate) const INVENTORY: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "package inventory differs from its reachable objects",
);
pub(crate) const DESTINATION: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "package destination already exists",
);
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
#[derive(Debug, Clone, Copy)]
pub(crate) struct ArtifactLimits {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub private: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub dev: u128,
    pub ino: u128,
    pub bytes: u128,
    mode: u64,
    uid: u32,
}
pub(crate) fn filesystem_error(error: std::io::Error) -> EngineError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return MISSING;
    }
    if error.kind() == std::io::ErrorKind::UnexpectedEof
        || error.raw_os_error().is_some_and(|code| {
            [
                rustix::io::Errno::NOTDIR,
                rustix::io::Errno::ISDIR,
                rustix::io::Errno::LOOP,
                rustix::io::Errno::NAMETOOLONG,
            ]
            .iter()
            .any(|kind| kind.raw_os_error() == code)
        })
    {
        return INVALID;
    }
    runtime::IO
}
fn stamp(directory: &File, name: &OsStr, private: bool) -> Result<Stamp, EngineError> {
    let s = unix::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| filesystem_error(e.into()))?;
    if FileType::from_raw_mode(s.st_mode) != FileType::RegularFile
        || s.st_mode & 0o111 != 0
        || s.st_nlink != 1
    {
        return Err(INVALID);
    }
    if private && (s.st_uid != rustix::process::geteuid().as_raw() || s.st_mode & 0o077 != 0) {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "artifact files must be private and owner controlled",
        ));
    }
    Ok(Stamp {
        dev: s.st_dev as u128,
        ino: s.st_ino as u128,
        bytes: u128::try_from(s.st_size).map_err(|_| BUDGET)?,
        mode: s.st_mode as u64,
        uid: s.st_uid,
    })
}
pub(crate) fn walk_directory(
    path: &Path,
    export_parent: bool,
    check: &mut impl FnMut() -> Result<(), EngineError>,
) -> Result<File, EngineError> {
    if !path.is_absolute() {
        return Err(INVALID);
    }
    let mut current = File::from(
        unix::open("/", FLAGS | OFlags::DIRECTORY, Mode::empty())
            .map_err(|e| filesystem_error(e.into()))?,
    );
    for component in path.components() {
        check()?;
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                if export_parent
                    && [".package-", ".diagnostics-"]
                        .iter()
                        .any(|prefix| name.to_string_lossy().starts_with(prefix))
                {
                    return Err(INVALID);
                }
                current = File::from(
                    unix::openat(&current, name, FLAGS | OFlags::DIRECTORY, Mode::empty())
                        .map_err(|e| filesystem_error(e.into()))?,
                );
                if export_parent {
                    match unix::statat(&current, "manifest.json", AtFlags::SYMLINK_NOFOLLOW) {
                        Err(rustix::io::Errno::NOENT) => {}
                        Ok(_) => return Err(INVALID),
                        Err(e) => return Err(filesystem_error(e.into())),
                    }
                }
            }
            _ => return Err(INVALID),
        }
    }
    Ok(current)
}
fn private_directory(file: &File) -> Result<(), EngineError> {
    let metadata = file.metadata().map_err(filesystem_error)?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "package export parent must be private and owner controlled",
        ));
    }
    Ok(())
}
fn open_handle(
    path: &Path,
    private: bool,
    check: &mut impl FnMut() -> Result<(), EngineError>,
) -> Result<File, EngineError> {
    if !private {
        return walk_directory(path, false, check);
    }
    let parent = walk_directory(path.parent().ok_or(INVALID)?, true, check)?;
    private_directory(&parent)?;
    let file = File::from(
        unix::openat(
            &parent,
            path.file_name().ok_or(INVALID)?,
            FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        )
        .map_err(|e| filesystem_error(e.into()))?,
    );
    private_directory(&file)?;
    Ok(file)
}
pub(crate) fn inventory_with(
    directory: &File,
    limits: ArtifactLimits,
    check: &mut impl FnMut() -> Result<(), EngineError>,
) -> Result<BTreeMap<String, Stamp>, EngineError> {
    let mut entries = BTreeMap::new();
    let mut bytes = 0u128;
    for entry in Dir::read_from(directory).map_err(|e| filesystem_error(e.into()))? {
        check()?;
        let entry = entry.map_err(|e| filesystem_error(e.into()))?;
        let name = entry.file_name().to_str().map_err(|_| INVALID)?;
        if name == "." || name == ".." {
            continue;
        }
        if name.len() > 96 || entries.len() >= limits.max_entries {
            return Err(BUDGET);
        }
        let observed = stamp(directory, OsStr::new(name), limits.private)?;
        bytes = bytes.checked_add(observed.bytes).ok_or(BUDGET)?;
        if bytes > limits.max_bytes as u128 {
            return Err(BUDGET);
        }
        if entries
            .values()
            .any(|s: &Stamp| s.dev == observed.dev && s.ino == observed.ino)
            || entries.insert(name.to_owned(), observed).is_some()
        {
            return Err(INVALID);
        }
    }
    Ok(entries)
}
#[derive(Debug)]
pub(crate) struct ArtifactDirectory {
    pub file: File,
    pub path: PathBuf,
    pub entries: BTreeMap<String, Stamp>,
    limits: ArtifactLimits,
}
impl ArtifactDirectory {
    pub fn open_with(
        path: &Path,
        limits: ArtifactLimits,
        check: &mut impl FnMut() -> Result<(), EngineError>,
    ) -> Result<Self, EngineError> {
        check()?;
        let file = open_handle(path, limits.private, check)?;
        let entries = inventory_with(&file, limits, check)?;
        Ok(Self {
            file,
            path: path.to_owned(),
            entries,
            limits,
        })
    }
    pub fn length(&self, name: &str) -> Result<u64, EngineError> {
        u64::try_from(self.entries.get(name).ok_or(MISSING)?.bytes).map_err(|_| BUDGET)
    }
    fn validate_named(
        &self,
        check: &mut impl FnMut() -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        if self.limits.private {
            let named = open_handle(&self.path, true, check)?;
            if !same_directory(&self.file, &named)? {
                return Err(INVALID);
            }
        }
        Ok(())
    }
    pub fn read_with(
        &self,
        name: &str,
        limit: usize,
        check: &mut impl FnMut() -> Result<(), EngineError>,
    ) -> Result<Vec<u8>, EngineError> {
        check()?;
        self.validate_named(check)?;
        let before = self.entries.get(name).ok_or(MISSING)?;
        if before.bytes > limit as u128 {
            return Err(BUDGET);
        }
        if &stamp(&self.file, OsStr::new(name), self.limits.private)? != before {
            return Err(INVALID);
        }
        let file = File::from(
            unix::openat(&self.file, name, FLAGS, Mode::empty())
                .map_err(|e| filesystem_error(e.into()))?,
        );
        let metadata = file.metadata().map_err(filesystem_error)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o111 != 0
            || metadata.dev() as u128 != before.dev
            || metadata.ino() as u128 != before.ino
            || metadata.len() as u128 != before.bytes
            || metadata.uid() != before.uid
        {
            return Err(INVALID);
        }
        let bytes = read_span_with(
            file,
            usize::try_from(before.bytes).map_err(|_| BUDGET)?,
            limit,
            check,
        )?;
        if &stamp(&self.file, OsStr::new(name), self.limits.private)? != before {
            return Err(INVALID);
        }
        self.validate_named(check)?;
        check()?;
        Ok(bytes)
    }
    pub fn validate_exact(
        &self,
        expected: &[(String, u64)],
        check: &mut impl FnMut() -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        self.validate_named(check)?;
        if inventory_with(&self.file, self.limits, check)? != self.entries
            || self.entries.len() != expected.len()
            || expected.iter().any(|(name, len)| {
                self.entries
                    .get(name)
                    .is_none_or(|s| s.bytes != *len as u128)
            })
        {
            return Err(INVENTORY);
        }
        self.validate_named(check)
    }
}
pub(crate) fn same_directory(first: &File, second: &File) -> Result<bool, EngineError> {
    let a = first.metadata().map_err(filesystem_error)?;
    let b = second.metadata().map_err(filesystem_error)?;
    Ok(a.dev() == b.dev() && a.ino() == b.ino())
}
pub(crate) fn read_span_with(
    mut file: impl Read,
    size: usize,
    limit: usize,
    check: &mut impl FnMut() -> Result<(), EngineError>,
) -> Result<Vec<u8>, EngineError> {
    if size > limit {
        return Err(BUDGET);
    }
    check()?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| BUDGET)?;
    if bytes.capacity() > limit {
        return Err(BUDGET);
    }
    bytes.resize(size, 0);
    for chunk in bytes.chunks_mut(16 * 1024) {
        check()?;
        file.read_exact(chunk).map_err(filesystem_error)?;
    }
    if file.read(&mut [0u8; 1]).map_err(filesystem_error)? != 0 {
        return Err(INVALID);
    }
    check()?;
    Ok(bytes)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StageSync {
    CreatedParent,
    Contents,
    InstalledParent,
}
#[derive(Debug)]
pub(crate) struct ArtifactStageFailure {
    pub error: EngineError,
    pub stage: Option<PathBuf>,
}
pub(crate) struct ArtifactStage {
    parent: File,
    pub file: File,
    name: OsString,
    destination: OsString,
    pub path: PathBuf,
    installed: bool,
    synced: bool,
    named_identity_uncertain: bool,
}
impl ArtifactStage {
    pub fn create_with(
        destination: &Path,
        runtime: &Path,
        prefix: &str,
        max_path: usize,
        check: &mut impl FnMut() -> Result<(), EngineError>,
        sync: &mut impl FnMut(&File, StageSync) -> std::io::Result<()>,
    ) -> Result<Self, ArtifactStageFailure> {
        let mut stage = None;
        let result = (|| {
            check()?;
            let parent_path = destination.parent().ok_or(INVALID)?;
            if destination.starts_with(runtime) || runtime.starts_with(destination) {
                return Err(INVALID);
            }
            let parent = walk_directory(parent_path, true, check)?;
            private_directory(&parent)?;
            let destination_name = destination.file_name().ok_or(INVALID)?.to_os_string();
            match unix::statat(&parent, &destination_name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => {}
                Ok(_) => return Err(DESTINATION),
                Err(e) => return Err(filesystem_error(e.into())),
            }
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| runtime::IO)?
                .as_nanos();
            let name = OsString::from(format!(
                "{prefix}{}-{now:x}-{sequence:x}",
                std::process::id()
            ));
            let path = parent_path.join(&name);
            if path.as_os_str().len() > max_path {
                return Err(BUDGET);
            }
            check()?;
            unix::mkdirat(&parent, &name, Mode::from_raw_mode(0o700))
                .map_err(|e| filesystem_error(e.into()))?;
            stage = Some(path.clone());
            sync(&parent, StageSync::CreatedParent).map_err(|_| runtime::IO)?;
            let file = File::from(
                unix::openat(&parent, &name, FLAGS | OFlags::DIRECTORY, Mode::empty())
                    .map_err(|e| filesystem_error(e.into()))?,
            );
            Ok(Self {
                parent,
                file,
                name,
                destination: destination_name,
                path,
                installed: false,
                synced: false,
                named_identity_uncertain: false,
            })
        })();
        result.map_err(|error| ArtifactStageFailure { error, stage })
    }
    pub fn write_with(
        &self,
        name: &str,
        bytes: &[u8],
        check: &mut impl FnMut() -> Result<(), EngineError>,
        sync: &mut impl FnMut(&File) -> std::io::Result<()>,
        write: &mut impl FnMut(&mut File, &[u8]) -> std::io::Result<()>,
    ) -> Result<(), EngineError> {
        runtime::publish_immutable_at_with_writer(
            &self.file,
            OsStr::new(name),
            bytes,
            check,
            sync,
            write,
        )
    }
    pub fn publish_with(
        &mut self,
        check: &mut impl FnMut() -> Result<(), EngineError>,
        sync: &mut impl FnMut(&File, StageSync) -> std::io::Result<()>,
        before_install: &mut impl FnMut() -> Result<(), EngineError>,
        verify_names: bool,
    ) -> Result<(), EngineError> {
        check()?;
        sync(&self.file, StageSync::Contents).map_err(|_| runtime::IO)?;
        before_install()?;
        check()?;
        if verify_names {
            let names = (|| {
                let parent = walk_directory(self.path.parent().ok_or(INVALID)?, true, check)?;
                private_directory(&parent)?;
                let named = open_handle(&self.path, true, check)?;
                if !same_directory(&parent, &self.parent)? || !same_directory(&named, &self.file)? {
                    return Err(INVALID);
                }
                Ok(())
            })();
            if let Err(error) = names {
                self.named_identity_uncertain = true;
                return Err(error);
            }
        }
        unix::renameat_with(
            &self.parent,
            &self.name,
            &self.parent,
            &self.destination,
            unix::RenameFlags::NOREPLACE,
        )
        .map_err(|e| {
            if e == rustix::io::Errno::EXIST {
                DESTINATION
            } else {
                filesystem_error(e.into())
            }
        })?;
        self.installed = true;
        sync(&self.parent, StageSync::InstalledParent).map_err(|_| runtime::IO)?;
        self.synced = true;
        check()
    }
    pub fn installed(&self) -> bool {
        self.installed
    }
    pub fn installed_synced(&self) -> bool {
        self.synced
    }
    pub fn named_identity_uncertain(&self) -> bool {
        self.named_identity_uncertain
    }
}
