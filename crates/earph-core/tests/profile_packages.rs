// ABOUTME: Exercises declared package boundaries through real serde producers and strict readers.
// ABOUTME: Keeps owner-observed byte inventory distinct from imported manifest assertions.
use earph_core::{
    ErrorCode,
    graph_control::GraphIntent,
    ids::ProfileId,
    profile_control::ProfileCatalogReference,
    profile_packages::{
        MAX_PACKAGE_BYTES, MAX_PACKAGE_MANIFEST_BYTES, MAX_PACKAGE_OBJECTS, PackageManifest,
        PackageObject, PackageObjectKind, PackageObjectObservation, read_package_manifest,
    },
    profiles::{
        ProfileCorrectionKind, ProfileCorrectionReference, ProfileDocument, ProfileObjectHash,
        read_profile,
    },
};
use serde_json::{Value, json};

fn hash(digit: char) -> ProfileObjectHash {
    ProfileObjectHash::try_from(digit.to_string().repeat(64)).unwrap()
}
fn fixture() -> (ProfileDocument, GraphIntent, PackageManifest) {
    let profile = read_profile(&serde_json::to_vec(&json!({
        "schema_version":1,"id":"desk","revision":1,"name":"Desk headphones",
        "graph_sha256":"b".repeat(64),"source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"dac","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_db":-20.0,"hardware_cap_db":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},
        "corrections":[]
    })).unwrap()).unwrap();
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    let graph: GraphIntent = serde_json::from_value(json!({"graph":{"schema_version":1,"output":"out",
        "nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}})).unwrap();
    let manifest = PackageManifest {
        schema_version: 1,
        profile: ProfileCatalogReference {
            id: profile.id.clone(),
            revision: 1,
            profile_sha256: hash('a'),
        },
        objects: vec![
            PackageObject {
                kind: PackageObjectKind::Profile,
                sha256: hash('a'),
                bytes: serde_json::to_vec(&profile).unwrap().len() as u64,
            },
            PackageObject {
                kind: PackageObjectKind::Graph,
                sha256: hash('b'),
                bytes: serde_json::to_vec(&graph).unwrap().len() as u64,
            },
        ],
    };
    (profile, graph, manifest)
}
fn read(value: &Value) -> Result<PackageManifest, earph_core::EngineError> {
    read_package_manifest(&serde_json::to_vec(value).unwrap())
}

#[test]
fn real_profile_and_graph_producers_roundtrip_declared_package_references() {
    let (profile, graph, manifest) = fixture();
    let profile_bytes = serde_json::to_vec(&profile).unwrap();
    let graph_bytes = serde_json::to_vec(&graph).unwrap();
    let received = read_package_manifest(&serde_json::to_vec(&manifest).unwrap()).unwrap();
    let received_profile = read_profile(&profile_bytes).unwrap();
    let received_graph: GraphIntent = serde_json::from_slice(&graph_bytes).unwrap();
    received_graph.graph.validate_structure().unwrap();
    assert_eq!(received, manifest);
    assert_eq!(received_profile, profile);
    assert_eq!(received_graph, graph);
    received
        .validate_profile_references(&received_profile)
        .unwrap();
    // Hash computation belongs to the owner; these independent facts model its typed read set.
    received
        .validate_observed_inventory(&[
            PackageObjectObservation {
                kind: PackageObjectKind::Profile,
                sha256: hash('a'),
                bytes: profile_bytes.len() as u64,
            },
            PackageObjectObservation {
                kind: PackageObjectKind::Graph,
                sha256: hash('b'),
                bytes: graph_bytes.len() as u64,
            },
        ])
        .unwrap();
    assert_eq!(received_profile.volume.software_cap_db, -20.0);
    assert_eq!(
        received_profile.output.manual_setup.as_deref(),
        Some("headphones")
    );
}

#[test]
fn strict_manifest_rejects_unknown_fields_versions_hashes_and_duplicate_fields() {
    let (_, _, manifest) = fixture();
    let value = serde_json::to_value(&manifest).unwrap();
    for field in [
        "path",
        "url",
        "associations",
        "verified_device_context",
        "consent",
        "arming",
        "readback",
    ] {
        let mut hostile = value.clone();
        hostile[field] = json!("untrusted");
        assert_eq!(
            read(&hostile).unwrap_err().code,
            ErrorCode::AssetInvalid,
            "{field}"
        );
    }
    let mut future = value.clone();
    future["schema_version"] = json!(2);
    assert_eq!(
        read(&future).unwrap_err().code,
        ErrorCode::CapabilityUnsupported
    );
    for malformed in ["A".repeat(64), "../outside".into(), "abc".into()] {
        let mut hostile = value.clone();
        hostile["objects"][0]["sha256"] = json!(malformed);
        assert_eq!(read(&hostile).unwrap_err().code, ErrorCode::AssetInvalid);
    }
    for kind in [
        "archive",
        "executable",
        "recording",
        "session_checkpoint",
        "native_association",
    ] {
        let mut hostile = value.clone();
        hostile["objects"][0]["kind"] = json!(kind);
        assert_eq!(read(&hostile).unwrap_err().code, ErrorCode::AssetInvalid);
    }
    let wire = serde_json::to_string(&manifest).unwrap();
    let duplicate = wire.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert_eq!(
        read_package_manifest(duplicate.as_bytes())
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    let mut hostile = value;
    hostile["objects"][0]["filename"] = json!("../../outside");
    assert_eq!(read(&hostile).unwrap_err().code, ErrorCode::AssetInvalid);
}

#[test]
fn typed_object_identity_rejects_duplicates_conflicting_sizes_and_wrong_root() {
    let (_, _, manifest) = fixture();
    let mut duplicate = manifest.clone();
    duplicate.objects.push(duplicate.objects[0].clone());
    assert_eq!(
        duplicate.validate().unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    let mut conflict = manifest.clone();
    conflict.objects.push(PackageObject {
        kind: PackageObjectKind::TextSource,
        sha256: hash('a'),
        bytes: 1,
    });
    assert_eq!(
        conflict.validate().unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    conflict.objects[2].bytes = conflict.objects[0].bytes;
    conflict.validate().unwrap();
    assert_ne!(
        conflict.objects[0].file_name(),
        conflict.objects[2].file_name()
    );
    let mut missing = manifest.clone();
    missing.profile.profile_sha256 = hash('c');
    assert_eq!(
        missing.validate().unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    let mut zero = manifest.clone();
    zero.profile.revision = 0;
    assert_eq!(zero.validate().unwrap_err().code, ErrorCode::AssetInvalid);
    let mut extra_profile = manifest;
    extra_profile.objects.push(PackageObject {
        kind: PackageObjectKind::Profile,
        sha256: hash('c'),
        bytes: 1,
    });
    assert_eq!(
        extra_profile.validate().unwrap_err().code,
        ErrorCode::AssetInvalid
    );
}

#[test]
fn parsed_profile_requires_exact_root_graph_and_typed_correction_references() {
    let (mut profile, _, mut manifest) = fixture();
    let mut wrong_id = manifest.clone();
    wrong_id.profile.id = ProfileId::new("other").unwrap();
    assert_eq!(
        wrong_id
            .validate_profile_references(&profile)
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    let mut stale = manifest.clone();
    stale.profile.revision = 2;
    assert_eq!(
        stale
            .validate_profile_references(&profile)
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    manifest.objects[1].sha256 = hash('c');
    assert_eq!(
        manifest
            .validate_profile_references(&profile)
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    manifest.objects[1].sha256 = hash('b');
    profile.corrections.push(ProfileCorrectionReference {
        kind: ProfileCorrectionKind::TextReport,
        object_sha256: hash('d'),
    });
    manifest.objects.push(PackageObject {
        kind: PackageObjectKind::FirManifest,
        sha256: hash('d'),
        bytes: 10,
    });
    assert_eq!(
        manifest
            .validate_profile_references(&profile)
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    manifest.objects[2].kind = PackageObjectKind::TextReport;
    manifest.validate_profile_references(&profile).unwrap();
}

#[test]
fn observed_inventory_must_match_consumed_kinds_hashes_sizes_and_count() {
    let (_, _, manifest) = fixture();
    let observations = vec![
        PackageObjectObservation {
            kind: PackageObjectKind::Profile,
            sha256: hash('a'),
            bytes: manifest.objects[0].bytes,
        },
        PackageObjectObservation {
            kind: PackageObjectKind::Graph,
            sha256: hash('b'),
            bytes: manifest.objects[1].bytes,
        },
    ];
    manifest.validate_observed_inventory(&observations).unwrap();
    let mut reordered = observations.clone();
    reordered.reverse();
    manifest.validate_observed_inventory(&reordered).unwrap();
    assert_eq!(
        manifest
            .validate_observed_inventory(&observations[..1])
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    for case in 0..4 {
        let mut changed = observations.clone();
        match case {
            0 => changed[1].sha256 = hash('c'),
            1 => changed[1].bytes += 1,
            2 => changed[1].kind = PackageObjectKind::TextSource,
            3 => changed[1] = changed[0].clone(),
            _ => unreachable!(),
        }
        assert_eq!(
            manifest
                .validate_observed_inventory(&changed)
                .unwrap_err()
                .code,
            ErrorCode::AssetInvalid
        );
    }
    let mut surplus = observations;
    surplus.push(PackageObjectObservation {
        kind: PackageObjectKind::TextSource,
        sha256: hash('c'),
        bytes: 1,
    });
    assert_eq!(
        manifest
            .validate_observed_inventory(&surplus)
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
}

#[test]
fn object_count_spare_capacity_zero_lengths_and_overflow_are_bounded() {
    let (_, _, manifest) = fixture();
    let mut full = manifest.clone();
    for n in 0..MAX_PACKAGE_OBJECTS - 2 {
        full.objects.push(PackageObject {
            kind: PackageObjectKind::TextSource,
            sha256: ProfileObjectHash::try_from(format!("{:064x}", n + 1)).unwrap(),
            bytes: 1,
        });
    }
    full.objects.shrink_to_fit();
    read_package_manifest(&serde_json::to_vec(&full).unwrap()).unwrap();
    full.objects.push(PackageObject {
        kind: PackageObjectKind::TextSource,
        sha256: hash('c'),
        bytes: 1,
    });
    assert_eq!(
        read_package_manifest(&serde_json::to_vec(&full).unwrap())
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut spare = manifest.clone();
    spare.objects.reserve_exact(MAX_PACKAGE_OBJECTS + 1);
    assert_eq!(
        spare.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut retained_id = manifest.clone();
    let mut identifier = String::with_capacity(MAX_PACKAGE_MANIFEST_BYTES + 1);
    identifier.push_str("desk");
    retained_id.profile.id = ProfileId::new(identifier).unwrap();
    assert_eq!(
        retained_id.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut zero = manifest.clone();
    zero.objects[0].bytes = 0;
    assert_eq!(zero.validate().unwrap_err().code, ErrorCode::AssetInvalid);
    let mut overflowing = manifest;
    overflowing.objects[0].bytes = u64::MAX;
    assert_eq!(
        overflowing.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn actual_encoded_manifest_bytes_count_toward_the_total_package_limit() {
    let (_, _, mut manifest) = fixture();
    manifest.objects.push(PackageObject {
        kind: PackageObjectKind::FirWav,
        sha256: hash('c'),
        bytes: MAX_PACKAGE_BYTES,
    });
    let encoded_length = serde_json::to_vec(&manifest).unwrap().len();
    manifest.objects[2].bytes = MAX_PACKAGE_BYTES
        - encoded_length as u64
        - manifest.objects[0].bytes
        - manifest.objects[1].bytes;
    let mut wire = serde_json::to_vec(&manifest).unwrap();
    assert_eq!(wire.len(), encoded_length);
    read_package_manifest(&wire).unwrap();
    wire.push(b' ');
    assert_eq!(
        read_package_manifest(&wire).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let (_, _, small) = fixture();
    let mut padded = serde_json::to_vec(&small).unwrap();
    padded.resize(MAX_PACKAGE_MANIFEST_BYTES, b' ');
    read_package_manifest(&padded).unwrap();
    padded.push(b' ');
    assert_eq!(
        read_package_manifest(&padded).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn fixed_typed_names_reject_paths_aliases_and_undeclared_entries() {
    let (_, _, mut manifest) = fixture();
    for (kind, digit, suffix) in [
        (PackageObjectKind::TextReport, 'c', "text-report.json"),
        (PackageObjectKind::TextSource, 'd', "text-source.txt"),
        (PackageObjectKind::FirManifest, 'e', "fir-manifest.json"),
        (PackageObjectKind::FirWav, 'f', "wav"),
    ] {
        let object = PackageObject {
            kind,
            sha256: hash(digit),
            bytes: 1,
        };
        assert_eq!(
            object.file_name(),
            format!("{}.{suffix}", digit.to_string().repeat(64))
        );
        manifest.objects.push(object);
    }
    assert_eq!(
        manifest.objects[0].file_name(),
        format!("{}.profile.json", "a".repeat(64))
    );
    assert_eq!(
        manifest.objects[1].file_name(),
        format!("{}.graph.json", "b".repeat(64))
    );
    let name = manifest.objects[0].file_name();
    assert_eq!(manifest.object_named(&name).unwrap(), &manifest.objects[0]);
    for path in [
        format!("../{name}"),
        format!("./{name}"),
        format!("/{name}"),
        format!("C:\\{name}"),
        format!("%2e%2e/{name}"),
        format!("{name}/child"),
        format!("{name}.bak"),
        name.to_uppercase(),
        "manifest.json".into(),
        "\0".into(),
        "https://example.invalid/object".into(),
    ] {
        assert_eq!(
            manifest.object_named(&path).unwrap_err().code,
            ErrorCode::AssetInvalid,
            "{path:?}"
        );
    }
}
