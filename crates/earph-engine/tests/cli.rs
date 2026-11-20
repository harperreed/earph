// ABOUTME: Tests the actual owner and client executables through private Unix IPC.
// ABOUTME: Proves second-instance attachment, crash recovery, and typed CLI failures.
use serde_json::{Value, json};
mod support;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Output, Stdio},
};
use support::runtime;

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn start(runtime: &Path) -> Host {
    let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["owner", "--runtime"])
        .arg(runtime)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["status"],
        "owner"
    );
    Host(child)
}
fn call(runtime: &Path, command: &Value) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["request", "--runtime"])
        .arg(runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{command}").unwrap();
    child.wait_with_output().unwrap()
}
fn envelope(request: &str, owner: Value, revision: u64, payload: Value) -> Value {
    json!({"schema_version":1,"request_id":request,"zone_id":"main","owner_id":owner,"session_id":null,"expected_revision":revision,"payload":payload})
}
fn state(runtime: &Path, id: &str) -> Value {
    let output = call(
        runtime,
        &envelope(id, Value::Null, 0, json!({"method":"state.get"})),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["receipt"]["result"]["value"].clone()
}

#[test]
fn cli_renderer_query_replays_exactly_while_host_authority_remains_disabled() {
    let runtime = runtime();
    let mut host = start(&runtime);
    let initial = state(&runtime, "renderer-initial");
    let query = envelope(
        "cli-renderer",
        initial["owner_id"].clone(),
        0,
        json!({"method":"renderer.get"}),
    );
    let first = call(&runtime, &query);
    assert!(first.status.success());
    assert_eq!(call(&runtime, &query).stdout, first.stdout);
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(value["receipt"]["result"]["kind"], "renderer");
    let renderer: earph_core::renderer::RendererSnapshot =
        serde_json::from_value(value["receipt"]["result"]["value"].clone()).unwrap();
    assert!(!renderer.enabled);
    assert_eq!(renderer.controller_count, 0);
    let grant = call(
        &runtime,
        &envelope(
            "cli-disabled-renderer",
            initial["owner_id"].clone(),
            0,
            json!({
                "method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":100,
                "grants":{"controllers":[{"id":"controller-a","peers":["192.168.1.2"],"playback":null,"rights":["stage","transport"]}]}
            }),
        ),
    );
    assert_eq!(grant.status.code(), Some(1));
    let rejected: Value = serde_json::from_slice(&grant.stdout).unwrap();
    assert_eq!(
        rejected["receipt"]["error"]["code"],
        "CAPABILITY_UNSUPPORTED"
    );
    let untrusted = call(
        &runtime,
        &envelope(
            "cli-untrusted-renderer",
            initial["owner_id"].clone(),
            0,
            json!({
                "method":"renderer.request","controller_id":"controller-a","grant_generation":1,
                "action":{"operation":"stage_uri","expected_staging_generation":0,"uri":"untrusted:metadata","metadata":"private"}
            }),
        ),
    );
    assert_eq!(untrusted.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&untrusted.stdout).unwrap()["receipt"]["error"]["code"],
        "AUTH_DENIED"
    );
    assert_eq!(state(&runtime, "renderer-after"), initial);
    assert!(
        call(
            &runtime,
            &envelope(
                "renderer-shutdown",
                initial["owner_id"].clone(),
                0,
                json!({"method":"owner.shutdown"})
            )
        )
        .status
        .success()
    );
    assert!(host.0.wait().unwrap().success());
}

#[test]
fn executable_attaches_and_recovers_after_owner_process_crash() {
    let runtime = runtime();
    let mut host = start(&runtime);
    let first = state(&runtime, "first-state");
    let append = envelope(
        "append",
        first["owner_id"].clone(),
        0,
        json!({"method":"queue.append","path":"雪.flac"}),
    );
    let output = call(&runtime, &append);
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["receipt"]["revision"],
        1
    );
    let attached = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["owner", "--runtime"])
        .arg(&runtime)
        .output()
        .unwrap();
    assert!(attached.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&attached.stdout).unwrap()["receipt"]["result"]["value"]["owner_id"],
        first["owner_id"]
    );
    assert!(host.0.try_wait().unwrap().is_none());
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    let mut restarted = start(&runtime);
    let restored = state(&runtime, "restored-state");
    assert_eq!(restored["transport"], "paused");
    assert_eq!(restored["muted"], true);
    assert_eq!(restored["output_id"], Value::Null);
    assert_ne!(restored["owner_id"], first["owner_id"]);
    let stale = call(&runtime, &append);
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&stale.stdout).unwrap()["receipt"]["error"]["code"],
        "STALE_SESSION"
    );
    let queue = call(
        &runtime,
        &envelope(
            "queue",
            Value::Null,
            0,
            json!({"method":"queue.get","offset":0,"limit":64}),
        ),
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&queue.stdout).unwrap()["receipt"]["result"]["value"]["entries"]
            [0]["path"],
        "雪.flac"
    );
    let shutdown = call(
        &runtime,
        &envelope(
            "shutdown",
            restored["owner_id"].clone(),
            1,
            json!({"method":"owner.shutdown"}),
        ),
    );
    assert!(shutdown.status.success());
    assert!(restarted.0.wait().unwrap().success());
}

#[test]
fn executable_reports_usage_and_remote_protocol_failures() {
    let usage = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .arg("owner")
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let runtime = runtime();
    let mut host = start(&runtime);
    let output = call(
        &runtime,
        &envelope(
            "unsupported",
            Value::Null,
            0,
            json!({"method":"not.supported"}),
        ),
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["error"]["code"],
        "CAPABILITY_UNSUPPORTED"
    );
    let state = state(&runtime, "state");
    assert!(
        call(
            &runtime,
            &envelope(
                "shutdown",
                state["owner_id"].clone(),
                0,
                json!({"method":"owner.shutdown"})
            )
        )
        .status
        .success()
    );
    assert!(host.0.wait().unwrap().success());
}

#[test]
fn engine_cli_handles_file_limit_signal_and_continues_real_owner_requests() {
    let runtime = runtime();
    let mut host = start(&runtime);
    let pid = rustix::process::Pid::from_raw(host.0.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::XFSZ).unwrap();
    let observed = state(&runtime, "after-file-limit-signal");
    assert!(host.0.try_wait().unwrap().is_none());
    assert!(
        call(
            &runtime,
            &envelope(
                "shutdown-after-signal",
                observed["owner_id"].clone(),
                observed["revision"].as_u64().unwrap(),
                json!({"method":"owner.shutdown"})
            )
        )
        .status
        .success()
    );
    assert!(host.0.wait().unwrap().success());
}

#[test]
fn cli_stages_and_clears_unadmitted_metadata_without_persisting_private_input() {
    let runtime = runtime();
    let mut host = start(&runtime);
    let initial = state(&runtime, "staging-initial");
    let stage = envelope(
        "cli-stage-uri",
        initial["owner_id"].clone(),
        initial["revision"].as_u64().unwrap(),
        json!({"method":"sources.stage_uri","uri":"http://example.invalid/?token=private-fixture","metadata":"private metadata"}),
    );
    let applied = call(&runtime, &stage);
    assert!(applied.status.success());
    let value: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(value["receipt"]["result"]["kind"], "sources");
    assert_eq!(
        value["receipt"]["result"]["value"]["staged"]["admission"],
        "unadmitted"
    );
    assert_eq!(call(&runtime, &stage).stdout, applied.stdout);
    assert!(
        !String::from_utf8(applied.stdout)
            .unwrap()
            .contains("private")
    );
    let checkpoint = std::fs::read_to_string(runtime.join("queue.json")).unwrap();
    assert!(!checkpoint.contains("private-fixture"));
    assert!(!checkpoint.contains("private metadata"));
    let query = call(
        &runtime,
        &envelope(
            "cli-sources",
            Value::Null,
            0,
            json!({"method":"sources.get"}),
        ),
    );
    assert!(query.status.success());
    let query: Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(query["receipt"]["result"], value["receipt"]["result"]);
    let after = state(&runtime, "staging-after");
    let mut expected = initial;
    expected["revision"] = json!(1);
    expected["event_sequence"] = json!(1);
    assert_eq!(after, expected);
    let clear = call(
        &runtime,
        &envelope(
            "cli-clear-uri",
            after["owner_id"].clone(),
            1,
            json!({"method":"sources.stage_uri","uri":"","metadata":""}),
        ),
    );
    assert!(clear.status.success());
    let clear: Value = serde_json::from_slice(&clear.stdout).unwrap();
    assert_eq!(
        clear["receipt"]["result"]["value"],
        json!({"generation":2,"staged":null})
    );
    host.0.kill().unwrap();
    host.0.wait().unwrap();
    let _replacement = start(&runtime);
    let query = call(
        &runtime,
        &envelope(
            "cli-sources-restored",
            Value::Null,
            0,
            json!({"method":"sources.get"}),
        ),
    );
    assert!(query.status.success());
    let query: Value = serde_json::from_slice(&query.stdout).unwrap();
    assert_eq!(
        query["receipt"]["result"]["value"],
        json!({"generation":0,"staged":null})
    );
}

#[test]
fn cli_diagnostic_reads_use_actual_owner_and_preserve_unavailable_metrics() {
    let runtime = runtime();
    let mut host = start(&runtime);
    let before = state(&runtime, "diagnostic-before");
    for (method, kind) in [
        ("signal_path.get", "signal_path"),
        ("metrics.get", "metrics"),
        ("events.get", "events"),
        ("diagnostics.preview", "diagnostic_preview"),
    ] {
        let payload = if method == "events.get" {
            json!({"method":method,"cursor":null})
        } else {
            json!({"method":method})
        };
        let command = envelope(method, Value::Null, 0, payload);
        let output = call(&runtime, &command);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["receipt"]["result"]["kind"], kind);
        let body = serde_json::to_vec(&value["receipt"]["result"]["value"]).unwrap();
        if kind == "signal_path" {
            let read = earph_core::diagnostics::SignalPathRead::decode(&body).unwrap();
            assert!(read.report.resolved.is_none() && read.report.observed.is_none());
        } else if kind == "metrics" {
            let read = earph_core::diagnostics::MetricsRead::decode(&body).unwrap();
            assert!(read.report.underruns.value.is_none());
        } else if kind == "events" {
            let read = earph_core::diagnostics::EventsRead::decode(&body).unwrap();
            assert!(read.report.events.is_empty());
            assert!(!read.report.complete_audit_log);
        } else {
            let read = earph_core::diagnostics::PreviewRead::decode(&body).unwrap();
            let bundle =
                earph_core::diagnostics::DiagnosticBundle::decode(read.report.bytes.as_bytes())
                    .unwrap();
            assert_eq!(bundle.running_build, *earph_engine::build_provenance());
            assert!(bundle.running_build.record.is_some());
        }
        assert_eq!(call(&runtime, &command).stdout, output.stdout);
    }
    assert_eq!(state(&runtime, "diagnostic-after"), before);
    let stop = envelope(
        "diagnostic-shutdown",
        before["owner_id"].clone(),
        before["revision"].as_u64().unwrap(),
        json!({"method":"owner.shutdown"}),
    );
    assert!(call(&runtime, &stop).status.success());
    assert!(host.0.wait().unwrap().success());
}
