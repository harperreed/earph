// ABOUTME: Records the actual locked dependency graph and license sources.
// ABOUTME: Detects feature or source changes without claiming future adapters work.
use crate::{read_bounded, sha256};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn license_paths(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths: Vec<_> = fs::read_dir(root)
        .map_err(|_| "cannot read dependency sources")?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    let n = n.to_ascii_uppercase();
                    n.starts_with("LICENSE") || n.starts_with("COPYING") || n.ends_with("-LICENSE")
                })
        })
        .collect();
    paths.sort();
    Ok(paths)
}

fn reviewed_license(
    package: &Value,
    crate_root: &Path,
) -> Result<Option<(Vec<PathBuf>, Value)>, String> {
    let package_directory = format!(
        "{}-{}",
        package["name"].as_str().ok_or("missing dependency name")?,
        package["version"]
            .as_str()
            .ok_or("missing dependency version")?
    );
    if Path::new(&package_directory).components().count() != 1 {
        return Err("reviewed license provider must be a local directory".into());
    }
    let legacy_directory = match package["name"].as_str() {
        Some("objc2-audio-toolbox") if package["version"] == "0.3.2" => Some("objc2-0.3.2"),
        Some("objc2-core-audio" | "objc2-core-audio-types" | "objc2-core-foundation") => {
            Some("objc2-0.3.2")
        }
        Some("windows-link") => Some("windows-link-0.2.1"),
        Some("windows-sys") if package["version"] == "0.61.2" => Some("windows-sys-0.61.2"),
        Some("r-efi") if package["version"] == "6.0.0" => Some("r-efi-6.0.0"),
        Some(
            "uniffi"
            | "uniffi_bindgen"
            | "uniffi_core"
            | "uniffi_internal_macros"
            | "uniffi_macros"
            | "uniffi_meta"
            | "uniffi_pipeline"
            | "uniffi_udl",
        ) => Some("uniffi-0.32.2"),
        _ => None,
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/dependency-licenses")
        .join(legacy_directory.unwrap_or(&package_directory));
    if !root.is_dir() && legacy_directory.is_none() {
        return Ok(None);
    }
    validate_reviewed_license(package, crate_root, &root).map(Some)
}

fn validate_reviewed_license(
    package: &Value,
    crate_root: &Path,
    root: &Path,
) -> Result<(Vec<PathBuf>, Value), String> {
    let provenance: Value =
        serde_json::from_slice(&read_bounded(&root.join("provenance.json"), 64 * 1024)?)
            .map_err(|_| "invalid reviewed license provenance")?;
    if provenance["schema_version"] != 1
        || provenance["version"] != package["version"]
        || provenance["repository"] != package["repository"]
        || (provenance.get("license_expression").is_some()
            && provenance["license_expression"] != package["license"])
        || !provenance["packages"]
            .as_array()
            .is_some_and(|names| names.contains(&package["name"]))
    {
        return Err(format!(
            "dependency needs manual license review: {}",
            package["name"]
        ));
    }
    let vcs: Value = serde_json::from_slice(&read_bounded(
        &crate_root.join(".cargo_vcs_info.json"),
        64 * 1024,
    )?)
    .map_err(|_| "invalid dependency VCS provenance")?;
    if vcs["git"]["sha1"] != provenance["revision"] {
        return Err("reviewed license revision differs from published dependency sources".into());
    }
    let filename = provenance["license_file"]
        .as_str()
        .ok_or("missing reviewed license file")?;
    if Path::new(filename).components().count() != 1
        || !matches!(
            Path::new(filename).components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("reviewed license must be a local filename".into());
    }
    let path = root.join(filename);
    if provenance["license_sha256"] != sha256(&read_bounded(&path, 1024 * 1024)?) {
        return Err("reviewed license content differs from its recorded source".into());
    }
    Ok((vec![path], provenance))
}

pub fn snapshot() -> Result<(Value, String), String> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        // Include the actual binding generator beside the ordinary workspace dependency closure.
        .args([
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--features",
            "earph-ffi/bindgen",
        ])
        .output()
        .map_err(|_| "cannot run Cargo metadata")?;
    if !output.status.success() {
        return Err("locked Cargo metadata failed".into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    let mut packages = Vec::new();
    let mut notices = String::from(
        "Third-party dependency notices\n\nGenerated by earph-harness dependency-lock.\nFirst-party files use MIT; dependency terms remain their own.\n\n",
    );
    for package in metadata["packages"]
        .as_array()
        .ok_or("missing Cargo packages")?
    {
        if package["source"].is_null() {
            continue;
        }
        let root = Path::new(
            package["manifest_path"]
                .as_str()
                .ok_or("missing manifest path")?,
        )
        .parent()
        .ok_or("missing crate root")?;
        let (paths, license_provenance) = match reviewed_license(package, root)? {
            Some((paths, provenance)) => (paths, Some(provenance)),
            None => {
                let paths = license_paths(root)?;
                if paths.is_empty() {
                    return Err(format!(
                        "dependency needs manual license review: {}",
                        package["name"]
                    ));
                }
                (paths, None)
            }
        };
        let mut license_files = Vec::new();
        notices.push_str(&format!(
            "=== {} {} ({}) ===\n",
            package["name"], package["version"], package["license"]
        ));
        for path in paths {
            let bytes = read_bounded(&path, 1024 * 1024)?;
            let name = path
                .file_name()
                .ok_or("missing license filename")?
                .to_string_lossy();
            license_files.push(json!({"name":name,"sha256":sha256(&bytes)}));
            notices.push_str(&format!(
                "--- {name} ---\n{}\n",
                std::str::from_utf8(&bytes).map_err(|_| "non-UTF8 dependency license")?
            ));
        }
        let node = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("missing resolved nodes")?
            .iter()
            .find(|node| node["id"] == package["id"])
            .ok_or("unresolved dependency")?;
        let mut record = json!({
            "name":package["name"], "version":package["version"], "source":package["source"],
            "license_expression":package["license"], "features":node["features"],
            "manifest_sha256":sha256(&read_bounded(&root.join("Cargo.toml"), 1024*1024)?),
            "license_files":license_files,
        });
        if let Some(provenance) = license_provenance {
            record["license_provenance"] = provenance;
        }
        packages.push(record);
    }
    packages.sort_by_key(|p| {
        (
            p["name"].as_str().unwrap_or_default().to_owned(),
            p["version"].as_str().unwrap_or_default().to_owned(),
        )
    });
    let candidates: Vec<_> = [
        "symphonia",
        "rubato",
        "rustfft",
        "camilladsp",
        "sendspin-rs",
        "uniffi",
        "Core Audio",
        "ALSA",
        "Linux GUI",
        "operational store",
    ]
    .into_iter()
    .map(|name| {
        if name == "ALSA" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"alsa-sys 0.6.1"})
        } else if name == "Core Audio" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"objc2-core-audio 0.3.2"})
        } else if name == "symphonia" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"symphonia 0.6.1"})
        } else if name == "rubato" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"rubato 5.1.0; source audit and isolated proof"})
        } else if name == "uniffi" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"uniffi 0.32.2; attach-only client foundation"})
        } else if name == "Linux GUI" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"eframe 0.37.0; attach-only native GUI"})
        } else if name == "operational store" {
            json!({"name":name,"status":"selected","qualification":"partial","revision":"versioned JSON checkpoints v1"})
        } else {
            json!({"name":name,"status":"unselected","qualification":"not_run","revision":null})
        }
    })
    .collect();
    let final_len = notices.trim_end().len();
    notices.truncate(final_len);
    notices.push('\n');
    Ok((
        json!({"schema_version":1,"packages":packages,"candidates":candidates}),
        notices,
    ))
}

pub fn validate(inventory: &Value) -> Result<(), String> {
    let (actual, _) = snapshot()?;
    if *inventory != actual {
        return Err("dependency inventory differs from locked features or license sources".into());
    }
    Ok(())
}

#[cfg(test)]
mod license_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture() -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/license-inventory-fixtures")
            .join(format!(
                "{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn published_lowercase_notices_are_read_without_renaming() {
        let root = fixture();
        for name in [
            "LICENSE-MIT",
            "license-apache-2.0",
            "Copying",
            "foo-license",
        ] {
            fs::write(root.join(name), format!("notice for {name}\n")).unwrap();
        }
        fs::write(root.join("README.md"), "not a license").unwrap();
        fs::create_dir(root.join("LICENSE-directory")).unwrap();
        let paths = license_paths(&root).unwrap();
        let names: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "Copying",
                "LICENSE-MIT",
                "foo-license",
                "license-apache-2.0"
            ]
        );
        for file in paths {
            assert_eq!(
                fs::read_to_string(&file).unwrap(),
                format!(
                    "notice for {}\n",
                    file.file_name().unwrap().to_str().unwrap()
                )
            );
        }
    }

    #[test]
    fn lowercase_files_cannot_bypass_legacy_reviewed_provenance() {
        let root = fixture();
        fs::write(
            root.join("license-mit"),
            "a discoverable crate-local notice\n",
        )
        .unwrap();
        let provider = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/dependency-licenses/windows-link-0.2.1/provenance.json");
        let provenance: Value = serde_json::from_slice(&fs::read(provider).unwrap()).unwrap();
        let package = json!({
            "name": "windows-link", "version": "0.2.1",
            "repository": "https://github.com/microsoft/windows-rs", "license": "MIT OR Apache-2.0"
        });
        fs::write(
            root.join(".cargo_vcs_info.json"),
            serde_json::to_vec(&json!({"git":{"sha1":provenance["revision"]}})).unwrap(),
        )
        .unwrap();
        assert_eq!(license_paths(&root).unwrap().len(), 1);
        let (_, actual) = reviewed_license(&package, &root).unwrap().unwrap();
        assert_eq!(actual, provenance);
        fs::write(
            root.join(".cargo_vcs_info.json"),
            br#"{"git":{"sha1":"tampered-revision"}}"#,
        )
        .unwrap();
        assert!(reviewed_license(&package, &root).is_err());
    }

    #[test]
    fn reviewed_notices_reject_changed_identity_revision_path_and_bytes() {
        let root = fixture();
        let crate_root = root.join("crate");
        fs::create_dir(&crate_root).unwrap();
        let package = json!({
            "name": "example", "version": "1.2.3", "repository": "https://github.com/example/example",
            "license": "MIT"
        });
        let provenance = json!({
            "schema_version": 1, "packages": ["example"], "version": "1.2.3",
            "repository": package["repository"], "revision": "published-revision",
            "license_expression": "MIT", "license_file": "LICENSE-MIT", "license_sha256": sha256(b"exact notice\n")
        });
        fs::write(root.join("LICENSE-MIT"), b"exact notice\n").unwrap();
        fs::write(
            crate_root.join(".cargo_vcs_info.json"),
            br#"{"git":{"sha1":"published-revision"}}"#,
        )
        .unwrap();
        let write_provenance = |value: &Value| {
            fs::write(
                root.join("provenance.json"),
                serde_json::to_vec(value).unwrap(),
            )
            .unwrap();
        };
        write_provenance(&provenance);
        let (files, _) = validate_reviewed_license(&package, &crate_root, &root).unwrap();
        assert_eq!(fs::read(&files[0]).unwrap(), b"exact notice\n");
        for (field, value) in [
            ("schema_version", json!(2)),
            ("packages", json!(["other"])),
            ("version", json!("1.2.4")),
            ("repository", json!("https://github.com/other/repository")),
            ("license_expression", json!("Apache-2.0")),
            ("revision", json!("different-revision")),
            ("license_file", json!("../LICENSE-MIT")),
            ("license_file", json!("/LICENSE-MIT")),
            ("license_sha256", json!("different-hash")),
        ] {
            let mut changed = provenance.clone();
            changed[field] = value;
            write_provenance(&changed);
            assert!(
                validate_reviewed_license(&package, &crate_root, &root).is_err(),
                "accepted changed {field}"
            );
        }
        write_provenance(&provenance);
        fs::write(root.join("LICENSE-MIT"), b"tampered notice\n").unwrap();
        assert!(validate_reviewed_license(&package, &crate_root, &root).is_err());
    }
}
