// ABOUTME: Runs the local owner or forwards one shared JSON command.
// ABOUTME: Emits machine-readable replies and stable command failure statuses.
#![forbid(unsafe_code)]
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope},
    ids::{RequestId, ZoneId},
};
use earph_engine::{EngineOwner, RendererListen, request};
use std::{
    io::{self, BufRead, Read},
    path::PathBuf,
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 1_048_576;
const USAGE: &str = "\
usage: earph-engine owner --runtime ABSOLUTE_PRIVATE_DIRECTORY [--renderer-listen IPV4:PORT [--renderer-discovery]]
       earph-engine request --runtime ABSOLUTE_PRIVATE_DIRECTORY
       earph-engine import-fir --runtime ABSOLUTE_PRIVATE_DIRECTORY FILE
       earph-engine render-graph --help

  --renderer-listen IPV4:PORT
        Start the UPnP renderer on one interface address. Without it, the owner opens no network socket.
        Name the address of one interface; 0.0.0.0 and host names are refused. Port 0 picks a free port.
        A controller acts only after a local renderer.grants.replace command names its address.
  --renderer-discovery
        Also announce the renderer by SSDP multicast on that interface. Linux only.";

/// The renderer choices on an owner command line, before validation.
struct OwnerOptions {
    listen: Option<String>,
    discovery: bool,
}

/// Reads the options after `owner --runtime DIRECTORY`. `None` means a malformed command line.
fn owner_options(options: &[std::ffi::OsString]) -> Option<OwnerOptions> {
    let mut parsed = OwnerOptions {
        listen: None,
        discovery: false,
    };
    let mut options = options.iter();
    while let Some(option) = options.next() {
        match option.to_str()? {
            "--renderer-listen" if parsed.listen.is_none() => {
                parsed.listen = Some(options.next()?.to_str()?.to_owned());
            }
            "--renderer-discovery" if !parsed.discovery => parsed.discovery = true,
            _ => return None,
        }
    }
    Some(parsed)
}

fn execute() -> Result<u8, EngineError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "device-query-helper" {
        earph_engine::run_device_query_helper(&mut io::stdin().lock(), &mut io::stdout().lock())?;
        return Ok(0);
    }
    if args.first().is_some_and(|arg| arg == "render-graph") {
        if args.len() == 2 && args[1] == "--help" {
            earph_media::cli_io::write_bytes(
                &mut io::stdout().lock(),
                format!("{}\n", earph_engine::offline_render::USAGE).as_bytes(),
            )
            .map_err(|_| EngineError::new(ErrorCode::OutputFault, "cannot write offline usage"))?;
            return Ok(0);
        }
        let report = earph_engine::offline_render::command(&args[1..]);
        let code = if report.status == "pass" { 0 } else { 1 };
        earph_engine::offline_render::write_json_report(&report, &mut io::stdout().lock())?;
        return Ok(code);
    }
    if args.len() == 4 && args[0] == "import-fir" && args[1] == "--runtime" {
        let runtime = PathBuf::from(&args[2]);
        let source = PathBuf::from(&args[3]);
        let asset = match earph_engine::assets::import(&runtime, &source) {
            Err(error) if error.code == ErrorCode::SourceBusy => {
                match earph_engine::assets::import_via_owner(&runtime, &source) {
                    Ok(asset) => asset,
                    Err(forward) if forward.code == ErrorCode::DeviceUnavailable => {
                        return Err(error);
                    }
                    Err(forward) => return Err(forward),
                }
            }
            result => result?,
        };
        earph_engine::offline_render::write_json_report(
            &serde_json::json!({"schema_version":1,"status":"pass","asset":asset}),
            &mut io::stdout().lock(),
        )?;
        return Ok(0);
    }
    let options = match args.as_slice() {
        [mode, flag, _, options @ ..] if mode == "owner" && flag == "--runtime" => {
            owner_options(options)
        }
        [mode, flag, _] if mode == "request" && flag == "--runtime" => owner_options(&[]),
        _ => None,
    };
    let Some(options) = options else {
        earph_media::cli_io::write_bytes(&mut io::stderr().lock(), format!("{USAGE}\n").as_bytes())
            .map_err(|_| EngineError::new(ErrorCode::OutputFault, "cannot write command usage"))?;
        return Ok(2);
    };
    // Refuse an unclear renderer choice before this process touches the runtime.
    let renderer = match (options.listen, options.discovery) {
        (Some(listen), discovery) => Some(RendererListen::parse(&listen, discovery)?),
        (None, true) => {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "--renderer-discovery needs --renderer-listen IPV4:PORT",
            ));
        }
        (None, false) => None,
    };
    let runtime = PathBuf::from(&args[2]);
    let bytes = if args[0] == "owner" {
        match EngineOwner::bind(&runtime) {
            Ok(owner) => {
                let executable = std::env::current_exe().map_err(|_| {
                    EngineError::new(
                        ErrorCode::DeviceUnavailable,
                        "cannot resolve the production device helper",
                    )
                })?;
                let owner = owner.with_device_discovery(
                    earph_engine::DeviceDiscoveryHelper::from_executable(&executable)?,
                );
                // A requested renderer that cannot start fails the owner start.
                let owner = match renderer {
                    Some(listen) => owner.with_renderer_listen(listen)?,
                    None => owner,
                };
                let mut announcement =
                    serde_json::json!({"status":"owner", "owner_id":owner.snapshot().owner_id});
                if let Some(listener) = owner.renderer_listener() {
                    announcement["renderer"] = serde_json::json!(listener);
                }
                earph_engine::offline_render::write_json_report(
                    &announcement,
                    &mut io::stdout().lock(),
                )?;
                owner.serve()?;
                return Ok(0);
            }
            Err(error) if error.code == ErrorCode::SourceBusy && renderer.is_some() => {
                return Err(EngineError::new(
                    ErrorCode::SourceBusy,
                    "an owner already runs at this runtime; a running owner cannot adopt renderer options",
                ));
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
    earph_media::cli_io::write_bytes(&mut stdout, &reply)
        .and_then(|_| earph_media::cli_io::write_bytes(&mut stdout, b"\n"))
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
    let result = earph_media::cli_io::initialize_process()
        .map_err(|_| {
            EngineError::new(
                ErrorCode::OutputFault,
                "cannot initialize CLI file-limit handling",
            )
        })
        .and_then(|_| execute());
    match result {
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
