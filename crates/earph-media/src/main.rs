// ABOUTME: Runs read-only local media inspection and offline rendering.
// ABOUTME: Prints bounded JSON results without opening audio hardware.
use earph_core::{EngineError, ErrorCode, ids::Epochs};
use earph_media::{LocalFile, render};
use serde_json::{Value, json};
use std::{path::PathBuf, process::ExitCode};

fn run() -> Result<Value, EngineError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let usage = EngineError::new(
        ErrorCode::AssetInvalid,
        "usage: earph-media inspect FILE | render --output NEW_DIR [--seek FRAME] FILE...",
    );
    if args.len() == 2 && args[0] == "inspect" {
        let file = LocalFile::open(&PathBuf::from(&args[1]), Epochs::default())?;
        return Ok(
            json!({"schema_version":1,"status":"pass","format":file.format(),"total_frames":file.total_frames(),"qualification":"header_and_first_packet_only"}),
        );
    }
    if args.len() < 4 || args[0] != "render" || args[1] != "--output" {
        return Err(usage);
    }
    let output = PathBuf::from(&args[2]);
    let mut start = 3;
    let mut seek = 0;
    if args.get(start).is_some_and(|s| s == "--seek") {
        seek = args
            .get(start + 1)
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse().ok())
            .ok_or(usage)?;
        start += 2;
    }
    let files: Vec<_> = args[start..].iter().map(PathBuf::from).collect();
    serde_json::to_value(render(&files, &output, seek)?).map_err(|_| usage)
}

fn main() -> ExitCode {
    let result =
        run().unwrap_or_else(|error| json!({"schema_version":1,"status":"fail","error":error}));
    let success = result["status"] == "pass";
    println!("{result}");
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
