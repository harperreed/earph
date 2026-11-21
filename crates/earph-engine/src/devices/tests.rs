// ABOUTME: Exercises discovery supervision with actual owned exec processes and bounded pipes.
// ABOUTME: Verifies failure, cancellation, protocol limits, and reaping without native devices.
#![cfg(target_os = "linux")]
use super::*;
#[path = "../../tests/support/device_process.rs"]
mod device_process;

fn query() -> DeviceQuery {
    DeviceQuery {
        backend: OutputBackend::Alsa,
        endpoint: None,
    }
}
fn reaped(executable: &Path) {
    if let Ok(pid) = fs::read_to_string(executable.with_extension("pid")) {
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "helper remains alive"
        );
    }
}
#[test]
fn actual_child_timeout_crash_and_pipe_limits_always_reap() {
    for (mode, expected) in [
        ("wait", ErrorCode::DeviceUnavailable),
        ("crash", ErrorCode::DeviceUnavailable),
        ("oversize", ErrorCode::ResourceBudgetExceeded),
        ("stderr", ErrorCode::ResourceBudgetExceeded),
        ("malformed", ErrorCode::AssetInvalid),
        ("trailing", ErrorCode::AssetInvalid),
    ] {
        let executable = device_process::fixture(mode);
        let mut command = Command::new(&executable);
        command.arg(HELPER_ROLE);
        let error = supervise(
            command,
            &query(),
            &AtomicBool::new(false),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(error.code, expected, "{mode}: {error:?}");
        reaped(&executable);
    }
}
#[test]
fn actual_child_cancellation_preserves_original_command_and_confirms_reaping() {
    let executable = device_process::fixture("wait");
    let command = Command::new(&executable);
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelling = cancel.clone();
    let signal = thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        cancelling.store(true, Ordering::Release);
    });
    let error = supervise(command, &query(), &cancel, Duration::from_secs(10)).unwrap_err();
    signal.join().unwrap();
    assert_eq!(error.code, ErrorCode::OperationCancelled, "{error:?}");
    reaped(&executable);
}
#[test]
fn actual_spawn_failure_reports_its_os_code_without_the_private_path() {
    let executable = device_process::fixture("valid").with_extension("missing");
    let error = supervise(
        Command::new(&executable),
        &query(),
        &AtomicBool::new(false),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    assert!(
        error.message.contains("spawn"),
        "missing failure stage: {error:?}"
    );
    assert!(
        error.message.contains("OS error 2"),
        "missing actual ENOENT: {error:?}"
    );
    assert!(!error.message.contains(executable.to_str().unwrap()));
    assert_eq!(error.native_status, None);
    assert_eq!(error.native_object_id, None);
}
#[test]
fn fixture_publisher_copies_independent_executable_bytes_and_reports_failure() {
    let source = device_process::fixture("valid");
    let original = fs::read(&source).unwrap();
    let destination = source.with_file_name("fixture-valid-clone");
    let copied = Command::new(&source)
        .arg("--copy-fixture")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(copied.status.success(), "{copied:?}");
    assert_eq!(
        fs::read(&destination).expect("publisher did not create its executable"),
        original
    );
    let observation = supervise(
        Command::new(&destination),
        &query(),
        &AtomicBool::new(false),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(matches!(
        observation,
        DeviceObservation::List { complete: true, .. }
    ));
    assert!(destination.with_extension("pid").is_file());
    assert!(destination.with_extension("input").is_file());
    reaped(&destination);
    fs::OpenOptions::new()
        .append(true)
        .open(&destination)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    let changed = fs::read(&destination).unwrap();
    let collision = Command::new(&source)
        .arg("--copy-fixture")
        .arg(&destination)
        .output()
        .unwrap();
    assert!(!collision.status.success());
    assert_eq!(fs::read(&destination).unwrap(), changed);
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        fs::read(device_process::fixture("valid")).unwrap(),
        original
    );
    let invalid = source
        .parent()
        .unwrap()
        .join("absent-directory")
        .join("helper");
    let failed = Command::new(&source)
        .arg("--copy-fixture")
        .arg(&invalid)
        .output()
        .unwrap();
    assert!(!failed.status.success(), "failed copy reported success");
    assert!(!invalid.exists());
}
#[test]
fn explicit_helper_preserves_only_native_environment_and_rejects_changed_bytes() {
    let executable = device_process::fixture("valid");
    let helper = DeviceDiscoveryHelper::from_executable(&executable).unwrap();
    let observation = supervise(
        helper.command().unwrap(),
        &query(),
        &AtomicBool::new(false),
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(matches!(
        observation,
        DeviceObservation::List { complete: true, .. }
    ));
    reaped(&executable);
    let environment = fs::read_to_string(executable.with_extension("environment")).unwrap();
    for key in environment.lines() {
        assert!(
            ["LANG", "LC_ALL", "ALSA_CONFIG_PATH"].contains(&key),
            "unexpected inherited key {key}"
        );
    }
    let input = fs::read(executable.with_extension("input")).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&input[4..]).unwrap();
    assert_eq!(
        body,
        serde_json::json!({"schema_version":1,"query":{"backend":"alsa","endpoint":null}})
    );
    use std::fs::OpenOptions;
    OpenOptions::new()
        .append(true)
        .open(&executable)
        .unwrap()
        .write_all(b"changed")
        .unwrap();
    assert_eq!(helper.command().err().unwrap().code, ErrorCode::AuthDenied);
}
