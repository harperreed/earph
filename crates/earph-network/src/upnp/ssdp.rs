// ABOUTME: Generates bounded UDA1.0 discovery messages and schedules selected-interface replies.
// ABOUTME: Owns sockets explicitly without enabling discovery or granting controller authority.
use super::{DeviceIdentity, Service, SoapError};
use std::{
    collections::{BTreeMap, hash_map::RandomState},
    hash::BuildHasher,
    io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket},
    sync::Arc,
    time::{Duration, Instant},
};

pub const SSDP_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const GROUP: SocketAddrV4 = SocketAddrV4::new(SSDP_GROUP, 1900);
const RENDERER: &str = "urn:schemas-upnp-org:device:MediaRenderer:1";
const MAX_DATAGRAM: usize = 2048;
const MAX_SCHEDULED: usize = 32;
#[derive(Debug, PartialEq, Eq)]
pub enum SearchError {
    Malformed,
    ResourceBudgetExceeded,
}
pub struct Search {
    target: String,
    mx: u64,
}
impl Search {
    pub fn mx_seconds(&self) -> u64 {
        self.mx
    }
    pub fn target(&self) -> &str {
        &self.target
    }
}

pub fn parse_search(bytes: &[u8]) -> Result<Search, SearchError> {
    if bytes.len() > MAX_DATAGRAM {
        return Err(SearchError::ResourceBudgetExceeded);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SearchError::Malformed)?;
    if !text.ends_with("\r\n\r\n")
        || !text
            .bytes()
            .all(|b| b == b'\r' || b == b'\n' || b == b'\t' || (32..127).contains(&b))
    {
        return Err(SearchError::Malformed);
    }
    let mut lines = text[..text.len() - 4].split("\r\n");
    if lines.next() != Some("M-SEARCH * HTTP/1.1") {
        return Err(SearchError::Malformed);
    }
    let mut headers = BTreeMap::new();
    for (index, line) in lines.enumerate() {
        if index == 32 {
            return Err(SearchError::ResourceBudgetExceeded);
        }
        let (name, value) = line.split_once(':').ok_or(SearchError::Malformed)?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return Err(SearchError::Malformed);
        }
        if headers
            .insert(name.to_ascii_lowercase(), value.trim())
            .is_some()
        {
            return Err(SearchError::Malformed);
        }
    }
    if !matches!(
        headers.get("host"),
        Some(&"239.255.255.250:1900" | &"239.255.255.250")
    ) || headers.get("man") != Some(&"\"ssdp:discover\"")
    {
        return Err(SearchError::Malformed);
    }
    let mx = headers
        .get("mx")
        .ok_or(SearchError::Malformed)?
        .parse::<u64>()
        .map_err(|_| SearchError::Malformed)?;
    if mx == 0 {
        return Err(SearchError::Malformed);
    }
    let target = *headers.get("st").ok_or(SearchError::Malformed)?;
    if target.is_empty() || target.len() > 128 {
        return Err(SearchError::Malformed);
    }
    Ok(Search {
        target: target.into(),
        mx: mx.min(5),
    })
}

pub struct Advertisement {
    pub(super) address: SocketAddrV4,
    identity: DeviceIdentity,
    targets: Vec<(String, String)>,
}
impl Advertisement {
    pub fn new(address: SocketAddrV4, identity: DeviceIdentity) -> Result<Self, SoapError> {
        if address.port() == 0
            || address.ip().is_unspecified()
            || address.ip().is_multicast()
            || address.ip().is_broadcast()
        {
            return Err(SoapError::InvalidArguments);
        }
        let mut targets = vec![
            (
                "upnp:rootdevice".into(),
                format!("{}::upnp:rootdevice", identity.udn),
            ),
            (identity.udn.clone(), identity.udn.clone()),
            (RENDERER.into(), format!("{}::{RENDERER}", identity.udn)),
        ];
        targets.extend(Service::ALL.map(|service| {
            (
                service.namespace().into(),
                format!("{}::{}", identity.udn, service.namespace()),
            )
        }));
        Ok(Self {
            address,
            identity,
            targets,
        })
    }
    fn notification(&self, alive: bool) -> Vec<Vec<u8>> {
        self.targets.iter().map(|(target,usn)|{
        let mut text=format!("NOTIFY * HTTP/1.1\r\nHOST: {GROUP}\r\nNT: {target}\r\nNTS: ssdp:{}\r\nUSN: {usn}\r\n",if alive{"alive"}else{"byebye"});
        if alive{text.push_str(&format!("CACHE-CONTROL: max-age=1800\r\nLOCATION: http://{}/upnp/device.xml\r\nSERVER: {}\r\n",self.address,self.identity.server));}text.push_str("\r\n");debug_assert!(text.len()<=MAX_DATAGRAM);text.into_bytes()
    }).collect()
    }
    pub fn alive(&self) -> Vec<Vec<u8>> {
        self.notification(true)
    }
    pub fn byebye(&self) -> Vec<Vec<u8>> {
        self.notification(false)
    }
    pub fn responses(&self, search: &Search) -> Vec<Vec<u8>> {
        self.targets.iter().filter(|(target,_)|search.target=="ssdp:all"||search.target==*target).map(|(target,usn)|format!("HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nEXT:\r\nLOCATION: http://{}/upnp/device.xml\r\nSERVER: {}\r\nST: {target}\r\nUSN: {usn}\r\n\r\n",self.address,self.identity.server).into_bytes()).collect()
    }
}
struct Scheduled {
    peer: SocketAddrV4,
    bytes: Arc<[u8]>,
    due: Duration,
    expires: Duration,
}
pub struct SsdpScheduler {
    advertisement: Advertisement,
    queue: Vec<Scheduled>,
    random: RandomState,
    nonce: u64,
    sent_at: Vec<Duration>,
}
impl SsdpScheduler {
    pub fn new(advertisement: Advertisement) -> Self {
        Self {
            advertisement,
            queue: Vec::with_capacity(MAX_SCHEDULED),
            random: RandomState::new(),
            nonce: 0,
            sent_at: Vec::with_capacity(10),
        }
    }
    pub fn queued(&self) -> usize {
        self.queue.len()
    }
    pub fn schedule(
        &mut self,
        bytes: &[u8],
        peer: SocketAddrV4,
        now: Duration,
    ) -> Result<usize, SearchError> {
        if peer.port() == 0
            || peer.ip().is_unspecified()
            || peer.ip().is_multicast()
            || peer.ip().is_broadcast()
        {
            return Err(SearchError::Malformed);
        }
        let search = parse_search(bytes)?;
        let responses = self.advertisement.responses(&search);
        let count = responses.len();
        if self.queue.len() + count > MAX_SCHEDULED {
            return Err(SearchError::ResourceBudgetExceeded);
        }
        let expires = now
            .checked_add(Duration::from_secs(search.mx))
            .ok_or(SearchError::ResourceBudgetExceeded)?;
        self.nonce = self
            .nonce
            .checked_add(1)
            .ok_or(SearchError::ResourceBudgetExceeded)?;
        for (index, bytes) in responses.into_iter().enumerate() {
            let ms = self.random.hash_one((self.nonce, index, peer)) % (search.mx * 1000 + 1);
            self.queue.push(Scheduled {
                peer,
                bytes: bytes.into(),
                due: now + Duration::from_millis(ms),
                expires,
            });
        }
        Ok(count)
    }
    fn notifications(&mut self, alive: bool, now: Duration) {
        if !alive {
            self.queue.clear();
        }
        let bytes = if alive {
            self.advertisement.alive()
        } else {
            self.advertisement.byebye()
        };
        if self.queue.len() + bytes.len() > MAX_SCHEDULED {
            return;
        }
        for bytes in bytes {
            self.queue.push(Scheduled {
                peer: GROUP,
                bytes: bytes.into(),
                due: now,
                expires: now + Duration::from_secs(2),
            });
        }
    }
    pub fn next_due(&mut self, now: Duration) -> Option<(SocketAddrV4, Arc<[u8]>)> {
        self.queue.retain(|packet| packet.expires >= now);
        self.sent_at
            .retain(|at| now.saturating_sub(*at) < Duration::from_secs(1));
        if self.sent_at.len() == 10 {
            return None;
        }
        let (index, _) = self
            .queue
            .iter()
            .enumerate()
            .filter(|(_, p)| p.due <= now)
            .min_by_key(|(_, p)| p.due)?;
        let packet = self.queue.swap_remove(index);
        self.sent_at.push(now);
        Some((packet.peer, packet.bytes))
    }
}

/// True for a send failure that describes the network at this moment, such as a route or
/// interface that is briefly gone, or a packet filter reject. The socket itself still works.
fn transient_send_failure(kind: io::ErrorKind) -> bool {
    use io::ErrorKind::*;
    matches!(
        kind,
        NetworkUnreachable
            | HostUnreachable
            | NetworkDown
            | AddrNotAvailable
            | PermissionDenied
            | Interrupted
    )
}

pub struct SsdpEndpoint {
    receive: UdpSocket,
    send: UdpSocket,
    scheduler: SsdpScheduler,
    started: Instant,
    next_alive: Duration,
    multicast: bool,
}
impl SsdpEndpoint {
    pub(super) fn http_address(&self) -> SocketAddrV4 {
        self.scheduler.advertisement.address
    }
    /// Serve already-owned unicast sockets without unsolicited multicast advertisements.
    pub fn from_bound(
        receive: UdpSocket,
        send: UdpSocket,
        advertisement: Advertisement,
    ) -> io::Result<Self> {
        if receive.local_addr()?.ip() != *advertisement.address.ip()
            || send.local_addr()?.ip() != *advertisement.address.ip()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "discovery sockets must use the selected HTTP interface",
            ));
        }
        receive.set_nonblocking(true)?;
        send.set_nonblocking(true)?;
        Ok(Self {
            receive,
            send,
            scheduler: SsdpScheduler::new(advertisement),
            started: Instant::now(),
            next_alive: Duration::ZERO,
            multicast: false,
        })
    }
    /// Acquire only the explicitly selected interface; never fall back to a wildcard bind.
    #[cfg(target_os = "linux")]
    pub fn bind_multicast(advertisement: Advertisement, ttl: u32) -> io::Result<Self> {
        use socket2::{Domain, Protocol, Socket, Type};
        if !(1..=255).contains(&ttl) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "multicast TTL must be 1 through 255",
            ));
        }
        let interface = *advertisement.address.ip();
        let send = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        send.set_reuse_address(true)?;
        send.bind(&SocketAddrV4::new(interface, 1900).into())?;
        send.set_multicast_if_v4(&interface)?;
        send.set_multicast_ttl_v4(ttl)?;
        send.set_nonblocking(true)?;
        let receive = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
        receive.set_reuse_address(true)?;
        receive.set_multicast_all_v4(false)?;
        receive.bind(&GROUP.into())?;
        receive.join_multicast_v4(&SSDP_GROUP, &interface)?;
        receive.set_nonblocking(true)?;
        Ok(Self {
            receive: receive.into(),
            send: send.into(),
            scheduler: SsdpScheduler::new(advertisement),
            started: Instant::now(),
            next_alive: Duration::ZERO,
            multicast: true,
        })
    }
    #[cfg(not(target_os = "linux"))]
    pub fn bind_multicast(_advertisement: Advertisement, _ttl: u32) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "selected-interface multicast acquisition is not qualified on this platform",
        ))
    }
    pub fn poll(&mut self) -> io::Result<usize> {
        let now = self.started.elapsed();
        if self.multicast && now >= self.next_alive {
            self.scheduler.notifications(true, now);
            self.next_alive = now + Duration::from_secs(900);
        }
        let mut bytes = [0; MAX_DATAGRAM + 1];
        for _ in 0..MAX_SCHEDULED {
            match self.receive.recv_from(&mut bytes) {
                Ok((length, SocketAddr::V4(peer))) => {
                    let _ = self.scheduler.schedule(&bytes[..length], peer, now);
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        let mut sent = 0;
        while let Some((peer, bytes)) = self.scheduler.next_due(now) {
            match self.send.send_to(&bytes, peer) {
                Ok(length) if length == bytes.len() => sent += 1,
                Ok(_) => return Err(io::Error::other("partial discovery datagram")),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                // A remote UDP port refusal can refer to an earlier reply on this socket.
                Err(error) if peer != GROUP && error.kind() == io::ErrorKind::ConnectionRefused => {
                }
                // The network refused this one datagram. The shared runtime also serves HTTP
                // control and events, so discard the datagram and keep that runtime.
                Err(error) if transient_send_failure(error.kind()) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(sent)
    }
    pub fn shutdown(mut self) -> io::Result<()> {
        if self.multicast {
            let now = self.started.elapsed();
            self.scheduler.notifications(false, now);
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.scheduler.queued() > 0 && Instant::now() < deadline {
                let now = self.started.elapsed();
                while let Some((peer, bytes)) = self.scheduler.next_due(now) {
                    self.send.send_to(&bytes, peer)?;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisements_and_replies_share_a_rolling_global_rate_bound() {
        let identity = DeviceIdentity::new(
            "uuid:01234567-89ab-cdef-0123-456789abcdef",
            "Fixture",
            "FixtureOS/1 UPnP/1.0 Earph/0.1",
        )
        .unwrap();
        let mut scheduler = SsdpScheduler::new(
            Advertisement::new(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 24680), identity).unwrap(),
        );
        scheduler.notifications(true, Duration::ZERO);
        scheduler.notifications(true, Duration::ZERO);
        for _ in 0..10 {
            assert!(scheduler.next_due(Duration::from_millis(900)).is_some());
        }
        assert!(
            scheduler.next_due(Duration::from_secs(1)).is_none(),
            "fixed-window rollover allowed another immediate burst"
        );
        assert!(scheduler.next_due(Duration::from_millis(1900)).is_some());
    }

    #[cfg(target_os = "linux")]
    mod send_failures {
        use super::*;
        use crate::upnp::HttpSurface;
        use std::{
            io::{Read, Write},
            net::TcpStream,
        };

        fn identity() -> DeviceIdentity {
            DeviceIdentity::new(
                "uuid:01234567-89ab-cdef-0123-456789abcdef",
                "Fixture",
                "FixtureOS/1 UPnP/1.0 Earph/0.1",
            )
            .unwrap()
        }
        /// Starts a real listener with loopback discovery that must first send `first`.
        /// A datagram for the returned socket follows it in the queue.
        fn surface_sending(first: Scheduled) -> (HttpSurface, UdpSocket) {
            let surface =
                HttpSurface::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0), identity()).unwrap();
            let mut endpoint = SsdpEndpoint::from_bound(
                UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
                UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
                Advertisement::new(surface.local_addr(), identity()).unwrap(),
            )
            .unwrap();
            let listener = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
            let SocketAddr::V4(peer) = listener.local_addr().unwrap() else {
                panic!("IPv4 required")
            };
            endpoint.scheduler.queue.push(first);
            endpoint.scheduler.queue.push(scheduled(peer, b"delivered"));
            surface.attach_discovery(endpoint).unwrap();
            (surface, listener)
        }
        fn scheduled(peer: SocketAddrV4, bytes: &[u8]) -> Scheduled {
            Scheduled {
                peer,
                bytes: bytes.into(),
                due: Duration::ZERO,
                expires: Duration::from_secs(3600),
            }
        }
        fn describes(address: SocketAddrV4) -> bool {
            let Ok(mut stream) = TcpStream::connect(address) else {
                return false;
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let request = format!(
                "GET /upnp/device.xml HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
            );
            let mut response = String::new();
            stream.write_all(request.as_bytes()).is_ok()
                && stream.read_to_string(&mut response).is_ok()
                && response.starts_with("HTTP/1.1 200")
        }

        #[test]
        fn a_send_that_the_network_refuses_keeps_discovery_and_http_control_running() {
            // The kernel refuses a broadcast destination on a socket without SO_BROADCAST.
            // That is a real EACCES, the error kind that a firewall reject also reports.
            let refused = SocketAddrV4::new(Ipv4Addr::new(127, 255, 255, 255), 9);
            let (mut surface, listener) = surface_sending(scheduled(refused, b"refused"));
            listener
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = [0; 32];
            let (length, _) = listener
                .recv_from(&mut bytes)
                .expect("the datagram after the refused send never left");
            assert_eq!(&bytes[..length], b"delivered");
            assert!(
                surface.poll_shutdown().is_none(),
                "one refused send stopped HTTP control"
            );
            assert!(describes(surface.local_addr()));
            let stats = surface.shutdown();
            assert!(stats.joined);
            assert_eq!(stats.service_error, None);
        }

        #[test]
        fn a_send_that_the_socket_cannot_make_still_stops_the_runtime_visibly() {
            // No UDP datagram can carry this payload, so the kernel reports EMSGSIZE.
            let peer = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9);
            let (mut surface, _listener) = surface_sending(scheduled(peer, &[0; 70_000]));
            let deadline = Instant::now() + Duration::from_secs(5);
            let stats = loop {
                if let Some(stats) = surface.poll_shutdown() {
                    break stats;
                }
                assert!(Instant::now() < deadline, "a broken send kept the runtime");
                std::thread::sleep(Duration::from_millis(10));
            };
            assert!(stats.joined);
            assert!(stats.service_error.is_some());
            assert!(!describes(surface.local_addr()));
        }

        #[test]
        fn only_network_conditions_count_as_transient_send_failures() {
            use io::ErrorKind::*;
            for kind in [
                NetworkUnreachable,
                HostUnreachable,
                NetworkDown,
                AddrNotAvailable,
                PermissionDenied,
                Interrupted,
            ] {
                assert!(transient_send_failure(kind), "{kind:?}");
            }
            for kind in [
                BrokenPipe,
                InvalidInput,
                NotConnected,
                Other,
                ConnectionRefused,
            ] {
                assert!(!transient_send_failure(kind), "{kind:?}");
            }
        }
    }
}
