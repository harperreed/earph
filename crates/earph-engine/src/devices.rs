// ABOUTME: Supervises one read-only exec helper outside the owner control loop.
// ABOUTME: Requires explicit verified helper configuration and confirms child reaping before settlement.
use earph_core::{EngineError, ErrorCode, control::OutputBackend, devices::*, session::Session};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
mod native;
const HELPER_ROLE: &str = "device-query-helper";
const REQUEST_BYTES: usize = 2048;
const STDERR_BYTES: usize = 16 * 1024;
const EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;
const IO: EngineError = EngineError::new(
    ErrorCode::DeviceUnavailable,
    "read-only device helper failed",
);
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid device helper protocol");

#[derive(Clone)]
pub struct DeviceDiscoveryHelper {
    executable: PathBuf,
    executable_sha256: [u8; 32],
    alsa_config: Option<PathBuf>,
}
impl DeviceDiscoveryHelper {
    /// Supplies the actual production engine executable; embedding hosts must opt in explicitly.
    pub fn from_executable(path: &Path) -> Result<Self, EngineError> {
        if !path.is_absolute() {
            return Err(INVALID);
        }
        let executable = fs::canonicalize(path).map_err(|_| IO)?;
        let executable_sha256 = executable_hash(&executable)?;
        let alsa_config = if cfg!(target_os = "linux") {
            std::env::var_os("ALSA_CONFIG_PATH")
                .map(|value| {
                    let path = PathBuf::from(value);
                    if path.is_absolute() {
                        Ok(path)
                    } else {
                        std::env::current_dir()
                            .map(|directory| directory.join(path))
                            .map_err(|_| IO)
                    }
                })
                .transpose()?
        } else {
            None
        };
        if let Some(path) = &alsa_config
            && (path.as_os_str().len() > 4096 || !fs::metadata(path).is_ok_and(|m| m.is_file()))
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "ALSA_CONFIG_PATH must name a bounded regular native configuration file",
            ));
        }
        Ok(Self {
            executable,
            executable_sha256,
            alsa_config,
        })
    }
    pub(crate) fn backend(&self) -> Option<OutputBackend> {
        if cfg!(target_os = "linux") {
            Some(OutputBackend::Alsa)
        } else if cfg!(target_os = "macos") {
            Some(OutputBackend::CoreAudio)
        } else {
            None
        }
    }
    fn command(&self) -> Result<Command, EngineError> {
        if executable_hash(&self.executable)? != self.executable_sha256 {
            return Err(EngineError::new(
                ErrorCode::AuthDenied,
                "configured discovery executable changed",
            ));
        }
        let mut command = Command::new(&self.executable);
        command
            .arg(HELPER_ROLE)
            .env_clear()
            .env("LANG", "C")
            .env("LC_ALL", "C");
        if let Some(path) = &self.alsa_config {
            command.env("ALSA_CONFIG_PATH", path);
        }
        Ok(command)
    }
}
fn executable_hash(path: &Path) -> Result<[u8; 32], EngineError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| IO)?;
    let uid = rustix::process::geteuid().as_raw();
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
        || metadata.permissions().mode() & 0o002 != 0
        || (metadata.uid() != uid && metadata.uid() != 0)
        || metadata.len() > EXECUTABLE_BYTES
    {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "discovery helper must be a bounded trusted executable",
        ));
    }
    let mut file = File::open(path).map_err(|_| IO)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0_u64;
    loop {
        let n = file.read(&mut buffer).map_err(|_| IO)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > EXECUTABLE_BYTES {
            return Err(DEVICE_BUDGET);
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().into())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperRequest {
    schema_version: u32,
    query: DeviceQuery,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HelperResponse {
    schema_version: u32,
    result: Result<DeviceObservation, DeviceQueryError>,
}

/// Runs only the read-only production helper role; it never accepts runtime paths or owner credentials.
pub fn run_helper(input: &mut impl Read, output: &mut impl Write) -> Result<(), EngineError> {
    let mut header = [0; 4];
    input.read_exact(&mut header).map_err(|_| INVALID)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > REQUEST_BYTES {
        return Err(DEVICE_BUDGET);
    }
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).map_err(|_| INVALID)?;
    let mut trailing = [0];
    if input.read(&mut trailing).map_err(|_| INVALID)? != 0 {
        return Err(INVALID);
    }
    let request: HelperRequest = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if request.schema_version != 1 {
        return Err(INVALID);
    }
    request.query.validate()?;
    let result = native::query(&request.query).and_then(|observation| {
        observation
            .retained_capacity_bytes(&request.query)
            .map_err(DeviceQueryError::from)?;
        Ok(observation)
    });
    let mut response = HelperResponse {
        schema_version: 1,
        result,
    };
    let mut bytes = serde_json::to_vec(&response).map_err(|_| INVALID)?;
    if bytes.len() > MAX_DEVICE_RESULT_BYTES {
        response.result = Err(DEVICE_BUDGET.into());
        bytes = serde_json::to_vec(&response).map_err(|_| INVALID)?;
    }
    output
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .and_then(|_| output.write_all(&bytes))
        .map_err(|_| IO)
}

struct Work {
    job: DeviceQueryJob,
    cancel: Arc<AtomicBool>,
}
struct Completion {
    job: DeviceQueryJob,
    observation: DeviceObservation,
}
pub(crate) struct DeviceJobs {
    sender: Option<SyncSender<Work>>,
    receiver: Receiver<Completion>,
    active: Option<(DeviceQueryJob, Arc<AtomicBool>)>,
    join: Option<JoinHandle<()>>,
}
impl DeviceJobs {
    pub(crate) fn spawn(helper: Option<DeviceDiscoveryHelper>) -> Result<Self, EngineError> {
        let (sender, jobs) = mpsc::sync_channel::<Work>(1);
        let (complete, receiver) = mpsc::sync_channel(1);
        let Some(helper) = helper else {
            return Ok(Self {
                sender: None,
                receiver,
                active: None,
                join: None,
            });
        };
        let join = thread::Builder::new()
            .name("earph-device-supervisor".into())
            .spawn(move || {
                while let Ok(work) = jobs.recv() {
                    let result =
                        helper
                            .command()
                            .map_err(DeviceQueryError::from)
                            .and_then(|command| {
                                supervise(
                                    command,
                                    &work.job.query,
                                    &work.cancel,
                                    Duration::from_millis(DEVICE_QUERY_TIMEOUT_MS),
                                )
                            });
                    let observation = match result {
                        Ok(observation) => observation,
                        Err(error) => DeviceObservation::Failed {
                            error,
                            cleanup_confirmed: true,
                        },
                    };
                    if complete
                        .send(Completion {
                            job: work.job,
                            observation,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|_| IO)?;
        Ok(Self {
            sender: Some(sender),
            receiver,
            active: None,
            join: Some(join),
        })
    }
    pub(crate) fn submit(&mut self, session: &Session) -> Result<(), EngineError> {
        if self.active.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "device helper is still active or retiring",
            ));
        }
        let job = session.device_query_job().ok_or(INVALID)?;
        let cancel = Arc::new(AtomicBool::new(false));
        self.sender
            .as_ref()
            .ok_or(IO)?
            .try_send(Work {
                job: job.clone(),
                cancel: cancel.clone(),
            })
            .map_err(|_| IO)?;
        self.active = Some((job, cancel));
        Ok(())
    }
    pub(crate) fn poll(&mut self, session: &mut Session, now: u64) {
        while let Ok(completion) = self.receiver.try_recv() {
            if self
                .active
                .as_ref()
                .is_none_or(|(job, _)| job != &completion.job)
            {
                continue;
            }
            self.active = None;
            if let Err(error) =
                session.complete_device_query(&completion.job, now, completion.observation)
            {
                let _ = session.fail_device_query(&completion.job, now, error);
            }
        }
    }
    pub(crate) fn cancel(&self) {
        if let Some((_, cancel)) = &self.active {
            cancel.store(true, Ordering::Release);
        }
    }
    pub(crate) fn is_idle(&self) -> bool {
        self.active.is_none()
    }
}
impl Drop for DeviceJobs {
    fn drop(&mut self) {
        self.cancel();
        self.sender = None;
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn supervise(
    mut command: Command,
    query: &DeviceQuery,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<DeviceObservation, DeviceQueryError> {
    let request = serde_json::to_vec(&HelperRequest {
        schema_version: 1,
        query: query.clone(),
    })
    .map_err(|_| DeviceQueryError::from(INVALID))?;
    if request.len() > REQUEST_BYTES {
        return Err(DEVICE_BUDGET.into());
    }
    let mut input = (request.len() as u32).to_be_bytes().to_vec();
    input.extend(request);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|_| DeviceQueryError::from(IO))?;
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take().expect("configured pipe");
    let mut stderr = child.stderr.take().expect("configured pipe");
    let mut failure = None;
    for fd in [
        stdin.as_ref().map(|s| s.as_fd()),
        Some(stdout.as_fd()),
        Some(stderr.as_fd()),
    ]
    .into_iter()
    .flatten()
    {
        if rustix::io::ioctl_fionbio(fd, true).is_err() {
            failure = Some(IO);
        }
    }
    let deadline = Instant::now() + timeout;
    let mut written = 0;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut out_eof = false;
    let mut err_eof = false;
    let mut status = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            failure.get_or_insert(EngineError::new(
                ErrorCode::OperationCancelled,
                "device discovery cancelled",
            ));
        }
        if Instant::now() >= deadline {
            failure.get_or_insert(EngineError::new(
                ErrorCode::DeviceUnavailable,
                "device helper exceeded its deadline",
            ));
        }
        if failure.is_some() {
            stdin = None;
            if status.is_none() {
                let _ = child.kill();
            }
        } else if let Some(pipe) = &mut stdin {
            match pipe.write(&input[written..]) {
                Ok(0) => {
                    failure = Some(IO);
                }
                Ok(n) => {
                    written += n;
                    if written == input.len() {
                        stdin = None;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => {
                    failure = Some(IO);
                }
            }
        }
        if failure.is_none() {
            if let Err(error) = drain(
                &mut stdout,
                &mut out,
                MAX_DEVICE_RESULT_BYTES + 4,
                &mut out_eof,
            ) {
                failure = Some(error);
            }
            if let Err(error) = drain(&mut stderr, &mut err, STDERR_BYTES, &mut err_eof) {
                failure = Some(error);
            }
        }
        match child.try_wait() {
            Ok(Some(value)) => status = Some(value),
            Ok(None) => {}
            Err(_) => {
                failure.get_or_insert(IO);
            }
        }
        // Never publish completion or release the only slot until the actual child is reaped.
        if let Some(status) = status
            && (failure.is_some() || (out_eof && err_eof))
        {
            if let Some(error) = failure {
                return Err(error.into());
            }
            if !status.success() {
                return Err(IO.into());
            }
            if out.len() < 4 {
                return Err(INVALID.into());
            }
            let length = u32::from_be_bytes(
                out[..4]
                    .try_into()
                    .map_err(|_| DeviceQueryError::from(INVALID))?,
            ) as usize;
            if length == 0 || length > MAX_DEVICE_RESULT_BYTES || out.len() != length + 4 {
                return Err(INVALID.into());
            }
            let response: HelperResponse =
                serde_json::from_slice(&out[4..]).map_err(|_| DeviceQueryError::from(INVALID))?;
            if response.schema_version != 1 {
                return Err(INVALID.into());
            }
            let observation = response.result?;
            if matches!(observation, DeviceObservation::Failed { .. }) {
                return Err(INVALID.into());
            }
            observation
                .retained_capacity_bytes(query)
                .map_err(DeviceQueryError::from)?;
            return Ok(observation);
        }
        thread::sleep(Duration::from_millis(2));
    }
}
use std::os::fd::AsFd;
fn drain(
    pipe: &mut impl Read,
    bytes: &mut Vec<u8>,
    limit: usize,
    eof: &mut bool,
) -> Result<(), EngineError> {
    let mut buffer = [0; 8192];
    loop {
        let allowed = buffer.len().min(limit.saturating_sub(bytes.len()) + 1);
        match pipe.read(&mut buffer[..allowed]) {
            Ok(0) => {
                *eof = true;
                return Ok(());
            }
            Ok(n) => {
                if bytes.len() + n > limit {
                    return Err(DEVICE_BUDGET);
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(IO),
        }
    }
}
#[cfg(test)]
mod tests;
