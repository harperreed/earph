// ABOUTME: Checks shared package hashing using real files and independent byte framing.
// ABOUTME: Verifies unavailable build facts and excludes parent-checkout and private tool text.
#[allow(dead_code)]
#[path = "../../../build_support/provenance.rs"]
mod provenance;
mod support;
use earph_core::provenance::{BuildFieldReason, BuildProducer, RunningBuildProvenance};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn retained_directory() -> std::mem::ManuallyDrop<support::TestDir> {
    // Preserve these real input fixtures instead of running TestDir's cleanup.
    std::mem::ManuallyDrop::new(support::TestDir::new())
}

fn independent_sha256(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    Sha256::digest(bytes)
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

fn fixture(root: &Path, inputs: &[&str]) {
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for input in inputs {
        let path = root.join(input);
        if checkout.join(input).is_dir() {
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("input.rs"), input).unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, input).unwrap();
        }
    }
}

#[test]
fn archive_does_not_inherit_parent_checkout_commit() {
    let temporary = retained_directory();
    assert!(provenance::git(&temporary.0, &["rev-parse", "HEAD"]).is_none());
    // A marker alone must not bless the containing repository either.
    fs::create_dir(temporary.0.join(".git")).unwrap();
    assert!(provenance::git(&temporary.0, &["rev-parse", "HEAD"]).is_none());
}

#[test]
fn component_order_and_u64_little_endian_framing_match_independent_digest() {
    let temporary = retained_directory();
    fs::create_dir(temporary.0.join("output")).unwrap();
    fs::write(temporary.0.join("output.rs"), b"flat").unwrap();
    fs::write(temporary.0.join("output/a.rs"), b"nested").unwrap();
    let mut framed = Vec::new();
    for (path, bytes) in [
        (b"output/a.rs".as_slice(), b"nested".as_slice()),
        (b"output.rs", b"flat"),
    ] {
        framed.extend_from_slice(&(path.len() as u64).to_le_bytes());
        framed.extend_from_slice(path);
        framed.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        framed.extend_from_slice(bytes);
    }
    let expected = independent_sha256(&framed);
    assert_eq!(
        provenance::source_digest(&temporary.0, &["output.rs", "output"]).unwrap(),
        expected
    );
    assert_eq!(
        provenance::source_digest(&temporary.0, &["output", "output.rs"]).unwrap(),
        expected
    );
    assert!(provenance::source_digest(&temporary.0, &["missing"]).is_err());
}

#[test]
fn each_declared_input_including_shared_helper_changes_the_digest() {
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    for inputs in [provenance::ENGINE_INPUTS, provenance::HARNESS_INPUTS] {
        let temporary = retained_directory();
        fixture(&temporary.0, inputs);
        let before = provenance::source_digest(&temporary.0, inputs).unwrap();
        for input in inputs {
            let path = if checkout.join(input).is_dir() {
                temporary.0.join(input).join("input.rs")
            } else {
                temporary.0.join(input)
            };
            let bytes = fs::read(&path).unwrap();
            fs::write(&path, b"changed package input").unwrap();
            assert_ne!(
                before,
                provenance::source_digest(&temporary.0, inputs).unwrap(),
                "unhashed {input}"
            );
            fs::write(path, bytes).unwrap();
        }
        let extension = temporary.0.join("extra.rs");
        fs::write(&extension, b"explicit extension").unwrap();
        assert_eq!(
            before,
            provenance::source_digest(&temporary.0, inputs).unwrap()
        );
        let mut extended = inputs.to_vec();
        extended.push("extra.rs");
        assert_ne!(
            before,
            provenance::source_digest(&temporary.0, &extended).unwrap()
        );
        assert!(inputs.contains(&"build_support/provenance.rs"));
        assert!(inputs.contains(&"crates/earph-engine/build.rs"));
    }
}

#[test]
fn native_mac_controller_source_changes_the_harness_digest() {
    let temporary = retained_directory();
    fixture(&temporary.0, provenance::HARNESS_INPUTS);
    let source = temporary.0.join("macos/Sources/Earph/EarphApp.swift");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(&source, b"import SwiftUI\n").unwrap();
    let before = provenance::source_digest(&temporary.0, provenance::HARNESS_INPUTS).unwrap();
    fs::write(&source, b"import SwiftUI\n// changed native application\n").unwrap();
    assert_ne!(
        before,
        provenance::source_digest(&temporary.0, provenance::HARNESS_INPUTS).unwrap(),
        "The native Mac controller must belong to the packaged source identity"
    );
}

#[test]
fn real_engine_producer_decodes_and_missing_git_stays_unavailable() {
    let temporary = retained_directory();
    fixture(&temporary.0, provenance::ENGINE_INPUTS);
    let generated = provenance::engine_provenance(&temporary.0,
        Some("rustc private compiler text\nrelease: 1.98.1\ncommit-hash: 0123456789abcdef0123456789abcdef01234567\nhost: /private/host"),
        Some("x86_64-unknown-linux-gnu"), Some("debug")).unwrap();
    let bytes = serde_json::to_vec(&generated).unwrap();
    let build = RunningBuildProvenance::decode(&bytes).unwrap();
    build.validate_for_engine().unwrap();
    let record = build.record.unwrap();
    assert_eq!(record.producer, BuildProducer::EnginePackage);
    assert_eq!(
        record.commit.unavailable_reason,
        Some(BuildFieldReason::NotProvided)
    );
    assert_eq!(
        record.source_dirty.unavailable_reason,
        Some(BuildFieldReason::NotProvided)
    );
    assert_eq!(record.compiler.value.unwrap().minor, 98);
    assert!(!String::from_utf8(bytes).unwrap().contains("private"));
}

#[test]
fn unavailable_and_unsupported_fields_are_explicit_and_bounded() {
    let temporary = retained_directory();
    fixture(&temporary.0, provenance::ENGINE_INPUTS);
    for (compiler, target, profile, reason) in [
        (None, None, None, BuildFieldReason::NotProvided),
        (
            Some("rustc /home/private compiler"),
            Some("/home/private/target.json"),
            Some("private-profile"),
            BuildFieldReason::UnsupportedValue,
        ),
    ] {
        let generated =
            provenance::engine_provenance(&temporary.0, compiler, target, profile).unwrap();
        let bytes = serde_json::to_vec(&generated).unwrap();
        let build = RunningBuildProvenance::decode(&bytes).unwrap();
        let record = build.record.unwrap();
        assert_eq!(record.compiler.unavailable_reason, Some(reason));
        assert_eq!(record.target.unavailable_reason, Some(reason));
        assert_eq!(record.cargo_profile_class.unavailable_reason, Some(reason));
        assert!(!String::from_utf8(bytes).unwrap().contains("private"));
    }
    for release in ["1.99.0-beta.2", "1.100.0-nightly"] {
        let verbose = format!("release: {release}\ncommit-hash: unknown");
        let generated =
            provenance::engine_provenance(&temporary.0, Some(&verbose), None, None).unwrap();
        let record = RunningBuildProvenance::decode(&serde_json::to_vec(&generated).unwrap())
            .unwrap()
            .record
            .unwrap();
        assert_eq!(
            record.compiler.value.unwrap().commit.unavailable_reason,
            Some(BuildFieldReason::UnsupportedValue)
        );
    }
}

#[test]
fn harness_keeps_its_legacy_json_contract_and_uses_shared_inputs() {
    let generated = earph_harness::build_provenance();
    let mut keys: Vec<_> = generated
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "commit",
            "dependency_lock_sha256",
            "profile",
            "rustc",
            "schema_version",
            "source_dirty",
            "source_inputs_sha256",
            "target"
        ]
    );
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert_eq!(
        generated["source_inputs_sha256"],
        provenance::source_digest(&checkout, provenance::HARNESS_INPUTS).unwrap()
    );
    assert_eq!(
        generated["dependency_lock_sha256"],
        independent_sha256(&fs::read(checkout.join("Cargo.lock")).unwrap())
    );
}
