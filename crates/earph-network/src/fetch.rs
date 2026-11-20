// ABOUTME: Owns one cancellable async transport and bounded synchronous byte pipe.
// ABOUTME: Retains worker ownership until task and operating-system thread retirement.
use crate::{FetchPermit, PolicyError, SourceBinding};
use std::{
    collections::VecDeque,
    future::{Future, poll_fn},
    io::{self, Read},
    net::SocketAddr,
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    task::Poll,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

const CHUNK_BYTES: usize = 16 * 1024;
const QUEUED_CHUNKS: usize = 4;
const COLLECTION_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    Admission(PolicyError),
    Busy,
    Cancelled,
    Connect,
    HeaderTimeout,
    StartupTimeout,
    Headers,
    Redirect,
    Status,
    ContentEncoding,
    Body,
    BodyTimeout,
    CollectionLimit,
    Internal,
    CleanupTimeout,
}
impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "network fetch: {self:?}")
    }
}
impl std::error::Error for FetchError {}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    Complete,
    Cancelled,
    Failed(FetchError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Startup,
    Headers,
    Body,
    Backpressure,
    Finished,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub binding: SourceBinding,
    pub terminal: Option<Terminal>,
    pub phase: Phase,
    pub queued_chunks: usize,
    pub received_bytes: u64,
}
struct Chunk {
    bytes: Vec<u8>,
    offset: usize,
}
struct Pipe {
    queue: VecDeque<Chunk>,
    terminal: Option<Terminal>,
    phase: Phase,
    received: u64,
    exited: bool,
}
struct Shared {
    binding: SourceBinding,
    pipe: Mutex<Pipe>,
    ready: Condvar,
    capacity: Notify,
    cancelled: Notify,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, Pipe> {
        self.pipe
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    fn finish(&self, terminal: Terminal) {
        let mut pipe = self.lock();
        if pipe.terminal.is_none() {
            pipe.terminal = Some(terminal);
            pipe.phase = Phase::Finished;
            if terminal != Terminal::Complete {
                pipe.queue.clear();
            }
        }
        self.ready.notify_all();
    }
    fn phase(&self, phase: Phase) {
        let mut pipe = self.lock();
        if pipe.terminal.is_none() {
            pipe.phase = phase;
        }
    }
}

struct Job {
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
    done: mpsc::Receiver<()>,
}
/// Owns at most one job. Retirement is required before replacement.
#[derive(Default)]
pub struct NetworkWorker {
    active: Option<Job>,
}
impl NetworkWorker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn start(&mut self, permit: FetchPermit) -> Result<MediaReader, FetchError> {
        if self.active.is_some() {
            return Err(FetchError::Busy);
        }
        permit
            .grant_for(&permit.url, Instant::now())
            .map_err(FetchError::Admission)?;
        let shared = Arc::new(Shared {
            binding: permit.binding.clone(),
            pipe: Mutex::new(Pipe {
                queue: VecDeque::new(),
                terminal: None,
                phase: Phase::Startup,
                received: 0,
                exited: false,
            }),
            ready: Condvar::new(),
            capacity: Notify::new(),
            cancelled: Notify::new(),
        });
        let owned = shared.clone();
        let (done_tx, done) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("earph-network".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(permit, owned.clone())
                }));
                if !matches!(result, Ok(Ok(()))) {
                    owned.finish(Terminal::Failed(FetchError::Internal));
                }
                let mut pipe = owned.lock();
                pipe.exited = true;
                if pipe.terminal.is_none() {
                    pipe.terminal = Some(Terminal::Failed(FetchError::Internal));
                    pipe.queue.clear();
                }
                owned.ready.notify_all();
                drop(pipe);
                let _ = done_tx.send(());
            })
            .map_err(|_| FetchError::Internal)?;
        self.active = Some(Job {
            shared: shared.clone(),
            thread,
            done,
        });
        Ok(MediaReader { shared })
    }
    /// Cancels unfinished transport and joins it within one second.
    /// A timeout retains the thread handle so callers can retry retirement.
    pub fn retire(&mut self) -> Result<(), FetchError> {
        let Some(job) = self.active.as_ref() else {
            return Ok(());
        };
        Cancellation {
            shared: job.shared.clone(),
        }
        .cancel();
        match job.done.recv_timeout(Duration::from_secs(1)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(FetchError::CleanupTimeout),
        }
        let job = self.active.take().expect("retained job");
        job.thread.join().map_err(|_| FetchError::Internal)
    }
}
impl Drop for NetworkWorker {
    fn drop(&mut self) {
        if let Some(job) = self.active.take() {
            Cancellation {
                shared: job.shared.clone(),
            }
            .cancel();
            // Explicit retirement is bounded. Drop joins rather than detaching unfinished ownership.
            let _ = job.thread.join();
        }
    }
}
#[derive(Clone)]
pub struct Cancellation {
    shared: Arc<Shared>,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.shared.finish(Terminal::Cancelled);
        self.shared.cancelled.notify_one();
        self.shared.capacity.notify_one();
    }
}
pub struct MediaReader {
    shared: Arc<Shared>,
}
impl MediaReader {
    pub fn observer(&self) -> FetchObserver {
        FetchObserver {
            shared: self.shared.clone(),
        }
    }
    pub fn cancellation(&self) -> Cancellation {
        Cancellation {
            shared: self.shared.clone(),
        }
    }
    pub fn observation(&self) -> Observation {
        self.observer().observation()
    }
    /// Collects finite media only, enforcing the limit on observed bytes.
    pub fn collect_bounded(&mut self) -> Result<Vec<u8>, FetchError> {
        let mut result = Vec::new();
        let mut buffer = [0; CHUNK_BYTES];
        loop {
            let n = self
                .read(&mut buffer)
                .map_err(|_| match self.observation().terminal {
                    Some(Terminal::Cancelled) => FetchError::Cancelled,
                    Some(Terminal::Failed(error)) => error,
                    _ => FetchError::Internal,
                })?;
            if n == 0 {
                return Ok(result);
            }
            if n > COLLECTION_BYTES - result.len() {
                self.cancellation().cancel();
                return Err(FetchError::CollectionLimit);
            }
            result.extend_from_slice(&buffer[..n]);
        }
    }
}

/// Independent diagnostics and publication fencing. Dropping an observer never cancels transport.
#[derive(Clone)]
pub struct FetchObserver {
    shared: Arc<Shared>,
}

/// Holds transport terminal publication while the caller accepts a bounded result.
/// Never decode, wait, retire, invoke callbacks, or perform output I/O while holding this guard.
/// Do not reenter transport, cancel, or drop its reader until this guard is released.
pub struct PublicationGuard<'a> {
    pipe: MutexGuard<'a, Pipe>,
}
impl PublicationGuard<'_> {
    pub fn terminal(&self) -> Option<Terminal> {
        self.pipe.terminal
    }
}
impl FetchObserver {
    pub fn observation(&self) -> Observation {
        let pipe = self.shared.lock();
        Observation {
            binding: self.shared.binding.clone(),
            terminal: pipe.terminal,
            phase: pipe.phase,
            queued_chunks: pipe.queue.len(),
            received_bytes: pipe.received,
        }
    }
    /// Accepts only the immutable binding and live or completed transport.
    /// When composing locks, acquire the caller's job state before this transport guard.
    pub fn publication_guard(
        &self,
        expected: &SourceBinding,
    ) -> Result<PublicationGuard<'_>, FetchError> {
        if expected != &self.shared.binding {
            return Err(FetchError::Admission(PolicyError::InvalidBinding));
        }
        let pipe = self.shared.lock();
        match pipe.terminal {
            Some(Terminal::Cancelled) => Err(FetchError::Cancelled),
            Some(Terminal::Failed(error)) => Err(error),
            None if pipe.exited => Err(FetchError::Internal),
            _ => Ok(PublicationGuard { pipe }),
        }
    }
}
impl Drop for MediaReader {
    fn drop(&mut self) {
        self.cancellation().cancel();
    }
}
impl Read for MediaReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let mut pipe = self.shared.lock();
        loop {
            match pipe.terminal {
                Some(Terminal::Cancelled) => {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        FetchError::Cancelled,
                    ));
                }
                Some(Terminal::Failed(error)) => return Err(io::Error::other(error)),
                _ => {}
            }
            if let Some(chunk) = pipe.queue.front_mut() {
                let n = bytes.len().min(chunk.bytes.len() - chunk.offset);
                bytes[..n].copy_from_slice(&chunk.bytes[chunk.offset..chunk.offset + n]);
                chunk.offset += n;
                if chunk.offset == chunk.bytes.len() {
                    pipe.queue.pop_front();
                    self.shared.capacity.notify_one();
                }
                return Ok(n);
            }
            if pipe.terminal == Some(Terminal::Complete) {
                return Ok(0);
            }
            if pipe.exited {
                return Err(io::Error::other(FetchError::Internal));
            }
            pipe = self
                .shared
                .ready
                .wait(pipe)
                .unwrap_or_else(|poison| poison.into_inner());
        }
    }
}

fn run(permit: FetchPermit, shared: Arc<Shared>) -> Result<(), FetchError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| FetchError::Internal)?;
    runtime.block_on(async {
        let mut task = tokio::spawn(fetch(permit, shared.clone()));
        let cancellation = shared.cancelled.notified();
        let mut cancellation = std::pin::pin!(cancellation);
        let completed = poll_fn(|cx| {
            if shared.lock().terminal == Some(Terminal::Cancelled)
                || cancellation.as_mut().poll(cx).is_ready()
            {
                return Poll::Ready(None);
            }
            match std::pin::Pin::new(&mut task).poll(cx) {
                Poll::Ready(result) => Poll::Ready(Some(result)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await;
        match completed {
            None => {
                task.abort();
                let _ = task.await;
                shared.finish(Terminal::Cancelled);
            }
            Some(Ok(Ok(()))) => shared.finish(Terminal::Complete),
            Some(Ok(Err(error))) => shared.finish(Terminal::Failed(error)),
            Some(Err(_)) => shared.finish(Terminal::Failed(FetchError::Internal)),
        }
    });
    Ok(())
}

struct DenyDns;
impl reqwest::dns::Resolve for DenyDns {
    fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "unpinned destination").into())
        })
    }
}
async fn fetch(permit: FetchPermit, shared: Arc<Shared>) -> Result<(), FetchError> {
    let startup = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut url = permit.url.clone();
    for redirects in 0..=3 {
        let grant = permit
            .grant_for(&url, Instant::now())
            .map_err(FetchError::Admission)?;
        let addresses: Vec<_> = grant
            .pins()
            .iter()
            .map(|ip| SocketAddr::new(*ip, grant.origin.port))
            .collect();
        let roots = grant
            .roots_der()
            .iter()
            .map(|bytes| {
                reqwest::Certificate::from_der(bytes)
                    .map_err(|_| FetchError::Admission(PolicyError::InvalidGrant))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .http1_only()
            .http1_max_headers(64)
            .connect_timeout(Duration::from_secs(3))
            .pool_max_idle_per_host(0)
            .dns_resolver(Arc::new(DenyDns))
            .resolve_to_addrs(&grant.origin.host, &addresses)
            .tls_certs_only(roots)
            .build()
            .map_err(|_| FetchError::Admission(PolicyError::InvalidGrant))?;
        shared.phase(Phase::Headers);
        let headers = tokio::time::Instant::now() + Duration::from_secs(5);
        let deadline = headers.min(startup);
        let response = tokio::time::timeout_at(
            deadline,
            client
                .get(url.0.clone())
                .header(reqwest::header::ACCEPT_ENCODING, "identity")
                .send(),
        )
        .await
        .map_err(|_| {
            if startup <= headers {
                FetchError::StartupTimeout
            } else {
                FetchError::HeaderTimeout
            }
        })?
        .map_err(|error| {
            if error.is_connect() {
                FetchError::Connect
            } else {
                FetchError::Headers
            }
        })?;
        if response.headers().len() > 64
            || response
                .headers()
                .iter()
                .try_fold(2usize, |n, (name, value)| {
                    n.checked_add(name.as_str().len() + value.as_bytes().len() + 4)
                })
                .is_none_or(|n| n > 32768)
        {
            return Err(FetchError::Headers);
        }
        if response.status().is_redirection() {
            if redirects == 3 {
                return Err(FetchError::Redirect);
            }
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .ok_or(FetchError::Redirect)?
                .to_str()
                .map_err(|_| FetchError::Redirect)?;
            url = url.redirected(location).map_err(FetchError::Admission)?;
            // Next iteration admits the exact origin before constructing a connection.
            continue;
        }
        if !response.status().is_success() {
            return Err(FetchError::Status);
        }
        for value in response
            .headers()
            .get_all(reqwest::header::CONTENT_ENCODING)
        {
            if !value.as_bytes().eq_ignore_ascii_case(b"identity") {
                return Err(FetchError::ContentEncoding);
            }
        }
        return body(response, &shared).await;
    }
    Err(FetchError::Redirect)
}
async fn wait_capacity(shared: &Shared) -> Result<(), FetchError> {
    loop {
        let ready = shared.capacity.notified();
        {
            let pipe = shared.lock();
            if pipe.terminal.is_some() {
                return Err(FetchError::Cancelled);
            }
            if pipe.queue.len() < QUEUED_CHUNKS {
                return Ok(());
            }
        }
        shared.phase(Phase::Backpressure);
        ready.await;
    }
}
async fn body(mut response: reqwest::Response, shared: &Shared) -> Result<(), FetchError> {
    loop {
        wait_capacity(shared).await?;
        shared.phase(Phase::Body);
        let chunk = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .map_err(|_| FetchError::BodyTimeout)?
            .map_err(|_| FetchError::Body)?;
        let Some(chunk) = chunk else {
            return Ok(());
        };
        for bytes in chunk.chunks(CHUNK_BYTES) {
            wait_capacity(shared).await?;
            let mut pipe = shared.lock();
            if pipe.terminal.is_some() {
                return Err(FetchError::Cancelled);
            }
            pipe.received = pipe
                .received
                .checked_add(bytes.len() as u64)
                .ok_or(FetchError::Internal)?;
            pipe.queue.push_back(Chunk {
                bytes: bytes.to_vec(),
                offset: 0,
            });
            shared.ready.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_resolver_refuses_even_a_real_system_hostname() {
        use reqwest::dns::Resolve;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let error = runtime
            .block_on(DenyDns.resolve("localhost".parse().unwrap()))
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "unpinned destination");
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use earph_core::ids::{OwnerId, SessionId, StagedMediaId};
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            binding: SourceBinding::new(
                OwnerId::new("owner").unwrap(),
                SessionId::new("session").unwrap(),
                StagedMediaId::new("media").unwrap(),
                1,
                1,
            )
            .unwrap(),
            pipe: Mutex::new(Pipe {
                queue: VecDeque::new(),
                terminal: None,
                phase: Phase::Startup,
                received: 0,
                exited: false,
            }),
            ready: Condvar::new(),
            capacity: Notify::new(),
            cancelled: Notify::new(),
        })
    }
    #[test]
    fn cleanup_timeout_retains_thread_ownership_until_a_later_join() {
        let shared = shared();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done) = mpsc::channel();
        let thread = thread::spawn(move || {
            release_rx.recv().unwrap();
            done_tx.send(()).unwrap();
        });
        let thread_id = thread.thread().id();
        let mut worker = NetworkWorker {
            active: Some(Job {
                shared,
                thread,
                done,
            }),
        };
        assert_eq!(worker.retire(), Err(FetchError::CleanupTimeout));
        assert_eq!(
            worker.active.as_ref().unwrap().thread.thread().id(),
            thread_id
        );
        release_tx.send(()).unwrap();
        worker.retire().unwrap();
        assert!(worker.active.is_none());
    }
    #[test]
    fn lost_producer_terminal_state_is_an_error_instead_of_eof() {
        let shared = shared();
        let producer = shared.clone();
        thread::spawn(move || {
            producer.lock().exited = true;
            producer.ready.notify_all();
        })
        .join()
        .unwrap();
        let mut reader = MediaReader { shared };
        assert_eq!(
            reader
                .observer()
                .publication_guard(&reader.observation().binding)
                .err(),
            Some(FetchError::Internal)
        );
        let mut byte = [0];
        assert_eq!(
            reader.read(&mut byte).unwrap_err().kind(),
            io::ErrorKind::Other
        );
    }
}
