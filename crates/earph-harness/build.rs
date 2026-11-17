// ABOUTME: Binds source revision, lock hash, and source digest into the executable.
// ABOUTME: Refreshes provenance when build inputs or Git references change.
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| {
            String::from_utf8(output.stdout)
                .ok()
                .map(|s| s.trim().to_owned())
        })
        .flatten()
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn files(path: &Path, result: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in fs::read_dir(path).expect("read build input directory") {
            files(&entry.expect("read build input entry").path(), result);
        }
    } else {
        result.push(path.to_owned());
    }
}
fn main() {
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .join("../..");
    let mut inputs = Vec::new();
    for relative in [
        "Cargo.toml",
        "Cargo.lock",
        "crates/earph-core/Cargo.toml",
        "crates/earph-core/src",
        "crates/earph-harness/Cargo.toml",
        "crates/earph-harness/src",
        "crates/earph-harness/build.rs",
        "crates/earph-alsa/Cargo.toml",
        "crates/earph-alsa/src",
        "crates/earph-coreaudio/Cargo.toml",
        "crates/earph-coreaudio/src",
        "docs/requirements.json",
        "docs/dependency-licenses/objc2-0.3.2",
        "fixtures/audio/manifest.json",
    ] {
        let path = root.join(relative);
        println!("cargo::rerun-if-changed={}", path.display());
        files(&path, &mut inputs);
    }
    // HEAD's contents stay constant when a branch advances, so watch its ref too.
    for name in [
        "HEAD".to_owned(),
        "index".to_owned(),
        "packed-refs".to_owned(),
    ]
    .into_iter()
    .chain(git(&root, &["symbolic-ref", "--quiet", "HEAD"]))
    {
        if let Some(path) = git(&root, &["rev-parse", "--git-path", &name]) {
            let path = PathBuf::from(path);
            let absolute = if path.is_absolute() {
                path
            } else {
                root.join(path)
            };
            if absolute.exists() {
                println!("cargo::rerun-if-changed={}", absolute.display());
            }
        }
    }
    inputs.sort();
    let mut source = Sha256::new();
    for path in inputs {
        let name = path
            .strip_prefix(&root)
            .expect("relative build input")
            .to_string_lossy();
        let bytes = fs::read(&path).expect("read build input");
        source.update((name.len() as u64).to_le_bytes());
        source.update(name.as_bytes());
        source.update((bytes.len() as u64).to_le_bytes());
        source.update(&bytes);
    }
    let source_sha256: String = source
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let rustc = Command::new(env::var_os("RUSTC").expect("Cargo compiler"))
        .arg("--version")
        .output()
        .expect("inspect compiler");
    assert!(rustc.status.success(), "compiler version failed");
    let provenance = json!({"schema_version":1,"commit":git(&root,&["rev-parse","HEAD"]),"source_dirty":git(&root,&["status","--porcelain"]).map(|s|!s.is_empty()),"dependency_lock_sha256":digest(&fs::read(root.join("Cargo.lock")).expect("read dependency lock")),"source_inputs_sha256":source_sha256,"rustc":String::from_utf8(rustc.stdout).expect("UTF8 compiler version").trim(),"target":env::var("TARGET").expect("Cargo target"),"profile":env::var("PROFILE").expect("Cargo profile")});
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"))
            .join("build-provenance.json"),
        serde_json::to_vec(&provenance).expect("serialize build provenance"),
    )
    .expect("write build provenance");
}
