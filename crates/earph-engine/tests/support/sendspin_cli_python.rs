// ABOUTME: Runs `earph sendspin` as a player against the pinned Python Sendspin peer.
// ABOUTME: Checks the handshake, the declared player state, the exact capture, a clear, two stream ends, and status.
use super::{
    linux::{Bench, text},
    pairing::Reaped,
    peer,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Lines, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
};

/// The silence that leads each played pattern, so a gain ramp never touches the pattern.
const PREAMBLE_FRAMES: u64 = 4800;
/// The head start that the server gives each played pattern.
const LEAD_US: i64 = 300_000;

/// The pinned Python peer as a server: line commands in, one JSON stage per line out.
struct Server {
    child: Reaped,
    commands: Option<ChildStdin>,
    stages: Lines<BufReader<ChildStdout>>,
}
impl Server {
    fn stage(&mut self, name: &str) -> Value {
        let line = self.stages.next().expect("the Python peer ended early");
        let value: Value = serde_json::from_str(&line.unwrap()).unwrap();
        assert_eq!(value["stage"], name, "inspect python-stderr.log");
        value
    }
    /// Sends one command and returns the stage that confirms it.
    fn command(&mut self, command: Value, confirmed: &str) -> Value {
        let commands = self.commands.as_mut().unwrap();
        writeln!(commands, "{command}").unwrap();
        commands.flush().unwrap();
        self.stage(confirmed)
    }
    /// Sends silence and then one pattern, stamped `lead_us` ahead on the server clock.
    fn audio(&mut self, first_frame: u16, frames: u16, preamble_frames: u64, lead_us: i64) {
        let sent = self.command(
            json!({"command":"audio","first_frame":first_frame,"frames":frames,
                "preamble_frames":preamble_frames,"lead_us":lead_us}),
            "sent",
        );
        assert_eq!(sent["frames"], preamble_frames + u64::from(frames));
    }
}

/// The frames of the pattern that the Python peer sends for the same range.
fn pattern(first_frame: u16, frames: u16) -> Vec<u8> {
    (first_frame..first_frame + frames)
        .flat_map(|frame| {
            let left = (frame + 1).to_le_bytes();
            let right = (!frame).to_le_bytes();
            [left[0], left[1], right[0], right[1]]
        })
        .collect()
}
/// The frames that the output accepted from received chunks: no idle silence and no scheduler silence.
fn audio_frames(state: &Value) -> u64 {
    let playback = &state["sendspin_playback"];
    let count = |value: &Value| value.as_u64().unwrap();
    count(&playback["submitted_frames"])
        - count(&playback["timing"]["idle_silence_frames"])
        - count(&playback["timing"]["inserted_silence_frames"])
}
fn status(bench: &Bench) -> Value {
    let output = bench.earph(&["status", "--json"]).finish();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Runs only by name, with `EARPH_SENDSPIN_PEER` set to the launcher of the pinned Python peer.
pub fn the_player_plays_streams_from_the_pinned_python_server() {
    let launcher = std::env::var_os("EARPH_SENDSPIN_PEER")
        .expect("set EARPH_SENDSPIN_PEER to the pinned Python peer launcher");
    let bench = Bench::new();
    let interface = bench.listener.local_addr().unwrap().ip();
    let mut child = Reaped(
        Command::new(launcher)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/sendspin_player_server.py"
            ))
            .arg(interface.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(fs::File::create(bench.directory.join("python-stderr.log")).unwrap())
            .spawn()
            .unwrap(),
    );
    let stages = BufReader::new(child.0.stdout.take().unwrap()).lines();
    let mut server = Server {
        commands: child.0.stdin.take(),
        child,
        stages,
    };
    // The peer takes the server side of the synthetic pairing record that the credential fixture holds.
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    let client = earph_sendspin::noise::static_public_key(&peer::CLIENT_PRIVATE).unwrap();
    let ready = server.command(
        json!({"server_private":hex(&peer::SERVER_PRIVATE),"psk":hex(&peer::RECORD_PSK),
            "client_id":URL_SAFE_NO_PAD.encode(client)}),
        "ready",
    );
    assert_eq!(ready["server_id"], bench.key.as_str());
    assert_eq!(ready["source_hashes"].as_object().unwrap().len(), 12);
    let url = format!("ws://{interface}:{}/sendspin", ready["port"]);
    let player = bench.sendspin(&["--server", &url, "--server-key", &bench.key]);

    // The strict pinned handshake accepted the player, and the pinned model read its hello.
    let hello = server.stage("hello");
    assert_eq!(hello["roles"], json!(["player@v1"]));
    assert_eq!(hello["formats"], json!([["pcm", 2, 48000, 16]]));
    assert!(hello["buffer_capacity"].as_u64() > Some(0));
    assert_eq!(
        hello["flagged"],
        json!([]),
        "the pinned model flags the hello"
    );
    // The player declares its state once its clock is synchronized. The pinned model applies the server's rules.
    let state = server.stage("state");
    assert_eq!(state["available"], true, "client/state carries `available`");
    assert_eq!(
        state["player"],
        json!({"output_delay_ms":0,"required_lead_time_ms":150,"min_buffer_ms":200,
            "supported_commands":[],"legacy_state":null,"ignored_commands":null})
    );
    bench.wait("the command unmutes", |state| state["muted"] == false);

    // First stream. The server clears audio that waits for its anchor, as it does on a track change.
    server.command(json!({"command":"start"}), "start");
    server.audio(0, 12000, 0, 2_000_000);
    server.command(json!({"command":"clear"}), "clear");
    server.audio(12000, 24000, PREAMBLE_FRAMES, LEAD_US);
    let first_stream = PREAMBLE_FRAMES + 24000;
    bench.wait("the output accepts the audio after the clear", |state| {
        audio_frames(state) >= first_stream
    });
    let state = status(&bench);
    assert_eq!(state["source"], "sendspin");
    assert_eq!(state["transport"], "playing");
    assert_eq!(state["audible_position_frames"], Value::Null);
    assert_eq!(audio_frames(&state), first_stream, "{state}");
    let playback = &state["sendspin_playback"];
    assert_eq!(playback["phase"], "streaming");
    assert_eq!(playback["reconnects"], 0);
    let timing = &playback["timing"];
    assert_eq!(timing["qualification"], "unqualified");
    assert_eq!(timing["correction"], "none");
    assert_eq!(timing["anchored"], true, "{timing}");
    assert_eq!(timing["anchors"], 1, "the cleared audio took no anchor");
    assert_eq!(timing["underruns"], 0);
    assert_eq!(timing["dropped_frames"], 0, "{timing}");
    assert!(timing["mapping_revision"].is_u64(), "{timing}");
    assert!(timing["start_error_us"].is_i64(), "{timing}");
    assert!(timing["last_error_us"].is_i64(), "{timing}");
    // The peer stamped the first chunk 300 ms ahead on its own clock. Transit takes a part of that.
    // The clock mapping is an estimate, so the lead may exceed the head start by a small error.
    let lead = timing["first_lead_us"].as_i64().unwrap();
    assert!((LEAD_US / 2..=LEAD_US + 20_000).contains(&lead), "{timing}");
    assert_eq!(timing["min_first_lead_us"], lead);
    let wait = timing["first_wait_us"].as_u64().unwrap();
    assert!(timing["max_first_wait_us"].as_u64() >= Some(wait));

    server.command(json!({"command":"end"}), "end");
    let ended = bench.wait("the stream end returns to buffering", |state| {
        state["transport"] == "buffering"
    });
    assert_eq!(ended["sendspin_playback"]["phase"], "awaiting_stream");
    assert_eq!(ended["sendspin_playback"]["timing"]["anchored"], false);

    // Second stream on the same connection. It anchors on its own first chunk.
    server.command(json!({"command":"start"}), "start");
    server.audio(40000, 12000, PREAMBLE_FRAMES, LEAD_US);
    let both_streams = first_stream + PREAMBLE_FRAMES + 12000;
    let state = bench.wait("the output accepts the second stream", |state| {
        audio_frames(state) >= both_streams
    });
    assert_eq!(audio_frames(&state), both_streams, "{state}");
    assert_eq!(state["transport"], "playing");
    let timing = &state["sendspin_playback"]["timing"];
    assert_eq!(timing["anchors"], 2, "{timing}");
    assert_eq!(timing["dropped_frames"], 0, "{timing}");
    server.command(json!({"command":"end"}), "end");
    bench.wait("the second stream end returns to buffering", |state| {
        state["transport"] == "buffering"
    });

    // The stop cancels the connection. A peer close would start a reconnect.
    let stop = bench.earph(&["stop"]).finish();
    assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
    server.stage("closed");
    // The peer exits once its command input ends.
    server.commands = None;
    assert!(
        server.child.0.wait().unwrap().success(),
        "inspect python-stderr.log"
    );
    let output = player.finish();
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(stdout.contains("sync unqualified"), "{stdout}");
    assert!(stdout.lines().last().unwrap().starts_with("result   ended"));

    // The capture holds silence and the two played patterns, in order. The cleared pattern never reached it.
    let capture = fs::read(bench.directory.join("capture.pcm")).unwrap();
    let mut rest = &capture[..];
    for (first_frame, frames) in [(12000, 24000), (40000, 12000)] {
        let played = pattern(first_frame, frames);
        let audible = rest.iter().position(|byte| *byte != 0);
        let audible = audible.expect("the capture holds every played pattern");
        assert!(audible.is_multiple_of(4) && audible >= PREAMBLE_FRAMES as usize * 4);
        assert!(
            rest[audible..].starts_with(&played),
            "the capture differs from the pattern at frame {first_frame}"
        );
        rest = &rest[audible + played.len()..];
    }
    assert!(
        rest.iter().all(|byte| *byte == 0),
        "audio follows the last played pattern"
    );
    bench.no_engine_remains();
}
