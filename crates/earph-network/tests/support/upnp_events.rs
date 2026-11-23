// ABOUTME: Drives the production UPnP adapter with real Core transactions and private sockets.
// ABOUTME: Keeps fixture authority explicit and shares no production mock mode.
#![allow(dead_code)]
use earph_core::{control::*, ids::*, renderer::*, renderer_events::*, session::Session};
use earph_network::upnp::*;
use std::{
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddrV4, TcpListener, TcpStream, UdpSocket},
    sync::Arc,
    time::{Duration, Instant},
};
pub fn private_ip() -> Ipv4Addr {
    let route = UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let IpAddr::V4(ip) = route.local_addr().unwrap().ip() else {
        panic!("IPv4 fixture")
    };
    assert!(ip.is_private());
    ip
}
pub fn listener() -> TcpListener {
    let l = TcpListener::bind((private_ip(), 0)).unwrap();
    l.set_nonblocking(true).unwrap();
    l
}
pub fn wait<T>(mut operation: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(value) = operation() {
            return value;
        }
        assert!(Instant::now() < end, "bounded fixture operation");
        std::thread::sleep(Duration::from_millis(2));
    }
}
pub fn read_http(stream: &mut TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let n = stream.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let h = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = h.lines().find_map(|l| {
                l.split_once(':')
                    .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
            });
            if length.is_some_and(|n| bytes.len() >= end + 4 + n) {
                break;
            }
        }
    }
    bytes
}
pub struct Owner {
    pub surface: HttpSurface,
    pub session: Session,
    pub start: Instant,
    serial: u64,
    pub peer: Ipv4Addr,
}
impl Owner {
    pub fn new(origins: Vec<RendererCallbackOrigin>) -> Self {
        let peer = private_ip();
        let surface = HttpSurface::bind(
            SocketAddrV4::new(peer, 0),
            DeviceIdentity::new(
                "uuid:01234567-89ab-cdef-0123-456789abcdef",
                "Event fixture",
                "Fixture/1 UPnP/1.0 Earph/1",
            )
            .unwrap(),
        )
        .unwrap();
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("event-wire-owner").unwrap(),
            ControlLimits::default(),
        );
        session.enable_renderer_control();
        let mut owner = Self {
            surface,
            session,
            start: Instant::now(),
            serial: 0,
            peer,
        };
        let grant = Command::RendererGrantsReplace {
            expected_generation: 0,
            lifetime_ms: 120000,
            grants: Arc::new(RendererGrantDocument {
                controllers: vec![RendererControllerGrant {
                    id: RendererControllerId::new("controller").unwrap(),
                    peers: vec![IpAddr::V4(peer)],
                    rights: vec![RendererRight::Stage, RendererRight::Events],
                    playback: None,
                }],
            }),
        };
        assert_eq!(owner.local(grant).status, ReceiptStatus::Applied);
        assert_eq!(
            owner
                .local(Command::RendererCallbackGrantsReplace {
                    controller_id: RendererControllerId::new("controller").unwrap(),
                    expected_controller_generation: 1,
                    expected_generation: 0,
                    lifetime_ms: 120000,
                    grants: Arc::new(RendererCallbackGrantDocument { origins })
                })
                .status,
            ReceiptStatus::Applied
        );
        owner
    }
    pub fn now(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap()
    }
    pub fn envelope(&mut self, payload: Command) -> CommandEnvelope {
        self.serial += 1;
        let s = self.session.snapshot();
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(format!("wire-{}", self.serial)).unwrap(),
            zone_id: s.zone_id,
            owner_id: s.owner_id,
            session_id: s.session_id,
            expected_revision: Some(s.revision),
            payload,
        }
    }
    pub fn local(&mut self, payload: Command) -> Receipt {
        let c = self.envelope(payload);
        let now = self.now();
        self.session.submit_with_network_effects(
            c,
            now,
            |_| Ok(Vec::new()),
            |_, _| Ok(()),
            |_| Ok(()),
        )
    }
    pub fn pending(
        &self,
        service: RendererService,
        callbacks: &[String],
    ) -> (TcpStream, PendingSubscription) {
        let slug = match service {
            RendererService::AvTransport => "avt",
            RendererService::RenderingControl => "rcs",
            RendererService::ConnectionManager => "cm",
        };
        let mut stream = TcpStream::connect(self.surface.local_addr()).unwrap();
        let urls = callbacks
            .iter()
            .map(|s| format!("<{s}>"))
            .collect::<Vec<_>>()
            .join(" ");
        write!(stream,"SUBSCRIBE /upnp/{slug}/events HTTP/1.1\r\nHost: fixture\r\nCALLBACK: {urls}\r\nNT: upnp:event\r\nTIMEOUT: Second-90\r\nContent-Length: 0\r\n\r\n").unwrap();
        let pending = wait(|| match self.surface.try_recv_incoming() {
            Some(IncomingRequest::Subscription(s)) => Some(s),
            Some(_) => panic!("subscription ticket"),
            None => None,
        });
        (stream, pending)
    }
    pub fn admit(&mut self, pending: &PendingSubscription) -> RendererSubscriptionInfo {
        let lease = pending.reserve_lease().unwrap();
        let command = self.envelope(Command::RendererSubscription {
            controller_id: RendererControllerId::new("controller").unwrap(),
            grant_generation: 1,
            callback_generation: 1,
            action: pending.action().clone(),
        });
        let now = self.now();
        let remaining = u64::try_from(
            pending
                .response_deadline()
                .saturating_duration_since(Instant::now())
                .as_nanos(),
        )
        .unwrap();
        let result = self.session.submit_renderer_subscription_with_effects(
            command,
            IpAddr::V4(pending.peer()),
            now,
            Some(RendererSubscriptionAdmission {
                lease,
                response_deadline_monotonic_ns: now + remaining,
            }),
            |_| Ok(()),
        );
        assert_eq!(
            result.receipt.status,
            ReceiptStatus::Applied,
            "{:?}",
            result.receipt
        );
        result.subscription.unwrap()
    }
    pub fn subscribe(
        &mut self,
        service: RendererService,
        callbacks: &[String],
    ) -> RendererSubscriptionInfo {
        let (mut socket, pending) = self.pending(service, callbacks);
        let info = self.admit(&pending);
        assert!(
            self.session
                .begin_renderer_delivery(&info.binding, self.now())
                .is_err()
        );
        assert!(self.surface.try_recv_response_completion().is_none());
        assert!(pending.respond(Ok(Some(&info))));
        let response = read_http(&mut socket);
        assert!(response.starts_with(b"HTTP/1.1 200"));
        let mut end = [1];
        assert_eq!(socket.read(&mut end).unwrap(), 0, "response TCP write FIN");
        let ack = wait(|| self.surface.try_recv_response_completion());
        assert!(ack.success);
        assert_eq!(ack.binding, info.binding);
        self.session
            .acknowledge_renderer_subscription_response(&ack.binding, ack.success, self.now())
            .unwrap();
        info
    }
    pub fn authorize(&self) {
        while let Some(attempt) = self.surface.try_recv_callback_attempt() {
            let now = self.now();
            let view = self
                .session
                .renderer_delivery_authority(&attempt.delivery, now);
            assert!(attempt.authorize(view, now));
        }
    }
    pub fn completion(&self) -> DeliveryCompletion {
        wait(|| {
            self.authorize();
            self.surface.try_recv_delivery_completion()
        })
    }
    pub fn dispatch(&mut self, info: &RendererSubscriptionInfo) -> Arc<RendererDelivery> {
        let delivery = self
            .session
            .begin_renderer_delivery(&info.binding, self.now())
            .unwrap();
        self.surface
            .start_delivery(delivery.clone(), self.now())
            .unwrap();
        delivery
    }
    pub fn finish(&mut self, completion: DeliveryCompletion) {
        assert!(completion.transport_joined);
        self.session
            .complete_renderer_delivery(
                &completion.delivery,
                completion.outcome,
                completion.transport_joined,
                self.now(),
            )
            .unwrap();
    }
    pub fn stage(&mut self, generation: u64, metadata: &str) {
        let c = self.envelope(Command::RendererRequest {
            controller_id: RendererControllerId::new("controller").unwrap(),
            grant_generation: 1,
            action: RendererAction::StageUri {
                expected_staging_generation: generation,
                uri: "http://private.example:80/track?a=1&b=2".into(),
                metadata: metadata.into(),
            },
        });
        assert_eq!(
            self.session
                .submit_renderer_with_network_effects(
                    c,
                    IpAddr::V4(self.peer),
                    self.now(),
                    |_, _| Ok(()),
                    |_| Ok(())
                )
                .status,
            ReceiptStatus::Applied
        );
    }
}
pub fn origin(listener: &TcpListener) -> RendererCallbackOrigin {
    RendererCallbackOrigin {
        origin: format!(
            "http://callback.test:{}",
            listener.local_addr().unwrap().port()
        ),
        pins: vec![IpAddr::V4(private_ip())],
    }
}
pub fn url(listener: &TcpListener, path: &str) -> String {
    format!(
        "http://callback.test:{}{path}",
        listener.local_addr().unwrap().port()
    )
}
