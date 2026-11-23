// ABOUTME: Owns bounded ordinary and safety request workers over the same LocalClient.
// ABOUTME: Stops queued work and joins workers without sending owner commands on release.
use crate::{DeliveryFailure, Lane, OwnerClient, Reply, Request};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchError {
    Full(Lane),
    Disconnected,
}

#[derive(Debug)]
pub struct Completion {
    pub request: Request,
    pub outcome: Result<Reply, DeliveryFailure>,
}

pub struct Dispatcher {
    ordinary: Option<SyncSender<Request>>,
    safety: Option<SyncSender<Request>>,
    completions: Option<Receiver<Completion>>,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

#[must_use = "Join these stopped workers off the UI thread"]
pub struct WorkerShutdown {
    workers: Vec<JoinHandle<()>>,
}
impl WorkerShutdown {
    pub fn join(mut self) -> thread::Result<()> {
        let mut failed = None;
        for worker in self.workers.drain(..) {
            if let Err(error) = worker.join()
                && failed.is_none()
            {
                failed = Some(error);
            }
        }
        failed.map_or(Ok(()), Err)
    }
}
impl Drop for WorkerShutdown {
    fn drop(&mut self) {
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Dispatcher {
    pub fn attach(runtime: String) -> Result<Self, DeliveryFailure> {
        let client = OwnerClient::attach(runtime)?;
        let (ordinary, ordinary_rx) = mpsc::sync_channel(1);
        let (safety, safety_rx) = mpsc::sync_channel(2);
        let (completed, completions) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let mut result = Self {
            ordinary: Some(ordinary),
            safety: Some(safety),
            completions: Some(completions),
            stop,
            workers: Vec::new(),
        };
        for (name, receiver) in [
            ("earph-desktop-ordinary", ordinary_rx),
            ("earph-desktop-safety", safety_rx),
        ] {
            let client = client.clone();
            let stopping = result.stop.clone();
            let sender = completed.clone();
            let worker = thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    while !stopping.load(Ordering::Acquire) {
                        let Ok(request) = receiver.recv() else {
                            break;
                        };
                        if stopping.load(Ordering::Acquire) {
                            break;
                        }
                        let outcome = client.request(&request);
                        if stopping.load(Ordering::Acquire)
                            || sender.send(Completion { request, outcome }).is_err()
                        {
                            break;
                        }
                    }
                })
                .map_err(|_| DeliveryFailure::Local {
                    code: "INTERNAL_FAULT".into(),
                    detail: "cannot start desktop request worker".into(),
                })?;
            result.workers.push(worker);
        }
        Ok(result)
    }
    pub fn try_submit(&self, request: Request) -> Result<(), DispatchError> {
        if self.stop.load(Ordering::Acquire) {
            return Err(DispatchError::Disconnected);
        }
        let lane = request.lane();
        let sender = match lane {
            Lane::Ordinary => self.ordinary.as_ref(),
            Lane::Safety => self.safety.as_ref(),
        }
        .ok_or(DispatchError::Disconnected)?;
        sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) => DispatchError::Full(lane),
            TrySendError::Disconnected(_) => DispatchError::Disconnected,
        })
    }
    pub fn try_next(&self) -> Option<Completion> {
        self.completions.as_ref()?.try_recv().ok()
    }
    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.ordinary.take();
        self.safety.take();
        self.completions.take();
    }
    /// Returns stopped worker ownership without waiting. Move it off the UI thread before joining.
    pub fn disconnect(mut self) -> WorkerShutdown {
        self.stop();
        WorkerShutdown {
            workers: std::mem::take(&mut self.workers),
        }
    }
}
impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.stop();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
