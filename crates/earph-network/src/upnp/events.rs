// ABOUTME: Owns bounded callback delivery on the explicitly bound listener runtime.
// ABOUTME: Requires fresh canonical callback authority before each pinned TCP attempt.
use super::budget::{Budget, EventBody, SENDER_BUFFER, Scratch};
use crate::{CheckedUrl, PolicyError, policy::permitted_address};
use earph_core::renderer_events::*;
use std::{
    fmt,
    future::{Future, poll_fn},
    io::{self, Write},
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    task::Poll,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    task::{Id, JoinSet},
};
const HEADERS: usize = 32768;
const BODY: usize = 2048;
const RESPONSE: usize = HEADERS + BODY + 32768;
struct Authorization {
    view: Option<RendererCallbackAuthorityView>,
    now_ns: u64,
    at: Instant,
}
pub struct CallbackAttempt {
    pub delivery: Arc<RendererDelivery>,
    reply: oneshot::Sender<Authorization>,
}
impl CallbackAttempt {
    pub fn authorize(self, view: Option<RendererCallbackAuthorityView>, now_ns: u64) -> bool {
        self.reply
            .send(Authorization {
                view,
                now_ns,
                at: Instant::now(),
            })
            .is_ok()
    }
}
impl fmt::Debug for CallbackAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallbackAttempt")
            .field("delivery", &self.delivery)
            .finish_non_exhaustive()
    }
}
pub struct DeliveryCompletion {
    pub delivery: Arc<RendererDelivery>,
    pub outcome: RendererDeliveryOutcome,
    pub transport_joined: bool,
    _retirement: Retirement,
}
impl fmt::Debug for DeliveryCompletion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeliveryCompletion")
            .field("delivery", &self.delivery)
            .field("outcome", &self.outcome)
            .field("transport_joined", &self.transport_joined)
            .finish_non_exhaustive()
    }
}
struct Record {
    delivery: Arc<RendererDelivery>,
    live: Arc<AtomicBool>,
}
struct HandleState {
    records: Mutex<Vec<Record>>,
}
struct Retirement {
    state: Arc<HandleState>,
    delivery: Arc<RendererDelivery>,
    joined: bool,
}
impl Drop for Retirement {
    fn drop(&mut self) {
        if self.joined {
            self.state
                .records
                .lock()
                .unwrap()
                .retain(|r| !same(&r.delivery, &self.delivery));
        }
    }
}
fn same(a: &RendererDelivery, b: &RendererDelivery) -> bool {
    a.binding == b.binding && a.delivery_id == b.delivery_id
}
struct Start {
    delivery: Arc<RendererDelivery>,
    body: EventBody,
    scratch: Scratch,
    live: Arc<AtomicBool>,
    deadline: Instant,
}
pub(super) struct EventHandle {
    state: Arc<HandleState>,
    starts: SyncSender<Start>,
    attempts: Receiver<CallbackAttempt>,
    completions: Receiver<DeliveryCompletion>,
    stop: Arc<AtomicBool>,
    budget: Arc<Budget>,
}
pub(super) struct EventRuntime {
    state: Arc<HandleState>,
    starts: Receiver<Start>,
    attempts: SyncSender<CallbackAttempt>,
    completions: SyncSender<DeliveryCompletion>,
    stop: Arc<AtomicBool>,
    tasks: JoinSet<RendererDeliveryOutcome>,
    identities: Vec<(Id, Arc<RendererDelivery>)>,
}
impl EventHandle {
    pub fn new(stop: Arc<AtomicBool>, budget: Arc<Budget>) -> (Self, EventRuntime) {
        let (start_tx, starts) = mpsc::sync_channel(4);
        let (attempts_tx, attempts) = mpsc::sync_channel(4);
        let (completions_tx, completions) = mpsc::sync_channel(4);
        let state = Arc::new(HandleState {
            records: Mutex::new(Vec::with_capacity(4)),
        });
        (
            Self {
                state: state.clone(),
                starts: start_tx,
                attempts,
                completions,
                stop: stop.clone(),
                budget,
            },
            EventRuntime {
                state,
                starts,
                attempts: attempts_tx,
                completions: completions_tx,
                stop,
                tasks: JoinSet::new(),
                identities: Vec::with_capacity(4),
            },
        )
    }
    pub fn start(&self, delivery: Arc<RendererDelivery>, now_ns: u64) -> Result<(), PolicyError> {
        if self.stop.load(Ordering::Acquire) || now_ns >= delivery.deadline_monotonic_ns {
            return Err(PolicyError::Expired);
        }
        let lease = self
            .budget
            .event(delivery.lease.lease_id())
            .ok_or(PolicyError::InvalidBinding)?;
        let erased: Arc<dyn RendererSubscriptionLease> = lease.clone();
        if !Arc::ptr_eq(&erased, &delivery.lease) || lease.service() != delivery.snapshot.service {
            return Err(PolicyError::InvalidBinding);
        }
        let mut records = self.state.records.lock().unwrap();
        if records.len() >= 4
            || records.iter().any(|r| {
                r.delivery.binding == delivery.binding || r.delivery.position == delivery.position
            })
        {
            return Err(PolicyError::Budget);
        }
        let mut body = lease.take()?;
        delivery
            .snapshot
            .encode_into(&mut body.value)
            .map_err(|_| PolicyError::Budget)?;
        let scratch = lease.pool.take(delivery.position)?;
        let remaining = delivery
            .deadline_monotonic_ns
            .saturating_sub(now_ns)
            .min(RENDERER_DELIVERY_NS);
        let deadline = Instant::now() + Duration::from_nanos(remaining);
        let live = Arc::new(AtomicBool::new(true));
        self.starts
            .try_send(Start {
                delivery: delivery.clone(),
                body,
                scratch,
                live: live.clone(),
                deadline,
            })
            .map_err(|_| PolicyError::Budget)?;
        records.push(Record { delivery, live });
        Ok(())
    }
    pub fn attempt(&self) -> Option<CallbackAttempt> {
        self.attempts.try_recv().ok()
    }
    pub fn completion(&self) -> Option<DeliveryCompletion> {
        self.completions.try_recv().ok()
    }
    pub fn cancel(&self, binding: &RendererSubscriptionBinding, id: u64) -> bool {
        let records = self.state.records.lock().unwrap();
        if let Some(r) = records
            .iter()
            .find(|r| r.delivery.binding == *binding && r.delivery.delivery_id == id)
        {
            r.live.store(false, Ordering::Release);
            true
        } else {
            false
        }
    }
}
impl EventRuntime {
    pub fn poll(&mut self) {
        while let Ok(start) = self.starts.try_recv() {
            let delivery = start.delivery.clone();
            let attempts = self.attempts.clone();
            let stop = self.stop.clone();
            let task = self
                .tasks
                .spawn(async move { send(start, attempts, stop).await });
            self.identities.push((task.id(), delivery));
        }
        while let Some(result) = self.tasks.try_join_next_with_id() {
            self.joined(result);
        }
    }
    fn joined(&mut self, result: Result<(Id, RendererDeliveryOutcome), tokio::task::JoinError>) {
        let (id, outcome, joined) = match result {
            Ok((id, outcome)) => (id, outcome, true),
            Err(e) => (e.id(), RendererDeliveryOutcome::JoinFailed, false),
        };
        if let Some(index) = self.identities.iter().position(|(key, _)| *key == id) {
            let (_, delivery) = self.identities.swap_remove(index);
            let completion = DeliveryCompletion {
                delivery: delivery.clone(),
                outcome,
                transport_joined: joined,
                _retirement: Retirement {
                    state: self.state.clone(),
                    delivery,
                    joined,
                },
            };
            let _ = self.completions.try_send(completion);
        }
    }
    pub async fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        for r in self.state.records.lock().unwrap().iter() {
            r.live.store(false, Ordering::Release);
        }
        self.poll();
        while let Some(result) = self.tasks.join_next_with_id().await {
            self.joined(result);
        }
    }
}

/// A NOTIFY-only permit. No media fetch authority or DNS resolution follows from it.
pub struct CallbackPermit<'a> {
    url: &'a str,
    pins: &'a [IpAddr],
    port: u16,
    authority_end: usize,
}
impl fmt::Debug for CallbackPermit<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CallbackPermit([redacted])")
    }
}
impl<'a> CallbackPermit<'a> {
    pub fn new(
        delivery: &'a RendererDelivery,
        view: &RendererCallbackAuthorityView,
        index: usize,
        now_ns: u64,
    ) -> Result<Self, PolicyError> {
        if view.binding != delivery.binding
            || view.delivery_id != delivery.delivery_id
            || !Arc::ptr_eq(&view.callbacks, &delivery.callbacks)
            || !Arc::ptr_eq(&view.grants, &delivery.grants)
        {
            return Err(PolicyError::InvalidBinding);
        }
        if now_ns >= view.expires_monotonic_ns
            || view.expires_monotonic_ns > delivery.deadline_monotonic_ns
        {
            return Err(PolicyError::Expired);
        }
        let url = delivery
            .callbacks
            .get(index)
            .ok_or(PolicyError::InvalidUrl)?;
        let parsed = callback_origin(url).map_err(|_| PolicyError::InvalidUrl)?;
        let checked = CheckedUrl::new(url)?;
        let origin = checked.origin();
        if origin.scheme != "http" {
            return Err(PolicyError::InvalidUrl);
        }
        let grant = delivery
            .grants
            .origins
            .iter()
            .find(|g| callback_origin(&g.origin).is_ok_and(|o| o == parsed))
            .ok_or(PolicyError::NotGranted)?;
        if grant.pins.is_empty()
            || grant.pins.len() > 8
            || grant.pins.iter().any(|ip| !permitted_address(*ip))
            || origin
                .host
                .parse::<IpAddr>()
                .is_ok_and(|literal| grant.pins.iter().any(|p| *p != literal))
        {
            return Err(PolicyError::InvalidGrant);
        }
        Ok(Self {
            url,
            pins: &grant.pins,
            port: parsed.port,
            authority_end: parsed.authority_end,
        })
    }
}
async fn bounded<F: Future>(
    future: F,
    live: &AtomicBool,
    stop: &AtomicBool,
    deadline: Instant,
) -> Result<F::Output, RendererDeliveryOutcome> {
    let mut future = std::pin::pin!(future);
    let mut tick = std::pin::pin!(tokio::time::sleep(Duration::from_millis(10)));
    poll_fn(|cx| {
        if !live.load(Ordering::Acquire) || stop.load(Ordering::Acquire) {
            return Poll::Ready(Err(RendererDeliveryOutcome::Cancelled));
        }
        if Instant::now() >= deadline {
            return Poll::Ready(Err(RendererDeliveryOutcome::Timeout));
        }
        if let Poll::Ready(value) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(value));
        }
        if tick.as_mut().poll(cx).is_ready() {
            tick.as_mut()
                .reset((Instant::now() + Duration::from_millis(10)).into());
            cx.waker().wake_by_ref();
        }
        Poll::Pending
    })
    .await
}
async fn send(
    mut start: Start,
    attempts: SyncSender<CallbackAttempt>,
    stop: Arc<AtomicBool>,
) -> RendererDeliveryOutcome {
    let delivery = &start.delivery;
    let mut last = RendererDeliveryOutcome::ConnectFailed;
    for index in 0..delivery.callbacks.len() {
        let Ok(origin) = callback_origin(&delivery.callbacks[index]) else {
            return RendererDeliveryOutcome::Cancelled;
        };
        let Some(grant) = delivery
            .grants
            .origins
            .iter()
            .find(|g| callback_origin(&g.origin).is_ok_and(|o| o == origin))
        else {
            return RendererDeliveryOutcome::Cancelled;
        };
        for pin in 0..grant.pins.len() {
            let (tx, rx) = oneshot::channel();
            if attempts
                .try_send(CallbackAttempt {
                    delivery: delivery.clone(),
                    reply: tx,
                })
                .is_err()
            {
                return RendererDeliveryOutcome::Cancelled;
            }
            let authorization = match bounded(rx, &start.live, &stop, start.deadline).await {
                Ok(Ok(a)) => a,
                Ok(Err(_)) => return RendererDeliveryOutcome::Cancelled,
                Err(outcome) => return outcome,
            };
            let Some(view) = authorization.view else {
                return RendererDeliveryOutcome::Cancelled;
            };
            let permit = match CallbackPermit::new(delivery, &view, index, authorization.now_ns) {
                Ok(p) => p,
                Err(_) => return RendererDeliveryOutcome::Cancelled,
            };
            let deadline = start.deadline.min(
                authorization.at
                    + Duration::from_nanos(
                        view.expires_monotonic_ns
                            .saturating_sub(authorization.now_ns),
                    ),
            );
            let address = SocketAddr::new(permit.pins[pin], permit.port);
            let mut stream =
                match bounded(TcpStream::connect(address), &start.live, &stop, deadline).await {
                    Ok(Ok(s)) => s,
                    Ok(Err(_)) => {
                        last = RendererDeliveryOutcome::ConnectFailed;
                        continue;
                    }
                    Err(outcome) => return outcome,
                };
            if make_request(
                &mut start.scratch.value,
                &permit,
                delivery,
                start.body.value.len(),
            )
            .is_err()
            {
                return RendererDeliveryOutcome::MalformedResponse;
            }
            let result = bounded(
                async {
                    stream.write_all(&start.scratch.value).await?;
                    stream.write_all(start.body.value.as_bytes()).await?;
                    stream.flush().await
                },
                &start.live,
                &stop,
                deadline,
            )
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    last = RendererDeliveryOutcome::ConnectFailed;
                    continue;
                }
                Err(outcome) => return outcome,
            }
            let result = bounded(
                read_response(&mut stream, &mut start.scratch.value),
                &start.live,
                &stop,
                deadline,
            )
            .await;
            last = match result {
                Ok(outcome) => outcome,
                Err(outcome) => outcome,
            };
            if matches!(
                last,
                RendererDeliveryOutcome::Delivered
                    | RendererDeliveryOutcome::InvalidSid
                    | RendererDeliveryOutcome::Cancelled
                    | RendererDeliveryOutcome::Timeout
            ) {
                return last;
            }
        }
    }
    last
}
struct HeaderWriter<'a>(&'a mut Vec<u8>);
impl Write for HeaderWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len() + bytes.len() > HEADERS {
            return Err(io::Error::other("callback request headers exceed limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn make_request(
    buffer: &mut Vec<u8>,
    permit: &CallbackPermit<'_>,
    delivery: &RendererDelivery,
    length: usize,
) -> io::Result<()> {
    if delivery.binding.sid.len() > 128
        || !delivery.binding.sid.bytes().all(|b| (33..127).contains(&b))
    {
        return Err(io::Error::other("invalid callback identity"));
    }
    buffer.clear();
    let mut out = HeaderWriter(buffer);
    out.write_all(b"NOTIFY ")?;
    let target = &permit.url[permit.authority_end..];
    if target.is_empty() || target.starts_with('?') {
        out.write_all(b"/")?;
    }
    for byte in target.bytes() {
        if byte.is_ascii() {
            out.write_all(&[byte])?;
        } else {
            write!(out, "%{byte:02X}")?;
        }
    }
    write!(
        out,
        " HTTP/1.1\r\nHost: {}\r\nSID: {}\r\nSEQ: {}\r\nNT: upnp:event\r\nNTS: upnp:propchange\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n",
        &permit.url[7..permit.authority_end],
        delivery.binding.sid,
        delivery.sequence
    )
}
async fn read_response(stream: &mut TcpStream, buffer: &mut Vec<u8>) -> RendererDeliveryOutcome {
    buffer.clear();
    loop {
        match inspect_response(buffer, false) {
            Ok(Some(outcome)) => return outcome,
            Err(outcome) => return outcome,
            Ok(None) => {}
        }
        if buffer.len() >= RESPONSE {
            return RendererDeliveryOutcome::ResponseOverflow;
        }
        let before = buffer.len();
        let end = (before + 4096).min(RESPONSE);
        debug_assert!(end <= SENDER_BUFFER);
        buffer.resize(end, 0);
        let read = stream.read(&mut buffer[before..]).await;
        match read {
            Ok(0) => {
                buffer.truncate(before);
                return match inspect_response(buffer, true) {
                    Ok(Some(outcome)) => outcome,
                    Err(outcome) => outcome,
                    _ => RendererDeliveryOutcome::MalformedResponse,
                };
            }
            Ok(n) => buffer.truncate(before + n),
            Err(_) => {
                buffer.truncate(before);
                return RendererDeliveryOutcome::MalformedResponse;
            }
        }
    }
}
fn inspect_response(
    bytes: &[u8],
    eof: bool,
) -> Result<Option<RendererDeliveryOutcome>, RendererDeliveryOutcome> {
    use RendererDeliveryOutcome::{MalformedResponse as Bad, ResponseOverflow as Overflow};
    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
        if bytes.len() > HEADERS {
            return Err(Overflow);
        }
        return if eof { Err(Bad) } else { Ok(None) };
    };
    if end + 4 > HEADERS {
        return Err(Overflow);
    }
    let text = std::str::from_utf8(&bytes[..end]).map_err(|_| Bad)?;
    let mut lines = text.split("\r\n");
    let status = lines.next().ok_or(Bad)?;
    let mut parts = status.splitn(3, ' ');
    if !matches!(parts.next(), Some("HTTP/1.1" | "HTTP/1.0")) {
        return Err(Bad);
    }
    let code = parts
        .next()
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or(Bad)?;
    if parts.next().is_none() {
        return Err(Bad);
    }
    let mut length = None;
    let mut chunked = false;
    let mut count = 0;
    for line in lines {
        count += 1;
        if count > 64 {
            return Err(Overflow);
        }
        let (name, value) = line.split_once(':').ok_or(Bad)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
            || value.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
        {
            return Err(Bad);
        }
        let value = value.trim_matches([' ', '\t']);
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() || value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Bad);
            }
            length = Some(value.parse::<usize>().map_err(|_| Overflow)?);
        }
        if name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !value.eq_ignore_ascii_case("chunked") {
                return Err(Bad);
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err(Bad);
    }
    if code == 412 {
        return Ok(Some(RendererDeliveryOutcome::InvalidSid));
    }
    if code != 200 {
        return Err(Bad);
    }
    if length.is_some_and(|n| n > BODY) {
        return Err(Overflow);
    }
    let body = &bytes[end + 4..];
    let complete = if chunked {
        if body.len() > BODY + HEADERS {
            return Err(Overflow);
        }
        chunked_complete(body, eof)?
    } else if let Some(length) = length {
        if body.len() < length && eof {
            return Err(Bad);
        }
        body.len() >= length
    } else {
        if body.len() > BODY {
            return Err(Overflow);
        }
        eof
    };
    Ok(complete.then_some(RendererDeliveryOutcome::Delivered))
}
fn chunked_complete(bytes: &[u8], eof: bool) -> Result<bool, RendererDeliveryOutcome> {
    use RendererDeliveryOutcome::{MalformedResponse as Bad, ResponseOverflow as Overflow};
    let mut cursor = 0usize;
    let mut decoded = 0usize;
    loop {
        let Some(end) = bytes[cursor..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|n| cursor + n)
        else {
            return if eof { Err(Bad) } else { Ok(false) };
        };
        let line = std::str::from_utf8(&bytes[cursor..end]).map_err(|_| Bad)?;
        let size = line.split(';').next().ok_or(Bad)?;
        if size.is_empty()
            || !size.bytes().all(|b| b.is_ascii_hexdigit())
            || line.bytes().any(|b| b < 32 || b == 127)
        {
            return Err(Bad);
        }
        let size = usize::from_str_radix(size, 16).map_err(|_| Overflow)?;
        cursor = end + 2;
        if size == 0 {
            if bytes.get(cursor..cursor + 2) == Some(b"\r\n") {
                return Ok(true);
            }
            if let Some(end) = bytes[cursor..].windows(4).position(|w| w == b"\r\n\r\n") {
                let trailers =
                    std::str::from_utf8(&bytes[cursor..cursor + end]).map_err(|_| Bad)?;
                if trailers.split("\r\n").count() > 64
                    || trailers.split("\r\n").any(|l| {
                        !l.contains(':') || l.bytes().any(|b| b < 32 && b != b'\t' || b == 127)
                    })
                {
                    return Err(Bad);
                }
                return Ok(true);
            }
            return if eof { Err(Bad) } else { Ok(false) };
        }
        decoded = decoded.checked_add(size).ok_or(Overflow)?;
        if decoded > BODY {
            return Err(Overflow);
        }
        let next = cursor
            .checked_add(size)
            .and_then(|n| n.checked_add(2))
            .ok_or(Overflow)?;
        if bytes.len() < next {
            return if eof { Err(Bad) } else { Ok(false) };
        }
        if &bytes[next - 2..next] != b"\r\n" {
            return Err(Bad);
        }
        cursor = next;
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{ids::*, remote_volume::RemoteVolume};

    // Track allocations only on the current fixture runtime thread.
    // The fixed registry excludes fixture sockets and preallocated resident/slot storage.
    struct Allocator;
    struct AllocationTrace {
        enabled: bool,
        entries: [(usize, usize); 512],
        live: usize,
        peak: usize,
        overflow: bool,
    }
    thread_local! {static ALLOCATIONS:std::cell::RefCell<AllocationTrace>=const{std::cell::RefCell::new(AllocationTrace{enabled:false,entries:[(0,0);512],live:0,peak:0,overflow:false})};}
    impl AllocationTrace {
        fn remove(&mut self, pointer: usize) {
            if let Some(entry) = self.entries.iter_mut().find(|(p, _)| *p == pointer) {
                self.live -= entry.1;
                *entry = (0, 0);
            }
        }
        fn add(&mut self, pointer: usize, size: usize) {
            if pointer == 0 {
                return;
            }
            if let Some(entry) = self.entries.iter_mut().find(|(p, _)| *p == 0) {
                *entry = (pointer, size);
                self.live += size;
                self.peak = self.peak.max(self.live);
            } else {
                self.overflow = true;
            }
        }
    }
    #[global_allocator]
    static ALLOCATOR: Allocator = Allocator;
    unsafe impl std::alloc::GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            let pointer = unsafe { std::alloc::System.alloc(layout) };
            let _ = ALLOCATIONS.try_with(|trace| {
                let mut t = trace.borrow_mut();
                if t.enabled {
                    t.add(pointer as usize, layout.size());
                }
            });
            pointer
        }
        unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
            let pointer = unsafe { std::alloc::System.alloc_zeroed(layout) };
            let _ = ALLOCATIONS.try_with(|trace| {
                let mut t = trace.borrow_mut();
                if t.enabled {
                    t.add(pointer as usize, layout.size());
                }
            });
            pointer
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
            let _ = ALLOCATIONS.try_with(|trace| {
                let mut t = trace.borrow_mut();
                if t.enabled {
                    t.remove(pointer as usize);
                }
            });
            unsafe { std::alloc::System.dealloc(pointer, layout) }
        }
        unsafe fn realloc(
            &self,
            pointer: *mut u8,
            layout: std::alloc::Layout,
            size: usize,
        ) -> *mut u8 {
            let replacement = unsafe { std::alloc::System.realloc(pointer, layout, size) };
            let _ = ALLOCATIONS.try_with(|trace| {
                let mut t = trace.borrow_mut();
                if t.enabled && !replacement.is_null() {
                    t.remove(pointer as usize);
                    t.add(replacement as usize, size);
                }
            });
            replacement
        }
    }
    #[test]
    fn real_sender_allocation_peak_fits_reserved_scratch_with_maximum_url_and_response() {
        use std::io::Read;
        let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let ip = route.local_addr().unwrap().ip();
        let listener = std::net::TcpListener::bind((ip, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut scratch = [0; 4096];
            loop {
                let n = socket.read(&mut scratch).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&scratch[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let prefix = "HTTP/1.1 200 OK\r\nContent-Length: 2048\r\nX-Padding: ";
            let head = format!("{prefix}{}\r\n\r\n", "x".repeat(HEADERS - prefix.len() - 4));
            assert_eq!(head.len(), HEADERS);
            socket.write_all(head.as_bytes()).unwrap();
            socket.write_all(&[b'x'; 2048]).unwrap();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async{
   let budget=Budget::new();let mut d=(*delivery(&budget)).clone();let origin=format!("http://callback.test:{port}");let prefix=format!("{origin}/");let mut url=prefix.clone();url.push_str(&"é".repeat((8192-prefix.len())/2));while url.len()<8192{url.push('x');}assert_eq!(url.len(),8192);d.callbacks=vec![url].into();d.grants=Arc::new(RendererCallbackGrantDocument{origins:vec![RendererCallbackOrigin{origin,pins:vec![ip]}]});let d=Arc::new(d);let stop=Arc::new(AtomicBool::new(false));let(handle,mut actor)=EventHandle::new(stop,budget);
   ALLOCATIONS.with(|trace|trace.borrow_mut().enabled=true);
   handle.start(d.clone(),0).unwrap();let deadline=Instant::now()+Duration::from_secs(3);let completion=loop{actor.poll();while let Some(attempt)=handle.attempt(){let view=RendererCallbackAuthorityView{binding:attempt.delivery.binding.clone(),delivery_id:attempt.delivery.delivery_id,callbacks:attempt.delivery.callbacks.clone(),grants:attempt.delivery.grants.clone(),expires_monotonic_ns:attempt.delivery.deadline_monotonic_ns};assert!(attempt.authorize(Some(view),0));}if let Some(completion)=handle.completion(){break completion;}assert!(Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(1)).await;};
   let(peak,overflow)=ALLOCATIONS.with(|trace|{let mut t=trace.borrow_mut();t.enabled=false;(t.peak,t.overflow)});assert!(!overflow,"allocation registry must retain complete evidence");assert_eq!(completion.outcome,RendererDeliveryOutcome::Delivered);assert!(completion.transport_joined);assert!(peak<=48*1024,"sender transient allocation peak {peak} exceeds its reserved allowance");println!("SENDER_ALLOCATION {{\"buffer_capacity\":{},\"transient_peak\":{peak},\"total_observed\":{},\"reserved\":131072,\"url_bytes\":8192,\"response_header_bytes\":32768,\"response_body_bytes\":2048}}",SENDER_BUFFER,SENDER_BUFFER+peak);actor.shutdown().await;
  });
        server.join().unwrap();
    }

    fn delivery(budget: &Arc<Budget>) -> Arc<RendererDelivery> {
        let lease = budget.reserve_event(RendererService::AvTransport).unwrap();
        let controller = RendererControllerId::new("controller").unwrap();
        Arc::new(RendererDelivery {
            binding: RendererSubscriptionBinding {
                owner_id: OwnerId::new("event-owner").unwrap(),
                renderer_epoch: 1,
                sid: "uuid:event".into(),
                generation: 1,
                controller_id: controller.clone(),
                controller_generation: 1,
                callback_generation: 1,
            },
            delivery_id: 1,
            position: 0,
            sequence: 0,
            initial: true,
            snapshot: Arc::new(RendererEventSnapshot {
                controller_id: controller,
                controller_generation: 1,
                projection: RendererProjection {
                    service: RendererService::AvTransport,
                    transport_state: "NO_MEDIA_PRESENT",
                    transport_status: "OK",
                    transport_actions: "",
                    presets: "FactoryDefaults",
                    duration_seconds: None,
                    position_seconds: None,
                    muted: true,
                    volume: RemoteVolume::from_volume(0).unwrap(),
                    connected: false,
                    descriptor: None,
                },
            }),
            callbacks: vec!["http://private.example:80/events".into()].into(),
            grants: Arc::new(RendererCallbackGrantDocument {
                origins: vec![RendererCallbackOrigin {
                    origin: "http://private.example:80".into(),
                    pins: vec!["192.168.1.2".parse().unwrap()],
                }],
            }),
            deadline_monotonic_ns: 30_000_000_000,
            lease,
        })
    }
    #[test]
    fn failed_join_keeps_exact_delivery_and_lease_after_completion_is_consumed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let budget = Budget::new();
            let d = delivery(&budget);
            let stop = Arc::new(AtomicBool::new(false));
            let (handle, mut actor) = EventHandle::new(stop, budget.clone());
            handle.state.records.lock().unwrap().push(Record {
                delivery: d.clone(),
                live: Arc::new(AtomicBool::new(true)),
            });
            let task = actor.tasks.spawn(std::future::pending());
            actor.identities.push((task.id(), d.clone()));
            task.abort();
            actor.shutdown().await;
            let result = handle.completion().unwrap();
            assert!(!result.transport_joined);
            assert_eq!(result.outcome, RendererDeliveryOutcome::JoinFailed);
            drop(result);
            assert_eq!(handle.state.records.lock().unwrap().len(), 1);
            assert!(budget.reserved() > 4 * 1024 * 1024);
        });
    }
    #[test]
    fn callback_policy_denies_forbidden_pins_and_all_exact_generation_mismatches() {
        let budget = Budget::new();
        let d = delivery(&budget);
        let view = RendererCallbackAuthorityView {
            binding: d.binding.clone(),
            delivery_id: d.delivery_id,
            callbacks: d.callbacks.clone(),
            grants: d.grants.clone(),
            expires_monotonic_ns: d.deadline_monotonic_ns,
        };
        assert!(CallbackPermit::new(&d, &view, 0, 0).is_ok());
        for field in 0..6 {
            let mut v = view.clone();
            match field {
                0 => v.binding.renderer_epoch += 1,
                1 => v.binding.controller_generation += 1,
                2 => v.binding.callback_generation += 1,
                3 => v.binding.generation += 1,
                4 => v.delivery_id += 1,
                _ => v.binding.owner_id = OwnerId::new("other-owner").unwrap(),
            };
            assert!(matches!(
                CallbackPermit::new(&d, &v, 0, 0),
                Err(PolicyError::InvalidBinding)
            ));
        }
        for pin in [
            "127.0.0.1",
            "100.64.0.1",
            "169.254.1.2",
            "192.0.2.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "::1",
        ] {
            let mut altered = (*d).clone();
            altered.grants = Arc::new(RendererCallbackGrantDocument {
                origins: vec![RendererCallbackOrigin {
                    origin: "http://private.example:80".into(),
                    pins: vec![pin.parse().unwrap()],
                }],
            });
            let mut v = view.clone();
            v.grants = altered.grants.clone();
            assert!(matches!(
                CallbackPermit::new(&altered, &v, 0, 0),
                Err(PolicyError::InvalidGrant)
            ));
        }
    }
    #[test]
    fn notify_response_parser_bounds_framing_and_requires_real_success() {
        for (wire, outcome) in [
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".as_slice(),
                RendererDeliveryOutcome::Delivered,
            ),
            (
                b"HTTP/1.1 412 Gone\r\nContent-Length: 0\r\n\r\n",
                RendererDeliveryOutcome::InvalidSid,
            ),
            (
                b"HTTP/1.1 302 Moved\r\nLocation: http://elsewhere/\r\nContent-Length: 0\r\n\r\n",
                RendererDeliveryOutcome::MalformedResponse,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n",
                RendererDeliveryOutcome::MalformedResponse,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 2049\r\n\r\n",
                RendererDeliveryOutcome::ResponseOverflow,
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n801\r\n",
                RendererDeliveryOutcome::ResponseOverflow,
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nx",
                RendererDeliveryOutcome::MalformedResponse,
            ),
        ] {
            let actual = match inspect_response(wire, true) {
                Ok(Some(value)) => value,
                Err(value) => value,
                _ => panic!("missing terminal result"),
            };
            assert_eq!(actual, outcome);
        }
    }
}
