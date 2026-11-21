// ABOUTME: Composes admitted HTTP transport and FLAC decoding with one bounded PCM slot.
// ABOUTME: Fences result acceptance and retains both threads through explicit retirement.
use earph_core::{EngineError, audio::PcmFormat};
use earph_media::{DecodedPcmFrame, StreamingFlac};
use earph_network::{
    Cancellation, FetchError, FetchObserver, FetchPermit, NetworkWorker, SourceBinding, Terminal,
};
use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Idle,
    Active,
    Retiring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Complete,
    Cancelled,
    DecodeFailed(EngineError),
    TransportFailed(FetchError),
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerError {
    Busy,
    StaleBinding,
    Transport(FetchError),
    Internal,
    CleanupTimeout,
}
impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "network FLAC worker: {self:?}")
    }
}
impl std::error::Error for WorkerError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ready {
    pub format: PcmFormat,
    pub total_frames: Option<u64>,
}

#[derive(Debug)]
pub struct AcceptedFrame {
    pub binding: SourceBinding,
    pub frame: DecodedPcmFrame,
}

#[derive(Debug, Clone)]
pub struct Observation {
    pub binding: SourceBinding,
    pub ready: Option<Ready>,
    pub pending_frame: bool,
    pub decoded_packets: u64,
    pub outcome: Option<Outcome>,
    pub transport: earph_network::Observation,
}

#[derive(Default)]
struct DecodeState {
    ready: Option<Ready>,
    pending: Option<DecodedPcmFrame>,
    decoded_packets: u64,
    eof: bool,
    outcome: Option<Outcome>,
}
impl DecodeState {
    fn settle(&mut self, outcome: Outcome) {
        // Cancellation revokes completed handoff, but never erases a genuine failure.
        if self
            .outcome
            .is_none_or(|previous| previous == Outcome::Complete)
        {
            self.outcome = Some(outcome);
        }
        if self.outcome.is_some_and(|o| o != Outcome::Complete) {
            self.pending = None;
        }
    }
    fn observe_transport(&mut self, terminal: Option<Terminal>) {
        match terminal {
            Some(Terminal::Failed(error)) => self.settle(Outcome::TransportFailed(error)),
            // Reader destruction during media rejection also cancels transport.
            // Only Job::cancel establishes owner cancellation; let the decoder settle its cause.
            Some(Terminal::Cancelled) => self.pending = None,
            _ => {}
        }
    }
}
#[derive(Default)]
struct Shared {
    state: Mutex<DecodeState>,
    capacity: Condvar,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, DecodeState> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

struct Job {
    binding: SourceBinding,
    shared: Arc<Shared>,
    observer: FetchObserver,
    cancellation: Cancellation,
    transport: NetworkWorker,
    decoder: Option<JoinHandle<()>>,
    done: mpsc::Receiver<()>,
}
impl Job {
    fn cancel(&self) {
        let mut state = self.shared.lock();
        state.observe_transport(self.observer.observation().terminal);
        state.settle(Outcome::Cancelled);
        self.shared.capacity.notify_all();
        drop(state);
        self.cancellation.cancel();
    }

    fn inspect(&self) -> Observation {
        let mut state = self.shared.lock();
        // Lock order is always decode state, then transport pipe.
        let transport = self.observer.observation();
        state.observe_transport(transport.terminal);
        let publishable = match self.observer.publication_guard(&self.binding) {
            Ok(guard) => {
                if state.outcome.is_none()
                    && state.eof
                    && state.pending.is_none()
                    && guard.terminal() == Some(Terminal::Complete)
                {
                    state.settle(Outcome::Complete);
                }
                true
            }
            Err(error) => {
                state.observe_transport(Some(if error == FetchError::Cancelled {
                    Terminal::Cancelled
                } else {
                    Terminal::Failed(error)
                }));
                false
            }
        };
        self.shared.capacity.notify_all();
        Observation {
            binding: self.binding.clone(),
            ready: if publishable && state.outcome.is_none_or(|o| o == Outcome::Complete) {
                state.ready
            } else {
                None
            },
            pending_frame: state.pending.is_some(),
            decoded_packets: state.decoded_packets,
            outcome: state.outcome,
            transport,
        }
    }

    fn take_frame(&self) -> Option<AcceptedFrame> {
        let mut state = self.shared.lock();
        let guard = match self.observer.publication_guard(&self.binding) {
            Ok(guard) => guard,
            Err(error) => {
                state.observe_transport(Some(if error == FetchError::Cancelled {
                    Terminal::Cancelled
                } else {
                    Terminal::Failed(error)
                }));
                self.shared.capacity.notify_all();
                return None;
            }
        };
        let frame = if state.outcome.is_none() {
            state.pending.take()
        } else {
            None
        };
        // The move under both locks is this worker's acceptance linearization point.
        drop(guard);
        self.shared.capacity.notify_all();
        frame.map(|frame| AcceptedFrame {
            binding: self.binding.clone(),
            frame,
        })
    }

    fn retire(&mut self) -> Result<(), WorkerError> {
        if self.decoder.is_some() {
            match self.done.recv_timeout(Duration::from_secs(1)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => return Err(WorkerError::CleanupTimeout),
            }
            if self
                .decoder
                .take()
                .expect("retained decoder")
                .join()
                .is_err()
            {
                self.shared.lock().settle(Outcome::Internal);
            }
        }
        self.transport.retire().map_err(|error| {
            if error == FetchError::CleanupTimeout {
                WorkerError::CleanupTimeout
            } else {
                WorkerError::Transport(error)
            }
        })
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        // Explicit retirement is bounded. Final destruction joins instead of detaching.
        if let Some(decoder) = self.decoder.take() {
            let _ = decoder.join();
        }
        // NetworkWorker's Drop joins its retained transport thread.
    }
}

#[derive(Default)]
enum State {
    #[default]
    Idle,
    Active(Box<Job>),
    Retiring(Box<Job>),
}

/// One admitted FLAC job. This API performs no owner onboarding or native output.
///
/// Capture `current` from live owner authority independently of the permit.
/// Serialize `start`, `inspect`, and `take_frame` with mutations of that authority.
/// An earlier snapshot or a binding copied from the permit cannot prove current authority.
/// A frame accepted here needs another authority fence at any later output acceptance boundary.
#[derive(Default)]
pub struct NetworkFlacWorker {
    state: State,
}
impl NetworkFlacWorker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn ownership(&self) -> Ownership {
        match self.state {
            State::Idle => Ownership::Idle,
            State::Active(_) => Ownership::Active,
            State::Retiring(_) => Ownership::Retiring,
        }
    }
    pub fn start(
        &mut self,
        permit: FetchPermit,
        current: &SourceBinding,
    ) -> Result<(), WorkerError> {
        if !matches!(self.state, State::Idle) {
            return Err(WorkerError::Busy);
        }
        if permit.binding() != current {
            return Err(WorkerError::StaleBinding);
        }
        let binding = permit.binding().clone();
        let mut transport = NetworkWorker::new();
        let reader = transport.start(permit).map_err(WorkerError::Transport)?;
        let observer = reader.observer();
        let cancellation = reader.cancellation();
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let thread_observer = observer.clone();
        let (done_tx, done) = mpsc::channel();
        let decoder = thread::Builder::new()
            .name("earph-network-flac".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    decode(reader, &thread_shared, &thread_observer);
                }));
                let mut state = thread_shared.lock();
                state.observe_transport(thread_observer.observation().terminal);
                if result.is_err() || (!state.eof && state.outcome.is_none()) {
                    state.settle(Outcome::Internal);
                }
                thread_shared.capacity.notify_all();
                drop(state);
                let _ = done_tx.send(());
            });
        let job = Box::new(Job {
            binding,
            shared,
            observer,
            cancellation,
            transport,
            decoder: None,
            done,
        });
        self.finish_start(job, decoder)
    }
    fn finish_start(
        &mut self,
        mut job: Box<Job>,
        decoder: std::io::Result<JoinHandle<()>>,
    ) -> Result<(), WorkerError> {
        match decoder {
            Ok(decoder) => {
                job.decoder = Some(decoder);
                self.state = State::Active(job);
                Ok(())
            }
            Err(_) => {
                let mut state = job.shared.lock();
                state.observe_transport(job.observer.observation().terminal);
                state.settle(Outcome::Internal);
                drop(state);
                job.cancel();
                // Retain transport even when decoder thread creation fails.
                self.state = State::Retiring(job);
                Err(WorkerError::Internal)
            }
        }
    }
    fn current_job(&mut self, current: &SourceBinding) -> Result<Option<&Job>, WorkerError> {
        let mismatched = match &self.state {
            State::Active(job) | State::Retiring(job) => &job.binding != current,
            State::Idle => false,
        };
        if mismatched {
            self.cancel();
            return Err(WorkerError::StaleBinding);
        }
        Ok(match &self.state {
            State::Idle => None,
            State::Active(job) | State::Retiring(job) => Some(job),
        })
    }
    /// Reads measured readiness and terminal facts only after checking current authority.
    pub fn inspect(&mut self, current: &SourceBinding) -> Result<Option<Observation>, WorkerError> {
        Ok(self.current_job(current)?.map(Job::inspect))
    }
    /// Moves at most one frame under decode-state and transport-publication locks.
    pub fn take_frame(
        &mut self,
        current: &SourceBinding,
    ) -> Result<Option<AcceptedFrame>, WorkerError> {
        Ok(self.current_job(current)?.and_then(Job::take_frame))
    }
    pub fn cancel(&mut self) {
        self.state = match std::mem::take(&mut self.state) {
            State::Idle => State::Idle,
            State::Active(job) | State::Retiring(job) => {
                job.cancel();
                State::Retiring(job)
            }
        };
    }
    /// Revokes handoff, then waits at most one second for each owned thread.
    /// A timeout retains the retiring job and rejects replacement until a later retry succeeds.
    /// Final Drop may block to preserve ownership rather than detach unfinished work.
    pub fn retire(&mut self) -> Result<(), WorkerError> {
        self.cancel();
        if let State::Retiring(job) = &mut self.state {
            job.retire()?;
        }
        self.state = State::Idle;
        Ok(())
    }
}

fn decode(reader: earph_network::MediaReader, shared: &Shared, observer: &FetchObserver) {
    let mut decoder = match StreamingFlac::open(reader) {
        Ok(decoder) => decoder,
        Err(error) => {
            decode_failure(shared, observer, error);
            return;
        }
    };
    {
        let mut state = shared.lock();
        state.observe_transport(observer.observation().terminal);
        if state.outcome.is_some() {
            return;
        }
        state.ready = Some(Ready {
            format: decoder.format(),
            total_frames: decoder.total_frames(),
        });
    }
    loop {
        let mut state = shared.lock();
        while state.pending.is_some() && state.outcome.is_none() {
            state = shared
                .capacity
                .wait(state)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        state.observe_transport(observer.observation().terminal);
        if state.outcome.is_some() {
            return;
        }
        drop(state);
        // Decoder and transport reads never run under either publication lock.
        let result = decoder.next_frame();
        match result {
            Ok(Some(frame)) => {
                let mut state = shared.lock();
                state.observe_transport(observer.observation().terminal);
                if state.outcome.is_some() {
                    return;
                }
                state.decoded_packets += 1;
                state.pending = Some(frame);
            }
            Ok(None) => {
                shared.lock().eof = true;
                return;
            }
            Err(error) => {
                decode_failure(shared, observer, error);
                return;
            }
        }
    }
}

fn decode_failure(shared: &Shared, observer: &FetchObserver, error: EngineError) {
    let mut state = shared.lock();
    // Reader destruction can cancel transport after an admission failure; retain the media cause.
    let outcome = match observer.observation().terminal {
        Some(Terminal::Failed(error)) => Outcome::TransportFailed(error),
        _ => Outcome::DecodeFailed(error),
    };
    state.settle(outcome);
    shared.capacity.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::ids::{OwnerId, SessionId, StagedMediaId};
    use earph_network::{CheckedUrl, OriginGrant};
    use std::{
        io::Read,
        net::{IpAddr, TcpListener, UdpSocket},
        time::Instant,
    };

    fn binding(generation: u64) -> SourceBinding {
        SourceBinding::new(
            OwnerId::new("retirement-owner").unwrap(),
            SessionId::new("retirement-session").unwrap(),
            StagedMediaId::new("retirement-media").unwrap(),
            generation,
            1,
        )
        .unwrap()
    }
    fn permit(url: &str, address: IpAddr, generation: u64) -> FetchPermit {
        let checked = CheckedUrl::new(url).unwrap();
        let binding = binding(generation);
        let grant = OriginGrant::new(
            checked.origin(),
            binding.owner().clone(),
            1,
            vec![address],
            Instant::now() + Duration::from_secs(30),
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
    struct HeldJob {
        job: Box<Job>,
        reader: earph_network::MediaReader,
        done: mpsc::Sender<()>,
        server: thread::JoinHandle<()>,
        url: String,
        address: IpAddr,
    }
    fn held_job() -> HeldJob {
        let route = UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let address = route.local_addr().unwrap().ip();
        assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
        let listener = TcpListener::bind((address, 0)).unwrap();
        let url = format!(
            "http://retirement-fixture.test:{}/",
            listener.local_addr().unwrap().port()
        );
        let (connected, connection) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                peer.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            connected.send(()).unwrap();
            let mut byte = [0];
            assert_eq!(peer.read(&mut byte).unwrap(), 0);
        });
        let mut transport = NetworkWorker::new();
        let reader = transport.start(permit(&url, address, 1)).unwrap();
        connection.recv_timeout(Duration::from_secs(3)).unwrap();
        let (done_tx, done) = mpsc::channel();
        let job = Box::new(Job {
            binding: binding(1),
            shared: Arc::new(Shared::default()),
            observer: reader.observer(),
            cancellation: reader.cancellation(),
            transport,
            decoder: None,
            done,
        });
        HeldJob {
            job,
            reader,
            done: done_tx,
            server,
            url,
            address,
        }
    }

    #[test]
    fn decoder_timeout_keeps_the_exact_thread_and_transport_for_retry() {
        let HeldJob {
            mut job,
            reader,
            done,
            server,
            url,
            address,
        } = held_job();
        let (release, wait) = mpsc::channel();
        let shared = job.shared.clone();
        let observer = job.observer.clone();
        let decoder = thread::spawn(move || {
            wait.recv().unwrap();
            decode(reader, &shared, &observer);
            done.send(()).unwrap();
        });
        let identity = decoder.thread().id();
        job.decoder = Some(decoder);
        let mut worker = NetworkFlacWorker {
            state: State::Active(job),
        };
        assert_eq!(worker.retire(), Err(WorkerError::CleanupTimeout));
        assert_eq!(worker.ownership(), Ownership::Retiring);
        let State::Retiring(job) = &worker.state else {
            panic!("lost retiring ownership");
        };
        assert_eq!(job.decoder.as_ref().unwrap().thread().id(), identity);
        assert_eq!(job.observer.observation().binding, binding(1));
        assert_eq!(
            worker.start(permit(&url, address, 2), &binding(2)),
            Err(WorkerError::Busy)
        );
        release.send(()).unwrap();
        worker.retire().unwrap();
        assert_eq!(worker.ownership(), Ownership::Idle);
        server.join().unwrap();
    }

    #[test]
    fn failed_decoder_start_retains_the_started_real_transport() {
        let HeldJob {
            job,
            reader,
            done: _done,
            server,
            url,
            address,
        } = held_job();
        let mut worker = NetworkFlacWorker::new();
        // Exercise the spawn-error branch without exhausting process-wide thread resources.
        let spawn_error = Err(std::io::Error::other("decoder thread could not start"));
        drop(reader);
        assert_eq!(
            worker.finish_start(job, spawn_error),
            Err(WorkerError::Internal)
        );
        assert_eq!(worker.ownership(), Ownership::Retiring);
        assert_eq!(
            worker.inspect(&binding(1)).unwrap().unwrap().outcome,
            Some(Outcome::Internal)
        );
        assert_eq!(
            worker.start(permit(&url, address, 2), &binding(2)),
            Err(WorkerError::Busy)
        );
        worker.retire().unwrap();
        assert_eq!(worker.ownership(), Ownership::Idle);
        server.join().unwrap();
    }

    #[test]
    fn reader_drop_during_media_rejection_does_not_publish_owner_cancellation() {
        let HeldJob {
            job,
            reader,
            done: _done,
            server,
            ..
        } = held_job();
        let error = StreamingFlac::open(std::io::Cursor::new(b"bad!".to_vec()))
            .err()
            .unwrap();
        // Media admission drops its source before returning the decoding error.
        drop(reader);
        let intermediate = job.inspect();
        decode_failure(&job.shared, &job.observer, error);
        let settled = job.inspect();
        let mut worker = NetworkFlacWorker {
            state: State::Active(job),
        };
        worker.retire().unwrap();
        server.join().unwrap();
        assert_eq!(intermediate.outcome, None);
        assert_eq!(intermediate.ready, None);
        assert_eq!(settled.outcome, Some(Outcome::DecodeFailed(error)));
    }
}
