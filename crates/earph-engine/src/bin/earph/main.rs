// ABOUTME: Plays local files through the Engine from one command and reports what happened.
// ABOUTME: Gives people and test scripts a small, direct way to exercise real playback.
#![forbid(unsafe_code)]
mod link;
mod play;
mod sendspin;

use link::{Failure, Link, Outcome, Target, text};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

const USAGE: &str = "\
usage: earph [--runtime DIRECTORY] COMMAND

  play FILE... [--output NAME] [--volume DB] [--period FRAMES] [--buffer FRAMES] [--replace-queue]
                 Play files in order, print progress, and exit when playback ends.
                 --replace-queue stops current playback and replaces the queue.
                 Exit status: 0 finished or stopped, 1 playback failed, 130 interrupted.
  sendspin --server ws://IP:PORT/PATH --server-key KEY [--output NAME] [--volume DB]
                 [--credential-helper PATH] [--credential-store STORE] [--credential-lock PATH]
                 Start an Engine for one Sendspin server and play what it sends.
                 Timing is best effort: the status line says `sync unqualified`.
                 The address must be numeric. Loopback, 100.64.0.0/10, and wss are refused.
                 Exit status: 0 ended, 1 failed, 130 interrupted.
  devices        List outputs. Any part of a name selects one with --output.
  status [--json]  Show what the Engine is doing.
  pause | resume | stop | mute | unmute
                 A Sendspin stream cannot pause, resume, or seek: pause stops it.
  volume DB      Set the volume, for example: earph volume -30
  seek POSITION  Jump to SECONDS or M:SS in the playing file.
  quit           Stop playback and the Engine.

Naming an output with --output (or EARPH_OUTPUT) approves audio on that output.
Without a name, play uses the only listed output. It refuses to pick between several.
The volume changes only when you pass --volume. A new Engine starts at -40 dB.
The runtime directory defaults to EARPH_RUNTIME, then ~/.earph-cli.
`play` starts an Engine when none runs there and stops that Engine when it exits.";

pub struct Arguments {
    pub runtime: PathBuf,
    pub command: String,
    pub positional: Vec<String>,
    pub output: Option<String>,
    pub volume_millidecibels: Option<i64>,
    pub period: Option<u64>,
    pub buffer: Option<u64>,
    pub replace_queue: bool,
    pub json: bool,
    pub server: Option<String>,
    pub server_key: Option<String>,
    pub credential_helper: Option<String>,
    pub credential_store: Option<String>,
    pub credential_lock: Option<String>,
}

fn decibels(value: &str) -> Outcome<i64> {
    let decibels: f64 = value
        .parse()
        .map_err(|_| Failure::usage(format!("volume must be a number of dB, not `{value}`")))?;
    if !(-120.0..=0.0).contains(&decibels) {
        return Err(Failure::usage("volume must be between -120 and 0 dB"));
    }
    Ok((decibels * 1000.0).round() as i64)
}
/// Reads a position as whole seconds or as minutes and seconds.
fn seconds(value: &str) -> Outcome<u64> {
    let (minutes, seconds) = value.rsplit_once(':').unwrap_or(("0", value));
    match (minutes.parse::<u64>(), seconds.parse::<u64>()) {
        (Ok(minutes), Ok(seconds)) => Ok(minutes.saturating_mul(60).saturating_add(seconds)),
        _ => Err(Failure::usage(format!(
            "a position is SECONDS or M:SS, not `{value}`"
        ))),
    }
}
fn parse(mut raw: impl Iterator<Item = String>) -> Outcome<Arguments> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut arguments = Arguments {
        runtime: std::env::var_os("EARPH_RUNTIME")
            .map(PathBuf::from)
            .or_else(|| home.map(|home| home.join(".earph-cli")))
            .ok_or_else(|| Failure::usage("set EARPH_RUNTIME or pass --runtime"))?,
        command: String::new(),
        positional: Vec::new(),
        output: std::env::var("EARPH_OUTPUT")
            .ok()
            .filter(|name| !name.is_empty()),
        volume_millidecibels: None,
        period: None,
        buffer: None,
        replace_queue: false,
        json: false,
        server: None,
        server_key: None,
        credential_helper: None,
        credential_store: None,
        credential_lock: None,
    };
    let mut options_ended = false;
    while let Some(argument) = raw.next() {
        let mut value = |name: &str| {
            raw.next()
                .ok_or_else(|| Failure::usage(format!("{name} needs a value")))
        };
        let frames = |name: &str, value: String| {
            value
                .parse::<u64>()
                .map_err(|_| Failure::usage(format!("{name} must be a whole number of frames")))
        };
        match argument.as_str() {
            _ if options_ended => arguments.positional.push(argument),
            "--" => options_ended = true,
            "--runtime" => arguments.runtime = PathBuf::from(value("--runtime")?),
            "--output" | "-o" => arguments.output = Some(value("--output")?),
            "--volume" | "-v" => {
                arguments.volume_millidecibels = Some(decibels(&value("--volume")?)?)
            }
            "--period" => arguments.period = Some(frames("--period", value("--period")?)?),
            "--buffer" => arguments.buffer = Some(frames("--buffer", value("--buffer")?)?),
            "--server" => arguments.server = Some(value("--server")?),
            "--server-key" => arguments.server_key = Some(value("--server-key")?),
            "--credential-helper" => {
                arguments.credential_helper = Some(value("--credential-helper")?)
            }
            "--credential-store" => arguments.credential_store = Some(value("--credential-store")?),
            "--credential-lock" => arguments.credential_lock = Some(value("--credential-lock")?),
            "--replace-queue" => arguments.replace_queue = true,
            "--json" => arguments.json = true,
            "--help" | "-h" => arguments.command = "help".into(),
            // A negative number is a value, as in `earph volume -30`.
            _ if argument.starts_with('-') && argument.parse::<f64>().is_err() => {
                return Err(Failure::usage(format!("unknown option {argument}")));
            }
            _ if arguments.command.is_empty() => arguments.command = argument,
            _ => arguments.positional.push(argument),
        }
    }
    if !arguments.runtime.is_absolute() {
        arguments.runtime = std::env::current_dir()
            .map_err(|error| Failure::usage(format!("cannot resolve the runtime: {error}")))?
            .join(&arguments.runtime);
    }
    Ok(arguments)
}

fn backend() -> &'static str {
    if cfg!(target_os = "macos") {
        "core_audio"
    } else {
        "alsa"
    }
}
/// Reads the device list. The Engine answers the same request again once discovery finishes.
pub fn devices(link: &mut Link) -> Outcome<Vec<Value>> {
    let state = link.state()?;
    let envelope = link.envelope(
        state["owner_id"].as_str(),
        None,
        None,
        json!({"method": "devices.list", "backend": backend()}),
    );
    let mut receipt = link.wait(Duration::from_secs(10), "the device list", |link| {
        let receipt = link.exchange(&envelope)?;
        Ok((receipt["status"] != "accepted").then_some(receipt))
    })?;
    if receipt["status"] != "applied" {
        return Err(Failure::engine(format!(
            "cannot list devices: {} ({})",
            text(&receipt["error"]["message"]),
            text(&receipt["error"]["code"])
        )));
    }
    match receipt["result"]["value"]["observation"]["devices"].take() {
        Value::Array(devices) => Ok(devices),
        _ => Err(Failure::engine("the Engine returned no device list")),
    }
}
pub fn device_name(device: &Value) -> String {
    ["display_name", "description"]
        .iter()
        .find_map(|key| device[key].as_str())
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
        .to_owned()
}
pub fn decibel_text(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "unknown".into(), |decibels| format!("{decibels:.1} dB"))
}
pub fn clock(frames: u64, rate: u64) -> String {
    let seconds = frames.checked_div(rate).unwrap_or(0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}
/// One line that describes the transport, position, and formats.
pub fn progress_line(state: &Value, total_frames: Option<u64>) -> String {
    let playback = &state["playback"];
    let rate = playback["format"]["sample_rate_hz"].as_u64().unwrap_or(0);
    let position = ["audible_source_position_frames", "source_position_frames"]
        .iter()
        .find_map(|key| playback[key].as_u64())
        .unwrap_or(0);
    let mut line = format!("{:<9} {}", text(&state["transport"]), clock(position, rate));
    if let Some(total) = total_frames {
        line.push_str(&format!(" / {}", clock(total, rate)));
    }
    // Core Audio reports its device rate as a fraction.
    let device_rate = playback["backend"]["device_format"]["sample_rate_hz"]
        .as_f64()
        .map(|rate| rate.round() as u64);
    match device_rate {
        Some(device) if rate != 0 && device != rate => {
            line.push_str(&format!("  {rate} Hz -> {device} Hz"))
        }
        _ if rate != 0 => line.push_str(&format!("  {rate} Hz")),
        _ => {}
    }
    line.push_str(&format!("  {}", decibel_text(&state["software_gain_db"])));
    if state["muted"] == true {
        line.push_str("  muted");
    }
    line
}

fn show_devices(link: &mut Link) -> Outcome<()> {
    let selected = link.state()?["output"]["endpoint"].take();
    for device in devices(link)? {
        let mark = if device["endpoint"] == selected {
            "*"
        } else {
            " "
        };
        let problem = device["selection_error"]
            .as_str()
            .map(|error| format!("  (unavailable: {error})"))
            .unwrap_or_default();
        println!(
            "{mark} {}  [{}]{problem}",
            device_name(&device),
            text(&device["endpoint"])
        );
    }
    Ok(())
}
fn show_status(link: &mut Link, json: bool) -> Outcome<()> {
    let state = link.state()?;
    if json {
        println!("{state}");
        return Ok(());
    }
    println!(
        "engine   {}  {}",
        text(&state["owner_id"]),
        link.runtime().display()
    );
    match state["output"]["endpoint"].as_str() {
        Some(endpoint) => println!(
            "output   {endpoint}  {}",
            if state["output_consented"] == true {
                "approved"
            } else {
                "not approved"
            }
        ),
        None => println!("output   none selected"),
    }
    println!(
        "volume   {} (limit {})",
        decibel_text(&state["software_gain_db"]),
        decibel_text(&state["volume_policy"]["software_cap_db"])
    );
    if state["source"] == "sendspin" {
        println!("source   Sendspin");
        println!("state    {}", sendspin::status_line(&state));
    } else {
        let queue = link.query(json!({"method": "queue.get", "offset": 0, "limit": 64}))?;
        let active = queue["entries"].as_array().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry["id"] == state["active_entry_id"])
        });
        if let Some(entry) = active {
            println!("track    {}", text(&entry["path"]));
        }
        println!(
            "state    {}",
            progress_line(
                &state,
                active.and_then(|entry| entry["status"]["total_frames"].as_u64())
            )
        );
    }
    if !state["error"].is_null() {
        println!("error    {}", text(&state["error"]));
    }
    Ok(())
}

fn run(arguments: Arguments, interrupted: Arc<AtomicBool>) -> Outcome<u8> {
    let simple = |payload: Value, target: fn() -> Target<'static>, done: &str| {
        if !arguments.positional.is_empty() {
            return Err(Failure::usage(format!(
                "{} takes no arguments",
                arguments.command
            )));
        }
        let mut link = Link::open(&arguments.runtime, false)?;
        let transport = matches!(arguments.command.as_str(), "pause" | "resume" | "stop");
        let state = link.state()?;
        if transport && state["session_id"].is_null() {
            return Err(Failure::playback("nothing is playing"));
        }
        let sendspin = state["source"] == "sendspin";
        if sendspin && arguments.command == "resume" {
            return Err(sendspin::unsupported("resume"));
        }
        // A live stream cannot pause, so the Engine stops a Sendspin stream on a pause.
        let done = if sendspin && arguments.command == "pause" {
            "stopped (a Sendspin stream cannot pause)"
        } else {
            done
        };
        if payload["method"] == "owner.shutdown" {
            // The Engine stops answering once it shuts down.
            link.mutate(payload, target())?;
        } else {
            link.complete(payload, target())?;
        }
        println!("{done}");
        Ok(0)
    };
    match arguments.command.as_str() {
        "" | "help" => {
            println!("{USAGE}");
            Ok(if arguments.command.is_empty() { 2 } else { 0 })
        }
        "play" => play::run(&arguments, &interrupted),
        "sendspin" => sendspin::run(&arguments, &interrupted),
        "devices" | "status" if !arguments.positional.is_empty() => Err(Failure::usage(format!(
            "{} takes no arguments",
            arguments.command
        ))),
        "devices" => {
            let mut link = Link::open(&arguments.runtime, true)?;
            let listed = show_devices(&mut link);
            link.close()?;
            listed.map(|()| 0)
        }
        "status" => {
            let mut link = Link::open(&arguments.runtime, false)?;
            show_status(&mut link, arguments.json).map(|()| 0)
        }
        "pause" => simple(
            json!({"method": "sessions.pause"}),
            || Target::Current,
            "paused",
        ),
        "resume" => simple(
            json!({"method": "sessions.resume"}),
            || Target::Current,
            "resumed",
        ),
        "stop" => simple(
            json!({"method": "sessions.stop"}),
            || Target::Current,
            "stopped",
        ),
        "mute" => simple(
            json!({"method": "mute.set", "muted": true}),
            || Target::None,
            "muted",
        ),
        "unmute" => simple(
            json!({"method": "mute.set", "muted": false}),
            || Target::None,
            "unmuted",
        ),
        "quit" => simple(
            json!({"method": "owner.shutdown"}),
            || Target::Current,
            "Engine stopped",
        ),
        "seek" => {
            let [value] = arguments.positional.as_slice() else {
                return Err(Failure::usage("usage: earph seek SECONDS or M:SS"));
            };
            let seconds = seconds(value)?;
            let mut link = Link::open(&arguments.runtime, false)?;
            let state = link.state()?;
            if state["source"] == "sendspin" {
                return Err(sendspin::unsupported("seek"));
            }
            let rate = state["playback"]["format"]["sample_rate_hz"].as_u64();
            let (Some(rate), false) = (rate, state["session_id"].is_null()) else {
                return Err(Failure::playback("nothing is playing"));
            };
            let total = play::queue(&mut link)?
                .iter()
                .find(|entry| entry["id"] == state["active_entry_id"])
                .and_then(|entry| entry["status"]["total_frames"].as_u64());
            let position = seconds.saturating_mul(rate);
            if let Some(total) = total.filter(|total| position >= *total) {
                return Err(Failure::playback(format!(
                    "this file ends at {}",
                    clock(total, rate)
                )));
            }
            link.complete(
                json!({"method": "sessions.seek", "position_frames": position}),
                Target::Current,
            )?;
            println!("seek {}", clock(position, rate));
            Ok(0)
        }
        "volume" => {
            let [value] = arguments.positional.as_slice() else {
                return Err(Failure::usage("usage: earph volume DB"));
            };
            let millidecibels = decibels(value)?;
            let mut link = Link::open(&arguments.runtime, false)?;
            link.mutate(
                json!({"method": "volume.set", "millidecibels": millidecibels, "ramp_frames": 960}),
                Target::None,
            )?;
            println!(
                "volume {}",
                decibel_text(&link.state()?["software_gain_db"])
            );
            Ok(0)
        }
        other => Err(Failure::usage(format!(
            "unknown command `{other}`\n\n{USAGE}"
        ))),
    }
}

fn main() -> ExitCode {
    let mut raw = std::env::args().skip(1).peekable();
    // The Engine re-runs this executable to list devices in a clean child process.
    if raw
        .peek()
        .is_some_and(|first| first == "device-query-helper")
        && raw.len() == 1
    {
        return match earph_engine::run_device_query_helper(
            &mut std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(3),
        };
    }
    let interrupted = Arc::new(AtomicBool::new(false));
    let result = earph_media::cli_io::initialize_process()
        .and_then(|()| {
            for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
                signal_hook::flag::register(signal, interrupted.clone())?;
            }
            Ok(())
        })
        .map_err(|error| Failure::engine(format!("cannot install signal handling: {error}")))
        .and_then(|()| parse(raw))
        .and_then(|arguments| run(arguments, interrupted));
    match result {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            eprintln!("earph: {}", failure.message);
            ExitCode::from(failure.exit)
        }
    }
}
