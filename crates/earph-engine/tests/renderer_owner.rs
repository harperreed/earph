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
