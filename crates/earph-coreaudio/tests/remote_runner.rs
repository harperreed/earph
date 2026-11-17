// ABOUTME: Checks the remote runner's input boundary before any connection is attempted.
// ABOUTME: Rejects an SSH option supplied where an authorized host alias is required.
#[test]
fn ssh_options_cannot_be_used_as_host_aliases() {
    let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/check-coreaudio-remote");
    let result = std::process::Command::new("bash")
        .arg(script)
        .args(["-F", "/Users/example/.cache/earph-coreaudio.Test/checkout"])
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn real_ssh_configuration_is_strict_with_and_without_peer_override() {
    let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/check-coreaudio-remote");
    for peer in [None, Some("127.0.0.1")] {
        let mut command = std::process::Command::new("bash");
        command.arg(&script).args([
            "--ssh-config",
            "earph-config-inspection",
            "/Users/example/.cache/earph-coreaudio.Test/checkout",
        ]);
        if let Some(peer) = peer {
            command.arg(peer);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["strict_host_key_checking"], true);
        assert_eq!(result["connection_attempted"], false);
    }
}
