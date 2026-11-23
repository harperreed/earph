// ABOUTME: Exercises explicit renderer configuration through the actual serial owner and HTTP sockets.
// ABOUTME: Verifies controller fencing and canonical SOAP projections without physical audio.
use earph_engine::{EngineOwner, RendererConfiguration};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn command(runtime: &Path, id: &str, payload: Value) -> Value {
    let state: Value = serde_json::from_slice(&earph_engine::request(runtime, &serde_json::to_vec(&json!({"schema_version":1,"request_id":format!("state-{id}"),"zone_id":"main","session_id":null,"expected_revision":null,"payload":{"method":"state.get"}})).unwrap()).unwrap()).unwrap();
    let snapshot = &state["receipt"]["result"]["value"];
    serde_json::from_slice::<Value>(&earph_engine::request(runtime, &serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":snapshot["owner_id"],"session_id":snapshot["session_id"],"expected_revision":snapshot["revision"],"payload":payload})).unwrap()).unwrap()).unwrap()["receipt"].clone()
}
fn soap(address: SocketAddrV4, name: &str, args: &str) -> String {
    let ns = "urn:schemas-upnp-org:service:AVTransport:1";
    let body = format!(
        r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{name} xmlns:u="{ns}">{args}</u:{name}></s:Body></s:Envelope>"#
    );
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    write!(stream,"POST /upnp/avt/control HTTP/1.1\r\nHost: {address}\r\nContent-Type: text/xml; charset=utf-8\r\nSOAPACTION: \"{ns}#{name}\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
#[test]
fn explicit_owner_renderer_executes_stage_and_rejects_revoked_peer() {
    let route = UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let std::net::IpAddr::V4(peer) = route.local_addr().unwrap().ip() else {
        panic!("IPv4 required")
    };
    assert!(peer.is_private());
    let runtime = std::env::temp_dir().join(format!(
        "eo-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let owner = EngineOwner::bind(&runtime).unwrap();
    assert!(!owner.snapshot().shutdown_requested);
    let owner = owner
        .with_renderer(
            RendererConfiguration::new(
                SocketAddrV4::new(peer, 0),
                "uuid:00000000-0000-0000-0000-000000000001",
                "Earph owner fixture",
                "Earph/1 UPnP/1.0 Earph/1",
            )
            .unwrap(),
        )
        .unwrap();
    let address = owner.renderer_address().unwrap();
    let join = std::thread::spawn(move || owner.serve());
    let result = command(
        &runtime,
        "grant",
        json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"fixture","peers":[peer],"rights":["stage","transport","events"],"playback":null}]}}),
    );
    assert_eq!(result["status"], "applied", "{result}");
    let response = soap(
        address,
        "SetAVTransportURI",
        "<InstanceID>0</InstanceID><CurrentURI>http://192.0.2.4:4321/public.flac</CurrentURI><CurrentURIMetaData>owned fixture</CurrentURIMetaData>",
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let response = soap(address, "GetMediaInfo", "<InstanceID>0</InstanceID>");
    assert!(
        response.contains("http://192.0.2.4:4321/public.flac"),
        "{response}"
    );
    let repeated = soap(
        address,
        "SetAVTransportURI",
        "<InstanceID>0</InstanceID><CurrentURI>http://192.0.2.4:4321/public.flac</CurrentURI><CurrentURIMetaData>owned fixture</CurrentURIMetaData>",
    );
    assert!(repeated.starts_with("HTTP/1.1 200"), "{repeated}");
    let staging = command(
        &runtime,
        "staging-generation",
        json!({"method":"sources.get"}),
    );
    // Core keeps the same descriptor for equal staging under the same controller.
    assert_eq!(staging["result"]["value"]["generation"], 1);
    let callback = std::net::TcpListener::bind((peer, 0)).unwrap();
    let callback_address = callback.local_addr().unwrap();
    let result = command(
        &runtime,
        "callbacks",
        json!({"method":"renderer.callbacks.replace","controller_id":"fixture","expected_controller_generation":1,"expected_generation":0,"lifetime_ms":60000,"grants":{"origins":[{"origin":format!("http://{callback_address}"),"pins":[peer]}]}}),
    );
    assert_eq!(result["status"], "applied", "{result}");
    let callback_thread = std::thread::spawn(move || {
        let (mut stream, _) = callback.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        let mut byte = [0];
        while !header.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
            assert!(header.len() < 32768);
        }
        let header = String::from_utf8(header).unwrap();
        let length = header
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(name, _)| name.eq_ignore_ascii_case("Content-Length"))
                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        stream.read_exact(&mut body).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
        (header, String::from_utf8(body).unwrap())
    });
    let mut subscribe = TcpStream::connect(address).unwrap();
    subscribe
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    write!(subscribe,"SUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: {address}\r\nNT: upnp:event\r\nCALLBACK: <http://{callback_address}/complete?whole=yes>\r\nTIMEOUT: Second-60\r\nConnection: close\r\n\r\n").unwrap();
    let mut response = String::new();
    subscribe.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let (headers, initial) = callback_thread.join().unwrap();
    assert!(
        headers.starts_with("NOTIFY /complete?whole=yes HTTP/1.1"),
        "{headers}"
    );
    assert!(
        headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("SEQ: 0")),
        "{headers}"
    );
    assert!(initial.contains("STOPPED"), "{initial}");
    let result = command(
        &runtime,
        "revoke",
        json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"fixture"}),
    );
    assert_eq!(result["status"], "applied", "{result}");
    let denied = soap(address, "GetMediaInfo", "<InstanceID>0</InstanceID>");
    assert!(
        denied.starts_with("HTTP/1.1 500") && denied.contains("<errorCode>606</errorCode>"),
        "{denied}"
    );
    let result = command(&runtime, "shutdown", json!({"method":"owner.shutdown"}));
    assert_ne!(result["status"], "rejected", "{result}");
    join.join().unwrap().unwrap();
}

#[test]
fn default_owner_has_no_renderer_listener() {
    let runtime = std::env::temp_dir().join(format!(
        "ed-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let owner = EngineOwner::bind(&runtime).unwrap();
    assert_eq!(owner.renderer_address(), None);
    let _ = Ipv4Addr::LOCALHOST;
}

struct OwnerFixture {
    runtime: std::path::PathBuf,
    address: SocketAddrV4,
    done: std::sync::mpsc::Receiver<Result<(), earph_core::EngineError>>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl OwnerFixture {
    fn new(peer: Ipv4Addr) -> Self {
        let runtime = std::env::temp_dir().join(format!(
            "ev-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let owner = EngineOwner::bind(&runtime)
            .unwrap()
            .with_renderer(
                RendererConfiguration::new(
                    SocketAddrV4::new(peer, 0),
                    "uuid:00000000-0000-0000-0000-000000000004",
                    "Event owner fixture",
                    "Earph/1 UPnP/1.0 Earph/1",
                )
                .unwrap(),
            )
            .unwrap();
        let address = owner.renderer_address().unwrap();
        let (done, receiver) = std::sync::mpsc::sync_channel(1);
        let join = std::thread::spawn(move || {
            let _ = done.send(owner.serve());
        });
        let fixture = Self {
            runtime,
            address,
            done: receiver,
            join: Some(join),
        };
        let result = command(
            &fixture.runtime,
            "grant",
            json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"fixture","peers":[peer],"rights":["stage","transport","events"],"playback":null}]}}),
        );
        assert_eq!(result["status"], "applied", "{result}");
        fixture
    }
    fn finish(&mut self) {
        let result = command(
            &self.runtime,
            "shutdown",
            json!({"method":"owner.shutdown"}),
        );
        assert_ne!(result["status"], "rejected", "{result}");
        self.done
            .recv_timeout(Duration::from_secs(8))
            .expect("real event owner shutdown did not join")
            .unwrap();
        self.join.take().unwrap().join().unwrap();
    }
}
impl Drop for OwnerFixture {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            let _ = std::panic::catch_unwind(|| {
                command(
                    &self.runtime,
                    "shutdown-after-failure",
                    json!({"method":"owner.shutdown"}),
                )
            });
            if self.done.recv_timeout(Duration::from_secs(8)).is_ok() {
                let _ = join.join();
            } else {
                std::mem::forget(join);
            }
        }
    }
}
fn private_peer() -> Ipv4Addr {
    let route = UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let std::net::IpAddr::V4(peer) = route.local_addr().unwrap().ip() else {
        panic!("IPv4 required")
    };
    assert!(peer.is_private());
    peer
}
struct CallbackServer {
    address: std::net::SocketAddr,
    received: std::sync::mpsc::Receiver<String>,
    hold: std::sync::Arc<std::sync::atomic::AtomicBool>,
    held_closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl CallbackServer {
    fn new(peer: Ipv4Addr) -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind((peer, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        let held_closed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (send, received) = std::sync::mpsc::sync_channel(16);
        let (holding, closed, stopping) = (hold.clone(), held_closed.clone(), stop.clone());
        let join = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let mut stream = match listener.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) => panic!("owned callback accept failed: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut bytes = Vec::new();
                let mut byte = [0];
                while !bytes.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                    assert!(bytes.len() < 32768);
                }
                let headers = String::from_utf8(bytes).unwrap();
                let count = header(&headers, "Content-Length")
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                assert!(count <= 262144);
                let mut body = vec![0; count];
                stream.read_exact(&mut body).unwrap();
                send.send(format!("{headers}{}", String::from_utf8(body).unwrap()))
                    .unwrap();
                if holding.load(Ordering::Acquire) {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(20)))
                        .unwrap();
                    while holding.load(Ordering::Acquire) && !stopping.load(Ordering::Acquire) {
                        match stream.read(&mut byte) {
                            Ok(0) => {
                                closed.store(true, Ordering::Release);
                                break;
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                                ) => {}
                            Ok(_) => panic!("unexpected callback request body suffix"),
                            Err(_) => {
                                closed.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                } else {
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .unwrap();
                    stream.shutdown(std::net::Shutdown::Write).unwrap();
                }
            }
        });
        Self {
            address,
            received,
            hold,
            held_closed,
            stop,
            join: Some(join),
        }
    }
    fn take(&self) -> String {
        self.received
            .recv_timeout(Duration::from_secs(5))
            .expect("actual owner did not dispatch NOTIFY")
    }
    fn wait_closed(&self) {
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !self.held_closed.load(std::sync::atomic::Ordering::Acquire) {
            assert!(
                std::time::Instant::now() < end,
                "revoked callback transport stayed open"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.hold.store(false, std::sync::atomic::Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}
fn header<'a>(response: &'a str, name: &str) -> Option<&'a str> {
    response.lines().find_map(|line| {
        line.split_once(':')
            .filter(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
    })
}
fn event_request(address: SocketAddrV4, method: &str, service: &str, headers: &str) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    write!(stream,"{method} /upnp/{service}/events HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n{headers}\r\n").unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}
fn callback_grant(
    owner: &OwnerFixture,
    callback: &CallbackServer,
    peer: Ipv4Addr,
    lifetime_ms: u32,
) {
    let result = command(
        &owner.runtime,
        "callbacks",
        json!({"method":"renderer.callbacks.replace","controller_id":"fixture","expected_controller_generation":1,"expected_generation":0,"lifetime_ms":lifetime_ms,"grants":{"origins":[{"origin":format!("http://{}",callback.address),"pins":[peer]}]}}),
    );
    assert_eq!(result["status"], "applied", "{result}");
}
fn subscribe(owner: &OwnerFixture, callback: &CallbackServer) -> String {
    event_request(
        owner.address,
        "SUBSCRIBE",
        "avt",
        &format!(
            "NT: upnp:event\r\nCALLBACK: <http://{}/notify?whole=yes>\r\nTIMEOUT: Second-60\r\n",
            callback.address
        ),
    )
}
#[test]
fn owner_event_pressure_preserves_progress_and_revoke_joins_actual_transport() {
    let peer = private_peer();
    let callback = CallbackServer::new(peer);
    let mut owner = OwnerFixture::new(peer);
    callback_grant(&owner, &callback, peer, 60000);
    assert!(soap(owner.address, "Stop", "<InstanceID>0</InstanceID>").starts_with("HTTP/1.1 200"));
    let mut sids = Vec::new();
    for _ in 0..6 {
        let response = subscribe(&owner, &callback);
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let sid = header(&response, "SID").unwrap().to_owned();
        let event = callback.take();
        assert_eq!(header(&event, "SID"), Some(sid.as_str()));
        assert_eq!(header(&event, "SEQ"), Some("0"));
        assert!(event.contains("NO_MEDIA_PRESENT"));
        sids.push(sid);
    }
    let response = subscribe(&owner, &callback);
    assert!(
        response.starts_with("HTTP/1.1 503"),
        "seventh full AVT lease must refuse before SID: {response}"
    );
    assert!(header(&response, "SID").is_none());
    assert!(
        soap(owner.address, "GetMediaInfo", "<InstanceID>0</InstanceID>")
            .starts_with("HTTP/1.1 200")
    );
    assert!(soap(owner.address, "Stop", "<InstanceID>0</InstanceID>").starts_with("HTTP/1.1 200"));
    let response = event_request(
        owner.address,
        "SUBSCRIBE",
        "rcs",
        &format!("SID: {}\r\nTIMEOUT: Second-60\r\n", sids[0]),
    );
    assert!(
        response.starts_with("HTTP/1.1 412"),
        "wrong-service SID renewed: {response}"
    );
    let response = event_request(
        owner.address,
        "SUBSCRIBE",
        "avt",
        &format!("SID: {}\r\nTIMEOUT: Second-60\r\n", sids[0]),
    );
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    for sid in sids {
        let response = event_request(
            owner.address,
            "UNSUBSCRIBE",
            "avt",
            &format!("SID: {sid}\r\n"),
        );
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    }
    callback
        .hold
        .store(true, std::sync::atomic::Ordering::Release);
    let end = std::time::Instant::now() + Duration::from_secs(5);
    let response = loop {
        let response = subscribe(&owner, &callback);
        if response.starts_with("HTTP/1.1 200") {
            break response;
        }
        assert!(
            response.starts_with("HTTP/1.1 503") && std::time::Instant::now() < end,
            "retired leases did not restore admission: {response}"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    let event = callback.take();
    assert_eq!(header(&event, "SID"), header(&response, "SID"));
    let result = command(
        &owner.runtime,
        "revoke-callbacks",
        json!({"method":"renderer.callbacks.revoke","controller_id":"fixture","expected_controller_generation":1,"expected_generation":1}),
    );
    assert_eq!(result["status"], "applied", "{result}");
    callback.wait_closed();
    assert!(subscribe(&owner, &callback).starts_with("HTTP/1.1 412"));
    owner.finish();
}
#[test]
fn owner_callback_expiry_retires_a_real_inflight_notify_without_media() {
    let peer = private_peer();
    let callback = CallbackServer::new(peer);
    let mut owner = OwnerFixture::new(peer);
    callback_grant(&owner, &callback, peer, 2500);
    callback
        .hold
        .store(true, std::sync::atomic::Ordering::Release);
    let response = subscribe(&owner, &callback);
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    let event = callback.take();
    assert!(event.contains("NO_MEDIA_PRESENT"));
    callback.wait_closed();
    assert!(subscribe(&owner, &callback).starts_with("HTTP/1.1 412"));
    owner.finish();
}

#[test]
fn owner_missing_source_returns_the_avtransport_transition_fault() {
    let peer = private_peer();
    let mut owner = OwnerFixture::new(peer);
    for (action, args) in [
        ("Play", "<InstanceID>0</InstanceID><Speed>1</Speed>"),
        ("Pause", "<InstanceID>0</InstanceID>"),
    ] {
        let response = soap(owner.address, action, args);
        assert!(
            response.starts_with("HTTP/1.1 500") && response.contains("<errorCode>701</errorCode>"),
            "{action}: {response}"
        );
    }
    owner.finish();
}

#[test]
fn owner_controller_replacement_rechecks_ambiguous_peer_before_staging() {
    let peer = private_peer();
    let mut owner = OwnerFixture::new(peer);
    let replaced = command(
        &owner.runtime,
        "replace-controllers",
        json!({"method":"renderer.grants.replace","expected_generation":1,"lifetime_ms":60000,"grants":{"controllers":[{"id":"fixture","peers":[peer],"rights":["stage","transport"],"playback":null},{"id":"other","peers":[peer],"rights":["stage","transport"],"playback":null}]}}),
    );
    assert_eq!(replaced["status"], "applied", "{replaced}");
    let denied = soap(
        owner.address,
        "SetAVTransportURI",
        "<InstanceID>0</InstanceID><CurrentURI>http://192.0.2.4:4321/public.flac</CurrentURI><CurrentURIMetaData>owned fixture</CurrentURIMetaData>",
    );
    assert!(denied.contains("<errorCode>606</errorCode>"), "{denied}");
    let staging = command(
        &owner.runtime,
        "staging-after-replacement",
        json!({"method":"sources.get"}),
    );
    assert_eq!(staging["result"]["value"]["generation"], 0);
    owner.finish();
}

/// Drives the shipped owner executable, so the command line option itself is under test.
#[cfg(target_os = "linux")]
mod owner_process {
    use super::{command, soap};
    use serde_json::{Value, json};
    use std::{
        io::{BufRead, BufReader, ErrorKind},
        net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket},
        path::PathBuf,
        process::{Child, Command, Output, Stdio},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 255, 255, 250), 1900);
    /// Serializes the tests that bind the loopback SSDP port.
    static SSDP_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn runtime() -> PathBuf {
        std::env::temp_dir().join(format!(
            "ep-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    fn owner_command(runtime: &PathBuf, options: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_earph-engine"));
        command
            .args(["owner", "--runtime"])
            .arg(runtime)
            .args(options)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }
    /// Runs an owner start that must fail before it serves.
    /// An owner that starts instead is stopped after a bounded wait, so the test cannot hang.
    fn refused(runtime: &PathBuf, options: &[&str]) -> Output {
        let mut child = owner_command(runtime, options).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{options:?} started an owner");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success(), "{options:?} started an owner");
        assert!(output.stdout.is_empty(), "{options:?} announced an owner");
        output
    }
    struct OwnerProcess {
        child: Child,
        runtime: PathBuf,
        announcement: Value,
    }
    impl OwnerProcess {
        fn start(options: &[&str]) -> Self {
            Self::start_at(runtime(), options)
        }
        fn start_at(runtime: PathBuf, options: &[&str]) -> Self {
            let mut child = owner_command(&runtime, options).spawn().unwrap();
            // A reader thread bounds the wait, so a stuck owner fails the test instead of hanging it.
            let mut stdout = BufReader::new(child.stdout.take().unwrap());
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let mut line = String::new();
                let _ = stdout.read_line(&mut line);
                let _ = sender.send(line);
            });
            let Ok(line) = receiver.recv_timeout(Duration::from_secs(20)) else {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "owner announced nothing in time: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            };
            let Ok(announcement) = serde_json::from_str::<Value>(&line) else {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "owner did not start: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            };
            assert_eq!(announcement["status"], "owner");
            Self {
                child,
                runtime,
                announcement,
            }
        }
        fn renderer_address(&self) -> SocketAddrV4 {
            self.announcement["renderer"]["address"]
                .as_str()
                .expect("announced renderer address")
                .parse()
                .unwrap()
        }
        fn renderer(&self, id: &str) -> Value {
            let receipt = command(&self.runtime, id, json!({"method":"renderer.get"}));
            assert_eq!(receipt["status"], "applied", "{receipt}");
            receipt["result"]["value"].clone()
        }
        /// Lists this process's IPv4 and IPv6 sockets as `table local-address state`.
        fn network_sockets(&self) -> Vec<String> {
            let inodes: Vec<String> = std::fs::read_dir(format!("/proc/{}/fd", self.child.id()))
                .unwrap()
                .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
                .filter_map(|link| {
                    link.to_str()?
                        .strip_prefix("socket:[")?
                        .strip_suffix(']')
                        .map(str::to_owned)
                })
                .collect();
            assert!(!inodes.is_empty(), "the owner holds its local IPC socket");
            let mut sockets = Vec::new();
            for table in ["tcp", "udp", "tcp6", "udp6", "raw", "raw6"] {
                let Ok(text) = std::fs::read_to_string(format!("/proc/net/{table}")) else {
                    continue;
                };
                for line in text.lines().skip(1) {
                    let fields: Vec<_> = line.split_whitespace().collect();
                    if fields
                        .get(9)
                        .is_some_and(|inode| inodes.iter().any(|i| i == inode))
                    {
                        sockets.push(format!("{table} {} {}", fields[1], fields[3]));
                    }
                }
            }
            sockets.sort();
            sockets
        }
        /// Stops the owner through its command socket and requires a clean exit.
        fn shutdown(mut self) {
            let receipt = command(
                &self.runtime,
                "shutdown",
                json!({"method":"owner.shutdown"}),
            );
            assert_ne!(receipt["status"], "rejected", "{receipt}");
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(status.success(), "owner exit {status}");
                    return;
                }
                assert!(Instant::now() < deadline, "owner did not exit");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    impl Drop for OwnerProcess {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    /// The kernel prints an IPv4 socket address as native-endian hex.
    fn proc_address(address: SocketAddrV4) -> String {
        format!(
            "{:08X}:{:04X}",
            u32::from_ne_bytes(address.ip().octets()),
            address.port()
        )
    }
    fn failure(output: &Output) -> String {
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    #[test]
    fn owner_process_without_the_renderer_option_opens_no_network_socket() {
        let owner = OwnerProcess::start(&[]);
        assert!(owner.announcement.get("renderer").is_none());
        let renderer = owner.renderer("default-renderer");
        assert_eq!(renderer["enabled"], false);
        assert_eq!(renderer["listener"], Value::Null);
        // No IPv4 or IPv6 socket means no listener and no way to send multicast.
        assert_eq!(owner.network_sockets(), Vec::<String>::new());
        owner.shutdown();
    }

    #[test]
    fn owner_process_renderer_option_serves_soap_and_closes_its_listener() {
        let owner = OwnerProcess::start(&["--renderer-listen", "127.0.0.1:0"]);
        let address = owner.renderer_address();
        assert_eq!(*address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(address.port(), 0);
        assert_eq!(owner.announcement["renderer"]["discovery"], false);
        let renderer = owner.renderer("listening-renderer");
        assert_eq!(renderer["enabled"], true);
        assert_eq!(
            renderer["listener"],
            json!({"address": address.to_string(), "discovery": false})
        );
        // One listening TCP socket on the selected address, and no UDP socket.
        assert_eq!(
            owner.network_sockets(),
            vec![format!("tcp {} 0A", proc_address(address))]
        );
        let refused = soap(address, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        assert!(
            refused.starts_with("HTTP/1.1 500") && refused.contains("<errorCode>606</errorCode>"),
            "an ungranted peer was served: {refused}"
        );
        let grant = command(
            &owner.runtime,
            "grant",
            json!({"method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":60000,"grants":{"controllers":[{"id":"fixture","peers":["127.0.0.1"],"rights":["stage","transport"],"playback":null}]}}),
        );
        assert_eq!(grant["status"], "applied", "{grant}");
        let read = soap(address, "GetTransportInfo", "<InstanceID>0</InstanceID>");
        assert!(
            read.starts_with("HTTP/1.1 200")
                && read.contains("<CurrentTransportState>NO_MEDIA_PRESENT</CurrentTransportState>"),
            "{read}"
        );
        let play = soap(
            address,
            "Play",
            "<InstanceID>0</InstanceID><Speed>1</Speed>",
        );
        assert!(
            play.starts_with("HTTP/1.1 500") && play.contains("<errorCode>701</errorCode>"),
            "Play without a staged source: {play}"
        );
        owner.shutdown();
        assert_eq!(
            TcpStream::connect(address).unwrap_err().kind(),
            ErrorKind::ConnectionRefused
        );
    }

    /// Reads datagrams until one satisfies `wanted`. Other senders may share this host.
    fn datagram(socket: &UdpSocket, what: &str, wanted: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut bytes = [0; 2048];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "no {what} arrived");
            socket.set_read_timeout(Some(remaining)).unwrap();
            if let Ok((length, _)) = socket.recv_from(&mut bytes)
                && let Ok(text) = std::str::from_utf8(&bytes[..length])
                && wanted(text)
            {
                return text.to_owned();
            }
        }
    }

    #[test]
    fn owner_process_discovery_option_advertises_answers_and_says_byebye_on_loopback() {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, socket_with, sockopt};
        let _port = SSDP_PORT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Both test sockets stay on the loopback interface. Nothing reaches the network.
        // Close-on-exec keeps this socket out of the owner process that the test inspects.
        let group = socket_with(
            AddressFamily::INET,
            SocketType::DGRAM,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        sockopt::set_socket_reuseaddr(&group, true).unwrap();
        bind(&group, &GROUP).unwrap();
        sockopt::set_ip_add_membership(&group, GROUP.ip(), &Ipv4Addr::LOCALHOST).unwrap();
        let group = UdpSocket::from(group);
        let searcher = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        sockopt::set_ip_multicast_if(&searcher, &Ipv4Addr::LOCALHOST).unwrap();

        let owner =
            OwnerProcess::start(&["--renderer-listen", "127.0.0.1:0", "--renderer-discovery"]);
        let address = owner.renderer_address();
        let location = format!("LOCATION: http://{address}/upnp/device.xml\r\n");
        assert_eq!(owner.announcement["renderer"]["discovery"], true);
        assert_eq!(
            owner.renderer("discoverable-renderer")["listener"],
            json!({"address": address.to_string(), "discovery": true})
        );
        let ssdp = proc_address(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1900));
        assert_eq!(
            owner.network_sockets(),
            vec![
                format!("tcp {} 0A", proc_address(address)),
                format!("udp {ssdp} 07"),
                format!("udp {} 07", proc_address(GROUP)),
            ]
        );
        let alive = datagram(&group, "alive advertisement", |text| {
            text.contains("NTS: ssdp:alive\r\n") && text.contains(&location)
        });
        let usn = alive
            .lines()
            .find_map(|line| line.strip_prefix("USN: "))
            .expect("advertised USN");
        let udn = usn.split("::").next().unwrap().to_owned();
        assert!(udn.starts_with("uuid:"), "{alive}");

        searcher
            .send_to(
                b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: upnp:rootdevice\r\n\r\n",
                GROUP,
            )
            .unwrap();
        let answer = datagram(&searcher, "search response", |text| {
            text.starts_with("HTTP/1.1 200 OK\r\n")
        });
        assert!(
            answer.contains(&location) && answer.contains(&format!("USN: {udn}::upnp:rootdevice")),
            "{answer}"
        );

        owner.shutdown();
        datagram(&group, "byebye advertisement", |text| {
            text.contains("NTS: ssdp:byebye\r\n") && text.contains(&udn)
        });
        assert_eq!(
            TcpStream::connect(address).unwrap_err().kind(),
            ErrorKind::ConnectionRefused
        );
    }

    /// Reads the device name that the description serves to any peer.
    fn described_udn(address: SocketAddrV4) -> String {
        use std::io::{Read, Write};
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        write!(
            stream,
            "GET /upnp/device.xml HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let (_, rest) = response.split_once("<UDN>").expect("described UDN");
        rest.split_once("</UDN>").unwrap().0.to_owned()
    }

    #[test]
    fn owner_process_keeps_its_device_identity_across_restarts() {
        let first = OwnerProcess::start(&["--renderer-listen", "127.0.0.1:0"]);
        let runtime = first.runtime.clone();
        let udn = described_udn(first.renderer_address());
        assert!(udn.starts_with("uuid:") && udn.len() == 41, "{udn}");
        first.shutdown();
        let second = OwnerProcess::start_at(runtime, &["--renderer-listen", "127.0.0.1:0"]);
        assert_eq!(described_udn(second.renderer_address()), udn);
        second.shutdown();
        let other = OwnerProcess::start(&["--renderer-listen", "127.0.0.1:0"]);
        assert_ne!(described_udn(other.renderer_address()), udn);
        other.shutdown();
    }

    const LISTEN_ERROR: &str =
        "--renderer-listen needs one concrete IPv4 address and port, such as 192.0.2.10:49152";
    const DISCOVERY_ALONE_ERROR: &str = "--renderer-discovery needs --renderer-listen IPV4:PORT";
    const IDENTITY_ERROR: &str = "invalid renderer identity";
    const PRIVATE_ERROR: &str = "runtime paths must be private and owned by the current user";
    const DISCOVERY_ERROR: &str = "cannot start renderer discovery on the selected interface";

    /// Requires exit status 3 and returns the message of the one JSON error on stderr.
    fn error_message(output: &Output) -> String {
        assert_eq!(output.status.code(), Some(3), "{}", failure(output));
        let document: Value = serde_json::from_slice(&output.stderr)
            .unwrap_or_else(|_| panic!("stderr is not one error document: {}", failure(output)));
        document["error"]["message"]
            .as_str()
            .expect("error message")
            .to_owned()
    }

    #[test]
    fn owner_process_refuses_a_missing_or_ambiguous_renderer_choice() {
        // `None` expects the usage text with exit status 2. `Some` expects that exact error.
        for (options, expected) in [
            (&["--renderer-listen", "0.0.0.0:0"][..], Some(LISTEN_ERROR)),
            (&["--renderer-listen", "127.0.0.1"], Some(LISTEN_ERROR)),
            (&["--renderer-listen", "localhost:0"], Some(LISTEN_ERROR)),
            (&["--renderer-listen", "[::1]:0"], Some(LISTEN_ERROR)),
            (
                &["--renderer-listen", "239.255.255.250:1900"],
                Some(LISTEN_ERROR),
            ),
            (
                &["--renderer-listen", "255.255.255.255:0"],
                Some(LISTEN_ERROR),
            ),
            (&["--renderer-listen", ""], Some(LISTEN_ERROR)),
            (&["--renderer-discovery"], Some(DISCOVERY_ALONE_ERROR)),
            (&["--renderer-listen"], None),
            (
                &[
                    "--renderer-listen",
                    "127.0.0.1:0",
                    "--renderer-listen",
                    "127.0.0.1:0",
                ],
                None,
            ),
            (
                &[
                    "--renderer-listen",
                    "127.0.0.1:0",
                    "--renderer-discovery",
                    "--renderer-discovery",
                ],
                None,
            ),
            (&["--renderer-listen", "127.0.0.1:0", "--renderer"], None),
        ] {
            let runtime = runtime();
            let output = refused(&runtime, options);
            match expected {
                Some(message) => assert_eq!(error_message(&output), message, "{options:?}"),
                None => {
                    assert_eq!(output.status.code(), Some(2), "{options:?}");
                    assert!(
                        failure(&output).starts_with("usage: earph-engine owner --runtime "),
                        "{options:?}: {}",
                        failure(&output)
                    );
                }
            }
            assert!(
                !runtime.exists(),
                "{options:?} touched the runtime before refusing"
            );
        }
    }

    /// Creates a private runtime that already holds `renderer-udn`, as `write` leaves it.
    fn runtime_with_identity(write: impl FnOnce(&std::path::Path)) -> PathBuf {
        use std::os::unix::fs::DirBuilderExt;
        let runtime = runtime();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&runtime)
            .unwrap();
        write(&runtime.join("renderer-udn"));
        runtime
    }
    fn write_identity(path: &std::path::Path, mode: u32, bytes: &[u8]) {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    const VALID_UDN: &[u8] = b"uuid:01234567-89ab-4def-8123-456789abcdef";

    #[test]
    fn owner_process_refuses_a_malformed_identity_file_and_keeps_it() {
        for bytes in [
            &b"not a device name"[..],
            b"",
            b"uuid:01234567-89ab-4def-8123-456789abcdef\n",
            b"uuid:01234567-89ab-4def-8123-456789abcdeg",
            b"\xff\xfe",
        ] {
            let runtime = runtime_with_identity(|path| write_identity(path, 0o600, bytes));
            let output = refused(&runtime, &["--renderer-listen", "127.0.0.1:0"]);
            assert_eq!(error_message(&output), IDENTITY_ERROR, "{bytes:?}");
            assert_eq!(
                std::fs::read(runtime.join("renderer-udn")).unwrap(),
                bytes,
                "the owner replaced a malformed identity"
            );
        }
        // The same runtime layout starts when the file holds a valid name.
        let runtime = runtime_with_identity(|path| write_identity(path, 0o600, VALID_UDN));
        let owner = OwnerProcess::start_at(runtime, &["--renderer-listen", "127.0.0.1:0"]);
        assert_eq!(
            described_udn(owner.renderer_address()).as_bytes(),
            VALID_UDN
        );
        owner.shutdown();
    }

    #[test]
    fn owner_process_refuses_an_identity_file_that_is_not_private() {
        use std::os::unix::fs::PermissionsExt;
        let readable = runtime_with_identity(|path| {
            write_identity(path, 0o600, VALID_UDN);
            // Set the mode after creation, so the process umask cannot hide it.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        });
        let output = refused(&readable, &["--renderer-listen", "127.0.0.1:0"]);
        assert_eq!(error_message(&output), PRIVATE_ERROR);
        assert_eq!(
            std::fs::read(readable.join("renderer-udn")).unwrap(),
            VALID_UDN
        );

        let linked = runtime_with_identity(|path| {
            let target = path.with_file_name("identity-target");
            write_identity(&target, 0o600, VALID_UDN);
            std::os::unix::fs::symlink(&target, path).unwrap();
        });
        let output = refused(&linked, &["--renderer-listen", "127.0.0.1:0"]);
        assert_eq!(error_message(&output), PRIVATE_ERROR);
        assert!(
            std::fs::symlink_metadata(linked.join("renderer-udn"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn owner_process_fails_to_start_when_discovery_cannot_bind() {
        let _port = SSDP_PORT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // Without address reuse, this socket makes the owner's discovery bind fail.
        let _occupied = UdpSocket::bind((Ipv4Addr::LOCALHOST, 1900)).unwrap();
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = free.local_addr().unwrap();
        drop(free);
        let runtime = runtime();
        let output = refused(
            &runtime,
            &[
                "--renderer-listen",
                &address.to_string(),
                "--renderer-discovery",
            ],
        );
        assert_eq!(error_message(&output), DISCOVERY_ERROR);
        assert_eq!(
            TcpStream::connect(address).unwrap_err().kind(),
            ErrorKind::ConnectionRefused
        );
        assert!(
            !runtime.join("renderer-udn").exists(),
            "a failed renderer start created the device identity"
        );
    }

    #[test]
    fn owner_process_fails_to_start_when_the_renderer_cannot_bind() {
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listen = occupied.local_addr().unwrap().to_string();
        let runtime = runtime();
        let output = refused(&runtime, &["--renderer-listen", &listen]);
        assert_eq!(output.status.code(), Some(3));
        assert!(
            failure(&output).contains("cannot bind renderer listener"),
            "{}",
            failure(&output)
        );
        // The failed start released the runtime, so a plain owner can start there.
        OwnerProcess::start_at(runtime, &[]).shutdown();
    }

    #[test]
    fn owner_process_refuses_renderer_options_when_an_owner_already_runs() {
        let owner = OwnerProcess::start(&[]);
        let output = refused(&owner.runtime, &["--renderer-listen", "127.0.0.1:0"]);
        assert_eq!(output.status.code(), Some(3));
        assert!(
            failure(&output).contains("cannot adopt renderer options"),
            "{}",
            failure(&output)
        );
        assert_eq!(owner.renderer("still-default")["enabled"], false);
        assert_eq!(owner.network_sockets(), Vec::<String>::new());
        owner.shutdown();
    }
}
