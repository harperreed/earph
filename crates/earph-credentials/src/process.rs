// ABOUTME: Supervises one pinned credential helper with bounded anonymous pipes.
// ABOUTME: Kills and reaps the exact owned child on cancellation or failure.
use crate::{FRAME_BYTES, StoreError, wire};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

pub(crate) fn executable_hash(path: &Path) -> Result<[u8; 32], StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| StoreError::Unavailable)?;
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o002 != 0
        || metadata.permissions().mode() & 0o111 == 0
        || (metadata.uid() != 0 && metadata.uid() != rustix::process::geteuid().as_raw())
        || metadata.len() > 128 * 1024 * 1024
    {
        return Err(StoreError::Unavailable);
    }
    let mut file = File::open(path).map_err(|_| StoreError::Unavailable)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 65536];
    let mut total = 0;
    loop {
        let n = file.read(&mut buf).map_err(|_| StoreError::Unavailable)?;
        if n == 0 {
            break;
        }
        total += n;
        if total > 128 * 1024 * 1024 {
            return Err(StoreError::Unavailable);
        }
        hash.update(&buf[..n]);
    }
    Ok(hash.finalize().into())
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), StoreError> {
    let flags = rustix::fs::fcntl_getfl(fd).map_err(|_| StoreError::Unavailable)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(|_| StoreError::Unavailable)
}
pub(crate) fn call(
    path: &Path,
    hash: &[u8; 32],
    request: &wire::Request,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<wire::Response, StoreError> {
    if executable_hash(path)? != *hash {
        return Err(StoreError::Unavailable);
    }
    let bytes = wire::frame(request)?;
    let deadline = Instant::now() + timeout;
    let mut child = OwnedChild(
        Command::new(path)
            .arg("credential-helper")
            .env_clear()
            .env("LANG", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| StoreError::Unavailable)?,
    );
    let mut input = Some(child.0.stdin.take().ok_or(StoreError::Unavailable)?);
    let mut output = child.0.stdout.take().ok_or(StoreError::Unavailable)?;
    nonblocking(input.as_ref().unwrap())?;
    nonblocking(&output)?;
    let mut written = 0;
    let mut response = Vec::new();
    let mut eof = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        if Instant::now() >= deadline {
            return Err(StoreError::Unavailable);
        }
        if let Some(stdin) = input.as_mut() {
            match stdin.write(&bytes[written..]) {
                Ok(0) => return Err(StoreError::Unavailable),
                Ok(n) => {
                    written += n;
                    if written == bytes.len() {
                        input = None;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(StoreError::Unavailable),
            }
        }
        let mut buf = [0; 8192];
        loop {
            match output.read(&mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    if response.len() + n > FRAME_BYTES + 4 {
                        return Err(StoreError::Unavailable);
                    }
                    response.extend_from_slice(&buf[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => return Err(StoreError::Unavailable),
            }
        }
        if let Some(status) = child.0.try_wait().map_err(|_| StoreError::Unavailable)?
            && eof
        {
            if !status.success() || input.is_some() {
                return Err(StoreError::Unavailable);
            }
            let reply: wire::Response = wire::read(&mut response.as_slice())?;
            if reply.version != 1 || reply.operation != request.operation {
                return Err(StoreError::Unavailable);
            }
            if reply
                .result
                .as_ref()
                .is_ok_and(|b| b.len() > crate::DOCUMENT_BYTES)
            {
                return Err(StoreError::Unavailable);
            }
            return Ok(reply);
        }
        thread::sleep(Duration::from_millis(2));
    }
}
