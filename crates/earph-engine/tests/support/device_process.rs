// ABOUTME: Compiles a standalone test helper and preserves every fixture executable.
// ABOUTME: Keeps controlled process behavior outside the production engine and its protocol.
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};
pub fn fixture(mode: &str) -> PathBuf {
    static COMPILED: OnceLock<PathBuf> = OnceLock::new();
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let compiled = COMPILED.get_or_init(|| {
        let directory = std::env::temp_dir().join(format!(
            "earph-discovery-process-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let executable = directory.join("compiled-helper");
        let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024"])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/support/device_helper.rs"))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        fs::write(directory.join("compiler.stdout"), &output.stdout).unwrap();
        fs::write(directory.join("compiler.stderr"), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    });
    let executable = compiled.parent().unwrap().join(format!(
        "fixture-{mode}-{}",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    // Keep writable executable descriptors outside the multithreaded parent's inheritance table.
    let output = Command::new(compiled)
        .arg("--copy-fixture")
        .arg(&executable)
        .output()
        .unwrap();
    fs::write(executable.with_extension("copy.stdout"), &output.stdout).unwrap();
    fs::write(executable.with_extension("copy.stderr"), &output.stderr).unwrap();
    assert!(output.status.success(), "fixture copy failed: {output:?}");
    executable
}
