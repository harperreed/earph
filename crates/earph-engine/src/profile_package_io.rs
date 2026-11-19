// ABOUTME: Opens package directories and regular objects through pinned Unix descriptors.
// ABOUTME: Stages exclusive package publication while preserving every interrupted artifact.
use super::*;
use earph_core::profile_packages::{MAX_PACKAGE_BYTES, MAX_PACKAGE_OBJECTS};
use rustix::fs::{self as unix, AtFlags, Dir, FileType, Mode, OFlags};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Component,
};

pub(super) const MISSING: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "required package object is missing",
);
pub(super) const INVENTORY: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "package inventory differs from its reachable objects",
);
pub(super) const DESTINATION: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "package destination already exists",
);
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    dev: u128,
    ino: u128,
    bytes: u128,
    mode: u64,
}
fn filesystem_error(error: std::io::Error) -> EngineError {
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
fn stamp(directory: &File, name: &OsStr) -> Result<Stamp, EngineError> {
    let s = unix::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
        .map_err(|e| filesystem_error(e.into()))?;
    if FileType::from_raw_mode(s.st_mode) != FileType::RegularFile
        || s.st_mode & 0o111 != 0
        || s.st_nlink != 1
    {
        return Err(INVALID);
    }
    Ok(Stamp {
        dev: s.st_dev as u128,
        ino: s.st_ino as u128,
        bytes: u128::try_from(s.st_size).map_err(|_| BUDGET)?,
        mode: s.st_mode as u64,
    })
}
fn open_directory(path: &Path) -> Result<File, EngineError> {
    walk_directory(path, false)
}
fn walk_directory(path: &Path, export_parent: bool) -> Result<File, EngineError> {
    let mut current = File::from(
        unix::open("/", FLAGS | OFlags::DIRECTORY, Mode::empty())
            .map_err(|e| filesystem_error(e.into()))?,
    );
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                if export_parent && name.to_string_lossy().starts_with(".package-") {
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
fn inventory(
    directory: &File,
    cancel: &AtomicBool,
) -> Result<BTreeMap<String, Stamp>, EngineError> {
    let mut entries = BTreeMap::new();
    let mut bytes = 0u128;
    for entry in Dir::read_from(directory).map_err(|e| filesystem_error(e.into()))? {
        check(cancel)?;
        let entry = entry.map_err(|e| filesystem_error(e.into()))?;
        let name = entry.file_name().to_str().map_err(|_| INVALID)?;
        if name == "." || name == ".." {
            continue;
        }
        if name.len() > 96 || entries.len() > MAX_PACKAGE_OBJECTS {
            return Err(BUDGET);
        }
        let observed = stamp(directory, OsStr::new(name))?;
        bytes = bytes.checked_add(observed.bytes).ok_or(BUDGET)?;
        if bytes > MAX_PACKAGE_BYTES as u128 {
            return Err(BUDGET);
        }
        if entries
            .values()
            .any(|s: &Stamp| s.dev == observed.dev && s.ino == observed.ino)
        {
            return Err(INVALID);
        }
        if entries.insert(name.to_owned(), observed).is_some() {
            return Err(INVALID);
        }
    }
    if entries.len() > MAX_PACKAGE_OBJECTS + 1 {
        return Err(BUDGET);
    }
    Ok(entries)
}
pub(super) struct PackageDirectory {
    file: File,
    pub path: PathBuf,
    entries: BTreeMap<String, Stamp>,
}
impl PackageDirectory {
    pub fn length(&self, name: &str) -> Result<u64, EngineError> {
        u64::try_from(self.entries.get(name).ok_or(MISSING)?.bytes).map_err(|_| BUDGET)
    }
    pub fn open(path: &Path, cancel: &AtomicBool) -> Result<Self, EngineError> {
        check(cancel)?;
        let file = open_directory(path)?;
        let entries = inventory(&file, cancel)?;
        Ok(Self {
            file,
            path: path.to_owned(),
            entries,
        })
    }
    pub fn read(
        &self,
        name: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, EngineError> {
        check(cancel)?;
        let before = self.entries.get(name).ok_or(MISSING)?;
        if before.bytes > limit as u128 {
            return Err(BUDGET);
        }
        if &stamp(&self.file, OsStr::new(name))? != before {
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
        {
            return Err(INVALID);
        }
        let bytes = read_span(
            file,
            usize::try_from(before.bytes).map_err(|_| BUDGET)?,
            limit,
            cancel,
        )?;
        if &stamp(&self.file, OsStr::new(name))? != before {
            return Err(INVALID);
        }
        check(cancel)?;
        Ok(bytes)
    }
    pub fn validate_inventory(
        &self,
        manifest: &earph_core::profile_packages::PackageManifest,
        cancel: &AtomicBool,
    ) -> Result<(), EngineError> {
        if inventory(&self.file, cancel)? != self.entries
            || self.entries.len() != manifest.objects.len() + 1
            || !self.entries.contains_key("manifest.json")
        {
            return Err(INVENTORY);
        }
        for object in &manifest.objects {
            if self
                .entries
                .get(&object.file_name())
                .is_none_or(|s| s.bytes != object.bytes as u128)
            {
                return Err(INVENTORY);
            }
        }
        Ok(())
    }
}
pub(super) fn read_span(
    mut file: impl Read,
    size: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, EngineError> {
    if size > limit {
        return Err(BUDGET);
    }
    check(cancel)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(size).map_err(|_| BUDGET)?;
    if bytes.capacity() > limit {
        return Err(BUDGET);
    }
    bytes.resize(size, 0);
    for chunk in bytes.chunks_mut(16 * 1024) {
        check(cancel)?;
        file.read_exact(chunk).map_err(filesystem_error)?;
    }
    if file.read(&mut [0u8; 1]).map_err(filesystem_error)? != 0 {
        return Err(INVALID);
    }
    check(cancel)?;
    Ok(bytes)
}
pub(super) struct PackageStage {
    parent: File,
    file: File,
    name: OsString,
    destination: OsString,
    pub path: PathBuf,
}
impl PackageStage {
    pub fn create(
        destination: &Path,
        runtime: &Path,
        cancel: &AtomicBool,
    ) -> Result<Self, EngineError> {
        check(cancel)?;
        let parent_path = destination.parent().ok_or(INVALID)?;
        if destination.starts_with(runtime) || runtime.starts_with(destination) {
            return Err(INVALID);
        }
        let parent = walk_directory(parent_path, true)?;
        let metadata = parent.metadata().map_err(filesystem_error)?;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(EngineError::new(
                ErrorCode::AuthDenied,
                "package export parent must be private and owner controlled",
            ));
        }
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
            ".package-{}-{now:x}-{sequence:x}",
            std::process::id()
        ));
        unix::mkdirat(&parent, &name, Mode::from_raw_mode(0o700))
            .map_err(|e| filesystem_error(e.into()))?;
        parent.sync_all().map_err(|_| runtime::IO)?;
        let file = File::from(
            unix::openat(&parent, &name, FLAGS | OFlags::DIRECTORY, Mode::empty())
                .map_err(|e| filesystem_error(e.into()))?,
        );
        Ok(Self {
            parent,
            file,
            name: name.clone(),
            destination: destination_name,
            path: parent_path.join(name),
        })
    }
    pub fn write(&self, name: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<(), EngineError> {
        runtime::publish_immutable_file_at(&self.file, OsStr::new(name), bytes, &mut || {
            check(cancel)
        })
    }
    pub fn publish(self, cancel: &AtomicBool) -> Result<(), EngineError> {
        self.publish_with_sync(cancel, &mut File::sync_all)
    }
    fn publish_with_sync(
        self,
        cancel: &AtomicBool,
        sync: &mut impl FnMut(&File) -> std::io::Result<()>,
    ) -> Result<(), EngineError> {
        check(cancel)?;
        sync(&self.file).map_err(|_| runtime::IO)?;
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
        sync(&self.parent).map_err(|_| runtime::IO)?;
        check(cancel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/package-io-tests")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&path)
            .unwrap();
        fs::canonicalize(path).unwrap()
    }
    #[test]
    fn pinned_directory_survives_name_replacement_and_refuses_swapped_files() {
        let root = fixture();
        let source = root.join("input");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("object"), b"original").unwrap();
        let directory = PackageDirectory::open(&source, &AtomicBool::new(false)).unwrap();
        fs::rename(&source, root.join("retained-input")).unwrap();
        fs::create_dir(&source).unwrap();
        fs::write(source.join("object"), b"replacement").unwrap();
        assert_eq!(
            directory
                .read("object", 64, &AtomicBool::new(false))
                .unwrap(),
            b"original"
        );
        fs::rename(
            root.join("retained-input/object"),
            root.join("retained-object"),
        )
        .unwrap();
        fs::write(root.join("retained-input/object"), b"swapped!").unwrap();
        assert!(
            directory
                .read("object", 64, &AtomicBool::new(false))
                .is_err()
        );
        assert_eq!(
            directory
                .read("object", 64, &AtomicBool::new(true))
                .unwrap_err()
                .code,
            ErrorCode::OperationCancelled
        );
    }
    #[test]
    fn chunk_cancellation_preserves_written_prefix_and_stage() {
        let root = fixture();
        let final_path = root.join("package");
        let stage =
            PackageStage::create(&final_path, &root.join("runtime"), &AtomicBool::new(false))
                .unwrap();
        let bytes = vec![42u8; 32768];
        let mut calls = 0;
        let error = runtime::publish_immutable_file_at(
            &stage.file,
            OsStr::new("object"),
            &bytes,
            &mut || {
                calls += 1;
                if calls == 3 { Err(CANCELLED) } else { Ok(()) }
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::OperationCancelled);
        assert!(!final_path.exists());
        let artifacts = fs::read_dir(&stage.path)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(fs::read(&artifacts[0]).unwrap(), bytes[..16384]);
        assert!(stage.publish(&AtomicBool::new(true)).is_err());
        assert!(!final_path.exists());
    }
    #[test]
    fn failed_parent_sync_preserves_the_installed_package() {
        let root = fixture();
        let final_path = root.join("package");
        let stage =
            PackageStage::create(&final_path, &root.join("runtime"), &AtomicBool::new(false))
                .unwrap();
        stage
            .write("object", b"complete", &AtomicBool::new(false))
            .unwrap();
        let mut calls = 0;
        let result = stage.publish_with_sync(&AtomicBool::new(false), &mut |file| {
            calls += 1;
            if calls == 2 {
                Err(std::io::Error::other("controlled parent sync failure"))
            } else {
                file.sync_all()
            }
        });
        assert_eq!(result, Err(runtime::IO));
        assert_eq!(fs::read(final_path.join("object")).unwrap(), b"complete");
    }
    #[test]
    fn shared_descriptor_publisher_accepts_only_one_basename() {
        let root = fixture();
        let directory = File::open(&root).unwrap();
        let escape = format!("../escaped-{}", root.file_name().unwrap().to_str().unwrap());
        assert!(
            runtime::publish_immutable_file_at(
                &directory,
                OsStr::new(&escape),
                b"retained path-test bytes",
                &mut || Ok(())
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod storage_error_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn actual_bad_read_descriptor_is_storage_while_eof_and_type_are_content() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/package-io-errors")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .unwrap();
        let root = fs::canonicalize(root).unwrap();
        let path = root.join("original");
        let mut write_only = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        write_only.write_all(b"abcd").unwrap();
        assert_eq!(
            read_span(write_only, 4, 4, &AtomicBool::new(false)),
            Err(runtime::IO)
        );
        assert_eq!(
            read_span(File::open(&path).unwrap(), 8, 8, &AtomicBool::new(false)),
            Err(INVALID)
        );
        assert_eq!(
            read_span(File::open(&path).unwrap(), 2, 2, &AtomicBool::new(false)),
            Err(INVALID)
        );
        assert_eq!(open_directory(&root.join("absent")).unwrap_err(), MISSING);
        assert_eq!(open_directory(&path).unwrap_err(), INVALID);
        assert_eq!(open_directory(&root.join("..")).unwrap_err(), INVALID);
        assert_eq!(
            inventory(&File::open(&path).unwrap(), &AtomicBool::new(false)).unwrap_err(),
            INVALID
        );
        assert_eq!(fs::read(&path).unwrap(), b"abcd");
    }
    #[test]
    fn controlled_eio_preserves_storage_diagnostics_and_possible_write_effects() {
        struct FaultAfterPrefix {
            file: File,
            left: usize,
        }
        impl Read for FaultAfterPrefix {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(rustix::io::Errno::IO.into());
                }
                let count = buffer.len().min(self.left);
                let actual = self.file.read(&mut buffer[..count])?;
                self.left -= actual;
                Ok(actual)
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/package-injected-eio")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("original");
        fs::write(&path, b"abcd").unwrap();
        // This controlled reader injects EIO after a real file prefix; it is not a physical disk-failure test.
        let error = read_span(
            FaultAfterPrefix {
                file: File::open(&path).unwrap(),
                left: 2,
            },
            4,
            4,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error, runtime::IO);
        for (operation, state) in [
            (
                ProfilePackageOperation::Inspect {
                    directory: "/owned/package".into(),
                },
                PackageFilesystemState::Unchanged,
            ),
            (
                ProfilePackageOperation::Import {
                    directory: "/owned/package".into(),
                    expected_manifest_sha256: ProfileObjectHash::try_from("0".repeat(64)).unwrap(),
                    profile: ProfileCatalogReference {
                        id: earph_core::ids::ProfileId::new("owned").unwrap(),
                        revision: 1,
                        profile_sha256: ProfileObjectHash::try_from("0".repeat(64)).unwrap(),
                    },
                },
                PackageFilesystemState::InspectRequired,
            ),
        ] {
            let output = super::super::package_failure(&operation, error);
            assert!(
                matches!(output,ProfileWorkOutput::PackageIssue{error:observed,issue:ProfilePackageIssue{reason:ProfilePackageReason::IoFailure,filesystem_state,..}} if observed==runtime::IO&&filesystem_state==state)
            );
        }
        assert_eq!(fs::read(path).unwrap(), b"abcd");
    }
    #[test]
    fn syscall_classification_keeps_content_refusals_distinct() {
        for error in [
            rustix::io::Errno::IO,
            rustix::io::Errno::ACCESS,
            rustix::io::Errno::BADF,
            rustix::io::Errno::MFILE,
        ] {
            assert_eq!(filesystem_error(error.into()), runtime::IO);
        }
        for error in [
            rustix::io::Errno::LOOP,
            rustix::io::Errno::NOTDIR,
            rustix::io::Errno::ISDIR,
            rustix::io::Errno::NAMETOOLONG,
        ] {
            assert_eq!(filesystem_error(error.into()), INVALID);
        }
        assert_eq!(filesystem_error(rustix::io::Errno::NOENT.into()), MISSING);
        assert_eq!(
            filesystem_error(std::io::Error::from(std::io::ErrorKind::UnexpectedEof)),
            INVALID
        );
    }
}
