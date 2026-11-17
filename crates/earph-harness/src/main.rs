// ABOUTME: Runs the offline engine qualification tools.
// ABOUTME: Reports structured outcomes without opening physical audio devices.
use earph_harness::{catalog, inventory, read_bounded};
use serde_json::{Value, json};
use std::{env, fs, path::Path, process::ExitCode};

fn run(args: &[String]) -> Result<Value, String> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["catalog-check", "--catalog", path] => {
            let summary =
                catalog::validate_catalog(&read_bounded(Path::new(path), 2 * 1024 * 1024)?)?;
            serde_json::to_value(summary).map_err(|e| e.to_string())
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
            json!({"commands":["catalog-check --catalog PATH","dependency-check --inventory PATH","dependency-lock --output PATH --notices PATH"],"physical_output":false}),
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
        Err(message) => {
            println!(
                "{}",
                json!({"status":"fail","error":{"code":"ASSET_INVALID","message":message}})
            );
            ExitCode::from(2)
        }
    }
}
