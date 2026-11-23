// ABOUTME: Exercises HTTP source authority through the real owner command socket.
// ABOUTME: Uses private runtimes and ALSA file-to-null output without physical devices.
use super::*;
use std::{
    net::{IpAddr, TcpListener, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

struct MediaServer {
    origin: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    closed: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<AtomicBool>,
    response_ready: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl MediaServer {
    fn new(body: Vec<u8>) -> Self {
        Self::with_framing(body, false)
    }
    fn delayed(body: Vec<u8>) -> Self {
        let server = Self::new(body);
        server.response_ready.store(false, Ordering::Release);
        server
    }
    fn release_response(&self) {
        self.response_ready.store(true, Ordering::Release);
    }
    fn with_framing(body: Vec<u8>, chunked: bool) -> Self {
        Self::with_mode(body, chunked, false, false)
    }
    fn stalled() -> Self {
        Self::with_mode(Vec::new(), false, true, false)
    }
    fn with_mode(body: Vec<u8>, chunked: bool, stall: bool, incomplete: bool) -> Self {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:9").unwrap();
        let ip = socket.local_addr().unwrap().ip();
        assert!(matches!(ip, IpAddr::V4(ip) if ip.is_private()));
        let listener = TcpListener::bind((ip, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = requests.clone();
        let closed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peer_closed = closed.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let response_ready = Arc::new(AtomicBool::new(true));
        let response = response_ready.clone();
        let join = thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut header = Vec::new();
                        let mut byte = [0];
                        while !header.ends_with(b"\r\n\r\n") && header.len() < 8192 {
                            if stream.read(&mut byte).unwrap_or(0) == 0 {
                                break;
                            }
                            header.push(byte[0]);
                        }
                        count.fetch_add(1, Ordering::AcqRel);
                        while !response.load(Ordering::Acquire)
                            && !cancelled.load(Ordering::Acquire)
                        {
                            thread::sleep(Duration::from_millis(1));
                        }
                        if cancelled.load(Ordering::Acquire) {
                            break;
                        }
                        if stall {
                            stream
                                .set_read_timeout(Some(Duration::from_millis(20)))
                                .unwrap();
                            while !cancelled.load(Ordering::Acquire) {
                                match stream.read(&mut byte) {
                                    Ok(0) => {
                                        peer_closed.fetch_add(1, Ordering::AcqRel);
                                        break;
                                    }
                                    Ok(_) => {}
                                    Err(error)
                                        if matches!(
                                            error.kind(),
                                            std::io::ErrorKind::WouldBlock
                                                | std::io::ErrorKind::TimedOut
                                        ) => {}
                                    Err(_) => break,
                                }
                            }
                            continue;
                        }
                        if chunked {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
                            );
                            for chunk in body.chunks(701) {
                                let _ = write!(stream, "{:x}\r\n", chunk.len());
                                let _ = stream.write_all(chunk);
                                let _ = stream.write_all(b"\r\n");
                            }
                            let _ = stream.write_all(b"0\r\n\r\n");
                        } else {
                            let _ = write!(
                                stream,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len() + usize::from(incomplete)
                            );
                            let _ = stream.write_all(&body);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("network fixture accept: {error}"),
                }
            }
        });
        Self {
            origin,
            requests,
            closed,
            stop,
            response_ready,
            join: Some(join),
        }
    }
    fn pins(&self) -> Value {
        json!([self
            .origin
            .strip_prefix("http://")
            .unwrap()
            .split(':')
            .next()
            .unwrap()])
    }
}
impl Drop for MediaServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}

fn flac(directory: &Path, frames: usize) -> (Vec<u8>, Vec<u8>) {
    let wave_file = directory.join("network.wav");
    let flac_file = directory.join("network.flac");
    let samples = wave(&wave_file, 0, frames);
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&wave_file)
        .args(["-c:a", "flac"])
        .arg(&flac_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (fs::read(flac_file).unwrap(), samples)
}
fn formatted_flac(
    directory: &Path,
    bits: u32,
    channels: usize,
    unknown: bool,
) -> (Vec<u8>, Vec<u8>) {
    let width = (bits / 8) as usize;
    let frames = 4097;
    let mut samples = Vec::with_capacity(frames * channels * width);
    for index in 0..frames * channels {
        let value = (index as i64 * 7919 % (1_i64 << bits) - (1_i64 << (bits - 1))) as i32;
        samples.extend(&value.to_le_bytes()[..width]);
    }
    let source = directory.join("typed.pcm");
    fs::write(&source, &samples).unwrap();
    let encoded = directory.join("typed.flac");
    let output = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            if bits == 16 { "s16le" } else { "s24le" },
            "-ar",
            "48000",
            "-ac",
        ])
        .arg(channels.to_string())
        .arg("-i")
        .arg(source)
        .args(["-c:a", "flac", "-frame_size", "4096"])
        .arg(&encoded)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut body = fs::read(encoded).unwrap();
    if unknown {
        assert_eq!(&body[..4], b"fLaC");
        assert_eq!(body[4] & 0x7f, 0);
        body[21] &= 0xf0;
        body[22..26].fill(0);
    }
    (body, samples)
}

fn grant(controller: &Controller, server: &MediaServer) {
    controller.apply(json!({"method":"sources.grants.replace", "expected_generation":0,
        "grants":{"origins":[{"origin":server.origin,"pins":server.pins(),"roots_der":[]}]}, "lifetime_ms":60_000}));
}
fn select_network_output(controller: &Controller) {
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.caps.set","software_millidecibels":0,"hardware_millidecibels":null}));
    controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
    consent(controller);
}
fn activate_network_graph(controller: &Controller, graph: Value) {
    let (_, prepared) = prepare_graph(controller, graph);
    let commit=controller.document(json!({"method":"graph.commit","token":prepared["token"],"graph_hash":prepared["preview"]["graph_hash"]}),None);
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
}
fn network_start(controller: &Controller, server: &MediaServer) -> Value {
    controller.apply(json!({"method":"sources.stage_uri", "uri":format!("{}/owned.flac",server.origin), "metadata":"private opaque metadata"}));
    let sources = controller.query(json!({"method":"sources.get"}));
    let document = controller.document(
        json!({"method":"sources.start_http_flac", "media_id":sources["staged"]["media_id"],
        "staging_generation":sources["generation"], "grant_generation":1,
        "device_id":"acceptance-output", "takeover":false}),
        Some(&format!("network-{}", controller.id())),
    );
    let response = controller.send(&document);
    assert_eq!(response["receipt"]["status"], "accepted", "{response}");
    document
}

#[test]
fn network_owner_real_flac_requires_graph_and_captures_exact_pcm_without_queue_entries() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, expected) = flac(&directory, 1025);
    let server = MediaServer::new(body);
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.caps.set","software_millidecibels":0,"hardware_millidecibels":null}));
    controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
    consent(controller);
    grant(controller, &server);
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
    let start = network_start(controller, &server);
    let ready = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    assert_eq!(ready["network_playback"]["total_frames"], 1025);
    assert!(ready["playback"].is_null());
    assert!(ready["active_entry_id"].is_null());
    assert_no_capture(&capture);
    let denied =
        controller.send(&controller.document(json!({"method":"mute.set","muted":false}), None));
    assert_eq!(denied["receipt"]["status"], "rejected", "{denied}");
    let (_, prepared) = prepare_graph(controller, delay_gain_graph(0, 1.0));
    let commit = controller.document(json!({"method":"graph.commit","token":prepared["token"],"graph_hash":prepared["preview"]["graph_hash"]}),None);
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let started = controller.settled(&start);
    assert_eq!(started["receipt"]["activation_frame"], 0);
    let completed = controller.wait(|state| state["network_playback"]["lifecycle"] == "completed");
    assert_eq!(
        completed["network_playback"]["source_position_frames"],
        1025
    );
    assert_eq!(fs::read(&capture).unwrap(), expected);
    assert_eq!(
        controller.query(json!({"method":"queue.get","offset":0,"limit":64}))["entries"],
        json!([])
    );
}

fn network_host() -> (Host, PathBuf, PathBuf) {
    network_host_using(None)
}
fn network_host_using(override_config: Option<&str>) -> (Host, PathBuf, PathBuf) {
    network_host_configure(|config, _capture| {
        if let Some(text) = override_config {
            fs::write(config, text).unwrap();
        }
    })
}
fn network_host_configure(configure: impl FnOnce(&Path, &Path)) -> (Host, PathBuf, PathBuf) {
    let directory = directory();
    let (config, capture) = configuration(&directory);
    configure(&config, &capture);
    let runtime = PathBuf::from(std::env::var_os("HOME").unwrap()).join(format!(
        ".en-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(runtime.as_os_str().len() <= 60);
    let mut host = Host::start_at(&config, runtime);
    let mut errors = host.child.stderr.take().unwrap();
    let mut log = fs::File::create(directory.join("owner.stderr")).unwrap();
    host.stderr_log = Some(thread::spawn(move || {
        std::io::copy(&mut errors, &mut log).unwrap();
    }));
    (host, directory, capture)
}

#[test]
fn network_owner_advertises_volatile_authority_and_rejects_real_policy_before_commit() {
    let (host, _directory, capture) = network_host();
    let controller = &host.controller;
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert_eq!(network["enabled"], true);
    assert_eq!(network["grant_generation"], 0);
    let invalid = controller.document(json!({"method":"sources.grants.replace", "expected_generation":0,
        "grants":{"origins":[{"origin":"http://127.0.0.1:43210", "pins":["127.0.0.1"], "roots_der":[]}]},
        "lifetime_ms":60_000}), None);
    let rejected = controller.send(&invalid);
    assert_eq!(rejected["receipt"]["status"], "rejected", "{rejected}");
    let invalid_roots = controller.document(json!({"method":"sources.grants.replace", "expected_generation":0,
        "grants":{"origins":[{"origin":"https://invalid-roots.test", "pins":["192.168.1.2"], "roots_der":[[1,2,3]]}]},
        "lifetime_ms":60_000}), None);
    let rejected = controller.send(&invalid_roots);
    assert_eq!(rejected["receipt"]["status"], "rejected", "{rejected}");
    let after = controller.query(json!({"method":"sources.network.get"}));
    assert_eq!(after["grant_generation"], 0);
    assert_eq!(after["origin_count"], 0);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_revoke_retires_ready_source_and_exact_replay_preserves_generation() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, _) = flac(&directory, 8193);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    let revoke = controller.document(
        json!({"method":"sources.grants.revoke","expected_generation":1}),
        None,
    );
    assert_eq!(controller.send(&revoke)["receipt"]["status"], "accepted");
    let settled = controller.settled(&revoke);
    assert_eq!(controller.send(&revoke), settled);
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert_eq!(network["grant_generation"], 2);
    assert!(network["active"].is_null());
    assert!(network["closing"].is_null());
    assert_eq!(controller.send(&start)["receipt"]["status"], "cancelled");
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_expires_authority_on_idle_paused_turn_without_fetching_again() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, _) = flac(&directory, 8193);
    let server = MediaServer::new(body);
    select_network_output(controller);
    controller.apply(json!({"method":"sources.grants.replace","expected_generation":0,
        "grants":{"origins":[{"origin":server.origin,"pins":server.pins(),"roots_der":[]}]},"lifetime_ms":750}));
    network_start(controller, &server);
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    assert_eq!(controller.send(&pause)["receipt"]["status"], "accepted");
    controller.settled(&pause);
    thread::sleep(Duration::from_millis(900));
    let state = controller.wait(|state| state["session_id"].is_null());
    assert_eq!(state["error"], "AUTH_DENIED");
    assert_eq!(state["network_playback"]["lifecycle"], "failed");
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert!(network["active"].is_null());
    assert!(network["closing"].is_null());
    assert_eq!(network["origin_count"], 0);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_known_and_chunked_unknown_16_24_bit_mono_stereo_preserve_every_sample() {
    for (bits, channels, unknown) in [(16, 1, false), (24, 1, true), (16, 2, true), (24, 2, false)]
    {
        let (host, directory, capture) = network_host();
        let controller = &host.controller;
        let (body, expected) = formatted_flac(&directory, bits, channels, unknown);
        let server = MediaServer::with_framing(body, unknown);
        select_network_output(controller);
        grant(controller, &server);
        let start = network_start(controller, &server);
        let ready = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
        let total = if unknown { Value::Null } else { json!(4097) };
        assert_eq!(ready["network_playback"]["total_frames"], total);
        assert_no_capture(&capture);
        let mut graph = delay_gain_graph(0, 1.0);
        for node in graph["nodes"].as_array_mut().unwrap() {
            node["format"]["channels"] = json!(if channels == 1 { "mono" } else { "stereo" });
        }
        activate_network_graph(controller, graph);
        controller.apply(json!({"method":"mute.set","muted":false}));
        controller.settled(&start);
        let ended = controller.wait(|state| state["session_id"].is_null());
        assert_eq!(ended["network_playback"]["total_frames"], total);
        assert_eq!(ended["network_playback"]["source_position_frames"], 4097);
        assert_eq!(ended["network_playback"]["submitted_frames"], 4097);
        assert_eq!(
            fs::read(capture).unwrap(),
            expected,
            "{bits} bit/{channels} channels/unknown={unknown}"
        );
    }
}

#[test]
fn network_owner_bad_media_preserves_first_failure_through_retirement() {
    let (host, _directory, capture) = network_host();
    let controller = &host.controller;
    let server = MediaServer::new(b"bad!".to_vec());
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    let state = controller.wait(|state| state["session_id"].is_null() && !state["error"].is_null());
    assert_eq!(state["error"], "ASSET_INVALID", "{state}");
    let receipt = controller.terminal(&start);
    assert_eq!(receipt["receipt"]["status"], "failed");
    assert_eq!(receipt["receipt"]["error"]["code"], "ASSET_INVALID");
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_stop_during_http_startup_settles_after_peer_closes() {
    let (host, _directory, capture) = network_host();
    let controller = &host.controller;
    let server = MediaServer::stalled();
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let stop = controller.document(json!({"method":"sessions.stop"}), None);
    assert_eq!(controller.send(&stop)["receipt"]["status"], "accepted");
    controller.settled(&stop);
    assert_eq!(server.closed.load(Ordering::Acquire), 1);
    let state = controller.state();
    assert!(state["session_id"].is_null());
    assert!(state["network_playback"].is_null());
    assert_eq!(controller.send(&start)["receipt"]["status"], "cancelled");
    assert_no_capture(&capture);
}

#[test]
fn network_owner_local_network_local_takeover_keeps_real_queue_selection_and_readiness() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let local = directory.join("local.wav");
    wave(&local, 0, 1025);
    select_network_output(controller);
    append_selected(controller, &local);
    let local_start = start(controller);
    controller.wait(|state| !state["playback"].is_null());
    let queue = controller.query(json!({"method":"queue.get","offset":0,"limit":64}));
    let (body, _) = flac(&directory, 4097);
    let server = MediaServer::new(body);
    grant(controller, &server);
    controller.apply(json!({"method":"sources.stage_uri","uri":format!("{}/owned.flac",server.origin),"metadata":"takeover"}));
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
    assert_eq!(controller.state()["source"], "local_file");
    let sources = controller.query(json!({"method":"sources.get"}));
    let network=controller.document(json!({"method":"sources.start_http_flac","media_id":sources["staged"]["media_id"],
        "staging_generation":sources["generation"],"grant_generation":1,"device_id":"acceptance-output","takeover":true}),Some("takeover-http"));
    assert_eq!(controller.send(&network)["receipt"]["status"], "accepted");
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    assert_eq!(
        controller.query(json!({"method":"queue.get","offset":0,"limit":64})),
        queue
    );
    let local_again=controller.document(json!({"method":"sessions.start","source":"local_file","device_id":"acceptance-output","takeover":true,"seekable":true}),Some("takeover-local"));
    assert_eq!(
        controller.send(&local_again)["receipt"]["status"],
        "accepted"
    );
    let ready =
        controller.wait(|state| state["source"] == "local_file" && !state["playback"].is_null());
    assert!(ready["network_playback"].is_null());
    assert_eq!(ready["active_entry_id"], queue["entries"][0]["id"]);
    assert_eq!(
        controller.query(json!({"method":"queue.get","offset":0,"limit":64})),
        queue
    );
    assert_eq!(
        controller.send(&local_start)["receipt"]["status"],
        "cancelled"
    );
    assert_no_capture(&capture);
}

#[test]
fn network_owner_nonseekable_pause_mute_resume_preserves_pcm_and_src_tail_once() {
    for src in [false, true] {
        let (host, directory, capture) = network_host();
        let controller = &host.controller;
        let mut gate = CaptureGate::new(&capture);
        let (body, original) = flac(&directory, 65537);
        let server = MediaServer::new(body);
        let expected = if src {
            src_reference(&original)
        } else {
            original
        };
        select_network_output(controller);
        if src {
            controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
        }
        grant(controller, &server);
        let start = network_start(controller, &server);
        let ready = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
        let source_epochs = ready["network_playback"]["source_epochs"].clone();
        activate_network_graph(
            controller,
            if src {
                graph_fixtures::src_roundtrip()
            } else {
                delay_gain_graph(0, 1.0)
            },
        );
        controller.apply(json!({"method":"mute.set","muted":false}));
        gate.wait_full();
        controller.settled(&start);
        let pause = controller.document(json!({"method":"sessions.pause"}), None);
        assert_eq!(controller.send(&pause)["receipt"]["status"], "accepted");
        gate.release();
        controller.settled(&pause);
        let paused = controller.state();
        assert_eq!(paused["network_playback"]["source_epochs"], source_epochs);
        assert!(
            paused["network_playback"]["source_position_frames"]
                .as_u64()
                .unwrap()
                > 0
        );
        controller.apply(json!({"method":"mute.set","muted":true}));
        let resume = controller.document(json!({"method":"sessions.resume"}), None);
        assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
        controller.apply(json!({"method":"mute.set","muted":false}));
        controller.settled(&resume);
        let ended = controller.wait(|state| state["session_id"].is_null());
        assert!(ended["error"].is_null(), "{ended}");
        assert_eq!(ended["network_playback"]["source_position_frames"], 65537);
        assert_eq!(ended["network_playback"]["source_epochs"], source_epochs);
        assert_eq!(server.requests.load(Ordering::Acquire), 1);
        assert_eq!(gate.finish(), expected, "src={src}");
    }
}

#[test]
fn network_owner_native_fault_retires_source_and_revokes_output_consent() {
    let (host, directory, _capture) = network_host_using(Some(
        "pcm.null { type null }\npcm.earph_capture { type file slave.pcm null file \"/dev/full\" format raw }\n",
    ));
    let controller = &host.controller;
    let (body, _) = flac(&directory, 65537);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    network_start(controller, &server);
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    activate_network_graph(controller, delay_gain_graph(0, 1.0));
    controller.apply(json!({"method":"mute.set","muted":false}));
    let failed =
        controller.wait(|state| state["session_id"].is_null() && state["error"] == "OUTPUT_FAULT");
    assert_eq!(failed["output_consented"], false);
    assert_eq!(failed["muted"], true);
    assert_eq!(failed["network_playback"]["lifecycle"], "failed");
    assert!(
        failed["network_playback"]["source_position_frames"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
}

#[test]
fn network_owner_strict_preservation_refuses_before_any_http_connection() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, _) = flac(&directory, 4097);
    let server = MediaServer::new(body);
    select_fixed(controller);
    grant(controller, &server);
    controller.apply(json!({"method":"sources.stage_uri","uri":format!("{}/owned.flac",server.origin),"metadata":"strict refusal"}));
    let sources = controller.query(json!({"method":"sources.get"}));
    let request=controller.document(json!({"method":"sources.start_http_flac","media_id":sources["staged"]["media_id"],
        "staging_generation":sources["generation"],"grant_generation":1,"device_id":"acceptance-output","takeover":false}),Some("strict-http"));
    let denied = controller.send(&request);
    assert_eq!(denied["receipt"]["status"], "rejected", "{denied}");
    assert_eq!(
        denied["receipt"]["error"]["code"],
        "FIDELITY_POLICY_CONFLICT"
    );
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_shutdown_during_startup_waits_for_transport_retirement() {
    let (mut host, _directory, capture) = network_host();
    let controller = &host.controller;
    let server = MediaServer::stalled();
    select_network_output(controller);
    grant(controller, &server);
    network_start(controller, &server);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let shutdown = controller.document(json!({"method":"owner.shutdown"}), None);
    let reply = controller.send(&shutdown);
    assert_eq!(reply["receipt"]["status"], "accepted", "{reply}");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(server.closed.load(Ordering::Acquire), 1);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_http_framing_failure_discards_buffered_pcm_without_native_submission() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, _) = flac(&directory, 4097);
    let server = MediaServer::with_mode(body, false, false, true);
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    let failed =
        controller.wait(|state| state["session_id"].is_null() && state["error"] == "ASSET_INVALID");
    assert_eq!(controller.terminal(&start)["receipt"]["status"], "failed");
    assert_eq!(failed["output_consented"], true);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_network_takeover_retires_old_context_before_new_readiness() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, _) = flac(&directory, 8193);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    let first = network_start(controller, &server);
    let before = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    controller.apply(json!({"method":"sources.stage_uri","uri":format!("{}/second.flac",server.origin),"metadata":"second source"}));
    let staged = controller.query(json!({"method":"sources.get"}));
    let start=controller.document(json!({"method":"sources.start_http_flac","media_id":staged["staged"]["media_id"],
        "staging_generation":staged["generation"],"grant_generation":1,"device_id":"acceptance-output","takeover":true}),Some("second-http-session"));
    assert_eq!(controller.send(&start)["receipt"]["status"], "accepted");
    let ready = controller.wait(|state| {
        state["network_playback"]["binding"]["source_generation"] == 2
            && state["network_playback"]["lifecycle"] == "ready"
    });
    assert_ne!(
        ready["network_playback"]["binding"],
        before["network_playback"]["binding"]
    );
    assert_eq!(
        ready["network_playback"]["binding"]["media_id"],
        staged["staged"]["media_id"]
    );
    assert_eq!(controller.send(&first)["receipt"]["status"], "cancelled");
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert!(network["closing"].is_null());
    assert_eq!(server.requests.load(Ordering::Acquire), 2);
    assert_no_capture(&capture);
}

#[test]
fn network_owner_checkpoint_failure_starts_no_transport_and_restart_restores_no_authority() {
    let (mut host, directory, capture) = network_host();
    let controller = &host.controller;
    let source = directory.join("retained-local.wav");
    wave(&source, 0, 65);
    append_selected(controller, &source);
    let queue = controller.query(json!({"method":"queue.get","offset":0,"limit":64}));
    let server = MediaServer::new(b"unused".to_vec());
    select_network_output(controller);
    grant(controller, &server);
    controller.apply(json!({"method":"sources.stage_uri","uri":format!("{}/owned.flac",server.origin),"metadata":"private staged value"}));
    let staged = controller.query(json!({"method":"sources.get"}));
    let request=controller.document(json!({"method":"sources.start_http_flac","media_id":staged["staged"]["media_id"],
        "staging_generation":staged["generation"],"grant_generation":1,"device_id":"acceptance-output","takeover":false}),Some("checkpoint-http"));
    let runtime = controller.runtime.clone();
    let checkpoint = runtime.join("queue.json");
    let preserved = runtime.join("queue.before-failure.json");
    let durable = fs::read_to_string(&checkpoint).unwrap();
    assert!(!durable.contains(&server.origin));
    assert!(!durable.contains("private staged value"));
    fs::rename(&checkpoint, &preserved).unwrap();
    fs::create_dir(&checkpoint).unwrap();
    let reply = controller.send(&request);
    assert_eq!(reply["receipt"]["status"], "failed", "{reply}");
    assert!(!host.child.wait().unwrap().success());
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
    assert_no_capture(&capture);
    fs::rename(&checkpoint, runtime.join("queue.failure-directory")).unwrap();
    fs::rename(&preserved, &checkpoint).unwrap();
    let restarted = Host::start_at(&directory.join("alsa.conf"), runtime);
    let controller = &restarted.controller;
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert_eq!(network["grant_generation"], 0);
    assert_eq!(network["origin_count"], 0);
    assert!(network["active"].is_null());
    assert!(network["closing"].is_null());
    assert!(controller.query(json!({"method":"sources.get"}))["staged"].is_null());
    assert_eq!(controller.state()["output_consented"], false);
    assert_eq!(
        controller.query(json!({"method":"queue.get","offset":0,"limit":64})),
        queue
    );
}

#[test]
fn network_owner_revoking_unused_network_grants_does_not_cancel_local_playback() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let source = directory.join("unrelated-local.wav");
    let expected = wave(&source, 0, 1025);
    select_network_output(controller);
    append_selected(controller, &source);
    let local = start(controller);
    controller.wait(|state| !state["playback"].is_null());
    let server = MediaServer::new(b"unused".to_vec());
    grant(controller, &server);
    controller.apply(json!({"method":"sources.grants.revoke","expected_generation":1}));
    controller.apply(json!({"method":"mute.set","muted":false}));
    controller.settled(&local);
    let ended = controller.wait(|state| state["transport"] == "idle");
    assert!(ended["error"].is_null());
    assert_eq!(fs::read(capture).unwrap(), expected);
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
}

#[test]
fn network_owner_reserves_retirement_and_start_before_accepting_a_backpressured_takeover() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let mut gate = CaptureGate::new(&capture);
    let (body, expected) = flac(&directory, 65537);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    let first = network_start(controller, &server);
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    activate_network_graph(controller, delay_gain_graph(0, 1.0));
    controller.apply(json!({"method":"mute.set","muted":false}));
    gate.wait_full();
    controller.settled(&first);
    for _ in 0..31 {
        controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
    }
    controller.apply(json!({"method":"sources.stage_uri","uri":format!("{}/next.flac",server.origin),"metadata":"capacity candidate"}));
    let staged = controller.query(json!({"method":"sources.get"}));
    let before = controller.state();
    let request=controller.document(json!({"method":"sources.start_http_flac","media_id":staged["staged"]["media_id"],
        "staging_generation":staged["generation"],"grant_generation":1,"device_id":"acceptance-output","takeover":true}),Some("capacity-takeover"));
    let reply = controller.send(&request);
    assert_eq!(reply["receipt"]["status"], "failed", "{reply}");
    assert_eq!(reply["receipt"]["error"]["code"], "SOURCE_BUSY");
    let after = controller.state();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["session_id"], before["session_id"]);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    gate.release();
    controller.wait(|state| state["session_id"].is_null());
    assert_eq!(gate.finish(), expected);
}

#[test]
fn network_owner_grant_revoke_uses_safety_capacity_when_ordinary_controls_are_full() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let mut gate = CaptureGate::new(&capture);
    let (body, expected) = flac(&directory, 65537);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    activate_network_graph(controller, delay_gain_graph(0, 1.0));
    controller.apply(json!({"method":"mute.set","muted":false}));
    gate.wait_full();
    controller.settled(&start);
    for _ in 0..32 {
        controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
    }
    let revoke = controller.document(
        json!({"method":"sources.grants.revoke","expected_generation":1}),
        None,
    );
    assert_eq!(controller.send(&revoke)["receipt"]["status"], "accepted");
    let network = controller.query(json!({"method":"sources.network.get"}));
    assert_eq!(network["grant_generation"], 2);
    assert!(network["active"].is_null());
    assert!(!network["closing"].is_null());
    assert_eq!(controller.state()["muted"], true);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
    gate.release();
    controller.settled(&revoke);
    let ended = controller.state();
    assert!(ended["session_id"].is_null());
    assert!(ended["error"].is_null(), "{ended}");
    assert_eq!(ended["output_consented"], true);
    let captured = gate.finish();
    assert!(!captured.is_empty());
    assert!(captured.len() < expected.len());
    assert_eq!(captured, expected[..captured.len()]);
    assert_eq!(
        ended["network_playback"]["submitted_frames"]
            .as_u64()
            .unwrap()
            * 6,
        captured.len() as u64
    );
}

#[test]
fn network_owner_graph_replacement_keeps_source_epoch_and_exact_accepted_frontier() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let mut gate = CaptureGate::new(&capture);
    let (body, original) = flac(&directory, 65537);
    let server = MediaServer::new(body);
    select_network_output(controller);
    grant(controller, &server);
    let start = network_start(controller, &server);
    let ready = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    activate_network_graph(controller, delay_gain_graph(0, 1.0));
    controller.apply(json!({"method":"mute.set","muted":false}));
    gate.wait_full();
    controller.settled(&start);
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    assert_eq!(controller.send(&pause)["receipt"]["status"], "accepted");
    gate.release();
    controller.settled(&pause);
    let paused = controller.state();
    let source_frontier = paused["network_playback"]["source_position_frames"]
        .as_u64()
        .unwrap() as usize;
    let stream = &paused["network_playback"]["processing"]["streaming"];
    assert_eq!(
        stream["generated_output_frames"], stream["backend_origin_frames"],
        "fixture must replace without discarded generated output"
    );
    activate_network_graph(controller, delay_gain_graph(0, 0.5));
    let installed = controller.state();
    assert_eq!(
        installed["network_playback"]["source_epochs"],
        ready["network_playback"]["source_epochs"]
    );
    assert_eq!(
        installed["network_playback"]["source_position_frames"],
        source_frontier
    );
    assert!(installed["graph_epoch"].as_u64().unwrap() > paused["graph_epoch"].as_u64().unwrap());
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
    controller.apply(json!({"method":"mute.set","muted":false}));
    controller.settled(&resume);
    controller.wait(|state| state["session_id"].is_null());
    let mut expected = original[..source_frontier * 6].to_vec();
    for sample in original[source_frontier * 6..].as_chunks::<3>().0 {
        let value = (f64::from(integer24(sample)) * 0.5).round() as i32;
        expected.extend(&value.to_le_bytes()[..3]);
    }
    assert_eq!(gate.finish(), expected);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
}

#[test]
fn network_owner_graph_replacement_discloses_real_held_client_output_discard() {
    let (host, directory, capture) = network_host_configure(|config, capture| {
        let capture = capture
            .to_str()
            .unwrap()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        fs::write(config,format!("pcm.null {{ type null }}\npcm.rate_null {{ type rate slave.pcm null slave.rate 44100 converter \"linear\" }}\npcm.delay_plug {{ type plug slave.pcm rate_null slave.format S32_LE }}\npcm.earph_capture {{ type file slave.pcm delay_plug file \"{capture}\" format raw }}\n")).unwrap();
    });
    let controller = &host.controller;
    let mut gate = CaptureGate::new(&capture);
    let (body, original) = flac(&directory, 65537);
    let server = MediaServer::new(body);
    select_network_output(controller);
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    grant(controller, &server);
    let start = network_start(controller, &server);
    let ready = controller.wait(|state| state["network_playback"]["lifecycle"] == "ready");
    activate_network_graph(controller, graph_fixtures::src_roundtrip());
    controller.apply(json!({"method":"mute.set","muted":false}));
    gate.wait_full();
    controller.settled(&start);
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    assert_eq!(controller.send(&pause)["receipt"]["status"], "accepted");
    gate.release();
    controller.settled(&pause);
    let paused = controller.state();
    fs::write(
        directory.join("before-network-replacement.json"),
        serde_json::to_vec_pretty(&paused).unwrap(),
    )
    .unwrap();
    let source_frontier = paused["network_playback"]["source_position_frames"]
        .as_u64()
        .unwrap() as usize;
    let stream = &paused["network_playback"]["processing"]["streaming"];
    let discarded = stream["generated_output_frames"].as_u64().unwrap()
        - stream["backend_origin_frames"].as_u64().unwrap();
    assert!(
        discarded > 0,
        "real software rate slave must retain a measurable client restart delay: {paused}"
    );
    activate_network_graph(controller, delay_gain_graph(0, 0.5));
    let installed = controller.state();
    fs::write(
        directory.join("after-network-replacement.json"),
        serde_json::to_vec_pretty(&installed).unwrap(),
    )
    .unwrap();
    let boundary = &installed["network_playback"]["replacement_boundary"];
    assert_eq!(
        boundary["binding"],
        installed["network_playback"]["binding"]
    );
    assert_eq!(
        boundary["source_epochs"],
        ready["network_playback"]["source_epochs"]
    );
    assert_eq!(boundary["source_position_frames"], source_frontier);
    assert_eq!(boundary["discarded_output_frames"], discarded);
    assert_eq!(
        boundary["previous_output_epochs"],
        paused["network_playback"]["output_epochs"]
    );
    assert_eq!(
        boundary["output_epochs"],
        installed["network_playback"]["output_epochs"]
    );
    assert_eq!(
        boundary["previous_output_format"],
        paused["network_playback"]["output_format"]
    );
    assert!(installed["graph_epoch"].as_u64().unwrap() > paused["graph_epoch"].as_u64().unwrap());
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
    controller.apply(json!({"method":"mute.set","muted":false}));
    controller.settled(&resume);
    let ended = controller.wait(|state| state["session_id"].is_null());
    assert_eq!(ended["network_playback"]["replacement_boundary"], *boundary);
    let mut suffix: Vec<u8> = Vec::new();
    for sample in original[source_frontier * 6..].as_chunks::<3>().0 {
        let value = (f64::from(integer24(sample)) * 0.5 * 10_f64.powf(-48.0 / 20.0)).round() as i32;
        suffix.extend(&value.to_le_bytes()[..3]);
    }
    let captured = gate.finish();
    assert!(captured.len() >= suffix.len());
    let prefix = captured.len() - suffix.len();
    let previous_output = src_reference(&original);
    assert!(prefix > 0 && prefix <= previous_output.len());
    let mut expected = previous_output[..prefix].to_vec();
    expected.extend(suffix);
    assert_eq!(captured, expected);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
}

#[test]
fn network_owner_pause_during_http_startup_still_publishes_readiness_and_can_resume() {
    let (host, directory, capture) = network_host();
    let controller = &host.controller;
    let (body, expected) = flac(&directory, 4097);
    let server = MediaServer::delayed(body);
    select_network_output(controller);
    grant(controller, &server);
    network_start(controller, &server);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.requests.load(Ordering::Acquire) == 0 {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let preparing = controller.state();
    assert_eq!(preparing["transport"], "preparing");
    assert!(preparing["network_playback"].is_null());
    controller.apply(json!({"method":"sessions.pause"}));
    assert_eq!(controller.state()["transport"], "paused");
    assert_eq!(
        controller.query(json!({"method":"sources.network.get"}))["lifecycle"],
        "preparing"
    );
    server.release_response();
    let paused = controller.wait(|state| {
        state["transport"] == "paused" && state["network_playback"]["lifecycle"] == "paused"
    });
    assert_eq!(paused["network_playback"]["total_frames"], 4097);
    assert_eq!(paused["network_playback"]["source_position_frames"], 0);
    assert_no_capture(&capture);
    activate_network_graph(controller, delay_gain_graph(0, 1.0));
    assert_no_capture(&capture);
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
    controller.apply(json!({"method":"mute.set","muted":false}));
    controller.settled(&resume);
    controller.wait(|state| state["session_id"].is_null());
    assert_eq!(fs::read(capture).unwrap(), expected);
    assert_eq!(server.requests.load(Ordering::Acquire), 1);
}
