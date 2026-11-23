// ABOUTME: Checks the shared deployment target consumed by the native Mac build.
// ABOUTME: Exercises real Bash exports and rejects invalid compiler targets.
use std::process::{Command, Output};

fn configure(arch: &str, version: Option<&str>) -> Output {
    let mut command = Command::new("bash");
    command
        .args(["-euc", "source \"$1\"; configure_macos_target \"$2\"; bash -c 'printf \"%s\\n%s\\n\" \"$MACOSX_DEPLOYMENT_TARGET\" \"$swift_target\"'", "macos-target"])
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/macos-deployment-target.sh"))
        .arg(arch)
        .env_remove("MACOSX_DEPLOYMENT_TARGET")
        .env_remove("swift_target");
    if let Some(version) = version {
        command.env("MACOSX_DEPLOYMENT_TARGET", version);
    }
    command.output().unwrap()
}

#[test]
fn rust_and_swift_children_share_the_default_target() {
    let output = configure("arm64", None);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"26.0\narm64-apple-macosx26.0\n");
}

#[test]
fn explicit_target_reaches_both_compilers() {
    let output = configure("x86_64", Some("26.5"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"26.5\nx86_64-apple-macosx26.5\n");
}

#[test]
fn invalid_targets_fail_before_building() {
    for (arch, version) in [
        ("aarch64", "26.0"),
        ("arm64", "26;false"),
        ("arm64", "26.0.1.2"),
    ] {
        let output = configure(arch, Some(version));
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("error: invalid macOS"));
    }
}
