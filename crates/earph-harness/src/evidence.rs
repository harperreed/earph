// ABOUTME: Executes the offline qualification matrix and records actual provenance.
// ABOUTME: Verifies artifact hashes and outcomes before accepting a passing record.
use crate::{atomic_json, fixture::*, read_bounded, render::*, sha256};
use earph_core::{ErrorCode, audio::MemoryBudget, ids::Epochs};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    env, fs,
    path::{Component, Path},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

pub type EvidenceRecord = Value;
pub type EvidenceError = String;

fn now() -> Result<u128, String> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock precedes Unix epoch")?
        .as_nanos())
}
fn command_output(program: &str, args: &[&str], directory: &Path) -> Result<String, String> {
    let result = Command::new(program)
        .args(args)
        .current_dir(directory)
        .output()
        .map_err(|_| "cannot inspect build provenance")?;
    if !result.status.success() {
        return Err("build provenance command failed".into());
    }
    Ok(String::from_utf8(result.stdout)
        .map_err(|_| "non-UTF8 provenance")?
        .trim()
        .to_owned())
}
fn save_report(root: &Path, stem: &str, report: &RenderReport) -> Result<(String, String), String> {
    let report_path = format!("faults/{stem}.json");
    let capture_path = format!("faults/{stem}.pcm");
    fs::write(root.join(&capture_path), report.capture_bytes())
        .map_err(|_| "cannot write fault capture")?;
    atomic_json(
        &root.join(&report_path),
        &serde_json::to_value(report).map_err(|_| "cannot serialize fault report")?,
    )?;
    Ok((report_path, capture_path))
}
fn actual_report(result: Result<RenderReport, RenderFailure>) -> RenderReport {
    match result {
        Ok(report) => report,
        Err(failure) => *failure.report,
    }
}

fn matrix(root: &Path, bundle: &FixtureBundle) -> Result<(Vec<Value>, Vec<Value>), String> {
    let mut clean = Vec::new();
    for fixture in &bundle.fixtures {
        let fixture_path = root
            .join("fixtures")
            .join(fixture.payload_file.replace(".pcm", ".json"));
        for block_frames in [1, 64, 256, 1024] {
            let (_, input) = load_fixture(&fixture_path)?;
            let expected: Vec<i32> = (0..fixture
                .spec
                .format
                .scalar_count(fixture.spec.frames)
                .map_err(|e| e.to_string())?)
                .map(|i| input.integer_sample(i).expect("validated integer payload") as i32)
                .collect();
            let request = RenderRequest {
                fixture: fixture.spec.clone(),
                block_frames,
                budget: MemoryBudget::default(),
                epochs: Epochs::default(),
                faults: FaultSchedule::default(),
            };
            let report = actual_report(render_pcm(request, input));
            let sample_differences = expected
                .iter()
                .zip(&report.capture)
                .filter(|(a, b)| a != b)
                .count();
            let count_difference = expected.len().abs_diff(report.capture.len());
            let pass = report.status == "pass"
                && sample_differences == 0
                && count_difference == 0
                && report.output_frames == fixture.spec.frames
                && report.payload_sha256 == fixture.payload_sha256;
            clean.push(json!({"fixture_file":fixture.payload_file,"fixture_sha256":fixture.payload_sha256,"block_frames":block_frames,"expected_frames":fixture.spec.frames,"observed_frames":report.output_frames,"sample_differences":sample_differences,"sample_count_difference":count_difference,"capture_sha256":report.payload_sha256,"pass":pass}));
        }
    }
    fs::create_dir(root.join("faults")).map_err(|_| "cannot create fault artifact directory")?;
    let fixture_path = root.join("fixtures/signed16-mono-44100-boundary.json");
    let source_bytes = read_bounded(
        &root.join("fixtures/signed16-mono-44100-boundary.pcm"),
        4096,
    )?;
    let mut faults = Vec::new();
    for at_frame in [0, 64, 1030] {
        for (index, fault) in [
            FaultKind::ShortWrite { max_frames: 3 },
            FaultKind::ZeroProgress,
            FaultKind::Underrun,
            FaultKind::BackendFailure,
            FaultKind::MalformedChunk,
            FaultKind::StaleEpoch,
        ]
        .into_iter()
        .enumerate()
        {
            let mut reports = Vec::new();
            for repeat in 0..2 {
                let (fixture, input) = load_fixture(&fixture_path)?;
                let request = RenderRequest {
                    fixture: fixture.spec,
                    block_frames: 256,
                    budget: MemoryBudget::default(),
                    epochs: Epochs::default(),
                    faults: FaultSchedule {
                        schema_version: 1,
                        faults: vec![ScheduledFault { at_frame, fault }],
                    },
                };
                let report = actual_report(render_pcm(request, input));
                let (report_path, capture_path) =
                    save_report(root, &format!("{at_frame}-{index}-{repeat}"), &report)?;
                reports.push((report, report_path, capture_path));
            }
            let (a, first_report, first_capture) = &reports[0];
            let (b, second_report, second_capture) = &reports[1];
            let (expected_frames, expected_error) = fault_expectation(fault, at_frame);
            let repeatable = a.capture == b.capture && a.events == b.events && a.error == b.error;
            let pass = repeatable
                && a.output_frames == expected_frames
                && a.error.map(|e| e.code) == expected_error
                && a.capture_bytes() == source_bytes[..expected_frames as usize * 2];
            faults.push(json!({"at_frame":at_frame,"fault":fault,"first_report":first_report,"second_report":second_report,"first_capture":first_capture,"second_capture":second_capture,"source_prefix_sha256":sha256(&source_bytes[..expected_frames as usize*2]),"repeatable":repeatable,"pass":pass}));
        }
    }
    Ok((clean, faults))
}

fn fault_expectation(fault: FaultKind, at_frame: u64) -> (u64, Option<ErrorCode>) {
    match fault {
        FaultKind::ShortWrite { .. } => (1031, None),
        FaultKind::MalformedChunk => (at_frame, Some(ErrorCode::AssetInvalid)),
        FaultKind::StaleEpoch => (at_frame, Some(ErrorCode::StaleSession)),
        _ => (at_frame, Some(ErrorCode::OutputFault)),
    }
}
fn collect_artifacts(
    root: &Path,
    directory: &Path,
    depth: usize,
    result: &mut Vec<Value>,
) -> Result<(), String> {
    if depth > 4 || result.len() > 512 {
        return Err("artifact collection limit exceeded".into());
    }
    for entry in fs::read_dir(directory).map_err(|_| "cannot enumerate artifacts")? {
        let entry = entry.map_err(|_| "cannot inspect artifact entry")?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|_| "cannot inspect artifact type")?;
        if kind.is_dir() {
            collect_artifacts(root, &path, depth + 1, result)?;
        } else if kind.is_file() {
            let bytes = read_bounded(&path, 2 * 1024 * 1024)?;
            let relative = path
                .strip_prefix(root)
                .map_err(|_| "artifact escaped root")?
                .to_str()
                .ok_or("non-UTF8 artifact path")?;
            result.push(json!({"path":relative,"sha256":sha256(&bytes),"bytes":bytes.len()}));
        } else {
            return Err("artifact is not a regular file".into());
        }
    }
    Ok(())
}
fn recorded_command() -> Vec<String> {
    let mut arguments = env::args();
    let executable = arguments.next().unwrap_or_default();
    let mut result = vec![
        Path::new(&executable)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    ];
    let mut redact = false;
    for argument in arguments {
        result.push(if redact {
            "<output-directory>".into()
        } else if Path::new(&argument).is_absolute() {
            "<redacted-path>".into()
        } else {
            argument.clone()
        });
        redact = argument == "--output";
    }
    result
}

pub fn qualify(output: &Path) -> Result<EvidenceRecord, EvidenceError> {
    let started = now()?;
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let build_provenance = crate::build_provenance();
    let commit = string(&build_provenance, "commit")?;
    let dirty = build_provenance["source_dirty"]
        .as_bool()
        .ok_or("build source state unavailable")?;
    let execution_checkout = json!({"commit":command_output("git",&["rev-parse","HEAD"],&repo).ok(),"source_dirty":command_output("git",&["status","--porcelain"],&repo).ok().map(|s|!s.is_empty())});
    let build_id = sha256(&read_bounded(
        &env::current_exe().map_err(|_| "cannot locate running executable")?,
        128 * 1024 * 1024,
    )?);
    let dependency_lock_sha256 = string(&build_provenance, "dependency_lock_sha256")?;
    fs::create_dir(output).map_err(|_| "qualification destination must be a new directory")?;
    let bundle = write_bundle(&output.join("fixtures"))?;
    if read_bounded(&output.join("fixtures/manifest.json"), 2 * 1024 * 1024)?
        != include_bytes!("../../../fixtures/audio/manifest.json")
    {
        return Err("generated fixture manifest differs from committed baseline".into());
    }
    let (clean, faults) = matrix(output, &bundle)?;
    atomic_json(&output.join("render-results.json"), &json!(clean))?;
    atomic_json(&output.join("fault-results.json"), &json!(faults))?;
    let pass = clean.iter().chain(&faults).all(|row| row["pass"] == true);
    let mut artifacts = Vec::new();
    collect_artifacts(output, output, 0, &mut artifacts)?;
    artifacts.sort_by_key(|a| a["path"].as_str().unwrap_or_default().to_owned());
    let fixture_hashes: Vec<_> = bundle
        .fixtures
        .iter()
        .map(|f| f.payload_sha256.clone())
        .collect();
    let record = json!({
        "schema_version":1,"evidence_id":format!("foundation-{started}"),"status":if pass{"pass"}else{"fail"},
        "requirement_ids":["ARCH-06"],"acceptance_ids":["AC-06"],"commit":commit,"source_dirty":dirty,"build_id":build_id,"dependency_lock_sha256":dependency_lock_sha256,"build_provenance":build_provenance,"execution_checkout":execution_checkout,
        "environment":{"os":env::consts::OS,"os_version":command_output("uname",&["-r"],&repo)?,"architecture":env::consts::ARCH,"backend":"virtual","backend_version":env!("CARGO_PKG_VERSION"),"device_identity":"virtual-output-v1","firmware_or_driver":null,"measurement_boundary":"engine_output"},
        "fixture_sha256":fixture_hashes,"graph_sha256":null,"harness_version":env!("CARGO_PKG_VERSION"),"command":recorded_command(),
        "reproduce_command":["cargo","run","--locked","-p","earph-harness","--","qualify","--output","<new-directory>"],
        "started_at":started.to_string(),"finished_at":now()?.to_string(),"timestamp_unit":"unix_nanoseconds",
        "expected_result":"288 exact clean renders; 18 fault cases, each repeated twice; scoped engine-output evidence",
        "observed_result":{"clean_renders":clean.len(),"fault_cases":faults.len(),"fault_repeats":2,"fixtures":bundle.fixtures.len()},
        "artifacts":artifacts,"limitations":["Virtual output only; physical devices, macOS, protocols and DSP remain unqualified.","Graph hash is null because no DSP graph exists.","Recorded argv redacts local output paths; other arguments and build identity remain exact.","This matrix does not replace the separate canonical unit, integration, CLI and allocation checks."],"reviewer":null
    });
    publish_record(output, record)
}

fn publish_record(
    output: &Path,
    mut record: EvidenceRecord,
) -> Result<EvidenceRecord, EvidenceError> {
    let validation = validate(&record, output);
    if validation.is_err() {
        record["status"] = json!("fail");
    }
    atomic_json(&output.join("evidence.json"), &record)?;
    validation?;
    Ok(record)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing evidence field: {key}"))
}
fn hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn artifact_bytes(root: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let path = Path::new(relative);
    if path.components().next().is_none()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("invalid relative artifact path".into());
    }
    let canonical = root
        .join(path)
        .canonicalize()
        .map_err(|_| "missing artifact")?;
    if !canonical.starts_with(root.canonicalize().map_err(|_| "missing artifact root")?)
        || !canonical.is_file()
    {
        return Err("artifact escaped root or is not a file".into());
    }
    read_bounded(&canonical, 2 * 1024 * 1024)
}
fn artifact_json(root: &Path, relative: &str) -> Result<Value, String> {
    serde_json::from_slice(&artifact_bytes(root, relative)?)
        .map_err(|_| "invalid artifact JSON".into())
}

pub fn validate(record: &EvidenceRecord, root: &Path) -> Result<(), EvidenceError> {
    for key in [
        "schema_version",
        "evidence_id",
        "status",
        "requirement_ids",
        "acceptance_ids",
        "commit",
        "build_id",
        "dependency_lock_sha256",
        "environment",
        "fixture_sha256",
        "graph_sha256",
        "harness_version",
        "command",
        "started_at",
        "finished_at",
        "expected_result",
        "observed_result",
        "artifacts",
        "limitations",
        "reviewer",
    ] {
        if record.get(key).is_none() {
            return Err(format!("missing evidence field: {key}"));
        }
    }
    if record["schema_version"] != 1 || record["status"] != "pass" {
        return Err("evidence is not a supported passing record".into());
    }
    if record["requirement_ids"] != json!(["ARCH-06"])
        || record["acceptance_ids"] != json!(["AC-06"])
    {
        return Err("offline fault matrix cannot qualify other requirements or hardware".into());
    }
    for key in ["evidence_id", "harness_version", "expected_result"] {
        string(record, key)?;
    }
    let commit = string(record, "commit")?;
    if ![40, 64].contains(&commit.len()) || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid source commit".into());
    }
    for key in ["build_id", "dependency_lock_sha256"] {
        if !hash(string(record, key)?) {
            return Err("invalid provenance hash".into());
        }
    }
    let build = &record["build_provenance"];
    if build["schema_version"] != 1
        || build["commit"] != record["commit"]
        || build["source_dirty"] != record["source_dirty"]
        || build["dependency_lock_sha256"] != record["dependency_lock_sha256"]
        || !hash(string(build, "source_inputs_sha256")?)
    {
        return Err("build provenance does not match the record".into());
    }
    for field in ["rustc", "target", "profile"] {
        string(build, field)?;
    }
    if !record["source_dirty"].is_boolean()
        || record["command"].as_array().is_none_or(Vec::is_empty)
    {
        return Err("missing build state or invocation".into());
    }
    let start = string(record, "started_at")?
        .parse::<u128>()
        .map_err(|_| "invalid start timestamp")?;
    let finish = string(record, "finished_at")?
        .parse::<u128>()
        .map_err(|_| "invalid finish timestamp")?;
    if record["timestamp_unit"] != "unix_nanoseconds" || finish < start {
        return Err("invalid evidence time range".into());
    }
    for key in [
        "os",
        "os_version",
        "architecture",
        "backend_version",
        "device_identity",
    ] {
        string(&record["environment"], key)?;
    }
    if record["environment"]["backend"] != "virtual"
        || record["environment"]["measurement_boundary"] != "engine_output"
        || !record["graph_sha256"].is_null()
    {
        return Err("unsupported verification boundary".into());
    }
    if record["limitations"].as_array().is_none_or(Vec::is_empty) {
        return Err("missing verification limitations".into());
    }
    let catalog: Value = serde_json::from_str(include_str!("../../../docs/requirements.json"))
        .map_err(|_| "invalid embedded catalog")?;
    for (key, collection) in [
        ("requirement_ids", "requirements"),
        ("acceptance_ids", "acceptance_scenarios"),
    ] {
        let ids = record[key]
            .as_array()
            .filter(|ids| !ids.is_empty())
            .ok_or("missing evidence coverage")?;
        for id in ids {
            if !catalog[collection]
                .as_array()
                .ok_or("invalid catalog")?
                .iter()
                .any(|row| row["id"] == *id)
            {
                return Err("unknown evidence coverage ID".into());
            }
        }
    }
    if record["observed_result"]
        != json!({"clean_renders":288,"fault_cases":18,"fault_repeats":2,"fixtures":72})
    {
        return Err("incomplete qualification matrix".into());
    }
    let artifacts = record["artifacts"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 512)
        .ok_or("missing or excessive artifacts")?;
    let mut names = BTreeSet::new();
    let mut total = 0usize;
    for artifact in artifacts {
        let name = string(artifact, "path")?;
        if !names.insert(name) {
            return Err("duplicate artifact path".into());
        }
        let bytes = artifact_bytes(root, name)?;
        total = total
            .checked_add(bytes.len())
            .ok_or("artifact size overflow")?;
        if total > 64 * 1024 * 1024
            || artifact["sha256"] != sha256(&bytes)
            || artifact["bytes"] != bytes.len()
        {
            return Err("artifact size or hash mismatch".into());
        }
    }
    for path in [
        "render-results.json",
        "fault-results.json",
        "fixtures/manifest.json",
    ] {
        if !names.contains(path) {
            return Err("missing required artifact".into());
        }
    }
    let bundle: FixtureBundle =
        serde_json::from_value(artifact_json(root, "fixtures/manifest.json")?)
            .map_err(|_| "invalid fixture bundle")?;
    if artifact_bytes(root, "fixtures/manifest.json")?
        != include_bytes!("../../../fixtures/audio/manifest.json")
    {
        return Err("fixture bundle differs from qualified baseline".into());
    }
    if bundle.schema_version != 1 || bundle.fixtures.len() != 72 {
        return Err("incomplete fixture bundle".into());
    }
    let hashes: Vec<_> = bundle
        .fixtures
        .iter()
        .map(|f| f.payload_sha256.clone())
        .collect();
    if record["fixture_sha256"] != json!(hashes) {
        return Err("fixture hash list mismatch".into());
    }
    for fixture in &bundle.fixtures {
        let payload = format!("fixtures/{}", fixture.payload_file);
        let manifest = payload.replace(".pcm", ".json");
        if !names.contains(payload.as_str())
            || !names.contains(manifest.as_str())
            || sha256(&artifact_bytes(root, &payload)?) != fixture.payload_sha256
        {
            return Err("fixture payload is missing or differs from its manifest".into());
        }
    }
    let clean = artifact_json(root, "render-results.json")?;
    let rows = clean
        .as_array()
        .filter(|r| r.len() == 288)
        .ok_or("missing clean runs")?;
    let mut cases = BTreeSet::new();
    for row in rows {
        let fixture_name = string(row, "fixture_file")?;
        let fixture = bundle
            .fixtures
            .iter()
            .find(|f| f.payload_file == fixture_name)
            .ok_or("unknown matrix fixture")?;
        let size = row["block_frames"]
            .as_u64()
            .filter(|s| [1, 64, 256, 1024].contains(s))
            .ok_or("invalid matrix block size")?;
        if !cases.insert((fixture_name, size))
            || row["pass"] != true
            || row["sample_differences"] != 0
            || row["sample_count_difference"] != 0
            || row["expected_frames"] != fixture.spec.frames
            || row["observed_frames"] != fixture.spec.frames
            || row["capture_sha256"] != fixture.payload_sha256
            || row["fixture_sha256"] != fixture.payload_sha256
        {
            return Err("failed or duplicated clean render".into());
        }
    }
    let faults = artifact_json(root, "fault-results.json")?;
    let source_bytes = artifact_bytes(root, "fixtures/signed16-mono-44100-boundary.pcm")?;
    let mut cases = BTreeSet::new();
    for row in faults
        .as_array()
        .filter(|rows| rows.len() == 18)
        .ok_or("missing fault runs")?
    {
        let fault: FaultKind =
            serde_json::from_value(row["fault"].clone()).map_err(|_| "invalid recorded fault")?;
        if matches!(fault,FaultKind::ShortWrite{max_frames} if max_frames!=3) {
            return Err("unqualified short-write configuration".into());
        }
        let at = row["at_frame"]
            .as_u64()
            .filter(|at| [0, 64, 1030].contains(at))
            .ok_or("invalid fault frame")?;
        if !cases.insert((at, row["fault"].to_string()))
            || row["pass"] != true
            || row["repeatable"] != true
        {
            return Err("failed or repeated fault case".into());
        }
        let (frames, error) = fault_expectation(fault, at);
        let expected_prefix = source_bytes
            .get(..frames as usize * 2)
            .ok_or("source prefix is truncated")?;
        if row["source_prefix_sha256"] != sha256(expected_prefix) {
            return Err("fault source prefix hash mismatch".into());
        }
        let mut previous = None;
        for (report_key, capture_key) in [
            ("first_report", "first_capture"),
            ("second_report", "second_capture"),
        ] {
            let report_path = string(row, report_key)?;
            let capture_path = string(row, capture_key)?;
            if !names.contains(report_path) || !names.contains(capture_path) {
                return Err("untracked fault artifact".into());
            }
            let report = artifact_json(root, report_path)?;
            let bytes = artifact_bytes(root, capture_path)?;
            let expected_status = if error.is_none() { "pass" } else { "fail" };
            if report["status"] != expected_status
                || report["output_frames"] != frames
                || report["error"]["code"] != json!(error)
                || report["payload_sha256"] != sha256(&bytes)
                || bytes.len() != frames as usize * 2
                || bytes != expected_prefix
            {
                return Err("fault outcome differs from its expected contract".into());
            }
            let injected: Vec<_> = report["events"]
                .as_array()
                .ok_or("missing fault events")?
                .iter()
                .filter(|event| event["event"]["kind"] == "fault")
                .collect();
            if injected.len() != 1
                || injected[0]["at_frame"] != at
                || injected[0]["event"]["fault"] != row["fault"]
            {
                return Err("reported fault differs from scheduled fault".into());
            }
            if let Some(prior) = &previous
                && prior != &report
            {
                return Err("fault event sequence differs on replay".into());
            }
            previous = Some(report);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejected_validation_cannot_leave_a_passing_final_record() {
        let directory = crate::test_support::TestDir::new();
        let record = json!({"schema_version":1,"status":"pass"});
        assert!(publish_record(&directory.0, record).is_err());
        let stored: Value =
            serde_json::from_slice(&fs::read(directory.0.join("evidence.json")).unwrap()).unwrap();
        assert_eq!(stored["status"], "fail");
    }
}
