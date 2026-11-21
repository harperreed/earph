// ABOUTME: Builds exclusive native application candidates from explicit products.
// ABOUTME: Verifies bounded package inventories without installing or running them.
use crate::sha256;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Product {
    pub path: PathBuf,
    pub byte_length: u64,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageRequest {
    pub target: String,
    pub desktop: Product,
    pub engine: Product,
    pub workspace_manifest: PathBuf,
    pub cargo_lock: PathBuf,
    pub dependency_inventory: PathBuf,
    pub license: PathBuf,
    pub specification_license: PathBuf,
    pub notices: PathBuf,
    pub build_record: PathBuf,
    pub mpl_sources: PathBuf,
    pub linux_final_prefix: Option<String>,
}

const MAX_PRODUCT: u64 = 512 * 1024 * 1024;
const MAX_METADATA: u64 = 8 * 1024 * 1024;
const MAX_MANIFEST: u64 = 128 * 1024;
const MANIFEST: &str = "package.json";
const SOURCE_SCOPE: &str = "All exact MPL-2.0 packages in the audited workspace feature union. This does not assert that every archive is linked into every product.";
const INSTRUCTIONS: &str = "MPL-2.0 source delivery\n\nThe adjacent mpl-sources directory contains original, unmodified crates.io source archives and index.json. The index names each archive, version, checksum, and upstream URL. These cover the audited workspace feature union, not a per-product linkage assertion.\n\nRecipients may obtain the covered source directly from those included archives. The archives preserve their original licence and copyright notices. MPL-2.0 governs covered source; the application's MIT licence does not replace dependency terms. See https://www.mozilla.org/en-US/MPL/2.0/ and the MPL licence in THIRD_PARTY_NOTICES.txt. Packaging and verification do not extract these archives.\n\nThis unsigned development candidate is unreleased. Checksums do not authenticate distribution. Installation, signing, macOS launch, native audio, SDK distribution, and rollback require separate qualification. Linux runtime dependencies are not bundled or fully enumerated.\n";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct DeclaredBuild {
    workspace_version: String,
    target: String,
    source_commit: Option<String>,
    source_archive_sha256: Option<String>,
    profile: String,
    rustc_version: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: String,
    byte_length: u64,
    sha256: String,
    mode: u32,
    kind: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    target: String,
    workspace_version: String,
    cargo_lock_sha256: String,
    product_provenance_state: String,
    declared_build: DeclaredBuild,
    package_tool: serde_json::Value,
    signing: String,
    product_signatures: String,
    release: String,
    native_validation: String,
    linux_final_prefix: Option<String>,
    files: Vec<Entry>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Sources {
    schema_version: u32,
    scope: String,
    license: String,
    source_form: String,
    cargo_lock_sha256: String,
    dependency_inventory_sha256: String,
    packages: Vec<Source>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Source {
    name: String,
    version: String,
    archive: String,
    sha256: String,
    public_archive_url: String,
    byte_length: u64,
}

fn canonical() -> [(&'static str, &'static [u8]); 6] {
    [
        ("LICENSE", include_bytes!("../../../LICENSE")),
        (
            "SENDSPIN-SPEC-LICENSE.md",
            include_bytes!("../../../SENDSPIN-SPEC-LICENSE.md"),
        ),
        (
            "THIRD_PARTY_NOTICES.txt",
            include_bytes!("../../../THIRD_PARTY_NOTICES.txt"),
        ),
        ("Cargo.lock", include_bytes!("../../../Cargo.lock")),
        (
            "dependency-lock.json",
            include_bytes!("../../../docs/dependency-lock.json"),
        ),
        ("Cargo.toml", include_bytes!("../../../Cargo.toml")),
    ]
}
fn version() -> Result<String, String> {
    let text = include_str!("../../../Cargo.toml");
    let section = text
        .split("[workspace.package]")
        .nth(1)
        .ok_or("missing canonical workspace version")?;
    let section = section.split('[').next().unwrap_or(section);
    section
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("version = \"")
                .and_then(|s| s.strip_suffix('"'))
        })
        .map(str::to_owned)
        .ok_or("missing canonical workspace version".into())
}
fn target(target: &str) -> Result<(&'static str, u32), String> {
    match target {
        "x86_64-unknown-linux-gnu" => Ok(("elf", 62)),
        "aarch64-unknown-linux-gnu" => Ok(("elf", 183)),
        "x86_64-apple-darwin" => Ok(("macho", 0x01000007)),
        "aarch64-apple-darwin" => Ok(("macho", 0x0100000c)),
        _ => Err("unsupported native package target".into()),
    }
}
fn layout(target_name: &str) -> Result<(&'static str, &'static str), String> {
    if target(target_name)?.0 == "elf" {
        Ok(("bin", "share/earph"))
    } else {
        Ok(("Earph.app/Contents/MacOS", "Earph.app/Contents/Resources"))
    }
}
fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn check_build(build: &DeclaredBuild, target: &str) -> Result<(), String> {
    if build.workspace_version != version()?
        || build.target != target
        || !matches!(build.profile.as_str(), "debug" | "release")
        || build.source_commit.as_ref().is_some_and(|s| !hex(s, 40))
        || build
            .source_archive_sha256
            .as_ref()
            .is_some_and(|s| !hex(s, 64))
        || build.rustc_version.as_ref().is_some_and(|s| {
            s.len() > 100
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" .()-".contains(&b))
        })
    {
        return Err(
            "declared build version, target, or bounded public provenance is invalid".into(),
        );
    }
    Ok(())
}
fn no_symlinks(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let meta = fs::symlink_metadata(ancestor)
            .map_err(|e| format!("cannot inspect path component: {e}"))?;
        if meta.file_type().is_symlink() {
            return Err("symlink path component is forbidden".into());
        }
    }
    Ok(())
}
fn opened(path: &Path, limit: u64) -> Result<(File, Metadata), String> {
    no_symlinks(path)?;
    let before = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !before.is_file() || before.len() > limit {
        return Err("input is not a bounded regular file".into());
    }
    let file = File::open(path).map_err(|e| format!("cannot open regular input: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file()
        || meta.len() > limit
        || before.dev() != meta.dev()
        || before.ino() != meta.ino()
    {
        return Err("input is not an unchanged bounded regular file".into());
    }
    Ok((file, meta))
}
fn stable(path: &Path, before: &Metadata, file: &File) -> Result<(), String> {
    no_symlinks(path)?;
    let after = file.metadata().map_err(|e| e.to_string())?;
    let entry = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if before.dev() != entry.dev()
        || before.ino() != entry.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err("input changed during read".into());
    }
    Ok(())
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let (mut file, meta) = opened(path, limit)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 != meta.len() {
        return Err("input changed or exceeded its bound".into());
    }
    stable(path, &meta, &file)?;
    Ok(bytes)
}
fn json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    serde_json::from_slice(&read(path, MAX_MANIFEST)?)
        .map_err(|e| format!("invalid bounded package JSON: {e}"))
}
/// Loads a bounded request without following symlink path components.
pub fn load_request(path: &Path) -> Result<PackageRequest, String> {
    json(path)
}
fn relative(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 200
        || value.contains('\\')
        || !value.is_ascii()
        || value
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
        || !Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err("invalid package-relative path".into());
    }
    Ok(())
}
fn header(bytes: &[u8], target_name: &str) -> Result<(), String> {
    let (format, machine) = target(target_name)?;
    let valid = if format == "elf" {
        bytes.len() >= 64
            && &bytes[..7] == b"\x7fELF\x02\x01\x01"
            && u16::from_le_bytes([bytes[18], bytes[19]]) as u32 == machine
            && matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
            && bytes[20..24] == 1u32.to_le_bytes()
            && bytes[24..32] != 0u64.to_le_bytes()
            && bytes[52..54] == 64u16.to_le_bytes()
    } else {
        bytes.len() >= 32
            && bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe]
            && bytes[4..8] == machine.to_le_bytes()
            && bytes[12..16] == 2u32.to_le_bytes()
    };
    if !valid {
        return Err("executable header format or architecture differs from target".into());
    }
    Ok(())
}
fn write_new(
    root: &Path,
    relative_path: &str,
    bytes: &[u8],
    mode: u32,
    kind: &str,
) -> Result<Entry, String> {
    relative(relative_path)?;
    let path = root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        no_symlinks(parent)?;
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("cannot create exclusive package file: {e}"))?;
    file.write_all(bytes)
        .map_err(|e| format!("cannot write package file: {e}"))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(Entry {
        path: relative_path.into(),
        byte_length: bytes.len() as u64,
        sha256: sha256(bytes),
        mode,
        kind: kind.into(),
    })
}
fn copy(
    root: &Path,
    path: &str,
    product: &Product,
    kind: &str,
    target_name: Option<&str>,
) -> Result<Entry, String> {
    relative(path)?;
    let limit = if target_name.is_some() {
        MAX_PRODUCT
    } else {
        MAX_METADATA
    };
    if product.byte_length > limit || !hex(&product.sha256, 64) {
        return Err("invalid expected length or SHA256".into());
    }
    let (mut input, meta) = opened(&product.path, limit)?;
    let mode = meta.mode() & 0o7777;
    if meta.len() != product.byte_length
        || mode & 0o7000 != 0
        || (target_name.is_some() && mode & 0o111 == 0)
        || (target_name.is_none() && (mode & 0o111 != 0 || mode & 0o400 == 0))
    {
        return Err("input length or permissions do not match package contract".into());
    }
    let output = root.join(path);
    let parent = output.parent().ok_or("missing package parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    no_symlinks(parent)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&output)
        .map_err(|e| format!("cannot create exclusive copied file: {e}"))?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let mut first = true;
    loop {
        let count = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if first {
            if let Some(target) = target_name {
                header(&buffer[..count], target)?;
            }
            first = false;
        }
        length = length
            .checked_add(count as u64)
            .ok_or("input length overflow")?;
        if length > product.byte_length {
            return Err("input grew during copy".into());
        }
        hash.update(&buffer[..count]);
        destination
            .write_all(&buffer[..count])
            .map_err(|e| format!("package copy failed: {e}"))?;
    }
    stable(&product.path, &meta, &input)?;
    let digest = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if length != product.byte_length || digest != product.sha256 || (first && target_name.is_some())
    {
        return Err("copied input length or SHA256 differs".into());
    }
    destination
        .set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|e| e.to_string())?;
    destination.sync_all().map_err(|e| e.to_string())?;
    Ok(Entry {
        path: path.into(),
        byte_length: length,
        sha256: digest,
        mode,
        kind: kind.into(),
    })
}
fn canonical_product(path: &Path, bytes: &[u8]) -> Product {
    Product {
        path: path.into(),
        byte_length: bytes.len() as u64,
        sha256: sha256(bytes),
    }
}
fn sources(index: &Sources) -> Result<(), String> {
    let canonical = canonical();
    if index.schema_version != 1
        || index.scope != SOURCE_SCOPE
        || index.license != "MPL-2.0"
        || index.source_form != "Unmodified crates.io source archives"
        || index.cargo_lock_sha256 != sha256(canonical[3].1)
        || index.dependency_inventory_sha256 != sha256(canonical[4].1)
        || index.packages.len() != 15
    {
        return Err("source index does not bind the canonical dependency set".into());
    }
    let inventory: serde_json::Value =
        serde_json::from_slice(canonical[4].1).map_err(|e| e.to_string())?;
    let expected: BTreeSet<_> = inventory["packages"]
        .as_array()
        .ok_or("invalid canonical inventory")?
        .iter()
        .filter(|p| p["license_expression"] == "MPL-2.0")
        .map(|p| {
            (
                p["name"].as_str().unwrap_or_default().to_owned(),
                p["version"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let lock = std::str::from_utf8(canonical[3].1).map_err(|e| e.to_string())?;
    let mut found = BTreeSet::new();
    for source in &index.packages {
        let key = (source.name.clone(), source.version.clone());
        if !expected.contains(&key)
            || !found.insert(key)
            || source.archive != format!("{}-{}.crate", source.name, source.version)
            || source.public_archive_url
                != format!(
                    "https://static.crates.io/crates/{}/{}",
                    source.name, source.archive
                )
            || !hex(&source.sha256, 64)
            || source.byte_length == 0
            || source.byte_length > MAX_METADATA
        {
            return Err("invalid, duplicate, or unexpected source archive".into());
        }
        relative(&source.archive)?;
        let locked = lock
            .split("[[package]]")
            .find(|section| {
                section
                    .lines()
                    .any(|l| l == format!("name = \"{}\"", source.name))
                    && section
                        .lines()
                        .any(|l| l == format!("version = \"{}\"", source.version))
            })
            .ok_or("source archive absent from canonical lock")?;
        if !locked
            .lines()
            .any(|l| l == format!("checksum = \"{}\"", source.sha256))
        {
            return Err("source archive SHA256 differs from canonical lock".into());
        }
    }
    if found != expected {
        return Err("source archive set is incomplete".into());
    }
    Ok(())
}
fn desktop(prefix: &str) -> Result<String, String> {
    if !prefix.starts_with('/')
        || prefix.len() > 1024
        || prefix.contains('=')
        || prefix.chars().any(|c| c.is_control())
        || prefix.split('/').any(|s| matches!(s, "." | ".."))
    {
        return Err("Linux final prefix must be a bounded absolute stable path without '='".into());
    }
    // Exec quoting is followed by desktop-entry string escaping. No shell runs here.
    let executable = format!("{}/bin/earph-desktop", prefix.trim_end_matches('/'));
    let mut quoted = String::from("\"");
    for c in executable.chars() {
        match c {
            '%' => quoted.push_str("%%"),
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    let escaped = quoted.replace('\\', "\\\\");
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName=Earph\nExec={escaped}\nTerminal=false\nCategories=AudioVideo;Audio;\n"
    ))
}
fn plist() -> Result<String, String> {
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>CFBundleIdentifier</key><string>com.harperreed.earph</string>\n<key>CFBundleName</key><string>Earph</string>\n<key>CFBundleExecutable</key><string>earph-desktop</string>\n<key>CFBundlePackageType</key><string>APPL</string>\n<key>CFBundleShortVersionString</key><string>{}</string>\n<key>CFBundleVersion</key><string>{}</string>\n</dict></plist>\n",
        version()?,
        version()?
    ))
}
/// Creates only a new candidate. Every failure preserves any partial directory.
pub fn build(request: &PackageRequest, output: &Path) -> Result<(), String> {
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        no_symlinks(parent)?;
    }
    fs::create_dir(output).map_err(|e| {
        format!("package destination must be new; existing destination unchanged: {e}")
    })?;
    build_new(request, output)
        .map_err(|e| format!("{e}; partial package retained at {}", output.display()))
}
fn build_new(request: &PackageRequest, output: &Path) -> Result<(), String> {
    let (bin, resources) = layout(&request.target)?;
    if target(&request.target)?.0 != "elf" && request.linux_final_prefix.is_some() {
        return Err("Linux final prefix is invalid for macOS".into());
    }
    let build_bytes = read(&request.build_record, MAX_MANIFEST)?;
    let record: DeclaredBuild = serde_json::from_slice(&build_bytes).map_err(|e| e.to_string())?;
    check_build(&record, &request.target)?;
    let source_index_path = request.mpl_sources.join("index.json");
    let index_bytes = read(&source_index_path, MAX_MANIFEST)?;
    let index: Sources = serde_json::from_slice(&index_bytes).map_err(|e| e.to_string())?;
    sources(&index)?;
    let mut files = vec![
        copy(
            output,
            &format!("{bin}/earph-desktop"),
            &request.desktop,
            "desktop",
            Some(&request.target),
        )?,
        copy(
            output,
            &format!("{bin}/earph-engine"),
            &request.engine,
            "engine",
            Some(&request.target),
        )?,
    ];
    let paths = [
        &request.license,
        &request.specification_license,
        &request.notices,
        &request.cargo_lock,
        &request.dependency_inventory,
        &request.workspace_manifest,
    ];
    for ((name, bytes), path) in canonical().into_iter().zip(paths) {
        files.push(copy(
            output,
            &format!("{resources}/{name}"),
            &canonical_product(path, bytes),
            "canonical-metadata",
            None,
        )?);
    }
    files.push(copy(
        output,
        &format!("{resources}/declared-build.json"),
        &canonical_product(&request.build_record, &build_bytes),
        "declared-provenance",
        None,
    )?);
    files.push(copy(
        output,
        &format!("{resources}/mpl-sources/index.json"),
        &canonical_product(&source_index_path, &index_bytes),
        "source-index",
        None,
    )?);
    for source in &index.packages {
        files.push(copy(
            output,
            &format!("{resources}/mpl-sources/{}", source.archive),
            &Product {
                path: request.mpl_sources.join(&source.archive),
                byte_length: source.byte_length,
                sha256: source.sha256.clone(),
            },
            "mpl-source-archive",
            None,
        )?);
    }
    files.push(write_new(
        output,
        &format!("{resources}/SOURCE-INSTRUCTIONS.txt"),
        INSTRUCTIONS.as_bytes(),
        0o644,
        "source-instructions",
    )?);
    if target(&request.target)?.0 == "elf" {
        if let Some(prefix) = &request.linux_final_prefix {
            files.push(write_new(
                output,
                "share/applications/earph.desktop",
                desktop(prefix)?.as_bytes(),
                0o644,
                "desktop-entry",
            )?);
        }
    } else {
        files.push(write_new(
            output,
            "Earph.app/Contents/Info.plist",
            plist()?.as_bytes(),
            0o644,
            "plist",
        )?);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest = Manifest {
        schema_version: 1,
        target: request.target.clone(),
        workspace_version: version()?,
        cargo_lock_sha256: sha256(canonical()[3].1),
        product_provenance_state: "declared-unverified".into(),
        declared_build: record,
        package_tool: crate::build_provenance(),
        signing: "unsigned".into(),
        product_signatures: "not-inspected".into(),
        release: "unreleased-development-candidate".into(),
        native_validation: "not-qualified-by-package-tool".into(),
        linux_final_prefix: request.linux_final_prefix.clone(),
        files,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("package manifest exceeds bound".into());
    }
    write_new(output, MANIFEST, &bytes, 0o644, "manifest")?;
    verify(output)
}

fn expected(manifest: &Manifest, index: &Sources) -> Result<BTreeMap<String, String>, String> {
    let (bin, resources) = layout(&manifest.target)?;
    let mut result = BTreeMap::new();
    for (name, kind) in [("earph-desktop", "desktop"), ("earph-engine", "engine")] {
        result.insert(format!("{bin}/{name}"), kind.into());
    }
    for (name, _) in canonical() {
        result.insert(format!("{resources}/{name}"), "canonical-metadata".into());
    }
    for (name, kind) in [
        ("declared-build.json", "declared-provenance"),
        ("SOURCE-INSTRUCTIONS.txt", "source-instructions"),
        ("mpl-sources/index.json", "source-index"),
    ] {
        result.insert(format!("{resources}/{name}"), kind.into());
    }
    for source in &index.packages {
        result.insert(
            format!("{resources}/mpl-sources/{}", source.archive),
            "mpl-source-archive".into(),
        );
    }
    if target(&manifest.target)?.0 == "elf" {
        if let Some(prefix) = &manifest.linux_final_prefix {
            desktop(prefix)?;
            result.insert(
                "share/applications/earph.desktop".into(),
                "desktop-entry".into(),
            );
        }
    } else {
        if manifest.linux_final_prefix.is_some() {
            return Err("macOS package has a Linux prefix".into());
        }
        result.insert("Earph.app/Contents/Info.plist".into(), "plist".into());
    }
    Ok(result)
}
fn scan(
    root: &Path,
    relative_path: &Path,
    files: &mut BTreeSet<String>,
    dirs: &mut BTreeSet<String>,
    budget: &mut usize,
) -> Result<(), String> {
    for entry in fs::read_dir(root.join(relative_path)).map_err(|e| e.to_string())? {
        if *budget == 0 {
            return Err("package tree exceeds entry bound".into());
        }
        *budget -= 1;
        let entry = entry.map_err(|e| e.to_string())?;
        let path = relative_path.join(entry.file_name());
        let name = path.to_str().ok_or("non-UTF8 package entry")?.to_owned();
        relative(&name)?;
        if path.components().count() > 6 {
            return Err("package tree exceeds depth bound".into());
        }
        let meta = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() {
            return Err("package contains symlink".into());
        }
        if meta.is_dir() {
            dirs.insert(name);
            scan(root, &path, files, dirs, budget)?;
        } else if meta.is_file() {
            files.insert(name);
        } else {
            return Err("package contains a nonregular entry".into());
        }
    }
    Ok(())
}
fn verify_entry(root: &Path, entry: &Entry, target_name: &str) -> Result<(), String> {
    relative(&entry.path)?;
    let executable = matches!(entry.kind.as_str(), "desktop" | "engine");
    if !hex(&entry.sha256, 64)
        || entry.byte_length
            > if executable {
                MAX_PRODUCT
            } else {
                MAX_METADATA
            }
        || entry.mode & !0o777 != 0
        || (executable && entry.mode & 0o111 == 0)
        || (!executable && (entry.mode & 0o111 != 0 || entry.mode & 0o400 == 0))
    {
        return Err("manifest file bounds or mode are invalid".into());
    }
    let path = root.join(&entry.path);
    let (mut file, meta) = opened(
        &path,
        if executable {
            MAX_PRODUCT
        } else {
            MAX_METADATA
        },
    )?;
    if meta.len() != entry.byte_length || meta.mode() & 0o7777 != entry.mode {
        return Err(format!("package length or mode differs: {}", entry.path));
    }
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut first = true;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if first && executable {
            header(&buffer[..count], target_name)?;
        }
        first = false;
        length += count as u64;
        if length > entry.byte_length {
            return Err("package file grew during verification".into());
        }
        hash.update(&buffer[..count]);
    }
    stable(&path, &meta, &file)?;
    let digest = hash
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if length != entry.byte_length || digest != entry.sha256 || (first && executable) {
        return Err(format!("package SHA256 differs: {}", entry.path));
    }
    Ok(())
}
/// Reads package state without repairing, extracting, installing, or running it.
pub fn verify(output: &Path) -> Result<(), String> {
    no_symlinks(output)?;
    let manifest: Manifest = json(&output.join(MANIFEST))?;
    if manifest.schema_version != 1
        || manifest.files.len() > 40
        || manifest.workspace_version != version()?
        || manifest.cargo_lock_sha256 != sha256(canonical()[3].1)
        || manifest.product_provenance_state != "declared-unverified"
        || manifest.signing != "unsigned"
        || manifest.product_signatures != "not-inspected"
        || manifest.release != "unreleased-development-candidate"
        || manifest.native_validation != "not-qualified-by-package-tool"
    {
        return Err("unsupported package manifest or release claims".into());
    }
    check_build(&manifest.declared_build, &manifest.target)?;
    let (_, resources) = layout(&manifest.target)?;
    let index: Sources = json(&output.join(resources).join("mpl-sources/index.json"))?;
    sources(&index)?;
    let expected = expected(&manifest, &index)?;
    let mut listed = BTreeSet::new();
    for entry in &manifest.files {
        if !listed.insert(entry.path.clone()) || expected.get(&entry.path) != Some(&entry.kind) {
            return Err("duplicate or noncanonical manifest layout".into());
        }
        verify_entry(output, entry, &manifest.target)?;
    }
    if listed != expected.keys().cloned().collect() {
        return Err("manifest omits required package files".into());
    }
    let mut actual = BTreeSet::new();
    let mut dirs = BTreeSet::new();
    scan(output, Path::new(""), &mut actual, &mut dirs, &mut 64)?;
    listed.insert(MANIFEST.into());
    let expected_dirs: BTreeSet<_> = listed
        .iter()
        .flat_map(|name| {
            Path::new(name)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
        })
        .collect();
    if actual != listed || dirs != expected_dirs {
        return Err("package contains extra or missing files or directories".into());
    }
    if fs::symlink_metadata(output.join(MANIFEST))
        .map_err(|e| e.to_string())?
        .mode()
        & 0o7777
        != 0o644
    {
        return Err("manifest permissions differ".into());
    }
    for (name, bytes) in canonical() {
        let path = format!("{resources}/{name}");
        let entry = manifest
            .files
            .iter()
            .find(|e| e.path == path)
            .ok_or("missing canonical metadata")?;
        if entry.byte_length != bytes.len() as u64 || entry.sha256 != sha256(bytes) {
            return Err("canonical metadata differs from package tool".into());
        }
    }
    for source in &index.packages {
        let path = format!("{resources}/mpl-sources/{}", source.archive);
        let entry = manifest
            .files
            .iter()
            .find(|e| e.path == path)
            .ok_or("missing source archive")?;
        if entry.sha256 != source.sha256 || entry.byte_length != source.byte_length {
            return Err("source archive differs from source index".into());
        }
    }
    let record: DeclaredBuild = json(&output.join(resources).join("declared-build.json"))?;
    if record != manifest.declared_build {
        return Err("declared build differs from manifest".into());
    }
    if read(
        &output.join(resources).join("SOURCE-INSTRUCTIONS.txt"),
        MAX_METADATA,
    )? != INSTRUCTIONS.as_bytes()
    {
        return Err("recipient source instructions differ".into());
    }
    if target(&manifest.target)?.0 == "elf" {
        if let Some(prefix) = &manifest.linux_final_prefix
            && read(
                &output.join("share/applications/earph.desktop"),
                MAX_MANIFEST,
            )? != desktop(prefix)?.as_bytes()
        {
            return Err("desktop entry differs from stable final prefix".into());
        }
    } else if read(&output.join("Earph.app/Contents/Info.plist"), MAX_MANIFEST)?
        != plist()?.as_bytes()
    {
        return Err("macOS property list differs from canonical layout".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
        bytes[18..20].copy_from_slice(&62u16.to_le_bytes());
        bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
        bytes[24..32].copy_from_slice(&0x400000u64.to_le_bytes());
        bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        bytes
    }
    #[test]
    fn executable_header_parser_checks_class_endianness_type_version_and_machine() {
        let bytes = elf();
        assert!(header(&bytes, "x86_64-unknown-linux-gnu").is_ok());
        for (offset, value) in [
            (4, 1),
            (5, 2),
            (6, 0),
            (16, 1),
            (18, 183),
            (20, 0),
            (52, 63),
        ] {
            let mut invalid = bytes.clone();
            invalid[offset] = value;
            assert!(
                header(&invalid, "x86_64-unknown-linux-gnu").is_err(),
                "{offset}"
            );
        }
        for length in [0, 4, 31, 63] {
            assert!(header(&bytes[..length], "x86_64-unknown-linux-gnu").is_err());
        }
        assert!(header(&bytes, "aarch64-unknown-linux-gnu").is_err());
        assert!(header(&bytes, "aarch64-apple-darwin").is_err());
        let mut mac = vec![0; 32];
        mac[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        mac[4..8].copy_from_slice(&0x0100000cu32.to_le_bytes());
        mac[12..16].copy_from_slice(&2u32.to_le_bytes());
        assert!(header(&mac, "aarch64-apple-darwin").is_ok());
        assert!(header(&mac, "x86_64-apple-darwin").is_err());
        mac[12] = 6;
        assert!(header(&mac, "aarch64-apple-darwin").is_err());
    }
    #[test]
    fn manifest_paths_have_one_canonical_relative_form() {
        assert!(relative("Earph.app/Contents/MacOS/earph-engine").is_ok());
        for path in [
            "",
            "/absolute",
            "../escape",
            "bin/../engine",
            "bin//engine",
            "bin/./engine",
            "bin/engine/",
            "bin\\engine",
            "雪",
        ] {
            assert!(relative(path).is_err(), "{path}");
        }
    }
    #[test]
    fn canonical_workspace_version_matches_cargo_inheritance() {
        assert_eq!(version().unwrap(), env!("CARGO_PKG_VERSION"));
    }
    #[test]
    fn linux_exec_escapes_both_desktop_value_and_argument_layers() {
        let entry = desktop("/opt/Doctor Biz/$dollar`tick`\\slash\"quote%").unwrap();
        assert!(entry.lines().any(|line|line==r#"Exec="/opt/Doctor Biz/\\$dollar\\`tick\\`\\\\slash\\"quote%%/bin/earph-desktop""#));
    }
}
