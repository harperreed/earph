// ABOUTME: Runs the offline engine qualification tools.
// ABOUTME: Reports structured outcomes without opening physical audio devices.
use earph_core::{EngineError, ErrorCode, audio::MemoryBudget, ids::Epochs};
use earph_harness::render::{self, FaultSchedule, RenderRequest};
use earph_harness::{atomic_json, catalog, evidence, fixture, inventory, read_bounded};
use serde_json::{Value, json};
use std::{env, fs, path::Path, process::ExitCode};
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;

struct CliFailure {
    code: ErrorCode,
    message: String,
}
impl From<String> for CliFailure {
    fn from(message: String) -> Self {
        Self {
            code: ErrorCode::AssetInvalid,
            message,
        }
    }
}
impl From<&str> for CliFailure {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}
impl From<EngineError> for CliFailure {
    fn from(error: EngineError) -> Self {
        Self {
            code: error.code,
            message: error.message.to_string(),
        }
    }
}

fn render_file(
    path: &str,
    block_frames: &str,
    directory: &str,
    faults: Option<&str>,
) -> Result<Value, CliFailure> {
    let (manifest, input) = fixture::load_fixture(Path::new(path))?;
    let block_frames = block_frames
        .parse()
        .map_err(|_| "invalid block frame count")?;
    let faults: FaultSchedule = if let Some(path) = faults {
        serde_json::from_slice(&read_bounded(Path::new(path), 64 * 1024)?)
            .map_err(|_| "invalid fault schedule")?
    } else {
        FaultSchedule::default()
    };
    fs::create_dir(directory).map_err(|_| "capture destination must be a new directory")?;
    let result = render::render_pcm(
        RenderRequest {
            fixture: manifest.spec,
            block_frames,
            budget: MemoryBudget::default(),
            epochs: Epochs::default(),
            faults,
        },
        input,
    );
    let (report, error) = match result {
        Ok(report) => (report, None),
        Err(failure) => (*failure.report, Some(failure.error)),
    };
    persist_render(Path::new(directory), &report)?;
    if let Some(error) = error {
        return Err(error.into());
    }
    Ok(
        json!({"status":"pass","output_frames":report.output_frames,"payload_sha256":report.payload_sha256}),
    )
}

fn persist_render(directory: &Path, report: &render::RenderReport) -> Result<(), CliFailure> {
    fs::write(directory.join("capture.pcm"), report.capture_bytes())
        .map_err(|_| "cannot write capture")?;
    fs::write(
        directory.join("capture.sha256"),
        format!("{}\n", report.payload_sha256),
    )
    .map_err(|_| "cannot write capture hash")?;
    atomic_json(&directory.join("report.json"), report)?;
    Ok(())
}

fn run(args: &[String]) -> Result<Value, CliFailure> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["build-info"] => Ok(earph_harness::build_provenance()),
        ["qualify", "--output", directory] => {
            let record = evidence::qualify(Path::new(directory))?;
            Ok(
                json!({"status":"pass","evidence_id":record["evidence_id"],"observed_result":record["observed_result"]}),
            )
        }
        ["evidence-check", "--record", path] => {
            let path = Path::new(path);
            let record = serde_json::from_slice(&read_bounded(path, 2 * 1024 * 1024)?)
                .map_err(|_| "invalid evidence document")?;
            evidence::validate(&record, path.parent().unwrap_or_else(|| Path::new(".")))?;
            Ok(json!({"status":"pass"}))
        }
        [
            "render",
            "--fixture",
            path,
            "--block-frames",
            frames,
            "--output",
            directory,
        ] => render_file(path, frames, directory, None),
        [
            "render",
            "--fixture",
            path,
            "--block-frames",
            frames,
            "--output",
            directory,
            "--faults",
            faults,
        ] => render_file(path, frames, directory, Some(faults)),
        ["fixtures", "--output", directory] => {
            let bundle = fixture::write_bundle(Path::new(directory))?;
            Ok(json!({"status":"pass","fixtures":bundle.fixtures.len()}))
        }
        ["catalog-check", "--catalog", path] => {
            let summary =
                catalog::validate_catalog(&read_bounded(Path::new(path), 2 * 1024 * 1024)?)?;
            Ok(serde_json::to_value(summary).map_err(|e| e.to_string())?)
        }
        ["dependency-check", "--inventory", path] => {
            let value = serde_json::from_slice(&read_bounded(Path::new(path), 2 * 1024 * 1024)?)
                .map_err(|e| e.to_string())?;
            inventory::validate(&value)?;
            Ok(json!({"status":"pass"}))
        }
        [
            "dependency-lock",
            "--output",
            path,
            "--notices",
            notices_path,
        ] => {
            let (value, notices) = inventory::snapshot()?;
            fs::write(
                path,
                serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
            )
            .map_err(|_| "cannot write inventory")?;
            fs::write(notices_path, notices).map_err(|_| "cannot write notices")?;
            Ok(json!({"status":"pass"}))
        }
        ["--help"] => Ok(
            json!({"commands":["build-info","catalog-check --catalog PATH","dependency-check --inventory PATH","dependency-lock --output PATH --notices PATH","fixtures --output DIR","render --fixture PATH --block-frames N --output DIR [--faults PATH]","qualify --output DIR","evidence-check --record PATH"],"physical_output":false}),
        ),
        _ => Err("unsupported command or arguments; use --help".into()),
    }
}

fn main() -> ExitCode {
    match run(&env::args().skip(1).collect::<Vec<_>>()) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            println!(
                "{}",
                json!({"status":"fail","error":{"code":error.code,"message":error.message}})
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incomplete_artifact_write_cannot_publish_a_passing_render_report() {
        let directory = test_support::TestDir::new();
        fs::create_dir(directory.0.join("capture.sha256")).unwrap();
        let report = render::render(RenderRequest {
            fixture: fixture::qualification_specs().pop().unwrap(),
            block_frames: 64,
            budget: MemoryBudget::default(),
            epochs: Epochs::default(),
            faults: FaultSchedule::default(),
        })
        .unwrap();
        assert!(persist_render(&directory.0, &report).is_err());
        assert!(!directory.0.join("report.json").exists());
        assert!(directory.0.join("capture.pcm").is_file());
    }
}
