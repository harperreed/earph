// ABOUTME: Runs one `earph play`: approve the named output, queue the files, start, and follow playback.
// ABOUTME: Stops what it started on interruption or failure, and removes its own queue entries.
use crate::{
    Arguments, decibel_text, device_name, devices,
    link::{Failure, Link, Outcome, Target, text},
    progress_line,
};
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const INTERRUPTED: u8 = 130;
/// At or below this level, quiet music is inaudible on laptop speakers.
const QUIET_DB: f64 = -30.0;

pub fn run(arguments: &Arguments, interrupted: &AtomicBool) -> Outcome<u8> {
    if arguments.positional.is_empty() {
        return Err(Failure::usage("usage: earph play FILE... [--output NAME]"));
    }
    let mut files = Vec::new();
    for file in &arguments.positional {
        let path = std::fs::canonicalize(file)
            .map_err(|error| Failure::usage(format!("cannot read {file}: {error}")))?;
        files.push(
            path.to_str()
                .ok_or_else(|| Failure::usage(format!("{file} is not a UTF-8 path")))?
                .to_owned(),
        );
    }
    let mut link = Link::open(&arguments.runtime, true)?;
    let mut entries = Vec::new();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let session = format!("cli-session-{:x}-{nanos:x}", std::process::id());
    let result = play(
        &mut link,
        arguments,
        &files,
        &session,
        &mut entries,
        interrupted,
    );
    if result.is_err() {
        stop_own(&mut link, &session);
    }
    for entry in &entries {
        let _ = link.mutate(json!({"method": "queue.remove", "id": entry}), Target::None);
    }
    let closed = link.close();
    let code = result?;
    closed?;
    Ok(code)
}

/// Leaves nothing playing that this command started. Another session is not ours to stop.
pub fn stop_own(link: &mut Link, session: &str) {
    let ours = |link: &mut Link| {
        link.state()
            .is_ok_and(|state| state["session_id"] == session && !idle(&state))
    };
    if ours(link) {
        let _ = link.mutate(json!({"method": "sessions.stop"}), Target::Current);
        let deadline = Instant::now() + Duration::from_secs(5);
        while ours(link) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
    }
}
fn idle(state: &Value) -> bool {
    state["session_id"].is_null()
        || matches!(
            state["transport"].as_str(),
            Some("idle" | "failed" | "suspended")
        )
}
pub fn queue(link: &mut Link) -> Outcome<Vec<Value>> {
    let mut entries = Vec::new();
    loop {
        let page =
            link.query(json!({"method": "queue.get", "offset": entries.len(), "limit": 64}))?;
        match page["entries"].as_array() {
            Some(more) if !more.is_empty() => entries.extend(more.iter().cloned()),
            _ => return Ok(entries),
        }
    }
}

fn label(device: &Value) -> String {
    format!("{}  [{}]", device_name(device), text(&device["endpoint"]))
}
/// Picks the output that `name` describes: an exact endpoint, or one name that contains it.
fn choose(link: &mut Link, name: &str) -> Outcome<String> {
    let listed = devices(link)?;
    if listed.iter().any(|device| device["endpoint"] == name) {
        return Ok(name.to_owned());
    }
    let wanted = name.to_lowercase();
    let matches: Vec<_> = listed
        .iter()
        .filter(|device| {
            !device["selection"].is_null() && label(device).to_lowercase().contains(&wanted)
        })
        .collect();
    match matches.as_slice() {
        [device] => Ok(text(&device["endpoint"])),
        // ALSA accepts configured names that its hint list omits.
        [] if cfg!(target_os = "linux") => Ok(name.to_owned()),
        [] => Err(Failure::usage(format!(
            "no output matches `{name}`; run `earph devices`"
        ))),
        several => Err(Failure::usage(format!(
            "`{name}` matches several outputs; be more specific:\n{}",
            several
                .iter()
                .map(|device| format!("  {}", label(device)))
                .collect::<Vec<_>>()
                .join("\n")
        ))),
    }
}
/// Picks the output when the user named none: the only one this computer lists.
fn only_output(link: &mut Link) -> Outcome<String> {
    let listed = devices(link)?;
    let selectable: Vec<_> = listed
        .iter()
        .filter(|device| !device["selection"].is_null())
        .collect();
    match selectable.as_slice() {
        [device] => Ok(text(&device["endpoint"])),
        [] => Err(Failure::usage(
            "no output is listed; name one with --output NAME",
        )),
        several => Err(Failure::usage(format!(
            "name the output to play through with --output NAME\nSet EARPH_OUTPUT to name it once per shell. The outputs are:\n{}",
            several
                .iter()
                .map(|device| format!("  {}", label(device)))
                .collect::<Vec<_>>()
                .join("\n")
        ))),
    }
}
pub fn prepare_output(link: &mut Link, arguments: &Arguments, state: &Value) -> Outcome<()> {
    let endpoint = match &arguments.output {
        Some(name) if name.trim().is_empty() => {
            return Err(Failure::usage("--output needs an output name"));
        }
        Some(name) => choose(link, name)?,
        None if !state["output"].is_null() && state["output_consented"] == true => return Ok(()),
        None => only_output(link)?,
    };
    let output = json!({
        "backend": crate::backend(),
        "route": "system",
        "endpoint": endpoint,
        "device_id": "cli-output",
        "profile_id": "listening",
        "period_frames": arguments.period.unwrap_or(64),
        // The Mac system queue feeds too slowly below this size; see gotchas.md.
        "buffer_frames": arguments.buffer.unwrap_or(if cfg!(target_os = "macos") { 4096 } else { 256 }),
        "volume_mode": "software",
        "fidelity": "compatible_conversion",
    });
    if state["output"] != output {
        link.mutate(
            json!({"method": "output.select", "output": output}),
            Target::None,
        )?;
    } else if state["output_consented"] == true {
        return Ok(());
    }
    link.mutate(
        json!({"method": "output.consent", "device_id": "cli-output", "profile_id": "listening"}),
        Target::None,
    )?;
    Ok(())
}

fn play(
    link: &mut Link,
    arguments: &Arguments,
    files: &[String],
    session: &str,
    entries: &mut Vec<Value>,
    interrupted: &AtomicBool,
) -> Outcome<u8> {
    let existing = queue(link)?;
    let busy = !idle(&link.state()?);
    if (busy || !existing.is_empty()) && !arguments.replace_queue {
        return Err(Failure::usage(format!(
            "the Engine at {} {}; pass --replace-queue to stop that and replace its queue",
            link.runtime().display(),
            if busy {
                "is already playing".to_owned()
            } else {
                format!("already holds {} queued files", existing.len())
            }
        )));
    }
    if busy {
        link.mutate(json!({"method": "sessions.stop"}), Target::Current)?;
        link.wait(
            Duration::from_secs(10),
            "the earlier playback to stop",
            |link| Ok(idle(&link.state()?).then_some(())),
        )?;
    }
    for entry in &existing {
        link.mutate(
            json!({"method": "queue.remove", "id": entry["id"]}),
            Target::None,
        )?;
    }
    let state = link.state()?;
    prepare_output(link, arguments, &state)?;
    if let Some(millidecibels) = arguments.volume_millidecibels {
        link.mutate(
            json!({"method": "volume.set", "millidecibels": millidecibels, "ramp_frames": 960}),
            Target::None,
        )?;
    }
    for file in files {
        link.mutate(
            json!({"method": "queue.append", "path": file}),
            Target::None,
        )?;
    }
    let queued = queue(link)?;
    entries.extend(queued.iter().map(|entry| entry["id"].clone()));
    let first = queued
        .first()
        .ok_or_else(|| Failure::engine("the Engine queued no files"))?["id"]
        .clone();
    link.mutate(json!({"method": "queue.select", "id": first}), Target::None)?;
    let state = link.state()?;
    announce_output(link, &state);
    link.mutate(
        json!({
            "method": "sessions.start",
            "source": "local_file",
            "device_id": state["output"]["device_id"],
            "takeover": !state["session_id"].is_null(),
            "seekable": true,
        }),
        Target::New(session),
    )?;
    let prepared = link.wait(
        Duration::from_secs(30),
        "the first file to prepare",
        |link| {
            let state = link.state()?;
            if interrupted.load(Ordering::Relaxed) {
                return Ok(Some(false));
            }
            if state["transport"] == "failed" {
                return Err(failed(link, &state));
            }
            if state["session_id"] != session {
                return Err(Failure::playback(
                    "another controller replaced this playback",
                ));
            }
            let ready = state["transport"] == "playing"
                || state["transport"] == "buffering"
                    && state["playback"]["source_entry_id"] == first
                    && !state["playback"]["backend"].is_null();
            Ok(ready.then_some(true))
        },
    )?;
    if prepared && link.state()?["muted"] == true {
        link.mutate(json!({"method": "mute.set", "muted": false}), Target::None)?;
    }
    follow(link, session, interrupted)
}

/// Prints the output and the volume, with a hint when that volume is hard to hear.
pub fn announce_output(link: &mut Link, state: &Value) {
    let endpoint = &state["output"]["endpoint"];
    // The identifier alone can be an opaque address. Name the output as the device list does.
    let named = devices(link).ok().and_then(|listed| {
        listed
            .iter()
            .find(|device| &device["endpoint"] == endpoint && !device_name(device).is_empty())
            .map(label)
    });
    println!(
        "output   {}  volume {}",
        named.unwrap_or_else(|| text(endpoint)),
        decibel_text(&state["software_gain_db"])
    );
    if state["software_gain_db"]
        .as_f64()
        .is_some_and(|decibels| decibels <= QUIET_DB)
    {
        println!(
            "hint     this volume is quiet; if you hear nothing, pass --volume DB or run `earph volume DB`"
        );
    }
}

/// Names the file the Engine rejected, or reports the output fault code.
fn failed(link: &mut Link, state: &Value) -> Failure {
    let rejected = queue(link).ok().and_then(|entries| {
        entries
            .into_iter()
            .find(|entry| entry["status"]["state"] == "failed")
    });
    Failure::playback(match rejected {
        Some(entry) => format!(
            "cannot play {}: {}",
            text(&entry["path"]),
            text(&entry["status"]["code"])
        ),
        None => format!(
            "playback failed: {}\nThe Engine prints the device fault as a `backend_fault` line.",
            text(&state["error"])
        ),
    })
}

/// Prints progress until this session ends, fails, or the user interrupts it.
fn follow(link: &mut Link, session: &str, interrupted: &AtomicBool) -> Outcome<u8> {
    // The Engine learns each file's length when it prepares that file.
    let mut queued = Vec::new();
    let terminal = std::io::stdout().is_terminal();
    let mut shown = (Value::Null, Value::Null);
    let mut line = String::new();
    let finish = |line: &str| {
        if terminal && !line.is_empty() {
            println!();
        }
    };
    loop {
        if interrupted.load(Ordering::Relaxed) {
            finish(&line);
            let _ = link.mutate(json!({"method": "sessions.stop"}), Target::Current);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !idle(&link.state()?) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            println!("result   interrupted");
            return Ok(INTERRUPTED);
        }
        let state = link.state()?;
        if state["session_id"] == session {
            if state["transport"] == "failed" {
                finish(&line);
                return Err(failed(link, &state));
            }
            if state["active_entry_id"] != shown.0 {
                queued = queue(link)?;
            }
            let entry = queued
                .iter()
                .find(|entry| entry["id"] == state["active_entry_id"]);
            if state["active_entry_id"] != shown.0 {
                finish(&line);
                if let Some(entry) = entry {
                    println!("track    {}", text(&entry["path"]));
                }
            }
            line = progress_line(
                &state,
                entry.and_then(|entry| entry["status"]["total_frames"].as_u64()),
            );
            if terminal {
                print!("\r{line}\x1b[K");
                let _ = std::io::stdout().flush();
            } else if (&state["active_entry_id"], &state["transport"]) != (&shown.0, &shown.1) {
                println!("{line}");
            }
            shown = (state["active_entry_id"].clone(), state["transport"].clone());
        } else if state["session_id"].is_null() {
            // The Engine keeps the final position, which shows whether the last file reached its end.
            // A failed session keeps its identity, so a cleared session ended without a fault.
            let last = queue(link)?.pop().unwrap_or_default();
            let position = state["playback"]["source_position_frames"].as_u64();
            let reached_end = state["playback"]["source_entry_id"] == last["id"]
                && position.is_some()
                && position >= last["status"]["total_frames"].as_u64();
            finish(&line);
            println!(
                "result   {}",
                if reached_end { "finished" } else { "stopped" }
            );
            return Ok(0);
        } else {
            finish(&line);
            return Err(Failure::playback(
                "another controller replaced this playback",
            ));
        }
        thread::sleep(Duration::from_millis(200));
    }
}
