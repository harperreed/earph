// ABOUTME: Rechecks portable ALSA file-plugin evidence against its raw artifacts.
// ABOUTME: Rejects incomplete matrices and claims beyond the measured plugin boundary.
use crate::{OpenRequest, Route};
use earph_harness::{
    fixture::{self, FixtureBundle},
    read_bounded, sha256,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

fn artifact(root: &Path, relative: &str, limit: usize) -> Result<Vec<u8>, String> {
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        let Component::Normal(name) = component else {
            return Err("artifact path must stay inside the matrix directory".into());
        };
        path.push(name);
        if std::fs::symlink_metadata(&path)
            .map_err(|_| "missing matrix artifact")?
            .file_type()
            .is_symlink()
        {
            return Err("matrix artifacts cannot use symlinks".into());
        }
    }
    read_bounded(&path, limit)
}
fn document(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|_| "invalid matrix artifact JSON".into())
}
fn require(condition: bool, message: &'static str) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
fn digest(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

pub fn validate_matrix(path: &Path) -> Result<Value, String> {
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let matrix = document(&read_bounded(path, 2 * 1024 * 1024)?)?;
    require(
        matrix["schema_version"] == 1
            && matrix["status"] == "pass"
            && matrix["measurement_boundary"] == "alsa_file_plugin"
            && matrix["physical_output"] == "not_run"
            && matrix["pipewire_coexistence"] == "not_run"
            && matrix["fixture_count"] == 72
            && matrix["case_count"] == 216,
        "matrix has an unsupported schema, outcome, scope, or count",
    )?;
    let rows = matrix["results"]
        .as_array()
        .ok_or("missing matrix results")?;
    require(rows.len() == 216, "matrix must contain 216 cases")?;
    let mut names = BTreeSet::new();
    for row in rows {
        let name = row["case"].as_str().ok_or("missing matrix case path")?;
        require(names.insert(name), "duplicate matrix case")?;
        require(
            Path::new(name)
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "invalid matrix case path",
        )?;
    }
    let bundle: FixtureBundle =
        serde_json::from_slice(&artifact(root, "fixtures/manifest.json", 2 * 1024 * 1024)?)
            .map_err(|_| "invalid fixture bundle")?;
    require(
        bundle.schema_version == 1 && bundle.fixtures.len() == 72,
        "matrix must contain 72 versioned fixtures",
    )?;
    let expected = fixture::qualification_specs();
    let mut specifications = BTreeSet::new();
    let mut build: Option<Value> = None;
    let mut environment: Option<Value> = None;
    for item in &bundle.fixtures {
        require(
            expected.contains(&item.spec),
            "fixture is outside the qualification matrix",
        )?;
        require(
            specifications.insert(
                serde_json::to_string(&item.spec).map_err(|_| "cannot inspect fixture spec")?,
            ),
            "duplicate fixture spec",
        )?;
        let payload = artifact(
            root,
            &format!("fixtures/{}", item.payload_file),
            1024 * 1024,
        )?;
        require(
            sha256(&payload) == item.payload_sha256,
            "fixture payload hash differs",
        )?;
        let payload_name = Path::new(&item.payload_file);
        require(
            payload_name.components().count() == 1,
            "fixture payload must be a local filename",
        )?;
        let stem = payload_name
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or("invalid fixture filename")?;
        let fixture_path = format!("fixtures/{stem}.json");
        let manifest_bytes = artifact(root, &fixture_path, 64 * 1024)?;
        let manifest: fixture::FixtureFile =
            serde_json::from_slice(&manifest_bytes).map_err(|_| "invalid fixture manifest")?;
        require(
            serde_json::to_value(&manifest).map_err(|_| "cannot inspect fixture")?
                == serde_json::to_value(item).map_err(|_| "cannot inspect bundle")?,
            "fixture manifest differs from bundle",
        )?;
        fixture::load_fixture(&root.join(&fixture_path))?;
        for period in [1, 64, 256] {
            let case = format!("cases/{stem}-{period}");
            let row = rows
                .iter()
                .find(|row| row["case"] == case)
                .ok_or("missing required ALSA case")?;
            require(
                row["status"] == "pass" && row["fixture"] == fixture_path,
                "case outcome or fixture differs",
            )?;
            let request_bytes = artifact(root, &format!("{case}/request.json"), 64 * 1024)?;
            let request: OpenRequest =
                serde_json::from_slice(&request_bytes).map_err(|_| "invalid case request")?;
            request
                .validate()
                .map_err(|_| "invalid case negotiation request")?;
            require(
                request.device == "capture"
                    && request.route == Route::System
                    && request.format == item.spec.format
                    && request.period_frames == period
                    && request.buffer_frames == period * 4,
                "case request differs from matrix",
            )?;
            let capture = artifact(root, &format!("{case}/capture.pcm"), 1024 * 1024)?;
            require(
                capture == payload
                    && row["capture_sha256"] == sha256(&capture)
                    && row["expected_payload_sha256"] == item.payload_sha256,
                "capture does not preserve the source payload",
            )?;
            let config = artifact(root, &format!("{case}/alsa.conf"), 4096)?;
            let report_bytes = artifact(root, &format!("{case}/report.json"), 128 * 1024)?;
            require(
                row["config_sha256"] == sha256(&config)
                    && row["request_sha256"] == sha256(&request_bytes)
                    && row["report_sha256"] == sha256(&report_bytes),
                "matrix artifact hash differs",
            )?;
            let report = document(&report_bytes)?;
            let observed = &report["report"]["observed"];
            let alsa_format = match item.spec.format.representation() {
                earph_core::audio::PcmRepresentation::Signed16 => "S16_LE",
                earph_core::audio::PcmRepresentation::Signed24 => "S24_3LE",
                earph_core::audio::PcmRepresentation::Signed32 => "S32_LE",
                earph_core::audio::PcmRepresentation::Float64 => {
                    return Err("float output is outside this matrix".into());
                }
            };
            require(
                observed["alsa_format"] == alsa_format
                    && observed["access"] == "RW_INTERLEAVED"
                    && observed["period_frames"] == period
                    && observed["buffer_frames"] == period * 4
                    && observed["significant_bits"]
                        == item.spec.format.representation().bytes() * 8
                    && observed["rate_numerator"]
                        .as_u64()
                        .zip(observed["rate_denominator"].as_u64())
                        .is_some_and(|(numerator, denominator)| {
                            denominator > 0
                                && u64::from(item.spec.format.sample_rate_hz())
                                    .checked_mul(denominator)
                                    == Some(numerator)
                        }),
                "installed parameters differ from the exact matrix request",
            )?;
            require(
                report["status"] == "pass"
                    && report["closed"] == true
                    && report["command"] == "exercise"
                    && report["report"]["requested"] == document(&request_bytes)?
                    && report["report"]["observed"]["format"]
                        == serde_json::to_value(item.spec.format)
                            .map_err(|_| "cannot inspect format")?
                    && report["report"]["pcm_type"] == "FILE"
                    && report["report"]["evidence_level"] == "opened"
                    && report["report"]["payload_preservation"] == "unverified"
                    && report["report"]["downstream_transformations"] == "unknown"
                    && report["exercise"]["evidence_level"] == "exercised"
                    && report["exercise"]["submitted_frames"] == item.spec.frames
                    && report["exercise"]["estimated_consumed_frames"] == item.spec.frames
                    && report["exercise"]["backend_delay_frames"] == 0
                    && report["exercise"]["measurement_boundary"] == "alsa_client_estimate"
                    && report["exercise"]["physical_output"] == "unverified"
                    && report["exercise"]["fixture_payload_sha256"] == item.payload_sha256,
                "report claims differ from the measured plugin scope",
            )?;
            let provenance = &report["build"];
            require(
                provenance["commit"].as_str().is_some_and(|value| {
                    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) && provenance["source_dirty"].is_boolean()
                    && digest(&provenance["source_inputs_sha256"])
                    && digest(&provenance["dependency_lock_sha256"])
                    && provenance["rustc"].is_string()
                    && provenance["target"].is_string()
                    && provenance["profile"].is_string(),
                "missing build provenance",
            )?;
            require(
                report["environment"]["alsa_version"].is_string()
                    && report["environment"]["os"] == "linux"
                    && report["environment"]["kernel"].is_string()
                    && report["environment"]["architecture"].is_string(),
                "missing runtime environment",
            )?;
            if let Some(first) = &build {
                require(first == provenance, "matrix mixes different builds")?;
            } else {
                build = Some(provenance.clone());
            }
            if let Some(first) = &environment {
                require(
                    first == &report["environment"],
                    "matrix mixes different runtime environments",
                )?;
            } else {
                environment = Some(report["environment"].clone());
            }
        }
    }
    let build = build.ok_or("matrix has no build")?;
    Ok(
        json!({"status":"pass","fixture_count":72,"case_count":216,"measurement_boundary":"alsa_file_plugin",
        "source_commit":build["commit"],"source_dirty":build["source_dirty"]}),
    )
}
