// ABOUTME: Exercises package copying and verification through real files.
// ABOUTME: Retains isolated fixtures and scopes generated headers to structural tests.
use earph_harness::{
    package::{self, PackageRequest, Product},
    sha256,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "earph-package-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}
fn product(root: &Path, name: &str) -> Product {
    // This is only a minimal ELF parser fixture, never app execution evidence.
    let mut bytes = vec![0u8; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
    bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes.extend_from_slice(name.as_bytes());
    let path = root.join(name);
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    Product {
        path,
        byte_length: bytes.len() as u64,
        sha256: sha256(&bytes),
    }
}
fn request(root: &Path) -> PackageRequest {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sources = root.join("sources");
    fs::create_dir(&sources).unwrap();
    let inventory: serde_json::Value =
        serde_json::from_slice(&fs::read(workspace.join("docs/dependency-lock.json")).unwrap())
            .unwrap();
    let lock = fs::read(workspace.join("Cargo.lock")).unwrap();
    let packages: Vec<_> = inventory["packages"].as_array().unwrap().iter().filter(|p| p["license_expression"] == "MPL-2.0").map(|p| {
        let name=p["name"].as_str().unwrap(); let version=p["version"].as_str().unwrap();
        let archive=format!("{name}-{version}.crate");
        let cache=std::env::var_os("CARGO_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cargo")).join("registry/cache");
        let source=fs::read_dir(cache).unwrap().map(|e|e.unwrap().path().join(&archive)).find(|p|p.is_file()).expect("locked source archive must be cached; run cargo fetch --locked first");
        let bytes=fs::read(source).unwrap(); fs::write(sources.join(&archive),&bytes).unwrap();
        serde_json::json!({"name":name,"version":version,"archive":archive,"byte_length":bytes.len(),"sha256":sha256(&bytes),"public_archive_url":format!("https://static.crates.io/crates/{name}/{name}-{version}.crate")})
    }).collect();
    fs::write(sources.join("index.json"),serde_json::to_vec(&serde_json::json!({"schema_version":1,"scope":"All exact MPL-2.0 packages in the audited workspace feature union. This does not assert that every archive is linked into every product.","license":"MPL-2.0","source_form":"Unmodified crates.io source archives","cargo_lock_sha256":sha256(&lock),"dependency_inventory_sha256":sha256(&fs::read(workspace.join("docs/dependency-lock.json")).unwrap()),"packages":packages})).unwrap()).unwrap();
    let build_record = root.join("build.json");
    fs::write(&build_record, br#"{"workspace_version":"0.1.0","target":"x86_64-unknown-linux-gnu","source_commit":null,"source_archive_sha256":null,"profile":"debug","rustc_version":null}"#).unwrap();
    PackageRequest {
        target: "x86_64-unknown-linux-gnu".into(),
        desktop: product(root, "desktop"),
        engine: product(root, "engine"),
        player: product(root, "player"),
        workspace_manifest: workspace.join("Cargo.toml"),
        cargo_lock: workspace.join("Cargo.lock"),
        dependency_inventory: workspace.join("docs/dependency-lock.json"),
        license: workspace.join("LICENSE"),
        specification_license: workspace.join("SENDSPIN-SPEC-LICENSE.md"),
        notices: workspace.join("THIRD_PARTY_NOTICES.txt"),
        build_record,
        mpl_sources: sources,
        linux_final_prefix: Some("/opt/earph".into()),
    }
}
#[test]
fn package_preserves_every_explicit_executable_bytes() {
    let root = directory();
    let request = request(&root);
    let output = root.join("candidate");
    fs::write(
        request.mpl_sources.join("private-receipt.json"),
        b"private cache paths must stay here",
    )
    .unwrap();
    package::build(&request, &output).unwrap();
    assert_eq!(
        fs::read(output.join("bin/earph-desktop")).unwrap(),
        fs::read(&request.desktop.path).unwrap()
    );
    assert_eq!(
        fs::read(output.join("bin/earph-engine")).unwrap(),
        fs::read(&request.engine.path).unwrap()
    );
    assert_eq!(
        fs::read(output.join("bin/earph")).unwrap(),
        fs::read(&request.player.path).unwrap()
    );
    assert!(
        !output
            .join("share/earph/mpl-sources/private-receipt.json")
            .exists()
    );
    assert_eq!(
        fs::read(output.join("share/earph/declared-build.json")).unwrap(),
        fs::read(&request.build_record).unwrap()
    );
    assert_eq!(
        fs::read(output.join("share/earph/mpl-sources/index.json")).unwrap(),
        fs::read(request.mpl_sources.join("index.json")).unwrap()
    );
    assert_eq!(
        fs::metadata(output.join("share/earph/LICENSE"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        fs::metadata(&request.license).unwrap().permissions().mode() & 0o7777
    );
}
#[test]
fn candidate_round_trip_rejects_corruption_extras_and_changed_modes() {
    let root = directory();
    let request = request(&root);
    let output = root.join("candidate");
    package::build(&request, &output).unwrap();
    package::verify(&output).unwrap();
    let engine = output.join("bin/earph-engine");
    let bytes = fs::read(&engine).unwrap();
    fs::write(&engine, b"changed").unwrap();
    assert!(package::verify(&output).is_err());
    fs::write(&engine, bytes).unwrap();
    package::verify(&output).unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(package::verify(&output).is_err());
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();
    package::verify(&output).unwrap();
    fs::write(output.join("extra"), b"extra").unwrap();
    assert!(package::verify(&output).is_err());
}
#[test]
fn occupied_destinations_and_symlink_inputs_preserve_originals() {
    let root = directory();
    let mut request = request(&root);
    let output = root.join("occupied");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("marker"), b"owned").unwrap();
    assert!(package::build(&request, &output).is_err());
    assert_eq!(fs::read(output.join("marker")).unwrap(), b"owned");
    let link = root.join("symlink");
    std::os::unix::fs::symlink(&request.engine.path, &link).unwrap();
    request.engine.path = link;
    let fresh = root.join("candidate");
    let error = package::build(&request, &fresh).unwrap_err();
    assert!(error.contains("symlink"), "{error}");
}
#[test]
fn hashes_metadata_limits_version_and_source_binding_are_enforced() {
    let root = directory();
    let mut request = request(&root);
    request.engine.sha256 = "0".repeat(64);
    assert!(package::build(&request, &root.join("bad-hash")).is_err());
    request.engine.sha256 = sha256(&fs::read(&request.engine.path).unwrap());
    let record = fs::read(&request.build_record).unwrap();
    fs::write(
        &request.build_record,
        String::from_utf8(record.clone())
            .unwrap()
            .replace("0.1.0", "99.0.0"),
    )
    .unwrap();
    assert!(package::build(&request, &root.join("version-mismatch")).is_err());
    fs::write(&request.build_record, vec![b' '; 128 * 1024 + 1]).unwrap();
    assert!(package::build(&request, &root.join("oversized")).is_err());
    fs::write(&request.build_record, record).unwrap();
    fs::write(
        request.mpl_sources.join("symphonia-0.6.1.crate"),
        b"wrong source",
    )
    .unwrap();
    assert!(package::build(&request, &root.join("changed-source")).is_err());
}
#[test]
fn verifier_rejects_traversal_duplicate_missing_entries_and_symlinks() {
    for mutation in [
        "traversal",
        "absolute",
        "duplicate",
        "missing",
        "symlink",
        "layout",
        "empty-directory",
        "oversized",
    ] {
        let root = directory();
        let request = request(&root);
        let output = root.join("candidate");
        package::build(&request, &output).unwrap();
        let path = output.join("package.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        match mutation {
            "traversal" => manifest["files"][0]["path"] = "../escape".into(),
            "absolute" => manifest["files"][0]["path"] = "/escape".into(),
            "oversized" => manifest["extra"] = " ".repeat(128 * 1024).into(),
            "duplicate" => {
                let entry = manifest["files"][0].clone();
                manifest["files"].as_array_mut().unwrap().push(entry);
            }
            "missing" => {
                manifest["files"].as_array_mut().unwrap().pop();
            }
            "layout" => manifest["target"] = "aarch64-apple-darwin".into(),
            "empty-directory" => fs::create_dir(output.join("unexpected")).unwrap(),
            "symlink" => {
                std::os::unix::fs::symlink(root.join("missing"), output.join("symlink")).unwrap()
            }
            _ => unreachable!(),
        }
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(package::verify(&output).is_err(), "{mutation}");
    }
}
#[test]
fn macos_layout_and_linux_final_prefix_are_explicit() {
    let root = directory();
    let mut request = request(&root);
    request.linux_final_prefix = Some("/opt/Doctor Biz/$dollar`tick`\\slash\"quote%".into());
    let output = root.join("linux");
    package::build(&request, &output).unwrap();
    package::verify(&output).unwrap();
    let entry = fs::read_to_string(output.join("share/applications/earph.desktop")).unwrap();
    assert!(entry.contains("%%"));
    assert!(!entry.contains("Exec=bin/"));
    request.linux_final_prefix = None;
    let no_entry = root.join("no-entry");
    package::build(&request, &no_entry).unwrap();
    package::verify(&no_entry).unwrap();
    assert!(!no_entry.join("share/applications").exists());
    request.target = "aarch64-apple-darwin".into();
    for product in [
        &mut request.engine,
        &mut request.desktop,
        &mut request.player,
    ] {
        let mut bytes = vec![0u8; 32];
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        bytes[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        fs::write(&product.path, &bytes).unwrap();
        product.byte_length = 32;
        product.sha256 = sha256(&bytes);
    }
    let record = fs::read_to_string(&request.build_record).unwrap();
    fs::write(
        &request.build_record,
        record.replace("x86_64-unknown-linux-gnu", "aarch64-apple-darwin"),
    )
    .unwrap();
    let output = root.join("macos");
    package::build(&request, &output).unwrap();
    package::verify(&output).unwrap();
    assert!(
        output
            .join("Earph.app/Contents/MacOS/earph-engine")
            .is_file()
    );
    assert!(output.join("Earph.app/Contents/MacOS/earph").is_file());
    let plist = fs::read_to_string(output.join("Earph.app/Contents/Info.plist")).unwrap();
    assert!(plist.contains("<string>0.1.0</string>"));
    assert!(plist.contains("com.harperreed.earph"));
    assert!(!plist.contains("LSMinimumSystemVersion"));
}
#[test]
fn package_cli_builds_and_verifies_an_explicit_request() {
    let root = directory();
    let request = request(&root);
    let request_file = root.join("request.json");
    fs::write(&request_file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = root.join("cli-candidate");
    let build = std::process::Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .args(["package-build", "--request"])
        .arg(&request_file)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stdout)
    );
    let verify = std::process::Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .arg("package-verify")
        .arg("--package")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        verify.status.success(),
        "{}",
        String::from_utf8_lossy(&verify.stdout)
    );
}
#[test]
fn incorrect_copy_retains_partial_output_and_names_its_path() {
    let root = directory();
    let mut request = request(&root);
    request.engine.sha256 = "f".repeat(64);
    let output = root.join("partial");
    let error = package::build(&request, &output).unwrap_err();
    assert!(error.contains(&output.display().to_string()), "{error}");
    assert_eq!(
        fs::read(output.join("bin/earph-desktop")).unwrap(),
        fs::read(&request.desktop.path).unwrap()
    );
    assert!(output.join("bin/earph-engine").is_file());
    assert!(!output.join("package.json").exists());
}
#[test]
fn wrong_architecture_truncated_headers_and_nonexecutable_inputs_are_rejected() {
    for mutation in [
        "architecture",
        "truncated",
        "nonexecutable",
        "unsupported-target",
    ] {
        let root = directory();
        let mut request = request(&root);
        match mutation {
            "architecture" => {
                let mut bytes = fs::read(&request.engine.path).unwrap();
                bytes[18..20].copy_from_slice(&183u16.to_le_bytes());
                fs::write(&request.engine.path, &bytes).unwrap();
                request.engine.sha256 = sha256(&bytes);
            }
            "truncated" => {
                fs::write(&request.engine.path, b"\x7fELF").unwrap();
                request.engine.byte_length = 4;
                request.engine.sha256 = sha256(b"\x7fELF");
            }
            "nonexecutable" => {
                fs::set_permissions(&request.engine.path, fs::Permissions::from_mode(0o644))
                    .unwrap()
            }
            "unsupported-target" => request.target = "mips-unknown-linux-gnu".into(),
            _ => unreachable!(),
        }
        let error = package::build(&request, &root.join("candidate")).unwrap_err();
        assert!(error.contains("partial package retained"), "{error}");
    }
}
#[test]
fn shared_objects_with_no_entry_point_are_not_application_products() {
    let root = directory();
    let mut request = request(&root);
    let mut bytes = fs::read(&request.engine.path).unwrap();
    bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
    bytes[24..32].copy_from_slice(&0u64.to_le_bytes());
    fs::write(&request.engine.path, &bytes).unwrap();
    request.engine.sha256 = sha256(&bytes);
    let output = root.join("shared-object");
    assert!(package::build(&request, &output).is_err());
}
#[cfg(target_os = "linux")]
#[test]
fn failed_writes_report_and_retain_partial_files() {
    let root = directory();
    let request = request(&root);
    let request_file = root.join("write-failure-request.json");
    fs::write(&request_file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = root.join("write-failure");
    let result = std::process::Command::new("/bin/bash")
        .arg("-c")
        .arg("ulimit -f 1\ntrap '' XFSZ\nexec \"$@\"")
        .arg("package-write-limit")
        .arg(env!("CARGO_BIN_EXE_earph-harness"))
        .arg("package-build")
        .arg("--request")
        .arg(request_file)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("partial package retained"), "{text}");
    assert!(output.join("bin/earph-desktop").is_file());
    assert!(output.join("share/earph/LICENSE").is_file());
    assert!(!output.join("package.json").exists());
}
#[cfg(target_os = "linux")]
#[test]
fn nonregular_inputs_are_rejected_without_opening_a_fifo() {
    let root = directory();
    let mut request = request(&root);
    let fifo = root.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    request.engine.path = fifo;
    let request_file = root.join("fifo-request.json");
    fs::write(&request_file, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = root.join("fifo-candidate");
    let result = std::process::Command::new("timeout")
        .arg("2s")
        .arg(env!("CARGO_BIN_EXE_earph-harness"))
        .arg("package-build")
        .arg("--request")
        .arg(request_file)
        .arg("--output")
        .arg(output)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "nonregular input must fail without blocking"
    );
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("regular file")
    );
}
#[test]
fn invalid_linux_final_paths_are_not_advertised_in_menu_entries() {
    for prefix in [
        "relative/earph",
        "/opt/earph=other",
        "/opt/../earph",
        "/opt/line\nbreak",
    ] {
        let root = directory();
        let mut request = request(&root);
        request.linux_final_prefix = Some(prefix.into());
        assert!(
            package::build(&request, &root.join("candidate")).is_err(),
            "{prefix}"
        );
    }
}
#[test]
fn copied_notices_must_be_canonical_and_missing_files_fail_verification() {
    let root = directory();
    let mut request = request(&root);
    let original = fs::read(&request.license).unwrap();
    let changed = root.join("changed-license");
    fs::write(&changed, b"changed licence").unwrap();
    request.license = changed;
    let error = package::build(&request, &root.join("bad-notice")).unwrap_err();
    assert!(
        error.contains("length") || error.contains("SHA256"),
        "{error}"
    );
    request.license = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../LICENSE");
    assert_eq!(fs::read(&request.license).unwrap(), original);
    let output = root.join("candidate");
    package::build(&request, &output).unwrap();
    fs::rename(
        output.join("bin/earph-engine"),
        root.join("retained-engine"),
    )
    .unwrap();
    assert!(package::verify(&output).is_err());
}
