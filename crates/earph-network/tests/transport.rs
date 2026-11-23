// ABOUTME: Qualifies the real admitted HTTP transport on a private interface.
// ABOUTME: Exercises bounded streaming, hostile framing, cancellation and retirement.
use earph_core::ids::{OwnerId, SessionId, StagedMediaId};
use earph_network::*;
use std::{
    io::{Read, Write},
    net::{IpAddr, TcpListener, TcpStream, UdpSocket},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

fn interface() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    // UDP connect selects a local route without sending any datagram.
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
    address
}
fn binding(generation: u64) -> SourceBinding {
    SourceBinding::new(
        OwnerId::new("socket-owner").unwrap(),
        SessionId::new("session").unwrap(),
        StagedMediaId::new("staged").unwrap(),
        generation,
        1,
    )
    .unwrap()
}
fn grant(url: &str) -> OriginGrant {
    OriginGrant::new(
        CheckedUrl::new(url).unwrap().origin(),
        OwnerId::new("socket-owner").unwrap(),
        1,
        vec![interface()],
        Instant::now() + Duration::from_secs(60),
        vec![],
    )
    .unwrap()
}
fn permit(url: &str, grants: Vec<OriginGrant>, generation: u64) -> FetchPermit {
    FetchPermit::new(
        CheckedUrl::new(url).unwrap(),
        binding(generation),
        &binding(generation),
        grants,
        Instant::now(),
    )
    .unwrap()
}
fn fixture(
    handler: impl FnOnce(TcpStream, String) + Send + 'static,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://media.test:{}/private?secret=hidden",
        listener.local_addr().unwrap().port()
    );
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        handler(stream, String::from_utf8(request).unwrap());
    });
    (url, handle)
}
fn collect(url: &str, grants: Vec<OriginGrant>) -> (Result<Vec<u8>, FetchError>, Observation) {
    let mut worker = NetworkWorker::new();
    let mut reader = worker.start(permit(url, grants, 1)).unwrap();
    let result = reader.collect_bounded();
    let observation = reader.observation();
    worker.retire().unwrap();
    (result, observation)
}
#[test]
fn known_and_chunked_bodies_preserve_bytes_and_hostname() {
    for chunked in [false, true] {
        let (url, server) = fixture(move |mut stream, request| {
            assert!(request.contains("host: media.test:"));
            assert!(!request.contains("authorization:") && !request.contains("referer:"));
            let response: &[u8] = if chunked {
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"
            } else {
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nabcde"
            };
            stream.write_all(response).unwrap();
        });
        let (body, observed) = collect(&url, vec![grant(&url)]);
        assert_eq!(body.unwrap(), b"abcde");
        assert_eq!(observed.terminal, Some(Terminal::Complete));
        assert_eq!(observed.binding, binding(1));
        server.join().unwrap();
    }
}
#[test]
fn authorized_redirects_revalidate_before_connecting() {
    let (target, destination) = fixture(|mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .unwrap();
    });
    let location = target.clone();
    let (source, redirect) = fixture(move |mut stream, _| {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
    });
    assert_eq!(
        collect(&source, vec![grant(&source), grant(&target)])
            .0
            .unwrap(),
        b"ok"
    );
    redirect.join().unwrap();
    destination.join().unwrap();
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let denied = format!(
        "http://ungranted.test:{}/",
        listener.local_addr().unwrap().port()
    );
    let (source, redirect) = fixture(move |mut stream, _| {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {denied}\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
    });
    assert_eq!(
        collect(&source, vec![grant(&source)]).0.unwrap_err(),
        FetchError::Admission(PolicyError::NotGranted)
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    redirect.join().unwrap();
}
#[test]
fn broken_framing_compression_and_header_budgets_are_errors() {
    let cases: Vec<(Vec<u8>, FetchError)> = vec![
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nshort".to_vec(),
            FetchError::Body,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nZ\r\nbroken\r\n".to_vec(),
            FetchError::Body,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Encoding: gzip\r\n\r\nxx".to_vec(),
            FetchError::ContentEncoding,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\nX-Large: {}\r\nContent-Length: 0\r\n\r\n",
                "a".repeat(32768)
            )
            .into_bytes(),
            FetchError::Headers,
        ),
        (
            format!(
                "HTTP/1.1 200 OK\r\n{}Content-Length: 0\r\n\r\n",
                "X-A: a\r\n".repeat(65)
            )
            .into_bytes(),
            FetchError::Headers,
        ),
    ];
    for (response, expected) in cases {
        let (url, server) = fixture(move |mut stream, _| {
            let _ = stream.write_all(&response);
        });
        let (result, observed) = collect(&url, vec![grant(&url)]);
        assert_eq!(result.unwrap_err(), expected);
        assert!(matches!(observed.terminal, Some(Terminal::Failed(_))));
        server.join().unwrap();
    }
}
#[test]
fn slow_consumer_has_bounded_backpressure_without_stall_timeout() {
    for chunked in [false, true] {
        let expected: Vec<u8> = (0..262144).map(|i| (i % 251) as u8).collect();
        let response_body = expected.clone();
        let (url, server) = fixture(move |mut stream, _| {
            if chunked {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .unwrap();
                for chunk in response_body.chunks(5000) {
                    write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                    stream.write_all(chunk).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
            } else {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 262144\r\n\r\n")
                    .unwrap();
                stream.write_all(&response_body).unwrap();
            }
        });
        let mut worker = NetworkWorker::new();
        let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while reader.observation().queued_chunks < 4 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(reader.observation().queued_chunks, 4);
        thread::sleep(Duration::from_millis(5200));
        assert_eq!(reader.observation().terminal, None);
        assert_eq!(reader.collect_bounded().unwrap(), expected);
        worker.retire().unwrap();
        server.join().unwrap();
    }
}
#[test]
fn cancellation_wakes_stalled_consumer_closes_peer_and_retires_before_replacement() {
    let (connected_tx, connected_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let (url, server) = fixture(move |mut stream, _| {
        connected_tx.send(()).unwrap();
        let mut byte = [0];
        closed_tx
            .send(stream.read(&mut byte).unwrap() == 0)
            .unwrap();
    });
    let mut worker = NetworkWorker::new();
    let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
    connected_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(
        worker.start(permit(&url, vec![grant(&url)], 2)).err(),
        Some(FetchError::Busy)
    );
    let cancel = reader.cancellation();
    let blocked = thread::spawn(move || {
        let mut byte = [0];
        reader.read(&mut byte).unwrap_err().kind()
    });
    cancel.cancel();
    assert_eq!(
        blocked.join().unwrap(),
        std::io::ErrorKind::ConnectionAborted
    );
    worker.retire().unwrap();
    assert!(closed_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    server.join().unwrap();
    let (url, server) = fixture(|mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nnew")
            .unwrap();
    });
    let mut reader = worker.start(permit(&url, vec![grant(&url)], 2)).unwrap();
    assert_eq!(reader.collect_bounded().unwrap(), b"new");
    assert_eq!(reader.observation().binding, binding(2));
    worker.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn stalled_body_and_headers_have_distinct_deadlines() {
    for body in [false, true] {
        let (url, server) = fixture(move |mut stream, _| {
            if body {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\n")
                    .unwrap();
            }
            let mut byte = [0];
            assert_eq!(stream.read(&mut byte).unwrap(), 0);
        });
        let (result, _) = collect(&url, vec![grant(&url)]);
        assert_eq!(
            result.unwrap_err(),
            if body {
                FetchError::BodyTimeout
            } else {
                FetchError::HeaderTimeout
            }
        );
        server.join().unwrap();
    }
}
#[test]
fn full_pipe_cancellation_discards_buffered_data() {
    let (url, server) = fixture(|mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10485760\r\n\r\n")
            .unwrap();
        let bytes = vec![7; 16384];
        while stream.write_all(&bytes).is_ok() {}
    });
    let mut worker = NetworkWorker::new();
    let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while reader.observation().queued_chunks != 4 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(reader.observation().queued_chunks, 4);
    reader.cancellation().cancel();
    assert_eq!(reader.collect_bounded().unwrap_err(), FetchError::Cancelled);
    worker.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn observed_collection_limit_and_expired_permits_stop_transport() {
    let (url, server) = fixture(|mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\n\r\n")
            .unwrap();
        let _ = stream.write_all(&vec![1; 1048577]);
    });
    assert_eq!(
        collect(&url, vec![grant(&url)]).0.unwrap_err(),
        FetchError::CollectionLimit
    );
    server.join().unwrap();
    let url = "http://media.test:34567/";
    let expires = Instant::now() + Duration::from_millis(20);
    let origin = OriginGrant::new(
        CheckedUrl::new(url).unwrap().origin(),
        OwnerId::new("socket-owner").unwrap(),
        1,
        vec![interface()],
        expires,
        vec![],
    )
    .unwrap();
    let p = permit(url, vec![origin], 1);
    thread::sleep(Duration::from_millis(30));
    assert_eq!(
        NetworkWorker::new().start(p).err(),
        Some(FetchError::Admission(PolicyError::Expired))
    );
}
#[test]
fn cancellation_terminates_standard_read_helpers() {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (url, server) = fixture(move |mut stream, _| {
        ready_tx.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let mut worker = NetworkWorker::new();
    let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
    let cancellation = reader.cancellation();
    let (result_tx, result_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let _ = result_tx.send(reader.read_to_end(&mut Vec::new()).map_err(|e| e.kind()));
    });
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    cancellation.cancel();
    assert_eq!(
        result_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(std::io::ErrorKind::ConnectionAborted)
    );
    reader_thread.join().unwrap();
    worker.retire().unwrap();
    server.join().unwrap();
}

struct TlsFiles {
    root: Vec<u8>,
    certificate: Vec<u8>,
    key: Vec<u8>,
}
fn tls_files() -> &'static TlsFiles {
    static FILES: std::sync::OnceLock<TlsFiles> = std::sync::OnceLock::new();
    FILES.get_or_init(|| {
        use std::{fs, path::PathBuf, process::Command};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/network-reader").join(format!("tls-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap(); }
        let run = |args: &[&str]| { let output = Command::new("openssl").args(args).current_dir(&root).output().unwrap(); assert!(output.status.success(), "fixture certificate generation failed"); };
        run(&["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.pem", "-days", "2", "-subj", "/CN=Earph task fixture CA", "-addext", "basicConstraints=critical,CA:TRUE"]);
        run(&["req", "-newkey", "rsa:2048", "-nodes", "-keyout", "server.key", "-out", "server.csr", "-subj", "/CN=media.test"]);
        fs::write(root.join("extensions.cnf"), "subjectAltName=DNS:media.test\nbasicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n").unwrap();
        run(&["x509", "-req", "-in", "server.csr", "-CA", "ca.pem", "-CAkey", "ca.key", "-CAcreateserial", "-out", "server.pem", "-days", "2", "-extfile", "extensions.cnf"]);
        run(&["x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der"]);
        run(&["x509", "-in", "server.pem", "-outform", "DER", "-out", "server.der"]);
        run(&["pkcs8", "-topk8", "-nocrypt", "-in", "server.key", "-outform", "DER", "-out", "server-key.der"]);
        TlsFiles { root: fs::read(root.join("ca.der")).unwrap(), certificate: fs::read(root.join("server.der")).unwrap(), key: fs::read(root.join("server-key.der")).unwrap() }
    })
}
#[test]
fn shared_origin_grant_borrows_the_exact_authoritative_pins_and_der() {
    use earph_core::network_source::{NetworkGrantDocument, NetworkOriginGrant};
    use std::sync::Arc;
    let document = Arc::new(NetworkGrantDocument {
        origins: vec![NetworkOriginGrant {
            origin: "https://media.test".into(),
            pins: vec![interface()],
            roots_der: vec![tls_files().root.clone()],
        }],
    });
    let grant = OriginGrant::from_document(
        document.clone(),
        0,
        binding(1).owner().clone(),
        1,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert!(std::ptr::eq(
        grant.pins(),
        document.origins[0].pins.as_slice()
    ));
    assert!(std::ptr::eq(
        grant.roots_der(),
        document.origins[0].roots_der.as_slice()
    ));
    assert!(std::ptr::eq(
        grant.roots_der()[0].as_slice(),
        document.origins[0].roots_der[0].as_slice()
    ));
    let retained = Arc::downgrade(&document);
    drop(document);
    assert!(retained.upgrade().is_some());
    drop(grant);
    assert!(retained.upgrade().is_none());
    let invalid = Arc::new(NetworkGrantDocument {
        origins: vec![NetworkOriginGrant {
            origin: "https://media.test/private".into(),
            pins: vec![interface()],
            roots_der: vec![tls_files().root.clone()],
        }],
    });
    assert!(
        OriginGrant::from_document(
            invalid.clone(),
            0,
            binding(1).owner().clone(),
            1,
            Instant::now() + Duration::from_secs(30)
        )
        .is_err()
    );
    assert!(
        OriginGrant::from_document(
            invalid,
            1,
            binding(1).owner().clone(),
            1,
            Instant::now() + Duration::from_secs(30)
        )
        .is_err()
    );
}
fn tls_fixture(host: &str, success: bool) -> (String, thread::JoinHandle<()>) {
    let files = tls_files();
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let url = format!(
        "https://{host}:{}/private?secret=hidden",
        listener.local_addr().unwrap().port()
    );
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let provider = std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![rustls::pki_types::CertificateDer::from(
                    files.certificate.clone(),
                )],
                rustls::pki_types::PrivateKeyDer::Pkcs8(
                    rustls::pki_types::PrivatePkcs8KeyDer::from(files.key.clone()),
                ),
            )
            .unwrap();
        let connection = rustls::ServerConnection::new(std::sync::Arc::new(config)).unwrap();
        let mut tls = rustls::StreamOwned::new(connection, stream);
        let mut request = Vec::new();
        let read = (|| -> std::io::Result<()> {
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16384 {
                let mut byte = [0];
                tls.read_exact(&mut byte)?;
                request.push(byte[0]);
            }
            Ok(())
        })();
        if success {
            read.unwrap();
            assert_eq!(tls.conn.server_name(), Some("media.test"));
            assert!(
                String::from_utf8(request)
                    .unwrap()
                    .contains("host: media.test:")
            );
            tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\ntrusted")
                .unwrap();
            tls.flush().unwrap();
        } else {
            assert!(read.is_err());
        }
    });
    (url, server)
}
fn tls_grant(url: &str) -> OriginGrant {
    OriginGrant::new(
        CheckedUrl::new(url).unwrap().origin(),
        OwnerId::new("socket-owner").unwrap(),
        1,
        vec![interface()],
        Instant::now() + Duration::from_secs(60),
        vec![tls_files().root.clone()],
    )
    .unwrap()
}
#[test]
fn pinned_tls_preserves_sni_and_requires_hostname_and_trusted_chain() {
    let (url, server) = tls_fixture("media.test", true);
    assert_eq!(collect(&url, vec![tls_grant(&url)]).0.unwrap(), b"trusted");
    server.join().unwrap();
    let (url, server) = tls_fixture("wrong.test", false);
    let (result, observation) = collect(&url, vec![tls_grant(&url)]);
    assert_eq!(result.unwrap_err(), FetchError::Connect);
    assert!(!format!("{observation:?}").contains("secret"));
    server.join().unwrap();
    let (url, server) = tls_fixture("media.test", false);
    let rootless = OriginGrant::new(
        CheckedUrl::new(&url).unwrap().origin(),
        OwnerId::new("socket-owner").unwrap(),
        1,
        vec![interface()],
        Instant::now() + Duration::from_secs(60),
        vec![tls_files().certificate.clone()],
    )
    .unwrap();
    // Trusting the leaf as a CA must not validate this chain.
    assert_eq!(
        collect(&url, vec![rootless]).0.unwrap_err(),
        FetchError::Connect
    );
    server.join().unwrap();
}
#[test]
fn shared_document_roots_drive_the_real_pinned_tls_transport() {
    use earph_core::network_source::{NetworkGrantDocument, NetworkOriginGrant};
    use std::sync::Arc;
    let (url, server) = tls_fixture("media.test", true);
    let origin = url.split('/').take(3).collect::<Vec<_>>().join("/");
    let document = Arc::new(NetworkGrantDocument {
        origins: vec![NetworkOriginGrant {
            origin,
            pins: vec![interface()],
            roots_der: vec![tls_files().root.clone()],
        }],
    });
    let grant = OriginGrant::from_document(
        document,
        0,
        OwnerId::new("socket-owner").unwrap(),
        1,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert_eq!(collect(&url, vec![grant]).0.unwrap(), b"trusted");
    server.join().unwrap();
}
#[test]
fn cancellation_during_tls_connect_closes_the_real_peer() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let url = format!(
        "https://media.test:{}/",
        listener.local_addr().unwrap().port()
    );
    let mut worker = NetworkWorker::new();
    let mut reader = worker
        .start(permit(&url, vec![tls_grant(&url)], 1))
        .unwrap();
    let (mut socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    reader.cancellation().cancel();
    assert_eq!(reader.collect_bounded().unwrap_err(), FetchError::Cancelled);
    worker.retire().unwrap();
    let mut hello = Vec::new();
    socket.read_to_end(&mut hello).unwrap();
}
#[test]
fn reader_usage_in_an_independent_process_retires_the_transport() {
    const CHILD_URL: &str = "EARPH_NETWORK_FIXTURE_CHILD_URL";
    if let Ok(url) = std::env::var(CHILD_URL) {
        let mut worker = NetworkWorker::new();
        let mut reader = worker.start(permit(&url, vec![grant(&url)], 9)).unwrap();
        assert_eq!(reader.collect_bounded().unwrap(), b"process-body");
        assert_eq!(reader.observation().binding, binding(9));
        worker.retire().unwrap();
        return;
    }
    let (closed_tx, closed_rx) = mpsc::channel();
    let (url, server) = fixture(move |mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nprocess-body")
            .unwrap();
        let mut byte = [0];
        closed_tx
            .send(stream.read(&mut byte).unwrap() == 0)
            .unwrap();
    });
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "reader_usage_in_an_independent_process_retires_the_transport",
            "--nocapture",
        ])
        .env(CHILD_URL, &url)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("independent reader exceeded process deadline");
        }
        thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert!(closed_rx.recv_timeout(Duration::from_secs(2)).unwrap());
    server.join().unwrap();
}

#[test]
fn redirect_limit_and_shared_startup_deadline_bound_repeated_hops() {
    for delayed in [false, true] {
        let listener = TcpListener::bind((interface(), 0)).unwrap();
        let url = format!(
            "http://media.test:{}/repeat",
            listener.local_addr().unwrap().port()
        );
        let location = url.clone();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                if delayed {
                    thread::sleep(Duration::from_secs(4));
                }
                let _ = write!(
                    stream,
                    "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                );
            }
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        });
        let started = Instant::now();
        let (result, _) = collect(&url, vec![grant(&url)]);
        assert_eq!(
            result.unwrap_err(),
            if delayed {
                FetchError::StartupTimeout
            } else {
                FetchError::Redirect
            }
        );
        assert!(started.elapsed() < Duration::from_secs(17));
        server.join().unwrap();
    }
}
#[test]
fn cancellation_during_body_and_reader_drop_close_peers() {
    for drop_reader in [false, true] {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (closed_tx, closed_rx) = mpsc::channel();
        let (url, server) = fixture(move |mut stream, _| {
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\na")
                .unwrap();
            ready_tx.send(()).unwrap();
            let mut byte = [0];
            closed_tx
                .send(stream.read(&mut byte).unwrap() == 0)
                .unwrap();
        });
        let mut worker = NetworkWorker::new();
        let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let mut byte = [0];
        reader.read_exact(&mut byte).unwrap();
        assert_eq!(byte, *b"a");
        if drop_reader {
            drop(reader);
        } else {
            reader.cancellation().cancel();
            assert_eq!(
                reader.read(&mut byte).unwrap_err().kind(),
                std::io::ErrorKind::ConnectionAborted
            );
        }
        worker.retire().unwrap();
        assert!(closed_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        server.join().unwrap();
    }
}

fn assert_raw_redirect_refused(reference: fn(u16) -> String) {
    let destination = TcpListener::bind((interface(), 0)).unwrap();
    destination.set_nonblocking(true).unwrap();
    let port = destination.local_addr().unwrap().port();
    let target = format!("http://media.test:{port}/next");
    let location = reference(port);
    let (source, server) = fixture(move |mut stream, _| {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
        )
        .unwrap();
    });
    let (result, _) = collect(&source, vec![grant(&source), grant(&target)]);
    server.join().unwrap();
    assert!(
        matches!(destination.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "raw redirect connected to its destination"
    );
    assert_eq!(
        result.unwrap_err(),
        FetchError::Admission(PolicyError::InvalidUrl)
    );
}
#[test]
fn raw_redirect_absolute_empty_userinfo_fails_before_connection() {
    assert_raw_redirect_refused(|port| format!("http://@media.test:{port}/next"));
}
#[test]
fn raw_redirect_network_reference_empty_userinfo_fails_before_connection() {
    assert_raw_redirect_refused(|port| format!("//@media.test:{port}/next"));
}
#[test]
fn raw_redirect_same_scheme_without_authority_fails_before_connection() {
    assert_raw_redirect_refused(|_| "http:next".into());
}

#[test]
fn independent_observer_fences_real_terminal_publication() {
    let (release, wait) = mpsc::channel();
    let (closed, closure) = mpsc::channel();
    let (url, server) = fixture(move |mut stream, _| {
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\na")
            .unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(stream);
        closed.send(()).unwrap();
    });
    let mut worker = NetworkWorker::new();
    let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
    let observer = reader.observer();
    let mut byte = [0];
    reader.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"a");
    assert!(observer.publication_guard(&binding(2)).is_err());
    let guard = observer.publication_guard(&binding(1)).unwrap();
    assert_eq!(guard.terminal(), None);
    release.send(()).unwrap();
    closure.recv_timeout(Duration::from_secs(3)).unwrap();
    // This accepted handoff precedes the failed body's terminal publication.
    assert_eq!(guard.terminal(), None);
    drop(guard);
    let deadline = Instant::now() + Duration::from_secs(3);
    while observer.observation().terminal.is_none() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert_eq!(
        observer.observation().terminal,
        Some(Terminal::Failed(FetchError::Body))
    );
    assert_eq!(
        observer.publication_guard(&binding(1)).err(),
        Some(FetchError::Body)
    );
    drop(observer.clone());
    assert_eq!(
        reader.observation().terminal,
        Some(Terminal::Failed(FetchError::Body))
    );
    worker.retire().unwrap();
    server.join().unwrap();
}

#[test]
fn observer_allows_complete_drain_and_rejects_cancelled_transport() {
    for cancel in [false, true] {
        let (release, wait) = mpsc::channel();
        let (connected, connection) = mpsc::channel();
        let (url, server) = fixture(move |mut stream, _| {
            connected.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na");
        });
        let mut worker = NetworkWorker::new();
        let mut reader = worker.start(permit(&url, vec![grant(&url)], 1)).unwrap();
        let observer = reader.observer();
        connection.recv_timeout(Duration::from_secs(3)).unwrap();
        drop(observer.clone());
        assert_eq!(reader.observation().terminal, None);
        if cancel {
            reader.cancellation().cancel();
        }
        release.send(()).unwrap();
        if cancel {
            assert_eq!(
                observer.publication_guard(&binding(1)).err(),
                Some(FetchError::Cancelled)
            );
        } else {
            assert_eq!(reader.collect_bounded().unwrap(), b"a");
            assert_eq!(
                observer.publication_guard(&binding(1)).unwrap().terminal(),
                Some(Terminal::Complete)
            );
        }
        worker.retire().unwrap();
        server.join().unwrap();
    }
}
