// ABOUTME: Launches only the explicit standalone owner and observes its actual identity and exit.
// ABOUTME: Retains private startup logs and never shuts down an owner on release.
use super::request_id;
use crate::{OwnerClient, Reply, Request};
use earph_core::{
    control::Command,
    ids::{OwnerId, ZoneId},
};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command as ProcessCommand},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostStatus {
    #[default]
    Idle,
    Starting,
    AttachedExisting {
        owner_id: OwnerId,
    },
    OwnedRunning {
        pid: u32,
        owner_id: OwnerId,
    },
    Exited {
        pid: u32,
        success: bool,
    },
    Failed {
        detail: String,
    },
}
struct LaunchReport {
    runtime: String,
    child: Option<Child>,
    status: HostStatus,
}
#[derive(Default)]
pub(super) struct OwnerHost {
    pub status: HostStatus,
    child: Option<Child>,
    launch: Option<JoinHandle<LaunchReport>>,
}
impl OwnerHost {
    pub fn launching(&self) -> bool {
        self.launch.is_some()
    }
    pub fn has_child(&self) -> bool {
        self.child.is_some()
    }
    pub fn launch(&mut self, executable: PathBuf, runtime: String) -> Result<(), String> {
        if self.launch.is_some() || self.child.is_some() {
            return Err(
                "This controller still owns a launch or running child; attach without launching"
                    .into(),
            );
        }
        if !executable.is_absolute() {
            return Err("Owner executable must be an absolute path".into());
        }
        self.status = HostStatus::Starting;
        self.launch = Some(
            thread::Builder::new()
                .name("earph-owner-launch".into())
                .spawn(move || launch(executable, runtime))
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
    pub fn poll(&mut self) -> Option<String> {
        let mut connected = None;
        if self.launch.as_ref().is_some_and(|j| j.is_finished()) {
            match self.launch.take().unwrap().join() {
                Ok(report) => {
                    if matches!(
                        report.status,
                        HostStatus::OwnedRunning { .. } | HostStatus::AttachedExisting { .. }
                    ) {
                        connected = Some(report.runtime);
                    }
                    self.child = report.child;
                    self.status = report.status;
                }
                Err(_) => {
                    self.status = HostStatus::Failed {
                        detail: "Owner launch worker panicked".into(),
                    }
                }
            }
        }
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.status = HostStatus::Exited {
                        pid: child.id(),
                        success: status.success(),
                    };
                    self.child.take();
                }
                Err(error) => {
                    self.status = HostStatus::Failed {
                        detail: format!("Cannot observe owned child: {error}"),
                    }
                }
                Ok(None) => {}
            }
        }
        connected
    }
}
impl Drop for OwnerHost {
    fn drop(&mut self) {
        // Normal window closure polls this worker first. This joining fallback owns no shutdown action.
        if let Some(join) = self.launch.take() {
            let _ = join.join();
        }
        // Child drop deliberately leaves a running standalone owner alive.
    }
}
fn state(client: &OwnerClient) -> Result<OwnerId, String> {
    let query = Request::query(
        request_id()?,
        ZoneId::new("main").map_err(|e| e.to_string())?,
        Command::State,
    )
    .map_err(|e| format!("{e:?}"))?;
    let reply = client.request(&query).map_err(|e| format!("{e:?}"))?;
    let Reply::Receipt(receipt) = reply else {
        return Err("Owner returned a top-level error".into());
    };
    receipt
        .state()
        .and_then(|s| s.owner_id.clone())
        .ok_or_else(|| "Owner snapshot lacks identity".into())
}
fn prefix(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(8193)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let truncated = bytes.len() > 8192;
    bytes.truncate(8192);
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&bytes),
        if truncated { " [truncated]" } else { "" }
    ))
}
fn launch(executable: PathBuf, runtime: String) -> LaunchReport {
    let mut child = None;
    let result = (|| -> Result<HostStatus, String> {
        let client = OwnerClient::attach(runtime.clone()).map_err(|e| format!("{e:?}"))?;
        if let Ok(owner_id) = state(&client) {
            return Ok(HostStatus::AttachedExisting { owner_id });
        }
        let parent = Path::new(&runtime)
            .parent()
            .ok_or("Missing runtime parent")?;
        let logs = parent.join(request_id()?.as_str());
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&logs)
            .map_err(|e| format!("Cannot create retained startup logs: {e}"))?;
        let stdout_path = logs.join("owner.stdout.log");
        let stderr_path = logs.join("owner.stderr.log");
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stdout_path)
            .map_err(|e| e.to_string())?;
        let errors = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stderr_path)
            .map_err(|e| e.to_string())?;
        child = Some(
            ProcessCommand::new(executable)
                .args(["owner", "--runtime"])
                .arg(&runtime)
                .stdin(std::process::Stdio::null())
                .stdout(output)
                .stderr(errors)
                .spawn()
                .map_err(|e| format!("Cannot launch owner: {e}; logs: {}", logs.display()))?,
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let process = child.as_mut().unwrap();
            let exited = process.try_wait().map_err(|e| e.to_string())?;
            let text = prefix(&stdout_path)?;
            let announcement = text
                .lines()
                .next()
                .and_then(|line| serde_json::from_str::<serde_json::Value>(line).ok());
            if let Some(status) = exited {
                child.take();
                if status.success()
                    && let Ok(owner_id) = state(&client)
                {
                    return Ok(HostStatus::AttachedExisting { owner_id });
                }
                return Err(format!(
                    "Owner exit {status}; {}; logs: {}",
                    prefix(&stderr_path)?,
                    logs.display()
                ));
            }
            if let Some(value) = announcement
                && value["status"] == "owner"
                && let Some(id) = value["owner_id"].as_str()
                && let Ok(owner_id) = state(&client)
                && owner_id.as_str() == id
            {
                return Ok(HostStatus::OwnedRunning {
                    pid: process.id(),
                    owner_id,
                });
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Owner startup outcome unknown; child retained; logs: {}",
                    logs.display()
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    })();
    LaunchReport {
        runtime,
        child,
        status: result.unwrap_or_else(|detail| HostStatus::Failed { detail }),
    }
}
