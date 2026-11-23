// ABOUTME: Owns a standalone engine process with retained logs and explicit configuration.
// ABOUTME: Waits for actual CLI bootstrap and shuts down only its owned child.
use super::{query, unique};
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{OwnerClient, Reply, Request};
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as ProcessCommand},
    thread,
    time::{Duration, Instant},
};

pub struct ProcessOwner {
    pub child: Child,
    pub runtime: PathBuf,
    pub artifacts: PathBuf,
}
impl ProcessOwner {
    pub fn start() -> Self {
        Self::with_configuration(None)
    }
    pub fn with_configuration(configuration: Option<&std::path::Path>) -> Self {
        let executable = std::env::current_exe().unwrap();
        let binary = executable
            .parent()
            .and_then(|dependencies| dependencies.parent())
            .expect("test executable must reside in the Cargo profile directory")
            .join("earph-engine");
        assert!(
            binary.is_absolute() && binary.is_file(),
            "build earph-engine before running the desktop owner E2E"
        );
        let artifacts = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = artifacts.join("owner");
        let mut command = ProcessCommand::new(&binary);
        command
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .stdout(File::create(artifacts.join("owner.stdout.log")).unwrap())
            .stderr(File::create(artifacts.join("owner.stderr.log")).unwrap());
        if let Some(configuration) = configuration {
            command.env("ALSA_CONFIG_PATH", configuration);
        }
        let child = command.spawn().unwrap();
        println!("Process owner artifacts: {}", artifacts.display());
        let mut owner = Self {
            child,
            runtime,
            artifacts,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while !fs::read(owner.artifacts.join("owner.stdout.log")).is_ok_and(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .is_ok_and(|value| value["status"] == "owner")
        }) {
            assert!(
                owner.child.try_wait().unwrap().is_none(),
                "actual owner exited at startup"
            );
            assert!(
                Instant::now() < deadline,
                "actual owner startup exceeded deadline"
            );
            thread::sleep(Duration::from_millis(1));
        }
        owner
    }
    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("owner exit: {status}"))
            };
        }
        let client = OwnerClient::attach(self.runtime.to_str().unwrap().to_owned())
            .map_err(|error| format!("{error:?}"))?;
        let reply = client
            .request(&query(Command::State))
            .map_err(|error| format!("{error:?}"))?;
        let Reply::Receipt(receipt) = reply else {
            return Err("missing owner state receipt".into());
        };
        let request = Request::mutation(
            RequestId::new(unique()).unwrap(),
            receipt.state().ok_or("missing state")?,
            Command::OwnerShutdown,
        )
        .unwrap();
        client
            .request(&request)
            .map_err(|error| format!("{error:?}"))?;
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("owner exit: {status}"))
                };
            }
            if Instant::now() >= deadline {
                return Err("owned process did not exit after shutdown".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for ProcessOwner {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            let _ = fs::write(self.artifacts.join("cleanup-error.txt"), error);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
