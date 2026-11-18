// ABOUTME: Runs the local owner or forwards one shared JSON command.
// ABOUTME: Emits machine-readable replies and stable command failure statuses.
#![forbid(unsafe_code)]
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope},
    ids::{RequestId, ZoneId},
};
use earph_engine::{EngineOwner, request};
use std::{
    io::{self, BufRead, Read, Write},
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 1_048_576;
const USAGE: &str = "usage: earph-engine <owner|request> --runtime ABSOLUTE_PRIVATE_DIRECTORY | import-fir --runtime ABSOLUTE_PRIVATE_DIRECTORY FILE | render-graph --help";

fn execute() -> Result<u8, EngineError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "render-graph") {
        if args.len() == 2 && args[1] == "--help" {
            println!("{}", earph_engine::offline_render::USAGE);
            return Ok(0);
        }
        let report = earph_engine::offline_render::command(&args[1..]);
        let code = if report.status == "pass" { 0 } else { 1 };
        earph_engine::offline_render::write_json_report(&report, &mut io::stdout().lock())?;
        return Ok(code);
    }
    if args.len() == 4 && args[0] == "import-fir" && args[1] == "--runtime" {
        let asset =
            earph_engine::assets::import(&PathBuf::from(&args[2]), &PathBuf::from(&args[3]))?;
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"status":"pass","asset":asset})
        );
        return Ok(0);
    }
    if args.len() != 3 || args[1] != "--runtime" || (args[0] != "owner" && args[0] != "request") {
        eprintln!("{USAGE}");
        return Ok(2);
    }
    let runtime = PathBuf::from(&args[2]);
    let bytes = if args[0] == "owner" {
        match EngineOwner::bind(&runtime) {
            Ok(owner) => {
                println!(
                    "{}",
                    serde_json::json!({"status":"owner", "owner_id":owner.snapshot().owner_id})
                );
                io::stdout().flush().map_err(|_| {
                    EngineError::new(ErrorCode::InternalFault, "cannot announce local owner")
                })?;
                owner.serve()?;
                return Ok(0);
            }
            Err(error) if error.code == ErrorCode::SourceBusy => {
                let generation = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| error)?
                    .as_nanos();
                let command = CommandEnvelope {
                    schema_version: 1,
                    request_id: RequestId::new(format!(
                        "attach-{}-{generation}",
                        std::process::id()
                    ))?,
                    zone_id: ZoneId::new("main")?,
                    owner_id: None,
                    session_id: None,
                    expected_revision: None,
                    payload: Command::State,
                };
                serde_json::to_vec(&command).map_err(|_| {
                    EngineError::new(ErrorCode::InternalFault, "cannot encode local command")
                })?
            }
            Err(error) => return Err(error),
        }
    } else {
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take(MAX_BYTES + 2)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| {
                EngineError::new(
                    ErrorCode::AssetInvalid,
                    "cannot read command from standard input",
                )
            })?;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
        }
        if bytes.len() as u64 > MAX_BYTES {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "command exceeds byte limit",
            ));
        }
        bytes
    };
    let reply = request(&runtime, &bytes)?;
    let document: serde_json::Value = serde_json::from_slice(&reply).map_err(|_| {
        EngineError::new(
            ErrorCode::AssetInvalid,
            "owner returned an invalid response",
        )
    })?;
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(&reply)
        .and_then(|_| stdout.write_all(b"\n"))
        .map_err(|_| EngineError::new(ErrorCode::InternalFault, "cannot write command response"))?;
    Ok(
        if matches!(
            document["receipt"]["status"].as_str(),
            Some("accepted" | "applied")
        ) {
            0
        } else {
            1
        },
    )
}

fn main() -> ExitCode {
    match execute() {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = earph_engine::offline_render::write_json_report(
                &serde_json::json!({"error":error}),
                &mut io::stderr().lock(),
            );
            ExitCode::from(3)
        }
    }
}
