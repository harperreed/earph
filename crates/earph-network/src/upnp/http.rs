// ABOUTME: Serves bounded one-request HTTP connections on one explicitly selected IPv4 address.
// ABOUTME: Delivers typed owner tickets through bounded queues without claiming owner execution.
use super::budget::{Active, BASE_RESERVATION, Budget, Lease};
use super::{
    Service, SoapError, SoapRequest, UpnpFault, decode_soap, device_description, encode_fault,
    encode_response, service_description,
};
use crate::PolicyError;
use bytes::Bytes;
use earph_core::renderer_events::*;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use std::sync::Mutex;
use std::{
    convert::Infallible,
    fmt, io,
    net::{Ipv4Addr, SocketAddrV4, TcpListener},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, net::TcpStream, sync::oneshot, task::JoinSet};

pub const MAX_HEADERS_BYTES: usize = 32768;
pub const MAX_HEADERS: usize = 64;
pub const MAX_SOAP_BYTES: usize = 262144;
pub use super::budget::ADAPTER_MEMORY_BYTES;
const READ_DEADLINE: Duration = Duration::from_secs(5);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub struct DeviceIdentity {
    pub(super) udn: String,
    pub(super) friendly_name: String,
    pub(super) server: String,
}
impl DeviceIdentity {
    pub fn new(udn: &str, friendly_name: &str, server: &str) -> Result<Self, SoapError> {
        device_description(udn, friendly_name)?;
        if server.is_empty()
            || server.len() > 128
            || !server.bytes().all(|b| (32..127).contains(&b))
            || !server.contains(" UPnP/1.0 ")
        {
            return Err(SoapError::InvalidArguments);
        }
        Ok(Self {
            udn: udn.into(),
            friendly_name: friendly_name.into(),
            server: server.into(),
        })
    }
}
impl fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceIdentity").finish_non_exhaustive()
    }
}

pub struct PendingAction {
    request: SoapRequest,
    id: u64,
    peer: Ipv4Addr,
    deadline: Instant,
    reply: Option<oneshot::Sender<(u16, Vec<u8>)>>,
    stopped: Arc<AtomicBool>,
    _lease: Arc<Lease>,
}
impl PendingAction {
    pub fn request(&self) -> &SoapRequest {
        &self.request
    }
    pub fn request_id(&self) -> u64 {
        self.id
    }
    pub fn peer(&self) -> Ipv4Addr {
        self.peer
    }
    pub fn is_live(&self) -> bool {
        !self.stopped.load(Ordering::Acquire)
            && Instant::now() < self.deadline
            && self.reply.as_ref().is_some_and(|r| !r.is_closed())
    }
    /// Reply only after the bridge completes its real scoped transaction.
    pub fn respond(mut self, result: Result<&[(&str, &str)], UpnpFault>) -> bool {
        if !self.is_live() {
            return false;
        }
        let encoded = match result {
            Ok(values) => match encode_response(&self.request, values) {
                Ok(bytes) => (200, bytes),
                Err(_) => (500, encode_fault(UpnpFault::ActionFailed)),
            },
            Err(fault) => (500, encode_fault(fault)),
        };
        self.reply.take().is_some_and(|r| r.send(encoded).is_ok())
    }
}
impl fmt::Debug for PendingAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingAction")
            .field("request", &self.request)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct SurfaceStats {
    pub reserved_bytes: usize,
    pub active: usize,
    pub queued: usize,
    pub joined: bool,
    pub service_error: Option<io::ErrorKind>,
}
pub struct HttpSurface {
    address: SocketAddrV4,
    stop: Arc<AtomicBool>,
    general: Receiver<PendingAction>,
    lifecycle: Receiver<PendingAction>,
    subscriptions: Receiver<PendingSubscription>,
    subscription_lifecycle: Receiver<PendingSubscription>,
    completions: Receiver<SubscriptionResponseCompletion>,
    joined: bool,
    service_error: Option<io::ErrorKind>,
    budget: Arc<Budget>,
    events: super::events::EventHandle,
    discovery: SyncSender<super::SsdpEndpoint>,
    discovery_attached: AtomicBool,
    thread: Option<thread::JoinHandle<io::Result<()>>>,
}
impl HttpSurface {
    pub fn bind(address: SocketAddrV4, identity: DeviceIdentity) -> io::Result<Self> {
        if address.ip().is_unspecified()
            || address.ip().is_multicast()
            || address.ip().is_broadcast()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "select one concrete IPv4 interface",
            ));
        }
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let std::net::SocketAddr::V4(address) = listener.local_addr()? else {
            return Err(io::Error::other("expected IPv4 listener"));
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (stop, budget) = (Arc::new(AtomicBool::new(false)), Budget::new());
        let (subscriptions_tx, subscriptions) = mpsc::sync_channel(4);
        let (subscription_lifecycle_tx, subscription_lifecycle) = mpsc::sync_channel(2);
        let (completions_tx, completions) = mpsc::sync_channel(16);
        let completion_credits = Arc::new(AtomicUsize::new(0));
        let (general_tx, general) = mpsc::sync_channel(14);
        let (lifecycle_tx, lifecycle) = mpsc::sync_channel(2);
        let (discovery_tx, discovery_rx) = mpsc::sync_channel::<super::SsdpEndpoint>(1);
        let shared = Arc::new(Shared {
            identity,
            device: Bytes::new(),
            services: Service::ALL.map(|s| Bytes::from(service_description(s))),
            stop: stop.clone(),
            budget: budget.clone(),
            general: general_tx,
            lifecycle: lifecycle_tx,
            subscriptions: subscriptions_tx,
            subscription_lifecycle: subscription_lifecycle_tx,
            completions: completions_tx,
            completion_credits,
        });
        let mut shared =
            Arc::try_unwrap(shared).map_err(|_| io::Error::other("description ownership"))?;
        shared.device = Bytes::from(
            device_description(&shared.identity.udn, &shared.identity.friendly_name)
                .map_err(|_| io::Error::other("invalid device identity"))?,
        );
        let static_bytes =
            shared.device.len() + shared.services.iter().map(Bytes::len).sum::<usize>();
        if static_bytes > BASE_RESERVATION / 2 {
            return Err(io::Error::other("description budget exceeded"));
        }
        let shared = Arc::new(shared);
        let thread_shared = shared.clone();
        let (events, mut event_runtime) =
            super::events::EventHandle::new(stop.clone(), budget.clone());
        let thread=thread::Builder::new().name("earph-upnp-wire".into()).spawn(move||runtime.block_on(async move{
            let listener=tokio::net::TcpListener::from_std(listener)?;let mut connections=JoinSet::new();let mut discovery=None;let mut failure=None;
            while !thread_shared.stop.load(Ordering::Acquire){
                event_runtime.poll();
                if let Ok(endpoint)=discovery_rx.try_recv(){discovery=Some(endpoint);}
                if let Some(endpoint)=discovery.as_mut() && let Err(error)=endpoint.poll(){failure=Some(error);break;}
                while connections.try_join_next().is_some(){}
                match tokio::time::timeout(Duration::from_millis(20),listener.accept()).await{
                    Ok(Ok((mut stream,peer)))=>{
                        let reserved=thread_shared.budget.request();
                        if reserved.is_none(){let _=tokio::time::timeout(Duration::from_millis(100),stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")).await;continue;}
                        thread_shared.budget.active.fetch_add(1,Ordering::AcqRel);
                        let shared=thread_shared.clone();let lease=reserved.expect("admitted request lease");let active=Active(shared.budget.clone());
                        connections.spawn(async move{
                            let _active=active;
                            let deadline=Instant::now()+Duration::from_secs(15);
                            let completion=Arc::new(ResponseCell::new(shared.completions.clone()));
                            let guard=ResponseGuard(completion.clone());
                            if let Err(status)=header_gate(&stream).await{let _=tokio::time::timeout(Duration::from_secs(1),stream.write_all(format!("HTTP/1.1 {status} Rejected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes())).await;return;}
                            let std::net::IpAddr::V4(peer)=peer.ip()else{return;};
                            let service=service_fn(move|request|serve(request,shared.clone(),peer,lease.clone(),deadline,completion.clone()));
                            let mut builder=http1::Builder::new();builder.keep_alive(false).max_headers(MAX_HEADERS).max_buf_size(MAX_HEADERS_BYTES).timer(TokioTimer::new()).header_read_timeout(READ_DEADLINE);
                            let result=tokio::time::timeout_at(deadline.into(),builder.serve_connection(TokioIo::new(stream),service)).await;
                            guard.0.finish(matches!(result,Ok(Ok(()))));
                        });
                    }
                    Ok(Err(error))=>{failure=Some(error);break;},
                    Err(_)=>{}
                }
            }
            thread_shared.stop.store(true,Ordering::Release);
            connections.abort_all();while connections.join_next().await.is_some(){}
            event_runtime.shutdown().await;
            if let Some(endpoint)=discovery && let Err(error)=endpoint.shutdown(){failure.get_or_insert(error);}
            match failure{Some(error)=>Err(error),None=>Ok(())}
        }))?;
        Ok(Self {
            address,
            stop,
            general,
            lifecycle,
            subscriptions,
            subscription_lifecycle,
            completions,
            joined: false,
            service_error: None,
            budget,
            events,
            discovery: discovery_tx,
            discovery_attached: AtomicBool::new(false),
            thread: Some(thread),
        })
    }
    pub fn local_addr(&self) -> SocketAddrV4 {
        self.address
    }
    pub fn attach_discovery(&self, endpoint: super::SsdpEndpoint) -> io::Result<()> {
        if endpoint.http_address() != self.address {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "discovery must reference this selected HTTP listener",
            ));
        }
        if self.discovery_attached.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "discovery is already attached",
            ));
        }
        self.discovery
            .try_send(endpoint)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "discovery runtime unavailable"))
    }
    pub fn try_recv(&self) -> Option<PendingAction> {
        loop {
            let request = self
                .lifecycle
                .try_recv()
                .or_else(|_| self.general.try_recv())
                .ok()?;
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            if request.is_live() {
                return Some(request);
            }
        }
    }
    pub fn try_recv_incoming(&self) -> Option<IncomingRequest> {
        while let Ok(action) = self.lifecycle.try_recv() {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            if action.is_live() {
                return Some(IncomingRequest::Action(action));
            }
        }
        while let Ok(subscription) = self.subscription_lifecycle.try_recv() {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            if subscription.is_live() {
                return Some(IncomingRequest::Subscription(subscription));
            }
        }
        while let Ok(subscription) = self.subscriptions.try_recv() {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
            if subscription.is_live() {
                return Some(IncomingRequest::Subscription(subscription));
            }
        }
        self.try_recv().map(IncomingRequest::Action)
    }
    pub fn try_recv_response_completion(&self) -> Option<SubscriptionResponseCompletion> {
        self.completions.try_recv().ok()
    }
    pub fn start_delivery(
        &self,
        delivery: Arc<RendererDelivery>,
        now_ns: u64,
    ) -> Result<(), PolicyError> {
        self.events.start(delivery, now_ns)
    }
    pub fn try_recv_callback_attempt(&self) -> Option<super::CallbackAttempt> {
        self.events.attempt()
    }
    pub fn try_recv_delivery_completion(&self) -> Option<super::DeliveryCompletion> {
        self.events.completion()
    }
    pub fn cancel_delivery(&self, binding: &RendererSubscriptionBinding, delivery_id: u64) -> bool {
        self.events.cancel(binding, delivery_id)
    }
    pub fn request_shutdown(&self) {
        self.stop.store(true, Ordering::Release);
    }
    pub fn poll_shutdown(&mut self) -> Option<SurfaceStats> {
        if self.thread.as_ref().is_some_and(|t| !t.is_finished()) {
            return None;
        }
        self.stop_and_join();
        Some(self.stats())
    }
    pub fn stats(&self) -> SurfaceStats {
        SurfaceStats {
            reserved_bytes: self.budget.reserved(),
            active: self.budget.active.load(Ordering::Acquire),
            queued: self.budget.queued.load(Ordering::Acquire),
            joined: self.joined,
            service_error: self.service_error,
        }
    }
    fn stop_and_join(&mut self) -> (bool, Option<io::ErrorKind>) {
        self.stop.store(true, Ordering::Release);
        let result = match self.thread.take().map(|thread| thread.join()) {
            None => (self.joined, self.service_error),
            Some(Ok(Ok(()))) => (true, None),
            Some(Ok(Err(error))) => (true, Some(error.kind())),
            Some(Err(_)) => (false, Some(io::ErrorKind::Other)),
        };
        while self
            .lifecycle
            .try_recv()
            .or_else(|_| self.general.try_recv())
            .is_ok()
        {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
        }
        self.joined = result.0;
        self.service_error = result.1;
        while self
            .subscription_lifecycle
            .try_recv()
            .or_else(|_| self.subscriptions.try_recv())
            .is_ok()
        {
            self.budget.queued.fetch_sub(1, Ordering::AcqRel);
        }
        result
    }
    pub fn shutdown(mut self) -> SurfaceStats {
        let (joined, service_error) = self.stop_and_join();
        let mut stats = self.stats();
        stats.joined = joined;
        stats.service_error = service_error;
        stats
    }
}
impl Drop for HttpSurface {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

struct Shared {
    identity: DeviceIdentity,
    device: Bytes,
    services: [Bytes; 3],
    stop: Arc<AtomicBool>,
    budget: Arc<Budget>,
    general: SyncSender<PendingAction>,
    lifecycle: SyncSender<PendingAction>,
    subscriptions: SyncSender<PendingSubscription>,
    subscription_lifecycle: SyncSender<PendingSubscription>,
    completions: SyncSender<SubscriptionResponseCompletion>,
    completion_credits: Arc<AtomicUsize>,
}
fn valid_host(value: &str) -> bool {
    let name_char = |b: u8| b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=".contains(&b);
    let (host, port) = if let Some(literal) = value.strip_prefix('[') {
        let Some((host, rest)) = literal.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            let Some((version, address)) = host.split_once('.') else {
                return false;
            };
            if !version.starts_with(['v', 'V'])
                || version.len() < 2
                || !version[1..].bytes().all(|b| b.is_ascii_hexdigit())
                || address.is_empty()
                || !address.bytes().all(|b| name_char(b) || b == b':')
            {
                return false;
            }
        }
        (
            host,
            if rest.is_empty() {
                None
            } else {
                match rest.strip_prefix(':') {
                    Some(port) => Some(port),
                    None => return false,
                }
            },
        )
    } else {
        let (host, port) = value
            .split_once(':')
            .map_or((value, None), |(h, p)| (h, Some(p)));
        let mut bytes = host.as_bytes().iter().copied();
        while let Some(byte) = bytes.next() {
            if byte == b'%' {
                if !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                    || !bytes.next().is_some_and(|b| b.is_ascii_hexdigit())
                {
                    return false;
                }
            } else if !name_char(byte) {
                return false;
            }
        }
        (host, port)
    };
    !host.is_empty() && port.is_none_or(|port| port.bytes().all(|b| b.is_ascii_digit()))
}
async fn header_gate(stream: &TcpStream) -> Result<(), u16> {
    let deadline = Instant::now() + READ_DEADLINE;
    let mut bytes = vec![0; MAX_HEADERS_BYTES + 1];
    loop {
        if Instant::now() >= deadline {
            return Err(408);
        }
        let count = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            stream.peek(&mut bytes),
        )
        .await
        .map_err(|_| 408u16)?
        .map_err(|_| 400u16)?;
        if count == 0 {
            return Err(400);
        }
        if let Some(end) = bytes[..count].windows(4).position(|b| b == b"\r\n\r\n") {
            if end + 4 > MAX_HEADERS_BYTES {
                return Err(431);
            }
            let text = std::str::from_utf8(&bytes[..end]).map_err(|_| 400u16)?;
            let mut content_length = None;
            let mut transfer = false;
            let mut host = false;
            let mut headers = 0;
            for line in text.split("\r\n").skip(1) {
                headers += 1;
                if headers > MAX_HEADERS {
                    return Err(431);
                }
                let (name, value) = line.split_once(':').ok_or(400u16)?;
                let value = value.trim_matches([' ', '\t']);
                if name.eq_ignore_ascii_case("host") {
                    if host || !valid_host(value) {
                        return Err(400);
                    }
                    host = true;
                }
                if name.eq_ignore_ascii_case("content-length") {
                    if content_length.is_some()
                        || !value.bytes().all(|b| b.is_ascii_digit())
                        || value.is_empty()
                    {
                        return Err(400);
                    }
                    let size = value.parse::<u64>().map_err(|_| 413u16)?;
                    if size > MAX_SOAP_BYTES as u64 {
                        return Err(413);
                    }
                    content_length = Some(size);
                }
                if name.eq_ignore_ascii_case("transfer-encoding") {
                    if transfer || !value.eq_ignore_ascii_case("chunked") {
                        return Err(400);
                    }
                    transfer = true;
                }
            }
            if !host || (transfer && content_length.is_some()) {
                return Err(400);
            }
            return Ok(());
        }
        if count > MAX_HEADERS_BYTES {
            return Err(431);
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}
fn response(shared: &Shared, status: u16, bytes: Bytes) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/xml; charset=\"utf-8\"")
        .header("server", &shared.identity.server)
        .header("connection", "close")
        .header("ext", "")
        .body(Full::new(bytes))
        .expect("validated static response headers")
}
async fn serve(
    mut request: Request<Incoming>,
    shared: Arc<Shared>,
    peer: Ipv4Addr,
    lease: Arc<Lease>,
    connection_deadline: Instant,
    completion: Arc<ResponseCell>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if matches!(request.method().as_str(), "SUBSCRIBE" | "UNSUBSCRIBE") {
        return Ok(serve_subscription(
            request,
            shared,
            peer,
            lease,
            connection_deadline,
            completion,
        )
        .await);
    }
    let result = async {
        if request.uri().scheme().is_some() || request.uri().query().is_some() {
            return (404, Vec::new());
        }
        if request.method() == hyper::Method::GET {
            if request.uri().path() == "/upnp/device.xml" {
                return (200, shared.device.to_vec());
            }
            for (index, service) in Service::ALL.iter().enumerate() {
                if request.uri().path() == format!("/upnp/{}/scpd.xml", service.slug()) {
                    return (200, shared.services[index].to_vec());
                }
            }
            return (404, Vec::new());
        }
        if request.method() != hyper::Method::POST {
            return (501, Vec::new());
        }
        let Some(service) = Service::ALL
            .into_iter()
            .find(|s| request.uri().path() == format!("/upnp/{}/control", s.slug()))
        else {
            return (404, Vec::new());
        };
        let read = tokio::time::timeout(READ_DEADLINE, async {
            let mut body = Vec::new();
            while let Some(frame) = request.body_mut().frame().await {
                let frame = frame.map_err(|_| 400u16)?;
                if let Some(data) = frame.data_ref() {
                    if data.len() > MAX_SOAP_BYTES - body.len() {
                        return Err(413);
                    }
                    body.extend_from_slice(data);
                } else {
                    return Err(400);
                }
            }
            Ok::<_, u16>(body)
        })
        .await;
        let body = match read {
            Ok(Ok(body)) => body,
            Ok(Err(status)) => return (status, Vec::new()),
            Err(_) => return (408, Vec::new()),
        };
        if request.headers().get_all("content-type").iter().count() != 1 {
            return (415, Vec::new());
        }
        let content_type = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let mut parts = content_type.split(';');
        if !parts
            .next()
            .is_some_and(|s| s.trim().eq_ignore_ascii_case("text/xml"))
            || parts.any(|part| {
                !part
                    .trim()
                    .replace('"', "")
                    .eq_ignore_ascii_case("charset=utf-8")
            })
        {
            return (415, Vec::new());
        }
        if request.headers().get_all("soapaction").iter().count() != 1 {
            return (500, encode_fault(UpnpFault::InvalidAction));
        }
        let action = request
            .headers()
            .get("soapaction")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let parsed = match decode_soap(service, action, &body) {
            Ok(parsed) => parsed,
            Err(error) => return (500, encode_fault(error.into())),
        };
        if let Err(fault) = parsed.validate_arguments() {
            return (500, encode_fault(fault));
        }
        drop(body);
        let lifecycle = matches!(parsed.name(), "Stop");
        if !lifecycle && !lease.admit_ordinary() {
            return (503, Vec::new());
        }
        let Ok(id) =
            NEXT_REQUEST.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
        else {
            return (503, Vec::new());
        };
        let (tx, rx) = oneshot::channel();
        let pending = PendingAction {
            request: parsed,
            id,
            peer,
            deadline: Instant::now() + READ_DEADLINE,
            reply: Some(tx),
            stopped: shared.stop.clone(),
            _lease: lease,
        };
        shared.budget.queued.fetch_add(1, Ordering::AcqRel);
        if (if lifecycle {
            &shared.lifecycle
        } else {
            &shared.general
        })
        .try_send(pending)
        .is_err()
        {
            shared.budget.queued.fetch_sub(1, Ordering::AcqRel);
            return (503, Vec::new());
        }
        match tokio::time::timeout(READ_DEADLINE, rx).await {
            Ok(Ok(reply)) => reply,
            _ => (500, encode_fault(UpnpFault::ActionFailed)),
        }
    }
    .await;
    Ok(response(&shared, result.0, Bytes::from(result.1)))
}

pub enum IncomingRequest {
    Action(PendingAction),
    Subscription(PendingSubscription),
}
struct CompletionCredit(Arc<AtomicUsize>);
impl Drop for CompletionCredit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct SubscriptionResponseCompletion {
    pub binding: RendererSubscriptionBinding,
    pub success: bool,
    _credit: CompletionCredit,
}
struct ResponseState {
    binding: Option<RendererSubscriptionBinding>,
    finished: Option<bool>,
    handed: Option<bool>,
    credit: Option<CompletionCredit>,
}
struct ResponseCell {
    state: Mutex<ResponseState>,
    sender: SyncSender<SubscriptionResponseCompletion>,
}
impl ResponseCell {
    fn new(sender: SyncSender<SubscriptionResponseCompletion>) -> Self {
        Self {
            state: Mutex::new(ResponseState {
                binding: None,
                finished: None,
                handed: None,
                credit: None,
            }),
            sender,
        }
    }
    fn emit(&self, s: &mut ResponseState) {
        let success = match (s.handed, s.finished) {
            (Some(false), _) => false,
            (Some(true), Some(finished)) => finished,
            _ => return,
        };
        if let (Some(binding), Some(credit)) = (s.binding.take(), s.credit.take()) {
            let _ = self.sender.try_send(SubscriptionResponseCompletion {
                binding,
                success,
                _credit: credit,
            });
        }
    }
    fn bind(&self, binding: RendererSubscriptionBinding) {
        self.state.lock().unwrap().binding = Some(binding);
    }
    fn handoff(&self, success: bool) {
        let mut s = self.state.lock().unwrap();
        s.handed = Some(success);
        self.emit(&mut s);
    }
    fn finish(&self, success: bool) {
        let mut s = self.state.lock().unwrap();
        if s.finished.is_none() {
            s.finished = Some(success);
        }
        self.emit(&mut s);
    }
}
struct ResponseGuard(Arc<ResponseCell>);
impl Drop for ResponseGuard {
    fn drop(&mut self) {
        self.0.finish(false);
    }
}
struct SubscriptionReply {
    status: u16,
    info: Option<(String, u32)>,
}
pub struct PendingSubscription {
    action: RendererSubscriptionAction,
    service: RendererService,
    id: u64,
    peer: Ipv4Addr,
    deadline: Instant,
    ticket_deadline: Instant,
    reply: Option<oneshot::Sender<SubscriptionReply>>,
    stopped: Arc<AtomicBool>,
    lease: Arc<Lease>,
    completion: Arc<ResponseCell>,
}
impl fmt::Debug for PendingSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingSubscription")
            .field("id", &self.id)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}
impl PendingSubscription {
    pub fn action(&self) -> &RendererSubscriptionAction {
        &self.action
    }
    pub fn service(&self) -> RendererService {
        self.service
    }
    pub fn request_id(&self) -> u64 {
        self.id
    }
    pub fn peer(&self) -> Ipv4Addr {
        self.peer
    }
    pub fn response_deadline(&self) -> Instant {
        self.deadline
    }
    pub fn is_live(&self) -> bool {
        !self.stopped.load(Ordering::Acquire)
            && Instant::now() < self.ticket_deadline
            && self.reply.as_ref().is_some_and(|r| !r.is_closed())
    }
    pub fn reserve_lease(&self) -> Result<Arc<super::EventLease>, PolicyError> {
        if !self.is_live() || !matches!(self.action, RendererSubscriptionAction::Subscribe { .. }) {
            return Err(PolicyError::InvalidBinding);
        }
        self.lease.budget.reserve_event(self.service)
    }
    pub fn respond(mut self, result: Result<Option<&RendererSubscriptionInfo>, u16>) -> bool {
        let initial = matches!(self.action, RendererSubscriptionAction::Subscribe { .. });
        if initial && let Ok(Some(info)) = &result {
            self.completion.bind(info.binding.clone());
        }
        let live = self.is_live();
        let reply = match result {
            Ok(info) => SubscriptionReply {
                status: 200,
                info: info.map(|i| (i.binding.sid.clone(), i.timeout_seconds)),
            },
            Err(status) => SubscriptionReply {
                status: if matches!(status, 400 | 412 | 500 | 503) {
                    status
                } else {
                    500
                },
                info: None,
            },
        };
        let sent = live && self.reply.take().is_some_and(|tx| tx.send(reply).is_ok());
        if initial {
            self.completion.handoff(sent);
        }
        sent
    }
}
fn parse_subscription(
    request: &Request<Incoming>,
    service: RendererService,
) -> Result<RendererSubscriptionAction, u16> {
    let headers = request.headers();
    for name in ["callback", "nt", "sid", "timeout"] {
        if headers.get_all(name).iter().count() > 1 {
            return Err(400);
        }
    }
    if headers.contains_key("transfer-encoding")
        || headers
            .get("content-length")
            .is_some_and(|v| v.as_bytes() != b"0")
    {
        return Err(400);
    }
    let text = |name| -> Result<Option<&str>, u16> {
        headers
            .get(name)
            .map(|v| v.to_str().map_err(|_| 400u16))
            .transpose()
    };
    let (sid, callback, nt, timeout) = (
        text("sid")?,
        text("callback")?,
        text("nt")?,
        text("timeout")?,
    );
    if request.method().as_str() == "UNSUBSCRIBE" {
        if callback.is_some() || nt.is_some() || timeout.is_some() {
            return Err(400);
        }
        return Ok(RendererSubscriptionAction::Unsubscribe {
            sid: subscription_sid(sid)?.into(),
        });
    }
    if sid.is_some() && (callback.is_some() || nt.is_some()) {
        return Err(400);
    }
    let timeout_seconds = match timeout {
        None | Some("Second-infinite") => 1800,
        Some(t) => t
            .strip_prefix("Second-")
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| *n > 0)
            .ok_or(400u16)?
            .min(1800),
    };
    if sid.is_some() {
        return Ok(RendererSubscriptionAction::Renew {
            sid: subscription_sid(sid)?.into(),
            timeout_seconds,
        });
    }
    if nt != Some("upnp:event") {
        return Err(412);
    }
    let mut rest = callback.ok_or(412u16)?.trim();
    let mut callbacks = Vec::with_capacity(8);
    while !rest.is_empty() {
        let r = rest.strip_prefix('<').ok_or(412u16)?;
        let end = r.find('>').ok_or(412u16)?;
        let url = &r[..end];
        if callbacks.len() == 8 || url.len() > 8192 {
            return Err(412);
        }
        let checked = crate::CheckedUrl::new(url).map_err(|_| 412u16)?;
        if checked.origin().scheme != "http" || callback_origin(url).is_err() {
            return Err(412);
        }
        callbacks.push(url.to_owned());
        rest = r[end + 1..].trim_start_matches([' ', '\t']);
    }
    if callbacks.is_empty() {
        return Err(412);
    }
    callbacks.shrink_to_fit();
    Ok(RendererSubscriptionAction::Subscribe {
        service,
        callbacks,
        timeout_seconds,
    })
}
fn subscription_sid(sid: Option<&str>) -> Result<&str, u16> {
    sid.filter(|s| {
        s.starts_with("uuid:") && s.len() <= 128 && s.bytes().all(|b| (33..127).contains(&b))
    })
    .ok_or(412)
}
async fn serve_subscription(
    request: Request<Incoming>,
    shared: Arc<Shared>,
    peer: Ipv4Addr,
    lease: Arc<Lease>,
    deadline: Instant,
    completion: Arc<ResponseCell>,
) -> Response<Full<Bytes>> {
    let service = Service::ALL
        .into_iter()
        .find(|s| request.uri().path() == s.event_path());
    let Some(service) = service else {
        return response(&shared, 404, Bytes::new());
    };
    if request.uri().scheme().is_some() || request.uri().query().is_some() {
        return response(&shared, 404, Bytes::new());
    }
    let service = match service {
        Service::AvTransport => RendererService::AvTransport,
        Service::RenderingControl => RendererService::RenderingControl,
        Service::ConnectionManager => RendererService::ConnectionManager,
    };
    let action = match parse_subscription(&request, service) {
        Ok(a) => a,
        Err(status) => return response(&shared, status, Bytes::new()),
    };
    let lifecycle = matches!(action, RendererSubscriptionAction::Unsubscribe { .. });
    if !lifecycle && !lease.admit_ordinary() {
        return response(&shared, 503, Bytes::new());
    }
    if matches!(action, RendererSubscriptionAction::Subscribe { .. }) {
        if shared
            .completion_credits
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 16).then_some(n + 1)
            })
            .is_err()
        {
            return response(&shared, 503, Bytes::new());
        }
        completion.state.lock().unwrap().credit =
            Some(CompletionCredit(shared.completion_credits.clone()));
    }
    let Ok(id) =
        NEXT_REQUEST.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
    else {
        return response(&shared, 503, Bytes::new());
    };
    let (tx, rx) = oneshot::channel();
    let ticket_deadline = deadline.min(Instant::now() + READ_DEADLINE);
    let pending = PendingSubscription {
        action,
        service,
        id,
        peer,
        deadline,
        ticket_deadline,
        reply: Some(tx),
        stopped: shared.stop.clone(),
        lease,
        completion,
    };
    shared.budget.queued.fetch_add(1, Ordering::AcqRel);
    if (if lifecycle {
        &shared.subscription_lifecycle
    } else {
        &shared.subscriptions
    })
    .try_send(pending)
    .is_err()
    {
        shared.budget.queued.fetch_sub(1, Ordering::AcqRel);
        return response(&shared, 503, Bytes::new());
    }
    let reply = match tokio::time::timeout_at(ticket_deadline.into(), rx).await {
        Ok(Ok(r)) => r,
        _ => return response(&shared, 500, Bytes::new()),
    };
    let mut response = response(&shared, reply.status, Bytes::new());
    if let Some((sid, timeout)) = reply.info {
        let Ok(value) = hyper::header::HeaderValue::from_str(&sid) else {
            return self::response(&shared, 500, Bytes::new());
        };
        response.headers_mut().insert("sid", value);
        response.headers_mut().insert(
            "timeout",
            hyper::header::HeaderValue::from_str(&format!("Second-{timeout}"))
                .expect("bounded timeout"),
        );
    }
    response
}
#[cfg(test)]
mod completion_tests {
    use super::*;
    fn binding() -> RendererSubscriptionBinding {
        use earph_core::ids::*;
        RendererSubscriptionBinding {
            owner_id: OwnerId::new("completion-owner").unwrap(),
            renderer_epoch: 1,
            sid: "uuid:completion".into(),
            generation: 1,
            controller_id: RendererControllerId::new("controller").unwrap(),
            controller_generation: 1,
            callback_generation: 1,
        }
    }
    #[test]
    fn response_handoff_waits_for_transport_shutdown_and_reports_failure_after_late_binding() {
        let (tx, rx) = mpsc::sync_channel(16);
        let credit = Arc::new(AtomicUsize::new(1));
        let cell = ResponseCell::new(tx);
        cell.state.lock().unwrap().credit = Some(CompletionCredit(credit.clone()));
        cell.bind(binding());
        cell.handoff(true);
        assert!(
            rx.try_recv().is_err(),
            "handing bytes to Hyper must not acknowledge a subscription"
        );
        cell.finish(true);
        let result = rx.try_recv().unwrap();
        assert!(result.success);
        assert_eq!(result.binding, binding());
        cell.finish(false);
        assert!(rx.try_recv().is_err());
        drop(result);
        assert_eq!(credit.load(Ordering::Acquire), 0);
        let (tx, rx) = mpsc::sync_channel(16);
        let credit = Arc::new(AtomicUsize::new(1));
        let cell = ResponseCell::new(tx);
        cell.state.lock().unwrap().credit = Some(CompletionCredit(credit.clone()));
        cell.finish(false);
        cell.bind(binding());
        cell.handoff(false);
        assert!(!rx.try_recv().unwrap().success);
        assert_eq!(credit.load(Ordering::Acquire), 0);
    }
}
