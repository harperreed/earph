// ABOUTME: Shares fallible CLI delivery and regular-fd file-size admission.
// ABOUTME: Installs file-limit signal handling only when a binary explicitly initializes its process.
use std::{
    io::{self, Write},
    os::fd::AsFd,
};

/// Initialize only at CLI startup, before I/O or additional threads.
/// Embedded library paths never call this function.
pub fn initialize_process() -> io::Result<()> {
    static FILE_LIMIT_REACHED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    signal_hook::flag::register(signal_hook::consts::SIGXFSZ, &FILE_LIMIT_REACHED).map(|_| ())
}

/// Write bytes through one fd policy. Buffered writers must start empty.
pub fn write_bytes(output: &mut (impl Write + AsFd), bytes: &[u8]) -> io::Result<()> {
    write_with_boundary(output, bytes, &mut || Ok(()))
}
fn write_with_boundary(
    output: &mut (impl Write + AsFd),
    mut bytes: &[u8],
    before_write: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        let metadata = rustix::fs::fstat(output.as_fd())?;
        let regular = rustix::fs::FileType::from_raw_mode(metadata.st_mode).is_file();
        let limit = regular
            .then(|| rustix::process::getrlimit(rustix::process::Resource::Fsize).current)
            .flatten();
        let admitted = if regular {
            let position = rustix::fs::seek(output.as_fd(), rustix::fs::SeekFrom::Current(0))?;
            let flags = rustix::fs::fcntl_getfl(output.as_fd())?;
            let offset = if flags.contains(rustix::fs::OFlags::APPEND) {
                u64::try_from(metadata.st_size)
                    .map_err(|_| io::Error::other("invalid regular-file size"))?
            } else {
                position
            };
            limit.map_or(bytes.len(), |limit| {
                usize::try_from(limit.saturating_sub(offset))
                    .unwrap_or(usize::MAX)
                    .min(bytes.len())
            })
        } else {
            bytes.len()
        };
        if admitted == 0 {
            return Err(rustix::io::Errno::FBIG.into());
        }
        before_write()?;
        let written = match output.write(&bytes[..admitted]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "CLI writer accepted no bytes",
                ));
            }
            Ok(written) => written,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if regular {
            output.flush()?;
            if let Some(limit) = limit
                && rustix::fs::seek(output.as_fd(), rustix::fs::SeekFrom::Current(0))? > limit
            {
                return Err(rustix::io::Errno::FBIG.into());
            }
        }
        bytes = &bytes[written..];
    }
    output.flush()
}
/// Deliver one compact JSON line and flush through the caller's locked writer.
pub fn write_json_line(
    value: &impl serde::Serialize,
    output: &mut (impl Write + AsFd),
) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_bytes(output, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs::{self, OpenOptions},
        io::Read,
        os::unix::{
            fs::{DirBuilderExt, OpenOptionsExt},
            net::UnixStream,
            process::ExitStatusExt,
        },
        path::PathBuf,
        process::Command,
    };

    fn retain_output(directory: &std::path::Path, output: &std::process::Output) {
        for (name, bytes) in [
            ("stdout.log", &output.stdout),
            ("stderr.log", &output.stderr),
        ] {
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(directory.join(name))
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }
    }

    #[test]
    fn file_limit_signal_child() {
        let Some(policy) = std::env::var_os("EARPH_TEST_FILE_LIMIT_SIGNAL") else {
            return;
        };
        if policy == "initialized" {
            initialize_process().unwrap();
        }
        let pid = rustix::process::Pid::from_raw(std::process::id() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::XFSZ).unwrap();
    }

    #[test]
    fn explicit_cli_initialization_handles_file_limit_signal() {
        for enabled in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "earph-signal-{}-{enabled}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "cli_io::tests::file_limit_signal_child",
                    "--nocapture",
                ])
                .env(
                    "EARPH_TEST_FILE_LIMIT_SIGNAL",
                    if enabled { "initialized" } else { "default" },
                )
                .output()
                .unwrap();
            retain_output(&directory, &output);
            if enabled {
                assert!(output.status.success());
            } else {
                assert_eq!(output.status.signal(), Some(signal_hook::consts::SIGXFSZ));
            }
        }
    }

    #[test]
    fn append_race_child() {
        let Some(directory) = std::env::var_os("EARPH_TEST_APPEND_CASE") else {
            return;
        };
        let directory = PathBuf::from(directory);
        let enabled = std::env::var_os("EARPH_TEST_APPEND_POLICY").is_some();
        let mut limit = rustix::process::getrlimit(rustix::process::Resource::Fsize);
        limit.current = Some(8192);
        rustix::process::setrlimit(rustix::process::Resource::Fsize, limit).unwrap();
        if enabled {
            initialize_process().unwrap();
        }
        assert_eq!(
            rustix::process::getrlimit(rustix::process::Resource::Fsize),
            limit
        );
        let mut output = OpenOptions::new()
            .append(true)
            .open(directory.join("append.json"))
            .unwrap();
        let mut barrier =
            std::fs::File::from(std::io::stdin().as_fd().try_clone_to_owned().unwrap());
        let result = write_with_boundary(&mut output, b"{}\n", &mut || {
            barrier.write_all(b"ready")?;
            let mut go = [0; 2];
            barrier.read_exact(&mut go)?;
            assert_eq!(&go, b"go");
            Ok(())
        });
        assert_eq!(
            result.unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::FBIG.raw_os_error())
        );
        assert_eq!(
            rustix::process::getrlimit(rustix::process::Resource::Fsize),
            limit
        );
    }

    #[test]
    fn concurrent_append_reports_file_limit_failure_with_platform_signal_policy() {
        for enabled in [false, true] {
            let directory = std::env::temp_dir().join(format!(
                "earph-append-{}-{}-{}",
                std::process::id(),
                enabled,
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
            OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(directory.join("append.json"))
                .unwrap();
            let (mut barrier, child_barrier) = UnixStream::pair().unwrap();
            barrier
                .set_read_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            barrier
                .set_write_timeout(Some(std::time::Duration::from_secs(20)))
                .unwrap();
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", "cli_io::tests::append_race_child", "--nocapture"])
                .env("EARPH_TEST_APPEND_CASE", &directory);
            if enabled {
                command.env("EARPH_TEST_APPEND_POLICY", "yes");
            } else {
                command.env_remove("EARPH_TEST_APPEND_POLICY");
            }
            let child = command
                .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                    child_barrier,
                )))
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut ready = [0; 5];
            barrier.read_exact(&mut ready).unwrap();
            assert_eq!(&ready, b"ready");
            let mut appended = OpenOptions::new()
                .append(true)
                .open(directory.join("append.json"))
                .unwrap();
            appended.write_all(&vec![b'x'; 8192]).unwrap();
            appended.sync_all().unwrap();
            barrier.write_all(b"go").unwrap();
            let output = child.wait_with_output().unwrap();
            retain_output(&directory, &output);
            let status = output.status;
            if enabled {
                assert!(
                    status.success(),
                    "initialized child terminated after real concurrent append: {status:?}"
                );
            } else if cfg!(target_os = "linux") {
                assert_eq!(status.signal(), Some(signal_hook::consts::SIGXFSZ));
            } else {
                assert!(status.success() || status.signal() == Some(signal_hook::consts::SIGXFSZ));
            }
            let bytes = fs::read(directory.join("append.json")).unwrap();
            assert_eq!(&bytes[..8192], vec![b'x'; 8192]);
            assert!(
                bytes.len() == 8192 || (cfg!(target_os = "macos") && bytes.len() == 8195),
                "unexpected append length: {}",
                bytes.len()
            );
            if bytes.len() > 8192 {
                assert_eq!(&bytes[8192..], b"{}\n");
            }
        }
    }
}
