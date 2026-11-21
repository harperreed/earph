// ABOUTME: Exercises the concrete FLAC worker through real admitted HTTP and FFmpeg media.
// ABOUTME: Checks current authority, bounded handoff, terminal causes, and process retirement.
use earph_core::{
    audio::PcmSamples,
    ids::{OwnerId, SessionId, StagedMediaId},
};
use earph_engine::network_decode::{NetworkFlacWorker, Outcome, Ownership, WorkerError};
use earph_network::{CheckedUrl, FetchError, FetchPermit, OriginGrant, SourceBinding, Terminal};
use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, TcpListener, TcpStream, UdpSocket},
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
#[allow(dead_code)]
#[path = "../../../build_support/provenance.rs"]
mod provenance;

#[test]
fn actual_network_manifest_and_sources_change_both_build_input_digests() {
    fn copy_tree(source: &std::path::Path, destination: &std::path::Path) {
        if source.is_dir() {
            fs::create_dir_all(destination).unwrap();
            for entry in fs::read_dir(source).unwrap() {
                let entry = entry.unwrap();
                copy_tree(&entry.path(), &destination.join(entry.file_name()));
            }
        } else {
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source, destination).unwrap();
        }
    }
    let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let packet = checkout
        .join("target/network-flac-worker/provenance")
        .join(format!("{}", std::process::id()));
    for (name, inputs) in [
        ("engine", provenance::ENGINE_INPUTS),
        ("harness", provenance::HARNESS_INPUTS),
    ] {
        let root = packet.join(name);
        for input in inputs {
            copy_tree(&checkout.join(input), &root.join(input));
        }
        // Copy independent runtime inputs even if a broken input list omits them.
        copy_tree(
            &checkout.join("crates/earph-network"),
            &root.join("crates/earph-network"),
        );
        let original = provenance::source_digest(&root, inputs).unwrap();
        for input in [
            "crates/earph-network/Cargo.toml",
            "crates/earph-network/src/fetch.rs",
            "crates/earph-network/src/policy.rs",
        ] {
            let path = root.join(input);
            let before = fs::read(&path).unwrap();
            let mut changed = before.clone();
            changed.push(b'\n');
            fs::write(&path, changed).unwrap();
            assert_ne!(
                provenance::source_digest(&root, inputs).unwrap(),
                original,
                "{name} misses {input}"
            );
            fs::write(&path, before).unwrap();
        }
    }
}

fn interface() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
    address
}
fn binding(generation: u64) -> SourceBinding {
    SourceBinding::new(
        OwnerId::new("network-owner").unwrap(),
        SessionId::new("network-session").unwrap(),
        StagedMediaId::new("network-media").unwrap(),
        generation,
        1,
    )
    .unwrap()
}
fn permit(url: &str, binding: &SourceBinding) -> FetchPermit {
    let checked = CheckedUrl::new(url).unwrap();
    let grant = OriginGrant::new(
        checked.origin(),
        binding.owner().clone(),
        binding.grant_generation(),
        vec![interface()],
        Instant::now() + Duration::from_secs(60),
        vec![],
    )
    .unwrap();
    FetchPermit::new(
        checked,
        binding.clone(),
        binding,
        vec![grant],
        Instant::now(),
    )
    .unwrap()
}
fn server(handler: impl FnOnce(TcpStream) + Send + 'static) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let url = format!(
        "http://worker-fixture.test:{}/owned.flac",
        listener.local_addr().unwrap().port()
    );
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("HTTP fixture accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 16384);
        }
        handler(stream);
    });
    (url, handle)
}
fn sample(index: usize, bits: u16) -> i32 {
    let limit = 1_i64 << (bits - 1);
    ((index as i64 * 7919 % (2 * limit)) - limit) as i32
}
fn fixture(bits: u16, frames: usize, known: bool) -> Vec<u8> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/network-flac-worker/fixtures")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&root).unwrap();
    let mut bytes = Vec::new();
    for i in 0..frames * 2 {
        bytes.extend(&sample(i, bits).to_le_bytes()[..usize::from(bits / 8)]);
    }
    fs::write(root.join("source.pcm"), bytes).unwrap();
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-n",
            "-f",
            if bits == 16 { "s16le" } else { "s24le" },
            "-ar",
            "48000",
            "-ac",
            "2",
            "-i",
        ])
        .arg(root.join("source.pcm"))
        .args(["-c:a", "flac", "-frame_size", "4096"])
        .arg(root.join("source.flac"))
        .output()
        .unwrap();
    fs::write(root.join("encoder.stderr"), &result.stderr).unwrap();
    assert!(
        result.status.success(),
        "FFmpeg: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut bytes = fs::read(root.join("source.flac")).unwrap();
    if !known {
        bytes[22] &= 0xf0;
        bytes[23..26].fill(0);
    }
    bytes
}
fn body(stream: &mut TcpStream, bytes: &[u8], chunked: bool) {
    if chunked {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
            .unwrap();
        for chunk in bytes.chunks(5003) {
            write!(stream, "{:x}\r\n", chunk.len()).unwrap();
            stream.write_all(chunk).unwrap();
            stream.write_all(b"\r\n").unwrap();
        }
        stream.write_all(b"0\r\n\r\n").unwrap();
    } else {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        stream.write_all(bytes).unwrap();
    }
}
fn wait_for(
    worker: &mut NetworkFlacWorker,
    current: &SourceBinding,
    predicate: impl Fn(&earph_engine::network_decode::Observation) -> bool,
) -> earph_engine::network_decode::Observation {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let observed = worker.inspect(current).unwrap().unwrap();
        if predicate(&observed) {
            return observed;
        }
        assert!(Instant::now() < deadline, "worker deadline: {observed:?}");
        thread::sleep(Duration::from_millis(1));
    }
}
fn consume(
    worker: &mut NetworkFlacWorker,
    current: &SourceBinding,
    bits: u16,
    frames: u64,
    known: bool,
) {
    let mut position = 0;
    let mut sequence = 0;
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let observed = worker.inspect(current).unwrap().unwrap();
        if let Some(ready) = observed.ready {
            assert_eq!(ready.total_frames, known.then_some(frames));
            assert_eq!(ready.format.sample_rate_hz(), 48000);
        }
        if let Some(accepted) = worker.take_frame(current).unwrap() {
            assert_eq!(accepted.binding, *current);
            let frame = accepted.frame;
            assert_eq!(frame.frame_position(), position);
            assert_eq!(frame.sequence(), sequence);
            match frame.samples() {
                PcmSamples::Signed16(values) => {
                    assert_eq!(bits, 16);
                    for (i, value) in values.iter().enumerate() {
                        assert_eq!(i32::from(*value), sample(position as usize * 2 + i, bits));
                    }
                }
                PcmSamples::Signed24(values) => {
                    assert_eq!(bits, 24);
                    for (i, value) in values.iter().enumerate() {
                        assert_eq!(*value, sample(position as usize * 2 + i, bits));
                    }
                }
                _ => panic!("unexpected samples"),
            }
            position += frame.frame_count();
            sequence += 1;
        }
        if let Some(outcome) = observed.outcome {
            assert_eq!(outcome, Outcome::Complete);
            assert_eq!(position, frames);
            assert_eq!(
                observed.ready.unwrap().total_frames,
                known.then_some(frames)
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "decode exceeded deadline: {observed:?}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn every_binding_field_is_checked_before_transport_starts() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://worker-fixture.test:{}/",
        listener.local_addr().unwrap().port()
    );
    let base = binding(1);
    for field in 0..5 {
        let current = SourceBinding::new(
            OwnerId::new(if field == 0 {
                "replacement-owner"
            } else {
                "network-owner"
            })
            .unwrap(),
            SessionId::new(if field == 1 {
                "replacement-session"
            } else {
                "network-session"
            })
            .unwrap(),
            StagedMediaId::new(if field == 2 {
                "replacement-media"
            } else {
                "network-media"
            })
            .unwrap(),
            if field == 3 { 2 } else { 1 },
            if field == 4 { 2 } else { 1 },
        )
        .unwrap();
        let mut worker = NetworkFlacWorker::new();
        assert_eq!(
            worker.start(permit(&url, &base), &current),
            Err(WorkerError::StaleBinding)
        );
        assert_eq!(worker.ownership(), Ownership::Idle);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn known_and_unknown_duration_preserve_exact_pcm_over_real_http() {
    for (bits, known, chunked) in [(16, true, false), (24, false, true)] {
        let bytes = fixture(bits, 20001, known);
        let (url, server) = server(move |mut stream| body(&mut stream, &bytes, chunked));
        let mut worker = NetworkFlacWorker::new();
        worker
            .start(permit(&url, &binding(1)), &binding(1))
            .unwrap();
        consume(&mut worker, &binding(1), bits, 20001, known);
        worker.retire().unwrap();
        assert_eq!(worker.ownership(), Ownership::Idle);
        server.join().unwrap();
    }
}

#[test]
fn a_full_frame_slot_stops_decoding_and_cancel_dominates_http_complete() {
    let bytes = fixture(16, 8199, true);
    let (url, server) = server(move |mut stream| body(&mut stream, &bytes, false));
    let mut worker = NetworkFlacWorker::new();
    worker
        .start(permit(&url, &binding(1)), &binding(1))
        .unwrap();
    let observed = wait_for(&mut worker, &binding(1), |o| {
        o.pending_frame && o.transport.terminal == Some(Terminal::Complete)
    });
    assert_eq!(observed.decoded_packets, 1);
    assert_eq!(
        worker.start(permit(&url, &binding(2)), &binding(2)),
        Err(WorkerError::Busy)
    );
    for _ in 0..100 {
        assert_eq!(
            worker
                .inspect(&binding(1))
                .unwrap()
                .unwrap()
                .decoded_packets,
            1
        );
    }
    worker.cancel();
    assert_eq!(worker.ownership(), Ownership::Retiring);
    let observed = worker.inspect(&binding(1)).unwrap().unwrap();
    assert_eq!(observed.outcome, Some(Outcome::Cancelled));
    assert!(!observed.pending_frame);
    assert!(worker.take_frame(&binding(1)).unwrap().is_none());
    worker.retire().unwrap();
    server.join().unwrap();
}

#[test]
fn real_http_failure_discards_pcm_without_another_decoder_read() {
    let bytes = fixture(16, 8199, true);
    let (release, wait) = mpsc::channel();
    let (url, server) = server(move |mut stream| {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            bytes.len() + 1
        )
        .unwrap();
        stream.write_all(&bytes).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let mut worker = NetworkFlacWorker::new();
    worker
        .start(permit(&url, &binding(1)), &binding(1))
        .unwrap();
    assert_eq!(
        wait_for(&mut worker, &binding(1), |o| o.pending_frame).decoded_packets,
        1
    );
    release.send(()).unwrap();
    server.join().unwrap();
    let observed = wait_for(&mut worker, &binding(1), |o| o.outcome.is_some());
    assert_eq!(
        observed.outcome,
        Some(Outcome::TransportFailed(FetchError::Body))
    );
    assert_eq!(observed.decoded_packets, 1);
    assert!(!observed.pending_frame);
    assert!(worker.take_frame(&binding(1)).unwrap().is_none());
    worker.cancel();
    assert_eq!(
        worker.inspect(&binding(1)).unwrap().unwrap().outcome,
        observed.outcome
    );
    worker.retire().unwrap();
}

#[test]
fn replacing_current_binding_rejects_pending_frames_and_completion() {
    for completed in [false, true] {
        let bytes = fixture(16, 8199, true);
        let (url, server) = server(move |mut stream| body(&mut stream, &bytes, false));
        let mut worker = NetworkFlacWorker::new();
        worker
            .start(permit(&url, &binding(1)), &binding(1))
            .unwrap();
        if completed {
            consume(&mut worker, &binding(1), 16, 8199, true);
        } else {
            wait_for(&mut worker, &binding(1), |o| o.pending_frame);
        }
        assert_eq!(
            worker.inspect(&binding(2)).err(),
            Some(WorkerError::StaleBinding)
        );
        assert_eq!(
            worker.take_frame(&binding(2)).err(),
            Some(WorkerError::StaleBinding)
        );
        assert_eq!(worker.ownership(), Ownership::Retiring);
        worker.retire().unwrap();
        server.join().unwrap();
    }
}

#[test]
fn complete_http_with_partial_flac_preserves_decode_failure_through_cancel() {
    let mut bytes = fixture(16, 8199, false);
    bytes.truncate(bytes.len() - 1);
    let (url, server) = server(move |mut stream| body(&mut stream, &bytes, false));
    let mut worker = NetworkFlacWorker::new();
    worker
        .start(permit(&url, &binding(1)), &binding(1))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let outcome = loop {
        if let Some(outcome) = worker.inspect(&binding(1)).unwrap().unwrap().outcome {
            break outcome;
        }
        worker.take_frame(&binding(1)).unwrap();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert!(matches!(outcome, Outcome::DecodeFailed(_)), "{outcome:?}");
    worker.cancel();
    assert_eq!(
        worker.inspect(&binding(1)).unwrap().unwrap().outcome,
        Some(outcome)
    );
    worker.retire().unwrap();
    server.join().unwrap();
}

#[test]
fn cancel_stalled_startup_and_body_wakes_decoder_and_closes_peer() {
    for body in [false, true] {
        let (connected, connection) = mpsc::channel();
        let (closed, closure) = mpsc::channel();
        let (url, server) = server(move |mut stream| {
            if body {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nfLaC")
                    .unwrap();
            }
            connected.send(()).unwrap();
            let mut byte = [0];
            closed.send(stream.read(&mut byte).unwrap() == 0).unwrap();
        });
        let mut worker = NetworkFlacWorker::new();
        worker
            .start(permit(&url, &binding(1)), &binding(1))
            .unwrap();
        connection.recv_timeout(Duration::from_secs(3)).unwrap();
        worker.cancel();
        assert_eq!(
            worker.inspect(&binding(1)).unwrap().unwrap().outcome,
            Some(Outcome::Cancelled)
        );
        worker.retire().unwrap();
        assert!(closure.recv_timeout(Duration::from_secs(3)).unwrap());
        server.join().unwrap();
    }
}

#[test]
fn cancel_full_byte_and_pcm_buffers_retires_both_threads_and_closes_peer() {
    let bytes = fixture(24, 300001, true);
    assert!(bytes.len() > 2 * 65536);
    let (closed, closure) = mpsc::channel();
    let (url, server) = server(move |mut stream| {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        let _ = stream.write_all(&bytes);
        let mut byte = [0];
        closed
            .send(match stream.read(&mut byte) {
                Ok(0) => true,
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => true,
                _ => false,
            })
            .unwrap();
    });
    let mut worker = NetworkFlacWorker::new();
    worker
        .start(permit(&url, &binding(1)), &binding(1))
        .unwrap();
    let observed = wait_for(&mut worker, &binding(1), |o| {
        o.pending_frame && o.transport.queued_chunks == 4
    });
    assert_eq!(observed.decoded_packets, 1);
    worker.cancel();
    assert!(!worker.inspect(&binding(1)).unwrap().unwrap().pending_frame);
    worker.retire().unwrap();
    assert_eq!(worker.ownership(), Ownership::Idle);
    assert!(closure.recv_timeout(Duration::from_secs(3)).unwrap());
    server.join().unwrap();
}

#[test]
fn independent_process_decodes_cancels_retires_and_replaces() {
    const CHILD: &str = "EARPH_FLAC_WORKER_CHILD";
    if let Ok(urls) = std::env::var(CHILD) {
        let urls: Vec<_> = urls.split('|').collect();
        let mut worker = NetworkFlacWorker::new();
        worker
            .start(permit(urls[0], &binding(1)), &binding(1))
            .unwrap();
        consume(&mut worker, &binding(1), 24, 8199, false);
        worker.retire().unwrap();
        worker
            .start(permit(urls[1], &binding(2)), &binding(2))
            .unwrap();
        wait_for(&mut worker, &binding(2), |o| o.pending_frame);
        worker.cancel();
        assert!(worker.take_frame(&binding(2)).unwrap().is_none());
        worker.retire().unwrap();
        worker
            .start(permit(urls[2], &binding(3)), &binding(3))
            .unwrap();
        consume(&mut worker, &binding(3), 16, 8199, true);
        worker.retire().unwrap();
        return;
    }
    let mut urls = Vec::new();
    let mut tasks = Vec::new();
    for (bits, known) in [(24, false), (16, true), (16, true)] {
        let bytes = fixture(bits, 8199, known);
        let (url, task) = server(move |mut stream| body(&mut stream, &bytes, false));
        urls.push(url);
        tasks.push(task);
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "independent_process_decodes_cancels_retires_and_replaces",
            "--nocapture",
        ])
        .env(CHILD, urls.join("|"))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("child worker deadline");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    for task in tasks {
        task.join().unwrap();
    }
}
