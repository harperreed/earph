// ABOUTME: Exercises optional numeric receipts through their real test producers.
// ABOUTME: Preserves private artifacts and proves that repeated exports cannot replace evidence.
pub fn directory(component: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let root = std::path::PathBuf::from(std::env::var_os("EARPH_DSP_EVIDENCE_DIR")?);
    assert!(
        root.is_absolute(),
        "EARPH_DSP_EVIDENCE_DIR must name an absolute writable parent"
    );
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .unwrap();
    let directory = root.join(component);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .expect("numeric evidence exists or is unwritable; select a fresh EARPH_DSP_EVIDENCE_DIR");
    Some(directory)
}

pub fn write_file(directory: &std::path::Path, name: &str, bytes: &[u8]) {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(directory.join(name))
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

pub fn check_export(
    test_name: &str,
    component: &str,
    receipt: &str,
) -> (serde_json::Value, std::path::PathBuf) {
    use std::os::unix::fs::DirBuilderExt;
    let root = std::env::temp_dir().join(format!(
        "earph-dsp-evidence-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&root)
        .unwrap();
    let run = || {
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env("EARPH_DSP_EVIDENCE_DIR", &root)
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let file = root.join(component).join(receipt);
    let original = std::fs::read(&file).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert!(parsed.as_object().is_some_and(|v| !v.is_empty()));
    assert!(
        !run().status.success(),
        "reusing an export directory must fail"
    );
    assert_eq!(std::fs::read(&file).unwrap(), original);
    (parsed, root.join(component))
}
