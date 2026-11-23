// ABOUTME: Owns bounded native credential operations outside transport runtimes.
// ABOUTME: Preserves uncertainty until an explicit fresh recovery read.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod process;
mod wire;

use earph_sendspin::credentials::CommittedCredentials;
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

const DOCUMENT_BYTES: usize = 65536;
const FRAME_BYTES: usize = DOCUMENT_BYTES * 4 + 8192;
const STATE_MAGIC: &[u8; 8] = b"EARPHCR1";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum NativeConfig {
    SecretService { address: String },
    Keychain { path: PathBuf },
}
impl fmt::Debug for NativeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeConfig([REDACTED])")
    }
}
impl NativeConfig {
    fn validate(&self) -> Result<(), StoreError> {
        let good = match self {
            Self::SecretService { address } => !address.is_empty() && address.len() <= 4096,
            Self::Keychain { path } => {
                path.is_absolute()
                    && path.as_os_str().len() <= 4096
                    && !path.as_os_str().as_encoded_bytes().contains(&0)
            }
        };
        if good {
            Ok(())
        } else {
            Err(StoreError::Unavailable)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreError {
    Missing,
    Locked,
    Unavailable,
    Ambiguous,
    InvalidDocument,
    Uncertain,
    Busy,
    Cancelled,
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "credential store: {self:?}")
    }
}
impl std::error::Error for StoreError {}

/// One cooperating store owner. Call only on a dedicated non-realtime supervisor thread.
/// Keep the same private lock path for every owner of this native document.
/// The lock stores only operation counters and uncertainty, never secret material.
pub struct CredentialStore {
    executable: PathBuf,
    executable_hash: [u8; 32],
    config: NativeConfig,
    lock: File,
    next: u64,
    uncertain: Option<u64>,
    recovered: bool,
    timeout: Duration,
}
impl fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialStore")
            .field("uncertain_operation", &self.uncertain)
            .finish_non_exhaustive()
    }
}
impl CredentialStore {
    pub fn open(
        executable: PathBuf,
        config: NativeConfig,
        lock_path: &Path,
        timeout: Duration,
    ) -> Result<Self, StoreError> {
        config.validate()?;
        if !executable.is_absolute()
            || !lock_path.is_absolute()
            || timeout.is_zero()
            || timeout > Duration::from_secs(30)
        {
            return Err(StoreError::Unavailable);
        }
        let directory = File::open(lock_path.parent().ok_or(StoreError::Unavailable)?)
            .map_err(|_| StoreError::Unavailable)?;
        let mut lock = File::from(
            rustix::fs::openat(
                &directory,
                lock_path.file_name().ok_or(StoreError::Unavailable)?,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::from_raw_mode(0o600),
            )
            .map_err(|_| StoreError::Unavailable)?,
        );
        let metadata = lock.metadata().map_err(|_| StoreError::Unavailable)?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(StoreError::Unavailable);
        }
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|e| {
            if e == rustix::io::Errno::WOULDBLOCK {
                StoreError::Busy
            } else {
                StoreError::Unavailable
            }
        })?;
        let mut state = Vec::new();
        if (&mut lock).take(25).read_to_end(&mut state).is_err() {
            let _ = flock(&lock, FlockOperation::Unlock);
            return Err(StoreError::Unavailable);
        }
        let (next, uncertain) = if state.is_empty() {
            (1, None)
        } else if state.len() == 24 && &state[..8] == STATE_MAGIC {
            let next = u64::from_be_bytes(state[8..16].try_into().unwrap());
            let pending = u64::from_be_bytes(state[16..24].try_into().unwrap());
            if next == 0 || pending >= next {
                let _ = flock(&lock, FlockOperation::Unlock);
                return Err(StoreError::Uncertain);
            }
            (next, if pending == 0 { None } else { Some(pending) })
        } else {
            let _ = flock(&lock, FlockOperation::Unlock);
            return Err(StoreError::Uncertain);
        };
        // A missing helper is an access error when called, never store absence.
        let executable_hash = process::executable_hash(&executable).unwrap_or([0; 32]);
        let mut store = Self {
            executable,
            executable_hash,
            config,
            lock,
            next,
            uncertain,
            recovered: false,
            timeout,
        };
        store.save_state()?;
        // File sync does not persist its directory entry. Keep this before any helper can run.
        directory.sync_all().map_err(|_| StoreError::Uncertain)?;
        Ok(store)
    }
    pub fn uncertain_operation(&self) -> Option<u64> {
        self.uncertain
    }
    pub fn load(&mut self, cancel: &AtomicBool) -> Result<CommittedCredentials, StoreError> {
        if self.uncertain.is_some() {
            return Err(StoreError::Uncertain);
        }
        self.read(cancel, false)
    }
    /// Fresh readback is the only operation permitted after an uncertain write.
    /// Reading never proves a timed-out native mutation has reached a terminal state.
    pub fn recover(&mut self, cancel: &AtomicBool) -> Result<CommittedCredentials, StoreError> {
        self.read(cancel, true)
    }
    fn read(
        &mut self,
        cancel: &AtomicBool,
        recovery: bool,
    ) -> Result<CommittedCredentials, StoreError> {
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let operation = self.operation()?;
        let result = self
            .call(operation, wire::Action::Read, cancel)
            .and_then(|bytes| decode(&bytes));
        if recovery {
            self.recovered = result.is_ok() || matches!(result, Err(StoreError::Missing));
        }
        result
    }
    /// Caller attests that the uncertain native mutation has reached a terminal state.
    /// A fresh read alone cannot establish this. Prove server completion outside this API first.
    /// Requires the exact pending operation and makes a fresh cancellable read before clearing it.
    pub fn acknowledge_native_completion(
        &mut self,
        operation: u64,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if self.uncertain != Some(operation) || !self.recovered {
            return Err(StoreError::Uncertain);
        }
        match self.read(cancel, false) {
            Ok(_) | Err(StoreError::Missing) => {}
            Err(error) => return Err(error),
        }
        self.uncertain = None;
        self.recovered = false;
        self.save_state()
    }
    /// Acknowledges only an exact fresh native readback. Never retries a write.
    pub fn commit(
        &mut self,
        document: &CommittedCredentials,
        cancel: &AtomicBool,
    ) -> Result<u64, StoreError> {
        if self.uncertain.is_some() {
            return Err(StoreError::Uncertain);
        }
        if cancel.load(Ordering::Acquire) {
            return Err(StoreError::Cancelled);
        }
        let bytes = document
            .encode_secret()
            .map_err(|_| StoreError::InvalidDocument)?;
        let operation = self.operation()?;
        // Persist intent before the child can issue any mutation.
        self.uncertain = Some(operation);
        self.recovered = false;
        self.save_state()?;
        let result = self.call(
            operation,
            wire::Action::Commit(bytes.as_secret_bytes().to_vec()),
            cancel,
        );
        match result {
            Ok(readback) if readback == bytes.as_secret_bytes() => {
                self.uncertain = None;
                self.save_state()?;
                Ok(operation)
            }
            // The helper returns these only before issuing a mutation.
            Err(
                e @ (StoreError::Missing
                | StoreError::Locked
                | StoreError::Unavailable
                | StoreError::Ambiguous
                | StoreError::InvalidDocument
                | StoreError::Cancelled),
            ) => {
                self.uncertain = None;
                self.save_state()?;
                Err(e)
            }
            _ => Err(StoreError::Uncertain),
        }
    }
    fn operation(&mut self) -> Result<u64, StoreError> {
        let operation = self.next;
        self.next = self.next.checked_add(1).ok_or(StoreError::Unavailable)?;
        self.save_state()?;
        Ok(operation)
    }
    fn save_state(&mut self) -> Result<(), StoreError> {
        let mut state = Vec::with_capacity(24);
        state.extend_from_slice(STATE_MAGIC);
        state.extend_from_slice(&self.next.to_be_bytes());
        state.extend_from_slice(&self.uncertain.unwrap_or(0).to_be_bytes());
        if self
            .lock
            .seek(SeekFrom::Start(0))
            .and_then(|_| self.lock.write_all(&state))
            .and_then(|_| self.lock.set_len(24))
            .and_then(|_| self.lock.sync_all())
            .is_err()
        {
            self.uncertain = Some(self.uncertain.unwrap_or(self.next.saturating_sub(1).max(1)));
            self.recovered = false;
            return Err(StoreError::Uncertain);
        }
        Ok(())
    }
    fn call(
        &self,
        operation: u64,
        action: wire::Action,
        cancel: &AtomicBool,
    ) -> Result<Vec<u8>, StoreError> {
        let write = matches!(action, wire::Action::Commit(_));
        let request = wire::Request {
            version: 1,
            operation,
            config: self.config.clone(),
            action,
        };
        process::call(
            &self.executable,
            &self.executable_hash,
            &request,
            cancel,
            self.timeout,
        )
        .map_err(|e| if write { StoreError::Uncertain } else { e })?
        .result
    }
}
impl Drop for CredentialStore {
    fn drop(&mut self) {
        let _ = flock(&self.lock, FlockOperation::Unlock);
    }
}
fn decode(bytes: &[u8]) -> Result<CommittedCredentials, StoreError> {
    CommittedCredentials::decode_secret(bytes).map_err(|_| StoreError::InvalidDocument)
}

/// Runs one native operation. Use only in the dedicated helper executable.
pub fn run_helper(input: &mut impl Read, output: &mut impl Write) -> Result<(), StoreError> {
    let request: wire::Request = wire::read(input)?;
    if request.version != 1 || request.operation == 0 {
        return Err(StoreError::Unavailable);
    }
    request.config.validate()?;
    if let wire::Action::Commit(bytes) = &request.action {
        decode(bytes)?;
    }
    let result = native(&request.config, &request.action);
    let response = wire::Response {
        version: 1,
        operation: request.operation,
        result,
    };
    wire::write(output, &response)
}
fn native(config: &NativeConfig, action: &wire::Action) -> Result<Vec<u8>, StoreError> {
    #[cfg(target_os = "linux")]
    if let NativeConfig::SecretService { address } = config {
        return linux::execute(address, action);
    }
    #[cfg(target_os = "macos")]
    if let NativeConfig::Keychain { path } = config {
        return macos::execute(path, action);
    }
    Err(StoreError::Unavailable)
}
