// ABOUTME: Exercises real diagnostic artifact jobs through owner IPC, CLI and the foreign client.
// ABOUTME: Retains exact exported bytes and fixtures while joining each silent owner normally.
use earph_core::{
    EngineError,
    control::ReceiptStatus,
    diagnostics::{DiagnosticArtifactReceipt, PreviewRead},
};
use earph_engine::{EngineOwner, request};
use earph_ffi::LocalClient;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::{fs::DirBuilderExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn envelope(id: &str, state: Option<&Value>, payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":state.map(|s|s["owner_id"].clone()),"session_id":state.map(|s|s["session_id"].clone()),"expected_revision":state.map(|s|s["revision"].clone()),"payload":payload})).unwrap()
}
struct Owner {
    runtime: PathBuf,
    join: Option<JoinHandle<Result<(), EngineError>>>,
}
impl Owner {
    fn start(runtime: PathBuf) -> Self {
        let owner = EngineOwner::bind(&runtime).unwrap();
        eprintln!(
            "owner fixture pid={} runtime={}",
            std::process::id(),
            runtime.display()
        );
        Self {
            runtime,
            join: Some(thread::spawn(move || owner.serve())),
        }
    }
    fn state(&self) -> Value {
        let bytes = request(
            &self.runtime,
            &envelope("state", None, json!({"method":"state.get"})),
        )
        .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()["receipt"]["result"]["value"].clone()
    }
    fn stop(&mut self) {
        if let Some(join) = self.join.take() {
            let bytes = request(
                &self.runtime,
                &envelope(
                    "shutdown",
                    Some(&self.state()),
                    json!({"method":"owner.shutdown"}),
                ),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap()["receipt"]["status"],
                "applied"
            );
            join.join().unwrap().unwrap();
            eprintln!(
                "owner fixture joined exit=0 runtime={}",
                self.runtime.display()
            );
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}
fn terminal(client: &LocalClient, command: &[u8]) -> Value {
    let end = Instant::now() + Duration::from_secs(8);
    loop {
        let bytes = client.request(command.to_vec()).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if value["receipt"]["status"] != "accepted" {
            return value;
        }
        assert!(Instant::now() < end, "pending operation did not retire");
        thread::sleep(Duration::from_millis(2));
    }
}
fn validated(value: &Value) -> DiagnosticArtifactReceipt {
    let status: ReceiptStatus = serde_json::from_value(value["receipt"]["status"].clone()).unwrap();
    DiagnosticArtifactReceipt::decode(
        &serde_json::to_vec(&value["receipt"]["result"]["value"]).unwrap(),
        status,
    )
    .unwrap()
}
fn cli(runtime: &Path, command: &[u8]) -> Value {
    let binary = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("earph-engine");
    let mut child = Command::new(binary)
        .args(["request", "--runtime"])
        .arg(runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id();
    child.stdin.take().unwrap().write_all(command).unwrap();
    let output = child.wait_with_output().unwrap();
    eprintln!("CLI fixture pid={pid} exit={:?}", output.status.code());
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn actual_owner_cli_and_foreign_client_reconcile_exact_export_and_restart() {
    let token = format!(
        "{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let runtime = PathBuf::from("/tmp").join(format!("eda-{token}"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/diagnostic-owner-artifacts")
        .join(&token);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .unwrap();
    let root = fs::canonicalize(root).unwrap();
    let mut owner = Owner::start(runtime.clone());
    let initial = owner.state();
    assert_eq!(initial["output_id"], Value::Null);
    assert_eq!(initial["muted"], true);
    let client = LocalClient::new(runtime.to_str().unwrap().into()).unwrap();
    let preview_command = envelope(
        "preview",
        Some(&initial),
        json!({"method":"diagnostics.preview"}),
    );
    let response: Value =
        serde_json::from_slice(&client.request(preview_command).unwrap()).unwrap();
    let preview =
        PreviewRead::decode(&serde_json::to_vec(&response["receipt"]["result"]["value"]).unwrap())
            .unwrap();
    let destination = root.join("bundle");
    let export = envelope(
        "lost-export",
        Some(&initial),
        json!({"method":"diagnostics.export","directory":destination,"expected_sha256":preview.report.sha256}),
    );
    // Deliver a real request and close its connection without reading the reply.
    let endpoint: Value =
        serde_json::from_slice(&fs::read(runtime.join("endpoint.json")).unwrap()).unwrap();
    let mut stream =
        UnixStream::connect(runtime.join(endpoint["socket"].as_str().unwrap())).unwrap();
    stream
        .write_all(&(export.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&export).unwrap();
    drop(stream);
    let complete = terminal(&client, &export);
    assert_eq!(complete["receipt"]["status"], "applied");
    let receipt = validated(&complete);
    assert_eq!(
        serde_json::to_value(receipt.effect).unwrap(),
        "installed_verified"
    );
    assert_eq!(
        fs::read(destination.join("diagnostics.json")).unwrap(),
        preview.report.bytes.as_bytes()
    );
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 2);
    assert_eq!(
        serde_json::from_slice::<Value>(&client.request(export.clone()).unwrap()).unwrap(),
        complete
    );
    let inspect = envelope(
        "inspect",
        Some(&initial),
        json!({"method":"diagnostics.inspect","directory":destination,"expected_sha256":preview.report.sha256}),
    );
    let accepted = cli(&runtime, &inspect);
    assert!(matches!(
        accepted["receipt"]["status"].as_str(),
        Some("accepted" | "applied")
    ));
    let inspected = terminal(&client, &inspect);
    assert_eq!(
        serde_json::to_value(validated(&inspected).inspection).unwrap(),
        "matching"
    );
    let cancel = envelope(
        "cancel-complete",
        Some(&initial),
        json!({"method":"diagnostics.cancel","request_id":"lost-export"}),
    );
    let cancelled = cli(&runtime, &cancel);
    assert_eq!(
        cancelled["receipt"]["result"]["value"]["state"],
        "already_terminal"
    );
    let after = owner.state();
    assert_eq!(after["revision"], initial["revision"]);
    assert_eq!(after["output_id"], Value::Null);
    assert_eq!(after["muted"], true);
    owner.stop();
    let mut restarted = Owner::start(runtime.clone());
    let current = restarted.state();
    assert_ne!(current["owner_id"], initial["owner_id"]);
    let stale: Value = serde_json::from_slice(&client.request(inspect).unwrap()).unwrap();
    assert_eq!(stale["receipt"]["error"]["code"], "STALE_SESSION");
    let reconcile = envelope(
        "reconcile",
        Some(&current),
        json!({"method":"diagnostics.inspect","directory":destination,"expected_sha256":preview.report.sha256}),
    );
    assert_eq!(
        serde_json::to_value(validated(&terminal(&client, &reconcile)).inspection).unwrap(),
        "matching"
    );
    assert_eq!(
        fs::read(destination.join("diagnostics.json")).unwrap(),
        preview.report.bytes.as_bytes()
    );
    restarted.stop();
}
