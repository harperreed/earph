// ABOUTME: Freezes bounded Engine package facts at compilation.
// ABOUTME: Records package inputs without claiming runtime executable or reproduction identity.
#[allow(dead_code)]
#[path = "../../build_support/provenance.rs"]
mod provenance;
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("../..");
    provenance::watch(&root, provenance::ENGINE_INPUTS);
    let compiler = env::var_os("RUSTC").and_then(|rustc| {
        let output = Command::new(rustc)
            .args(["--version", "--verbose"])
            .output()
            .ok()?;
        output.status.success().then_some(())?;
        String::from_utf8(output.stdout).ok()
    });
    let record = provenance::engine_provenance(
        &root,
        compiler.as_deref(),
        env::var("TARGET").ok().as_deref(),
        env::var("PROFILE").ok().as_deref(),
    )
    .expect("read Engine build inputs");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
            .join("build-provenance.json"),
        serde_json::to_vec(&record).expect("serialize Engine build provenance"),
    )
    .expect("write Engine build provenance");
}
