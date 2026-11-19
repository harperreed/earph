// ABOUTME: Exercises real owner package commands and exact-byte directory roundtrips.
// ABOUTME: Checks hostile inventories, publication safety, and saved-only authority.
use earph_engine::{EngineOwner, request};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};
mod support;

fn graph() -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    json!({"graph":{"schema_version":1,"output":"out","nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}})
}
fn draft() -> Value {
    json!({"name":"Owned desk","source_policy":"local_file","fidelity":"compatible_conversion","output":{"backend":"alsa","route":"system","device_id":"desk-dac","manual_setup":"headphones"},"volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},"latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})
}
fn call(runtime: &Path, command: &Value) -> Value {
    serde_json::from_slice(&request(runtime, &serde_json::to_vec(command).unwrap()).unwrap())
        .unwrap()
}
fn state(runtime: &Path) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    call(runtime, &json!({"schema_version":1,"request_id":format!("state-{}",NEXT.fetch_add(1,std::sync::atomic::Ordering::Relaxed)),"zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}}))["receipt"]["result"]["value"].clone()
}
fn command(runtime: &Path, id: &str, payload: Value) -> Value {
    let s = state(runtime);
    json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":s["owner_id"],"session_id":s["session_id"],"expected_revision":s["revision"],"payload":payload})
}
fn finish(runtime: &Path, id: &str, payload: Value) -> Value {
    let c = command(runtime, id, payload);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = call(runtime, &c);
        if r["receipt"]["status"] != "accepted" {
            return r;
        }
        assert!(Instant::now() < deadline, "package never settled: {r}");
        thread::sleep(Duration::from_millis(2));
    }
}
fn applied(runtime: &Path, id: &str, payload: Value) -> Value {
    let r = finish(runtime, id, payload);
    assert_eq!(r["receipt"]["status"], "applied", "{id}: {r}");
    r["receipt"]["result"]["value"].clone()
}
fn root() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/profile-package-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&p)
        .unwrap();
    fs::canonicalize(p).unwrap()
}
fn stop(runtime: &Path, host: thread::JoinHandle<()>) {
    applied(runtime, "shutdown", json!({"method":"owner.shutdown"}));
    host.join().unwrap();
}

#[test]
fn owner_previews_exports_inspects_and_imports_exact_saved_intent() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let reference = json!({"id":"desk","revision":1,"profile_sha256":saved["profile_sha256"]});
    let preview = applied(
        &runtime,
        "preview",
        json!({"method":"profiles.package_preview","profile":reference}),
    );
    assert_eq!(preview["operation"], "previewed");
    assert_eq!(preview["manifest"]["objects"].as_array().unwrap().len(), 2);
    assert_eq!(preview["review_sources"].as_array().unwrap().len(), 2);
    let parent = root();
    let destination = parent.join("exported");
    let exported = applied(
        &runtime,
        "export",
        json!({"method":"profiles.export","directory":destination,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"],"export_review":"reviewed_exact_candidate","content_review":"reviewed_no_private_data","redistribution":"refuse_measurement_bytes"}),
    );
    assert_eq!(exported["operation"], "exported");
    let inspected = applied(
        &runtime,
        "inspect",
        json!({"method":"profiles.package_inspect","directory":destination}),
    );
    assert_eq!(inspected["manifest"], preview["manifest"]);
    let fresh = support::runtime();
    let fresh_owner = EngineOwner::bind(&fresh).unwrap();
    let fresh_host = thread::spawn(move || fresh_owner.serve().unwrap());
    let before = state(&fresh);
    let imported = applied(
        &fresh,
        "import",
        json!({"method":"profiles.import_package","directory":destination,"profile":reference,"expected_manifest_sha256":inspected["manifest_sha256"]}),
    );
    assert_eq!(imported, saved);
    for o in inspected["manifest"]["objects"].as_array().unwrap() {
        let hash = o["sha256"].as_str().unwrap();
        let suffix = match o["kind"].as_str().unwrap() {
            "profile" => "profile.json",
            "graph" => "graph.json",
            _ => panic!("unexpected object"),
        };
        let name = format!("{hash}.{suffix}");
        assert_eq!(
            fs::read(destination.join(&name)).unwrap(),
            fs::read(fresh.join("profiles").join(&name)).unwrap()
        );
    }
    let after = state(&fresh);
    for field in [
        "output",
        "output_consented",
        "muted",
        "software_gain_db",
        "volume_policy",
        "configured_profile",
        "graph_epoch",
    ] {
        assert_eq!(after[field], before[field], "{field}");
    }
    let collision = finish(
        &fresh,
        "collision",
        json!({"method":"profiles.import_package","directory":destination,"profile":reference,"expected_manifest_sha256":inspected["manifest_sha256"]}),
    );
    assert_eq!(
        collision["receipt"]["error"]["code"], "ASSET_INVALID",
        "{collision}"
    );
    stop(&runtime, host);
    stop(&fresh, fresh_host);
    let recovered = EngineOwner::bind(&fresh).unwrap();
    let recovered_host = thread::spawn(move || recovered.serve().unwrap());
    assert_eq!(
        applied(&fresh, "get", json!({"method":"profiles.get","id":"desk"})),
        saved
    );
    stop(&fresh, recovered_host);
}

#[test]
fn export_refuses_to_nest_inside_an_existing_package() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let reference = json!({"id":"desk","revision":1,"profile_sha256":saved["profile_sha256"]});
    let preview = applied(
        &runtime,
        "preview",
        json!({"method":"profiles.package_preview","profile":reference}),
    );
    let parent = root();
    let destination = parent.join("exported");
    let payload = |directory: &Path| json!({"method":"profiles.export","directory":directory,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"],"export_review":"reviewed_exact_candidate","content_review":"reviewed_no_private_data","redistribution":"refuse_measurement_bytes"});
    applied(&runtime, "export", payload(&destination));
    let refused = finish(&runtime, "nested", payload(&destination.join("nested")));
    assert_eq!(refused["receipt"]["status"], "failed", "{refused}");
    assert!(!destination.join("nested").exists());
    applied(
        &runtime,
        "intact",
        json!({"method":"profiles.package_inspect","directory":destination}),
    );
    stop(&runtime, host);
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn export_saved(
    runtime: &Path,
    parent: &Path,
    id: &str,
    saved: &Value,
    redistribution: &str,
) -> (PathBuf, Value) {
    let reference = json!({"id":id,"revision":saved["profile"]["revision"],"profile_sha256":saved["profile_sha256"]});
    let preview = applied(
        runtime,
        &format!("preview-{id}"),
        json!({"method":"profiles.package_preview","profile":reference}),
    );
    let destination = parent.join(id);
    applied(
        runtime,
        &format!("export-{id}"),
        json!({"method":"profiles.export","directory":destination,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"],"export_review":"reviewed_exact_candidate","content_review":"reviewed_no_private_data","redistribution":redistribution}),
    );
    (destination, preview)
}
fn copy_package(source: &Path, destination: &Path, omit: Option<&str>) {
    fs::DirBuilder::new()
        .mode(0o700)
        .create(destination)
        .unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if omit.is_some_and(|s| name == s) {
            continue;
        }
        fs::copy(entry.path(), destination.join(name)).unwrap();
    }
}
#[test]
fn real_directory_consumer_rejects_hostile_and_unreachable_inventory() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let parent = root();
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let (source, preview) = export_saved(
        &runtime,
        &parent,
        "desk",
        &saved,
        "refuse_measurement_bytes",
    );
    let graph = preview["manifest"]["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "graph")
        .unwrap();
    let name = format!("{}.graph.json", graph["sha256"].as_str().unwrap());
    for case in [
        "missing",
        "symlink",
        "hardlink",
        "socket",
        "executable",
        "extra",
        "unused",
        "bad-hash",
        "manifest-size",
        "package-size",
        "unknown",
        "duplicate",
    ] {
        let directory = parent.join(case);
        copy_package(
            &source,
            &directory,
            if ["missing", "symlink", "hardlink"].contains(&case) {
                Some(&name)
            } else {
                None
            },
        );
        match case {
            "symlink" => symlink(source.join(&name), directory.join(&name)).unwrap(),
            "hardlink" => {
                let outside = parent.join("hardlinked-original");
                fs::copy(source.join(&name), &outside).unwrap();
                fs::hard_link(outside, directory.join(&name)).unwrap();
            }
            "socket" => {
                let socket = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("target")
                    .join(format!(
                        "socket-{}-{}",
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_nanos()
                    ));
                let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
                fs::rename(socket, directory.join("socket")).unwrap();
                drop(listener);
            }
            "executable" => {
                fs::set_permissions(directory.join(&name), fs::Permissions::from_mode(0o700))
                    .unwrap()
            }
            "extra" => fs::write(directory.join("extra"), b"retained undeclared content").unwrap(),
            "unused" => {
                let bytes = b"valid but unreachable";
                let hash = digest(bytes);
                fs::write(directory.join(format!("{hash}.text-source.txt")), bytes).unwrap();
                let mut manifest = preview["manifest"].clone();
                manifest["objects"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"kind":"text_source","sha256":hash,"bytes":bytes.len()}));
                fs::write(
                    directory.join("manifest.json"),
                    serde_json::to_vec(&manifest).unwrap(),
                )
                .unwrap();
            }
            "bad-hash" => fs::write(directory.join(&name), b"corrupt").unwrap(),
            "manifest-size" => fs::OpenOptions::new()
                .write(true)
                .open(directory.join("manifest.json"))
                .unwrap()
                .set_len(65537)
                .unwrap(),
            "package-size" => fs::OpenOptions::new()
                .write(true)
                .open(directory.join(&name))
                .unwrap()
                .set_len(32 * 1024 * 1024 + 1)
                .unwrap(),
            "unknown" => {
                let mut manifest = preview["manifest"].clone();
                manifest["url"] = json!("https://invalid.example/no-fetch");
                fs::write(
                    directory.join("manifest.json"),
                    serde_json::to_vec(&manifest).unwrap(),
                )
                .unwrap();
            }
            "duplicate" => {
                let bytes = fs::read_to_string(directory.join("manifest.json")).unwrap();
                fs::write(
                    directory.join("manifest.json"),
                    format!("{{\"schema_version\":1,{}", &bytes[1..]),
                )
                .unwrap();
            }
            _ => {}
        }
        let refused = finish(
            &runtime,
            &format!("reject-{case}"),
            json!({"method":"profiles.package_inspect","directory":directory}),
        );
        assert_eq!(refused["receipt"]["status"], "failed", "{case}: {refused}");
        assert_eq!(
            refused["receipt"]["result"]["kind"], "profile_package_issue",
            "{case}: {refused}"
        );
        if ["missing", "unused", "manifest-size", "package-size"].contains(&case) {
            assert_eq!(
                refused["receipt"]["result"]["value"]["reason"],
                match case {
                    "missing" => "missing_object",
                    "unused" => "invalid_object",
                    _ => "resource_limit",
                },
                "{case}: {refused}"
            );
        }
    }
    let symlink_root = parent.join("linked-root");
    symlink(&source, &symlink_root).unwrap();
    assert_eq!(
        finish(
            &runtime,
            "linked-root",
            json!({"method":"profiles.package_inspect","directory":symlink_root})
        )["receipt"]["status"],
        "failed"
    );
    applied(
        &runtime,
        "original-intact",
        json!({"method":"profiles.package_inspect","directory":source}),
    );
    stop(&runtime, host);
}

#[test]
fn import_preserves_noncanonical_original_documents_and_positive_revision() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let parent = root();
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let (_source, preview) = export_saved(
        &runtime,
        &parent,
        "desk",
        &saved,
        "refuse_measurement_bytes",
    );
    let directory = parent.join("noncanonical");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .unwrap();
    let graph_bytes = format!(
        " \n{}\n",
        serde_json::to_string_pretty(&saved["graph"]).unwrap()
    )
    .into_bytes();
    let graph_hash = digest(&graph_bytes);
    let mut profile = saved["profile"].clone();
    profile["revision"] = json!(7);
    profile["id"] = json!("portable");
    profile["graph_sha256"] = json!(graph_hash);
    let profile_bytes =
        format!("\n{}  ", serde_json::to_string_pretty(&profile).unwrap()).into_bytes();
    let profile_hash = digest(&profile_bytes);
    let reference = json!({"id":"portable","revision":7,"profile_sha256":profile_hash});
    let manifest = json!({"schema_version":1,"profile":reference,"objects":[{"kind":"profile","sha256":profile_hash,"bytes":profile_bytes.len()},{"kind":"graph","sha256":graph_hash,"bytes":graph_bytes.len()}]});
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    fs::write(directory.join("manifest.json"), &manifest_bytes).unwrap();
    fs::write(
        directory.join(format!("{profile_hash}.profile.json")),
        &profile_bytes,
    )
    .unwrap();
    fs::write(
        directory.join(format!("{graph_hash}.graph.json")),
        &graph_bytes,
    )
    .unwrap();
    let imported = applied(
        &runtime,
        "import-noncanonical",
        json!({"method":"profiles.import_package","directory":directory,"profile":reference,"expected_manifest_sha256":digest(&manifest_bytes)}),
    );
    assert_eq!(imported["profile"], profile);
    assert_eq!(imported["profile_sha256"], profile_hash);
    assert_eq!(
        fs::read(
            runtime
                .join("profiles")
                .join(format!("{profile_hash}.profile.json"))
        )
        .unwrap(),
        profile_bytes
    );
    assert_eq!(
        fs::read(
            runtime
                .join("profiles")
                .join(format!("{graph_hash}.graph.json"))
        )
        .unwrap(),
        graph_bytes
    );
    let (exported, repreview) = export_saved(
        &runtime,
        &parent,
        "portable",
        &imported,
        "refuse_measurement_bytes",
    );
    assert_ne!(repreview["manifest_sha256"], preview["manifest_sha256"]);
    assert_eq!(
        fs::read(exported.join(format!("{profile_hash}.profile.json"))).unwrap(),
        profile_bytes
    );
    stop(&runtime, host);
}

#[test]
fn owned_correction_sources_roundtrip_through_real_package_consumers() {
    use earph_core::audio::ChannelLayout;
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
    };
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let fresh = support::runtime();
    let owner = EngineOwner::bind(&fresh).unwrap();
    let fresh_host = thread::spawn(move || owner.serve().unwrap());
    let parent = root();
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../earph-media/tests/fixtures/correction");
    for (index, (format, name)) in [
        (CorrectionFormat::AutoEq, "owned-autoeq-shelves.txt"),
        (CorrectionFormat::Rew, "owned-rew.txt"),
        (
            CorrectionFormat::CamillaDsp,
            "owned-camilladsp-shelves.yaml",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let source = fs::read_to_string(fixtures.join(name)).unwrap();
        let report = import_correction_text(
            source.as_bytes(),
            format,
            ImportContext {
                sample_rate_hz: 48000,
                channels: ChannelLayout::Stereo,
                quantum_frames: 64,
            },
            ImportMetadata::default(),
        )
        .unwrap();
        let id = format!("text-{index}");
        let saved = applied(
            &runtime,
            &format!("create-{id}"),
            json!({"method":"profiles.import_correction","id":id,"expected_profile_revision":null,"draft":draft(),"input":{"kind":"text_report","report_json":serde_json::to_string(&report).unwrap(),"source_text":source}}),
        );
        let reference = json!({"id":id,"revision":1,"profile_sha256":saved["profile_sha256"]});
        let preview = applied(
            &runtime,
            &format!("review-{id}"),
            json!({"method":"profiles.package_preview","profile":reference}),
        );
        assert!(preview["measurement_review_required"].as_bool().unwrap());
        let refused_destination = parent.join(format!("refused-{id}"));
        let denied = finish(
            &runtime,
            &format!("deny-{id}"),
            json!({"method":"profiles.export","directory":refused_destination,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"],"export_review":"reviewed_exact_candidate","content_review":"reviewed_no_private_data","redistribution":"refuse_measurement_bytes"}),
        );
        assert_eq!(denied["receipt"]["error"]["code"], "AUTH_DENIED");
        assert!(!refused_destination.exists());
        let (directory, preview) = export_saved(
            &runtime,
            &parent,
            &id,
            &saved,
            "reviewed_required_measurements",
        );
        let imported = applied(
            &fresh,
            &format!("import-{id}"),
            json!({"method":"profiles.import_package","directory":directory,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"]}),
        );
        assert_eq!(imported, saved);
        let source_object = preview["manifest"]["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["kind"] == "text_source")
            .unwrap();
        assert_eq!(
            fs::read(directory.join(format!(
                "{}.text-source.txt",
                source_object["sha256"].as_str().unwrap()
            )))
            .unwrap(),
            source.as_bytes()
        );
    }
    for channels in [1u16, 2u16] {
        let id = format!("fir-{channels}");
        let source = parent.join(format!("{id}.wav"));
        let samples = if channels == 1 {
            vec![0.5_f64, -0.25]
        } else {
            vec![0.5_f64, -0.25, 0.125, 0.0]
        };
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((36 + samples.len() as u32 * 8).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(3u16.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(48000u32.to_le_bytes());
        bytes.extend((48000u32 * u32::from(channels) * 8).to_le_bytes());
        bytes.extend((channels * 8).to_le_bytes());
        bytes.extend(64u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend((samples.len() as u32 * 8).to_le_bytes());
        for sample in samples {
            bytes.extend(sample.to_le_bytes());
        }
        fs::write(&source, &bytes).unwrap();
        let saved = applied(
            &runtime,
            &format!("create-{id}"),
            json!({"method":"profiles.import_correction","id":id,"expected_profile_revision":null,"draft":draft(),"input":{"kind":"fir","path":source,"purpose":"correction","context":{"sample_rate_hz":48000,"channels":if channels==1{"mono"}else{"stereo"},"quantum_frames":64},"metadata_json":"{}","preamp_db":-3.0}}),
        );
        let reference = json!({"id":id,"revision":1,"profile_sha256":saved["profile_sha256"]});
        let (directory, preview) = export_saved(
            &runtime,
            &parent,
            &id,
            &saved,
            "reviewed_required_measurements",
        );
        let imported = applied(
            &fresh,
            &format!("import-{id}"),
            json!({"method":"profiles.import_package","directory":directory,"profile":reference,"expected_manifest_sha256":preview["manifest_sha256"]}),
        );
        assert_eq!(imported, saved);
        let correction = earph_media::decode_fir_wav(&bytes).unwrap();
        assert_eq!(
            fs::read(
                fresh
                    .join("assets")
                    .join(format!("{}.wav", correction.reference.sha256.as_str()))
            )
            .unwrap(),
            bytes
        );
    }
    stop(&runtime, host);
    stop(&fresh, fresh_host);
}

#[test]
fn package_preflights_known_preparation_holdings_before_fir_io() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let directory = root();
    let mut graph = graph();
    for node in graph["graph"]["nodes"].as_array_mut().unwrap() {
        node["format"]["precision"] = json!("float64");
    }
    let port = graph["graph"]["nodes"][0]["format"].clone();
    let wav_hash = "d".repeat(64);
    graph["graph"]["nodes"].as_array_mut().unwrap().push(json!({"id":"fir","kind":{"kind":"convolution","asset":{"sha256":wav_hash,"sample_rate_hz":48000,"channels":"stereo","taps_per_channel":2,"purpose":"correction"}},"format":port}));
    graph["graph"]["edges"] = json!([{"from":"in","to":"fir","from_port":0,"to_port":0},{"from":"fir","to":"out","from_port":0,"to_port":0}]);
    graph["processing"] =
        json!({"kind":"float64","quantization":"round_ties_away_saturate_no_dither"});
    let graph_bytes = serde_json::to_vec(&graph).unwrap();
    let graph_hash = digest(&graph_bytes);
    let mut profile = saved["profile"].clone();
    profile["graph_sha256"] = json!(graph_hash);
    profile["latency"]["max_resident_bytes"] = json!(1);
    profile["latency"]["max_preparation_bytes"] = json!(1);
    let profile_bytes = serde_json::to_vec(&profile).unwrap();
    let profile_hash = digest(&profile_bytes);
    let manifest = json!({"schema_version":1,"profile":{"id":"desk","revision":1,"profile_sha256":profile_hash},"objects":[{"kind":"profile","sha256":profile_hash,"bytes":profile_bytes.len()},{"kind":"graph","sha256":graph_hash,"bytes":graph_bytes.len()},{"kind":"fir_wav","sha256":wav_hash,"bytes":76}]});
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        directory.join(format!("{profile_hash}.profile.json")),
        profile_bytes,
    )
    .unwrap();
    fs::write(
        directory.join(format!("{graph_hash}.graph.json")),
        graph_bytes,
    )
    .unwrap();
    let refused = finish(
        &runtime,
        "preflight",
        json!({"method":"profiles.package_inspect","directory":directory}),
    );
    assert_eq!(
        refused["receipt"]["error"]["code"], "RESOURCE_BUDGET_EXCEEDED",
        "{refused}"
    );
    stop(&runtime, host);
}

struct OwnerProcess {
    child: std::process::Child,
    runtime: PathBuf,
}
impl OwnerProcess {
    fn start(runtime: PathBuf, logs: &Path, name: &str) -> Self {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .stdout(fs::File::create(logs.join(format!("{name}.stdout"))).unwrap())
            .stderr(fs::File::create(logs.join(format!("{name}.stderr"))).unwrap())
            .spawn()
            .unwrap();
        let mut process = Self { child, runtime };
        let deadline = Instant::now() + Duration::from_secs(10);
        let query = json!({"schema_version":1,"request_id":"ready","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}});
        loop {
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "owner exited before readiness; inspect {}",
                logs.display()
            );
            if request(&process.runtime, &serde_json::to_vec(&query).unwrap()).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "owner never became ready");
            thread::sleep(Duration::from_millis(2));
        }
        process
    }
    fn stop(mut self) {
        applied(
            &self.runtime,
            "shutdown-process",
            json!({"method":"owner.shutdown"}),
        );
        assert!(self.child.wait().unwrap().success());
    }
}
impl Drop for OwnerProcess {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn cli_finish(runtime: &Path, id: &str, payload: Value) -> Value {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let c = command(runtime, id, payload);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["request", "--runtime"])
            .arg(runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        writeln!(child.stdin.take().unwrap(), "{c}").unwrap();
        let output = child.wait_with_output().unwrap();
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{result}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if result["receipt"]["status"] != "accepted" {
            assert_eq!(result["receipt"]["status"], "applied", "{result}");
            return result["receipt"]["result"]["value"].clone();
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn actual_owner_and_request_executables_roundtrip_and_restore_packages() {
    let logs = root();
    let source = OwnerProcess::start(support::runtime(), &logs, "source");
    let saved = cli_finish(
        &source.runtime,
        "create",
        json!({"method":"profiles.create","id":"cli-owned","draft":draft(),"graph":graph()}),
    );
    let profile = json!({"id":"cli-owned","revision":1,"profile_sha256":saved["profile_sha256"]});
    let preview = cli_finish(
        &source.runtime,
        "preview",
        json!({"method":"profiles.package_preview","profile":profile}),
    );
    let directory = logs.join("portable");
    cli_finish(
        &source.runtime,
        "export",
        json!({"method":"profiles.export","directory":directory,"profile":profile,"expected_manifest_sha256":preview["manifest_sha256"],"export_review":"reviewed_exact_candidate","content_review":"reviewed_no_private_data","redistribution":"refuse_measurement_bytes"}),
    );
    let target = OwnerProcess::start(support::runtime(), &logs, "target");
    let runtime = target.runtime.clone();
    let inspected = cli_finish(
        &runtime,
        "inspect",
        json!({"method":"profiles.package_inspect","directory":directory}),
    );
    assert_eq!(
        cli_finish(
            &runtime,
            "import",
            json!({"method":"profiles.import_package","directory":directory,"profile":profile,"expected_manifest_sha256":inspected["manifest_sha256"]})
        ),
        saved
    );
    target.stop();
    source.stop();
    let restarted = OwnerProcess::start(runtime, &logs, "restarted");
    assert_eq!(
        cli_finish(
            &restarted.runtime,
            "get",
            json!({"method":"profiles.get","id":"cli-owned"})
        ),
        saved
    );
    let state = state(&restarted.runtime);
    assert_eq!(state["muted"], true);
    assert_eq!(state["output_consented"], false);
    assert!(state["configured_profile"].is_null());
    restarted.stop();
}

#[test]
fn owner_advertises_exact_directory_package_limits_and_required_reviews() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let capabilities = applied(
        &runtime,
        "capabilities",
        json!({"method":"capabilities.get"}),
    );
    assert_eq!(capabilities["profiles"]["packages"], true);
    assert_eq!(
        capabilities["profiles"]["package"],
        json!({"schema_version":1,"format":"directory","max_manifest_bytes":65536,"max_objects":64,"max_total_bytes":33554432,"new_ids_only":true,"preserves_imported_revision":true,"max_correction_chains":1,"requires_export_review":true,"automatic_privacy_proof":false,"automatic_application":false,"archives":false})
    );
    stop(&runtime, host);
}

#[test]
fn valid_hashes_cannot_substitute_for_original_correction_provenance() {
    use earph_core::audio::ChannelLayout;
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
    };
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let text = "Preamp: -6 dB\nFilter 1: ON PK Fc 1000 Hz Gain 0 dB Q 1\n";
    let report = import_correction_text(
        text.as_bytes(),
        CorrectionFormat::AutoEq,
        ImportContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            quantum_frames: 64,
        },
        ImportMetadata::default(),
    )
    .unwrap();
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.import_correction","id":"desk","expected_profile_revision":null,"draft":draft(),"input":{"kind":"text_report","report_json":serde_json::to_string(&report).unwrap(),"source_text":text}}),
    );
    let parent = root();
    let (source, preview) = export_saved(
        &runtime,
        &parent,
        "desk",
        &saved,
        "reviewed_required_measurements",
    );
    let malicious = parent.join("forged");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&malicious)
        .unwrap();
    let changed = text.replace("-6 dB", "-3 dB");
    let source_hash = digest(changed.as_bytes());
    let mut forged_report: Value =
        serde_json::from_str(saved["corrections"][0]["document_json"].as_str().unwrap()).unwrap();
    forged_report["source"]["sha256"] = json!(source_hash);
    forged_report["source"]["bytes"] = json!(changed.len());
    let report_bytes = serde_json::to_vec(&forged_report).unwrap();
    let report_hash = digest(&report_bytes);
    earph_media::correction_text::read_correction_report(&report_bytes).unwrap();
    let mut profile = saved["profile"].clone();
    profile["corrections"][0]["object_sha256"] = json!(report_hash);
    let profile_bytes = serde_json::to_vec(&profile).unwrap();
    let profile_hash = digest(&profile_bytes);
    let reference = json!({"id":"desk","revision":1,"profile_sha256":profile_hash});
    let mut manifest = preview["manifest"].clone();
    manifest["profile"] = reference.clone();
    for object in manifest["objects"].as_array_mut().unwrap() {
        let (bytes, hash, suffix) = match object["kind"].as_str().unwrap() {
            "profile" => (profile_bytes.clone(), profile_hash.clone(), "profile.json"),
            "graph" => {
                let hash = object["sha256"].as_str().unwrap().to_owned();
                let bytes = fs::read(source.join(format!("{hash}.graph.json"))).unwrap();
                (bytes, hash, "graph.json")
            }
            "text_report" => (
                report_bytes.clone(),
                report_hash.clone(),
                "text-report.json",
            ),
            "text_source" => (
                changed.as_bytes().to_vec(),
                source_hash.clone(),
                "text-source.txt",
            ),
            _ => panic!("unexpected type"),
        };
        object["sha256"] = json!(hash);
        object["bytes"] = json!(bytes.len());
        fs::write(malicious.join(format!("{hash}.{suffix}")), bytes).unwrap();
    }
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(malicious.join("manifest.json"), &manifest_bytes).unwrap();
    let declared = earph_core::profile_packages::read_package_manifest(&manifest_bytes).unwrap();
    declared
        .validate_profile_references(&earph_core::profiles::read_profile(&profile_bytes).unwrap())
        .unwrap();
    for object in &declared.objects {
        assert_eq!(
            digest(&fs::read(malicious.join(object.file_name())).unwrap()),
            object.sha256.as_str()
        );
    }
    let fresh = support::runtime();
    let owner = EngineOwner::bind(&fresh).unwrap();
    let fresh_host = thread::spawn(move || owner.serve().unwrap());
    let refused = finish(
        &fresh,
        "reject",
        json!({"method":"profiles.import_package","directory":malicious,"profile":reference,"expected_manifest_sha256":digest(&manifest_bytes)}),
    );
    assert_eq!(refused["receipt"]["status"], "failed", "{refused}");
    assert_eq!(refused["receipt"]["error"]["code"], "ASSET_INVALID");
    assert!(!fresh.join("profiles").exists());
    assert!(!fresh.join("assets").exists());
    stop(&runtime, host);
    stop(&fresh, fresh_host);
}
#[test]
fn package_import_refuses_source_overlap_with_owner_storage() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let parent = root();
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let (source, preview) = export_saved(
        &runtime,
        &parent,
        "desk",
        &saved,
        "refuse_measurement_bytes",
    );
    let fresh = support::runtime();
    let owner = EngineOwner::bind(&fresh).unwrap();
    let fresh_host = thread::spawn(move || owner.serve().unwrap());
    let directory = fresh.join("incoming-package");
    copy_package(&source, &directory, None);
    let refused = finish(
        &fresh,
        "refuse-overlap",
        json!({"method":"profiles.import_package","directory":directory,"profile":preview["manifest"]["profile"],"expected_manifest_sha256":preview["manifest_sha256"]}),
    );
    assert_eq!(refused["receipt"]["status"], "failed", "{refused}");
    assert_eq!(refused["receipt"]["error"]["code"], "ASSET_INVALID");
    assert!(!fresh.join("profiles").exists());
    assert_eq!(
        finish(
            &fresh,
            "refuse-inspection",
            json!({"method":"profiles.package_inspect","directory":directory})
        )["receipt"]["status"],
        "failed"
    );
    stop(&runtime, host);
    stop(&fresh, fresh_host);
}

#[test]
fn declared_large_and_cumulative_fir_banks_preflight_before_missing_wav() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let saved = applied(
        &runtime,
        "create",
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let published = || {
        let mut objects = fs::read_dir(runtime.join("profiles"))
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect::<Vec<_>>();
        objects.sort_by(|a, b| a.0.cmp(&b.0));
        objects
    };
    let before_objects = published();
    for (case, hashes, budget, expected) in [
        ("large", vec!['d'], 4_198_400u64, "RESOURCE_BUDGET_EXCEEDED"),
        (
            "cumulative",
            vec!['d', 'e'],
            25 * 1024 * 1024,
            "RESOURCE_BUDGET_EXCEEDED",
        ),
        ("shared", vec!['d', 'd'], 22 * 1024 * 1024, "ASSET_INVALID"),
    ] {
        let directory = root();
        let mut graph = graph();
        for node in graph["graph"]["nodes"].as_array_mut().unwrap() {
            node["format"]["precision"] = json!("float64");
        }
        let port = graph["graph"]["nodes"][0]["format"].clone();
        let mut edges = Vec::new();
        let mut prior = "in".to_owned();
        let mut objects = Vec::new();
        for (index, digit) in hashes.iter().enumerate() {
            let hash = digit.to_string().repeat(64);
            let id = format!("fir-{index}");
            graph["graph"]["nodes"].as_array_mut().unwrap().push(json!({"id":id,"kind":{"kind":"convolution","asset":{"sha256":hash,"sample_rate_hz":48000,"channels":"stereo","taps_per_channel":262144,"purpose":"correction"}},"format":port}));
            edges.push(json!({"from":prior,"to":id,"from_port":0,"to_port":0}));
            prior = id;
            if !hashes[..index].contains(digit) {
                objects.push(json!({"kind":"fir_wav","sha256":hash,"bytes":44+262144*2*8}));
            }
        }
        edges.push(json!({"from":prior,"to":"out","from_port":0,"to_port":0}));
        graph["graph"]["edges"] = json!(edges);
        graph["processing"] =
            json!({"kind":"float64","quantization":"round_ties_away_saturate_no_dither"});
        let graph_bytes = serde_json::to_vec(&graph).unwrap();
        let graph_hash = digest(&graph_bytes);
        let mut profile = saved["profile"].clone();
        profile["id"] = json!(case);
        profile["graph_sha256"] = json!(graph_hash);
        profile["latency"]["max_resident_bytes"] = json!(budget);
        profile["latency"]["max_preparation_bytes"] = json!(budget);
        let profile_bytes = serde_json::to_vec(&profile).unwrap();
        let profile_hash = digest(&profile_bytes);
        assert!(
            (profile_bytes.len() + graph_bytes.len()) as u64 + 4 * 1024 * 1024 < budget,
            "fixture must pass the document-only check"
        );
        objects.push(json!({"kind":"profile","sha256":profile_hash,"bytes":profile_bytes.len()}));
        objects.push(json!({"kind":"graph","sha256":graph_hash,"bytes":graph_bytes.len()}));
        let manifest = json!({"schema_version":1,"profile":{"id":case,"revision":1,"profile_sha256":profile_hash},"objects":objects});
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(directory.join("manifest.json"), &manifest_bytes).unwrap();
        fs::write(
            directory.join(format!("{profile_hash}.profile.json")),
            profile_bytes,
        )
        .unwrap();
        fs::write(
            directory.join(format!("{graph_hash}.graph.json")),
            graph_bytes,
        )
        .unwrap();
        let refused = finish(
            &runtime,
            case,
            json!({"method":"profiles.import_package","directory":directory,"profile":manifest["profile"],"expected_manifest_sha256":digest(&manifest_bytes)}),
        );
        assert_eq!(
            refused["receipt"]["error"]["code"], expected,
            "{case}: {refused}"
        );
        assert_eq!(
            published(),
            before_objects,
            "{case}: refusal published candidate objects"
        );
        assert!(!runtime.join("assets").exists());
        if case == "shared" {
            assert_eq!(
                refused["receipt"]["result"]["value"]["reason"],
                "missing_object"
            );
        }
    }
    stop(&runtime, host);
}
