// ABOUTME: Shares package-input hashing and build-time fact collection between build scripts.
// ABOUTME: Preserves path-component ordering and frames every path and payload with u64 lengths.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

pub const HARNESS_INPUTS: &[&str] = &[
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
    "crates/earph-media/Cargo.toml",
    "crates/earph-media/src",
    "crates/earph-network/Cargo.toml",
    "crates/earph-network/src",
    "crates/earph-sendspin/Cargo.toml",
    "crates/earph-sendspin/src",
    "crates/earph-credentials/Cargo.toml",
    "crates/earph-credentials/src",
    "crates/earph-engine/Cargo.toml",
    "crates/earph-engine/src",
    "crates/earph-dsp/Cargo.toml",
    "crates/earph-dsp/src",
    "crates/earph-ffi/Cargo.toml",
    "crates/earph-ffi/src",
    "crates/earph-desktop/Cargo.toml",
    "crates/earph-desktop/src",
    "docs/requirements.json",
    "docs/dependency-licenses",
    "fixtures/audio/manifest.json",
    "build_support/provenance.rs",
    "crates/earph-engine/build.rs",
];

// Version one covers the Engine package and its workspace runtime dependencies on every supported host.
pub const ENGINE_INPUTS: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "build_support/provenance.rs",
    "crates/earph-engine/build.rs",
    "crates/earph-engine/Cargo.toml",
    "crates/earph-engine/src",
    "crates/earph-core/Cargo.toml",
    "crates/earph-core/src",
    "crates/earph-alsa/Cargo.toml",
    "crates/earph-alsa/src",
    "crates/earph-coreaudio/Cargo.toml",
    "crates/earph-coreaudio/src",
    "crates/earph-media/Cargo.toml",
    "crates/earph-media/src",
    "crates/earph-network/Cargo.toml",
    "crates/earph-network/src",
    "crates/earph-sendspin/Cargo.toml",
    "crates/earph-sendspin/src",
    "crates/earph-credentials/Cargo.toml",
    "crates/earph-credentials/src",
    "crates/earph-dsp/Cargo.toml",
    "crates/earph-dsp/src",
    "docs/dependency-licenses",
];

fn git_output(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    output.status.success().then_some(())?;
    Some(String::from_utf8(output.stdout).ok()?.trim().to_owned())
}

pub fn git(root: &Path, args: &[&str]) -> Option<String> {
    // Git searches parents; archives nested in another checkout must not inherit that identity.
    if !root.join(".git").exists() {
        return None;
    }
    let top = git_output(root, &["rev-parse", "--show-toplevel"])?;
    if fs::canonicalize(top).ok()? != fs::canonicalize(root).ok()? {
        return None;
    }
    git_output(root, args)
}

pub fn digest(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn files(path: &Path, result: &mut Vec<PathBuf>) -> io::Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            files(&entry?.path(), result)?;
        }
    } else {
        result.push(path.to_owned());
    }
    Ok(())
}

pub fn source_digest(root: &Path, inputs: &[&str]) -> io::Result<String> {
    let mut paths = Vec::new();
    for relative in inputs {
        files(&root.join(relative), &mut paths)?;
    }
    paths.sort();
    let mut source = Sha256::new();
    for path in paths {
        let name = path
            .strip_prefix(root)
            .expect("relative build input")
            .to_string_lossy();
        let bytes = fs::read(&path)?;
        source.update((name.len() as u64).to_le_bytes());
        source.update(name.as_bytes());
        source.update((bytes.len() as u64).to_le_bytes());
        source.update(&bytes);
    }
    Ok(hex(source.finalize().as_slice()))
}

pub fn watch(root: &Path, inputs: &[&str]) {
    for relative in inputs {
        println!("cargo::rerun-if-changed={}", root.join(relative).display());
    }
    println!("cargo::rerun-if-changed={}", root.join(".git").display());
    // HEAD's contents stay constant when a branch advances, so watch its ref too.
    for name in [
        "HEAD".to_owned(),
        "index".to_owned(),
        "packed-refs".to_owned(),
    ]
    .into_iter()
    .chain(git(root, &["symbolic-ref", "--quiet", "HEAD"]))
    {
        if let Some(path) = git(root, &["rev-parse", "--git-path", &name]) {
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
}

fn available(value: Value) -> Value {
    json!({"value": value, "unavailable_reason": null})
}

fn unavailable(reason: &str) -> Value {
    json!({"value": null, "unavailable_reason": reason})
}

fn normalized(value: Option<&str>, accept: impl FnOnce(&str) -> Option<Value>) -> Value {
    match value {
        None => unavailable("not_provided"),
        Some(value) => accept(value)
            .map(available)
            .unwrap_or_else(|| unavailable("unsupported_value")),
    }
}

fn commit(value: &str) -> Option<Value> {
    (value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| json!(value))
}

fn compiler(verbose: &str) -> Option<Value> {
    let release = verbose
        .lines()
        .find_map(|line| line.strip_prefix("release: "))?;
    let (version, channel) = if let Some(version) = release.strip_suffix("-nightly") {
        (version, "nightly")
    } else if let Some((version, beta)) = release.split_once("-beta.") {
        beta.parse::<u16>().ok()?;
        (version, "beta")
    } else {
        (release, "stable")
    };
    let mut parts = version.split('.');
    let major = parts.next()?.parse::<u16>().ok()?;
    let minor = parts.next()?.parse::<u16>().ok()?;
    let patch = parts.next()?.parse::<u16>().ok()?;
    if major == 0 || parts.next().is_some() {
        return None;
    }
    let revision = verbose
        .lines()
        .find_map(|line| line.strip_prefix("commit-hash: "));
    Some(
        json!({"major":major,"minor":minor,"patch":patch,"channel":channel,
        "commit":normalized(revision, commit)}),
    )
}

pub fn engine_provenance(
    root: &Path,
    rustc: Option<&str>,
    target: Option<&str>,
    profile: Option<&str>,
) -> io::Result<Value> {
    let revision = git(root, &["rev-parse", "HEAD"]);
    let dirty = git(root, &["status", "--porcelain"]);
    Ok(
        json!({"schema_version":1,"state":"available","unavailable_reason":null,"record":{
            "producer":"engine_package","input_set_version":1,
            "commit":normalized(revision.as_deref(), commit),
            "source_dirty":dirty.map(|text| available(json!(!text.is_empty()))).unwrap_or_else(|| unavailable("not_provided")),
            "dependency_lock_sha256":digest(&fs::read(root.join("Cargo.lock"))?),
            "source_inputs_sha256":source_digest(root, ENGINE_INPUTS)?,
            "compiler":normalized(rustc, compiler),
            "target":normalized(target, |value| match value {
                "x86_64-unknown-linux-gnu" | "aarch64-unknown-linux-gnu" | "aarch64-apple-darwin" | "x86_64-apple-darwin" => Some(json!(value)),
                _ => None,
            }),
            "cargo_profile_class":normalized(profile, |value| match value {
                "debug" | "release" => Some(json!(value)), _ => None,
            }),
        }}),
    )
}
