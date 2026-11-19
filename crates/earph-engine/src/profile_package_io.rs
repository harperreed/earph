// ABOUTME: Applies profile manifest rules to the shared descriptor-pinned artifact primitives.
// ABOUTME: Preserves profile package errors, publication behavior, and retained filesystem stages.
use super::super::artifact_io::{self, ArtifactDirectory, ArtifactLimits, ArtifactStage};
use super::*;
#[cfg(test)]
use artifact_io::filesystem_error;
pub(super) use artifact_io::{DESTINATION, INVENTORY, MISSING};
use std::{ffi::OsStr, io::Read};
pub(super) type PackageDirectory = ArtifactDirectory;
pub(super) type PackageStage = ArtifactStage;
fn limits() -> ArtifactLimits {
    ArtifactLimits {
        max_entries: earph_core::profile_packages::MAX_PACKAGE_OBJECTS + 1,
        max_bytes: earph_core::profile_packages::MAX_PACKAGE_BYTES,
        private: false,
    }
}
#[cfg(test)]
fn open_directory(path: &Path) -> Result<File, EngineError> {
    artifact_io::walk_directory(path, false, &mut || Ok(()))
}
#[cfg(test)]
fn inventory(
    directory: &File,
    cancel: &AtomicBool,
) -> Result<std::collections::BTreeMap<String, artifact_io::Stamp>, EngineError> {
    artifact_io::inventory_with(directory, limits(), &mut || check(cancel))
}
impl ArtifactDirectory {
    pub fn open(path: &Path, cancel: &AtomicBool) -> Result<Self, EngineError> {
        Self::open_with(path, limits(), &mut || check(cancel))
    }
    pub fn read(
        &self,
        name: &str,
        limit: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, EngineError> {
        self.read_with(name, limit, &mut || check(cancel))
    }
    pub fn validate_inventory(
        &self,
        manifest: &earph_core::profile_packages::PackageManifest,
        cancel: &AtomicBool,
    ) -> Result<(), EngineError> {
        let mut expected = vec![("manifest.json".to_owned(), self.length("manifest.json")?)];
        expected.extend(manifest.objects.iter().map(|o| (o.file_name(), o.bytes)));
        self.validate_exact(&expected, &mut || check(cancel))
    }
}
pub(super) fn read_span(
    file: impl Read,
    size: usize,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, EngineError> {
    artifact_io::read_span_with(file, size, limit, &mut || check(cancel))
}
impl ArtifactStage {
    pub fn create(
        destination: &Path,
        runtime: &Path,
        cancel: &AtomicBool,
    ) -> Result<Self, EngineError> {
        Self::create_with(
            destination,
            runtime,
            ".package-",
            usize::MAX,
            &mut || check(cancel),
            &mut |f, _| f.sync_all(),
        )
        .map_err(|e| e.error)
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
        mut self,
        cancel: &AtomicBool,
        sync: &mut impl FnMut(&File) -> std::io::Result<()>,
    ) -> Result<(), EngineError> {
        self.publish_with(
            &mut || check(cancel),
            &mut |file, _| sync(file),
            &mut || Ok(()),
            false,
        )
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
