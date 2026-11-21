// ABOUTME: Verifies that actual Sendspin and credential inputs affect both build digests.
// ABOUTME: Retains isolated source copies and never edits the canonical checkout.
#[allow(dead_code)]
#[path = "../../../build_support/provenance.rs"]
mod provenance;
use std::{
    fs,
    path::{Path, PathBuf},
};
fn copy(source: &Path, destination: &Path) {
    if source.is_dir() {
        fs::create_dir_all(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            copy(&entry.path(), &destination.join(entry.file_name()));
        }
    } else {
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
}
#[test]
fn native_and_sendspin_runtime_sources_change_both_actual_digests() {
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let packet = std::env::temp_dir().join(format!(
        "earph-services-provenance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for (name, inputs) in [
        ("engine", provenance::ENGINE_INPUTS),
        ("harness", provenance::HARNESS_INPUTS),
    ] {
        let root = packet.join(name);
        for input in inputs {
            copy(&checkout.join(input), &root.join(input));
        }
        for package in ["earph-sendspin", "earph-credentials"] {
            copy(
                &checkout.join("crates").join(package),
                &root.join("crates").join(package),
            );
        }
        let original = provenance::source_digest(&root, inputs).unwrap();
        for input in [
            "crates/earph-sendspin/Cargo.toml",
            "crates/earph-sendspin/src/session.rs",
            "crates/earph-credentials/Cargo.toml",
            "crates/earph-credentials/src/lib.rs",
        ] {
            let path = root.join(input);
            let before = fs::read(&path).unwrap();
            let mut changed = before.clone();
            changed.extend_from_slice(b"\n// provenance test change\n");
            fs::write(&path, changed).unwrap();
            assert_ne!(
                provenance::source_digest(&root, inputs).unwrap(),
                original,
                "{name} omits {input}"
            );
            fs::write(&path, before).unwrap();
        }
    }
}
