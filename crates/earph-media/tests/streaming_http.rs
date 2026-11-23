// ABOUTME: Qualifies FLAC through admitted real HTTP readers on a private interface.
// ABOUTME: Covers framing, truncation, source cancellation and stalled bodies.
#[path = "support/streaming.rs"]
#[allow(dead_code)]
mod support;
use earph_core::{
    ErrorCode,
    audio::PcmSamples,
    ids::{OwnerId, SessionId, StagedMediaId},
};
use earph_media::StreamingFlac;
use earph_network::{CheckedUrl, FetchPermit, NetworkWorker, OriginGrant, SourceBinding};
use std::{
    io::{Read, Write},
    net::{IpAddr, TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn interface() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
    address
}
fn permit(url: &str) -> FetchPermit {
    let owner = OwnerId::new("flac-test-owner").unwrap();
    let binding = SourceBinding::new(
        owner.clone(),
        SessionId::new("flac-test-session").unwrap(),
        StagedMediaId::new("flac-test-source").unwrap(),
        1,
        1,
    )
    .unwrap();
    let checked = CheckedUrl::new(url).unwrap();
    let grant = OriginGrant::new(
        checked.origin(),
        owner,
        1,
        vec![interface()],
        Instant::now() + Duration::from_secs(60),
        vec![],
    )
    .unwrap();
    FetchPermit::new(
        checked,
        binding.clone(),
        &binding,
        vec![grant],
        Instant::now(),
    )
    .unwrap()
}
fn server(handler: impl FnOnce(TcpStream) + Send + 'static) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://flac-fixture.test:{}/owned.flac",
        listener.local_addr().unwrap().port()
    );
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
fn assert_failed(mut stream: StreamingFlac) {
    loop {
        match stream.next_frame() {
            Err(error) => {
                assert_eq!(error.code, ErrorCode::AssetInvalid);
                assert_eq!(stream.next_frame().unwrap_err(), error);
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => panic!("HTTP failure became clean media EOF"),
        }
    }
}
#[test]
fn known_and_chunked_http_preserve_all_pcm_with_slow_consumers() {
    for chunked in [false, true] {
        let bytes = support::fixture(24, 2, 70001, "verbatim");
        let (url, task) = server(move |mut stream| {
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
                stream.write_all(&bytes).unwrap();
            }
        });
        let mut worker = NetworkWorker::new();
        let source = worker.start(permit(&url)).unwrap();
        thread::sleep(Duration::from_millis(50));
        let observed = source.observation();
        assert!(observed.queued_chunks <= 4);
        let mut stream = StreamingFlac::open(source).unwrap();
        let mut position = 0;
        while let Some(frame) = stream.next_frame().unwrap() {
            assert_eq!(frame.frame_position(), position);
            let PcmSamples::Signed24(values) = frame.samples() else {
                panic!("24-bit PCM required");
            };
            for (i, value) in values.iter().enumerate() {
                assert_eq!(
                    *value,
                    support::sample(position as usize * 2 + i, 24, "verbatim")
                );
            }
            position += frame.frame_count();
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(position, 70001);
        drop(stream);
        worker.retire().unwrap();
        task.join().unwrap();
    }
}
#[test]
fn http_truncation_and_partial_media_tail_are_errors() {
    let original = support::fixture(16, 1, 8199, "verbatim");
    for broken_http in [true, false] {
        let mut bytes = support::metadata(&original, false, false);
        let size = bytes.len();
        bytes.truncate(size - 1);
        let (url, task) = server(move |mut stream| {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                if broken_http { size } else { bytes.len() }
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let mut worker = NetworkWorker::new();
        let source = worker.start(permit(&url)).unwrap();
        match StreamingFlac::open(source) {
            Ok(stream) => assert_failed(stream),
            Err(error) => assert_eq!(error.code, ErrorCode::AssetInvalid),
        }
        worker.retire().unwrap();
        task.join().unwrap();
    }
}
#[test]
fn cancellation_and_stalls_fail_when_the_decoder_observes_the_source() {
    for cancel in [true, false] {
        let bytes = support::fixture(16, 1, 8199, "verbatim");
        let (sent, received) = mpsc::channel();
        let (release, stalled) = mpsc::channel();
        let (url, task) = server(move |mut stream| {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            // Publish enough genuine data for construction and hold the incomplete HTTP body open.
            stream
                .write_all(&bytes[..support::audio_offset(&bytes) + 100])
                .unwrap();
            sent.send(()).unwrap();
            stalled.recv_timeout(Duration::from_secs(10)).unwrap();
        });
        let mut worker = NetworkWorker::new();
        let source = worker.start(permit(&url)).unwrap();
        let cancellation = source.cancellation();
        received.recv_timeout(Duration::from_secs(3)).unwrap();
        let stream = StreamingFlac::open(source).unwrap();
        if cancel {
            cancellation.cancel();
        }
        assert_failed(stream);
        release.send(()).unwrap();
        worker.retire().unwrap();
        task.join().unwrap();
    }
}

#[test]
fn cancellation_after_a_decoded_packet_remains_an_error_at_observed_end() {
    let bytes = support::fixture(16, 1, 8199, "verbatim");
    let (release, stalled) = mpsc::channel();
    let (url, task) = server(move |mut stream| {
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
            bytes.len() + 1
        )
        .unwrap();
        stream.write_all(&bytes).unwrap();
        stalled.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let mut worker = NetworkWorker::new();
    let source = worker.start(permit(&url)).unwrap();
    let cancellation = source.cancellation();
    let mut stream = StreamingFlac::open(source).unwrap();
    assert_eq!(stream.next_frame().unwrap().unwrap().frame_position(), 0);
    cancellation.cancel();
    // Buffered valid packets can precede the read that observes cancellation.
    assert_failed(stream);
    release.send(()).unwrap();
    worker.retire().unwrap();
    task.join().unwrap();
}
