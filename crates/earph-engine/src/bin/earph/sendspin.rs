// ABOUTME: Runs one `earph sendspin`: host an Engine for one Sendspin server and play what that server sends.
// ABOUTME: Checks the server address and key before it starts anything, and labels the timing unqualified.
use crate::{
    Arguments, clock, decibel_text,
    link::{Failure, Link, Outcome, Target, text},
    pairing,
    play::{INTERRUPTED, announce_output, prepare_output, stop_own},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use earph_credentials::NativeConfig;
use earph_engine::{SendspinConfiguration, sendspin_credentials::StoreConfiguration};
use earph_network::sendspin::SendspinConnectPermit;
use serde_json::{Value, json};
use std::{
    io::{IsTerminal, Write},
    net::IpAddr,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const USAGE: &str = "usage: earph sendspin --server ws://IP:PORT/PATH --server-key KEY [--output NAME] [--volume DB]";
/// This player offers one format: 48 kHz, stereo, 16-bit.
const RATE: u64 = 48000;
/// Each credential store operation gets this long before it counts as failed.
const STORE_TIMEOUT: Duration = Duration::from_secs(10);

/// Reads the server's numeric address, then checks the URL against the Engine's connection rules.
pub fn address(server: &str) -> Outcome<IpAddr> {
    if server.starts_with("wss://") {
        return Err(Failure::usage(
            "a wss:// server is refused: this player has no TLS. Use ws://IP:PORT/PATH",
        ));
    }
    // The URL may hold a password, so no message repeats it.
    let shape = || {
        Failure::usage(
            "--server must look like ws://IP:PORT/PATH, with a numeric IP address and a port",
        )
    };
    let (authority, _) = server
        .strip_prefix("ws://")
        .and_then(|rest| rest.split_once('/'))
        .ok_or_else(shape)?;
    let (host, _) = authority.rsplit_once(':').ok_or_else(shape)?;
    let ip: IpAddr = host.trim_matches(['[', ']']).parse().map_err(|_| shape())?;
    let owner = earph_core::ids::OwnerId::new("earph-cli")
        .map_err(|_| Failure::engine("cannot check the server address"))?;
    // The Engine applies this same check when it connects.
    let expires = Instant::now() + Duration::from_secs(60);
    SendspinConnectPermit::new(owner, 1, 1, expires, server, vec![ip]).map_err(|_| {
        Failure::usage(format!(
            "the Engine refuses the server address {ip}. It refuses loopback, link-local, 100.64.0.0/10, and other special-purpose addresses. Use the server's local network address as ws://IP:PORT/PATH"
        ))
    })?;
    Ok(ip)
}
/// Decodes a 32-byte key from 43 characters of unpadded URL-safe base64, the form of a Sendspin server ID.
pub fn decode_key(encoded: &str) -> Option<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(encoded).ok()?.try_into().ok()
}
/// Encodes a public key in the form that `--server-key` accepts.
pub fn encode_key(key: &[u8; 32]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}
/// Names the credential helper, the native store, and the store's lock file.
pub fn store(arguments: &Arguments) -> Outcome<StoreConfiguration> {
    let helper = match &arguments.credential_helper {
        Some(path) => PathBuf::from(path),
        // Cargo builds the helper beside this executable.
        None => std::env::current_exe()
            .map_err(|error| Failure::engine(format!("cannot locate this executable: {error}")))?
            .with_file_name("earph-credential-helper"),
    };
    let helper = std::fs::canonicalize(&helper).map_err(|_| {
        Failure::usage(format!(
            "cannot find the credential helper at {}; pass --credential-helper PATH",
            helper.display()
        ))
    })?;
    let mac = cfg!(target_os = "macos");
    // Linux keeps the identity in the keyring on the session bus. A Mac has no default keychain here.
    let named = arguments.credential_store.clone().or_else(|| {
        std::env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|_| !mac)
    });
    let named = named.filter(|name| !name.is_empty()).ok_or_else(|| {
        Failure::usage(if mac {
            "pass --credential-store KEYCHAIN-FILE: the keychain that holds the Sendspin identity"
        } else {
            "pass --credential-store D-BUS-ADDRESS: the bus of the keyring that holds the Sendspin identity. DBUS_SESSION_BUS_ADDRESS is unset"
        })
    })?;
    let lock_path = match &arguments.credential_lock {
        Some(path) => PathBuf::from(path),
        None => arguments.runtime.join("sendspin-credentials.lock"),
    };
    if !lock_path.is_absolute() || mac && !PathBuf::from(&named).is_absolute() {
        return Err(Failure::usage(
            "--credential-lock and a keychain file must be absolute paths",
        ));
    }
    Ok(StoreConfiguration {
        helper,
        native: if mac {
            NativeConfig::Keychain { path: named.into() }
        } else {
            NativeConfig::SecretService { address: named }
        },
        lock_path,
        operation_timeout: STORE_TIMEOUT,
    })
}

pub fn run(arguments: &Arguments, interrupted: &AtomicBool) -> Outcome<u8> {
    match arguments.positional.as_slice() {
        [] => {}
        [step] if step == "init" => return pairing::init(arguments),
        [step] if step == "token" => return pairing::token(arguments),
        [step] if step == "pair" => return pairing::pair(arguments, interrupted),
        _ => return Err(Failure::usage(USAGE)),
    }
    let Some(server) = &arguments.server else {
        return Err(Failure::usage(format!("--server is needed\n{USAGE}")));
    };
    let pinned_address = address(server)?;
    // No message repeats the key.
    let server_public_key = arguments
        .server_key
        .as_deref()
        .and_then(decode_key)
        .ok_or_else(|| {
            Failure::usage(
                "--server-key must be the server's public key: 43 characters of URL-safe base64",
            )
        })?;
    let credentials = store(arguments)?;
    let mut link = Link::open_sendspin(
        &arguments.runtime,
        SendspinConfiguration {
            server_url: server.clone(),
            pinned_address,
            server_public_key,
            credentials,
        },
    )?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let session = format!("cli-sendspin-{:x}-{nanos:x}", std::process::id());
    let result = stream(&mut link, arguments, &session, interrupted);
    if result.is_err() {
        stop_own(&mut link, &session);
    }
    let closed = link.close();
    let code = result?;
    closed?;
    Ok(code)
}

fn stream(
    link: &mut Link,
    arguments: &Arguments,
    session: &str,
    interrupted: &AtomicBool,
) -> Outcome<u8> {
    let state = link.state()?;
    prepare_output(link, arguments, &state)?;
    if let Some(millidecibels) = arguments.volume_millidecibels {
        link.mutate(
            json!({"method": "volume.set", "millidecibels": millidecibels, "ramp_frames": 960}),
            Target::None,
        )?;
    }
    let state = link.state()?;
    announce_output(&state);
    link.mutate(
        json!({"method": "sources.sendspin.start", "device_id": state["output"]["device_id"]}),
        Target::New(session),
    )?;
    let terminal = std::io::stdout().is_terminal();
    let finish = |line: &str| {
        if terminal && !line.is_empty() {
            println!();
        }
    };
    let (mut line, mut shown, mut unmuted) = (String::new(), None, false);
    loop {
        if interrupted.load(Ordering::Relaxed) {
            finish(&line);
            stop_own(link, session);
            println!("result   interrupted");
            return Ok(INTERRUPTED);
        }
        // This command hosts its Engine, so no answer means another command shut that Engine down.
        let Ok(state) = link.state() else {
            finish(&line);
            println!("result   ended: the Engine stopped");
            return Ok(0);
        };
        if state["session_id"] == session {
            // The session starts muted. The Engine allows the unmute once the server knows this player.
            let prepared = matches!(state["transport"].as_str(), Some("buffering" | "playing"));
            if prepared && !unmuted {
                unmuted = link
                    .mutate(json!({"method": "mute.set", "muted": false}), Target::None)
                    .is_ok();
            }
            line = status_line(&state);
            let change = Some((phase(&state), state["muted"] == true));
            if terminal {
                print!("\r{line}\x1b[K");
                let _ = std::io::stdout().flush();
            } else if change != shown {
                println!("{line}");
            }
            shown = change;
        } else if state["session_id"].is_null() {
            finish(&line);
            if state["transport"] == "failed" {
                return Err(failure(&text(&state["error"])));
            }
            // The Engine reports no reason for a clean end.
            println!(
                "result   ended: the server closed the connection, or another command stopped the stream"
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

/// Explains a failed stream and, for a credential problem, what to do next.
fn failure(code: &str) -> Failure {
    Failure::playback(match code {
        "AUTH_REQUIRED" => "no Sendspin identity is stored in the credential store (AUTH_REQUIRED). Earph has no command yet to create or pair one; see PLAYER-CLI.md".to_owned(),
        "AUTH_DENIED" => "access was denied (AUTH_DENIED). Unlock the credential store, then check that --server-key names the server this player is paired with".to_owned(),
        "ASSET_INVALID" => "the connection failed (ASSET_INVALID): the server did not answer, or it sent something this player refuses".to_owned(),
        _ => format!(
            "the Sendspin stream failed: {code}\nThe Engine prints a device fault as a `backend_fault` line."
        ),
    })
}
/// The refusal for a control that a live stream cannot honor.
pub fn unsupported(command: &str) -> Failure {
    Failure::playback(format!(
        "{command} is not supported for a Sendspin stream; use mute or stop"
    ))
}
fn phase(state: &Value) -> &'static str {
    match (
        state["transport"].as_str(),
        state["sendspin_playback"]["phase"].as_str(),
    ) {
        (Some("playing"), _) => "playing",
        (_, Some("awaiting_stream")) => "waiting for the server to start a stream",
        _ => "connecting",
    }
}
/// One line: the phase, the audio handed to the output, the volume, and the timing label.
/// It shows no audible position, because nothing measures one.
pub fn status_line(state: &Value) -> String {
    let playback = &state["sendspin_playback"];
    let mut line = format!(
        "{}  {} sent to the output  {}",
        phase(state),
        clock(playback["submitted_frames"].as_u64().unwrap_or(0), RATE),
        decibel_text(&state["software_gain_db"])
    );
    if state["muted"] == true {
        line.push_str("  muted");
    }
    line.push_str("  sync unqualified");
    if let Some(error) = playback["timing"]["start_error_us"].as_i64() {
        line.push_str(&format!("  start error {:+.1} ms", error as f64 / 1000.0));
    }
    line
}
