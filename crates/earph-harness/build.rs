// ABOUTME: Binds source revision, lock hash, and source digest into the executable.
// ABOUTME: Uses the shared package-input producer while retaining the harness JSON contract.
#[allow(dead_code)]
#[path = "../../build_support/provenance.rs"]
mod provenance;
use serde_json::json;
use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("../..");
    provenance::watch(&root, provenance::HARNESS_INPUTS);
    let rustc = Command::new(env::var_os("RUSTC").expect("Cargo compiler"))
        .arg("--version")
        .output()
        .expect("inspect compiler");
    assert!(rustc.status.success(), "compiler version failed");
    let provenance = json!({"schema_version":1,
        "commit":provenance::git(&root,&["rev-parse","HEAD"]),
        "source_dirty":provenance::git(&root,&["status","--porcelain"]).map(|s|!s.is_empty()),
        "dependency_lock_sha256":provenance::digest(&fs::read(root.join("Cargo.lock")).expect("read dependency lock")),
        "source_inputs_sha256":provenance::source_digest(&root, provenance::HARNESS_INPUTS).expect("read build inputs"),
        "rustc":String::from_utf8(rustc.stdout).expect("UTF8 compiler version").trim(),
        "target":env::var("TARGET").expect("Cargo target"),
        "profile":env::var("PROFILE").expect("Cargo profile")});
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
            .join("build-provenance.json"),
        serde_json::to_vec(&provenance).expect("serialize build provenance"),
    )
    .expect("write build provenance");
}
