// ABOUTME: Supplies bounded nonblocking services backed by a private synthetic durable fixture.
// ABOUTME: Retains each fixture storage thread and joins it after transport retirement.
#[path = "../../../earph-sendspin/tests/support/pairing_fixture.rs"]
mod durable;
pub use durable::FixtureStore;
use earph_network::sendspin::*;
use earph_sendspin::{pairing::*, session::AdmissionProposal};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

pub struct FixtureServices {
    pub directory: std::path::PathBuf,
    pub started: Arc<AtomicBool>,
    pub release: Option<mpsc::Receiver<()>>,
    pub threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    pub outcome: Option<CommitOutcome>,
    pub stall: Option<ServiceKind>,
    pub cancelled: Arc<AtomicBool>,
    pub candidate_delay_ms: u64,
}
struct PendingService {
    cancelled: Arc<AtomicBool>,
}
impl Drop for PendingService {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}
impl std::future::Future for PendingService {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, _: &mut std::task::Context<'_>) -> std::task::Poll<()> {
        std::task::Poll::Pending
    }
}
impl SendspinServices for FixtureServices {
    fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        let pending = (self.stall == Some(ServiceKind::Admission)).then(|| PendingService {
            cancelled: self.cancelled.clone(),
        });
        Box::pin(async move {
            if let Some(pending) = pending {
                pending.await;
            }
            Ok(true)
        })
    }
    fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        let pending = (self.stall == Some(ServiceKind::Candidate)).then(|| PendingService {
            cancelled: self.cancelled.clone(),
        });
        let delay = self.candidate_delay_ms;
        Box::pin(async move {
            if let Some(pending) = pending {
                pending.await;
            }
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Ok(Candidate::new([90; 32]))
        })
    }
    fn commit(
        &mut self,
        _: AttemptId,
        update: RecordUpdate,
    ) -> Operation<Result<(), CommitOutcome>> {
        let directory = self.directory.clone();
        let gate = self.release.take();
        let outcome = self.outcome;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.started.store(true, Ordering::SeqCst);
        self.threads.lock().unwrap().push(thread::spawn(move || {
            if let Some(gate) = gate {
                gate.recv().unwrap();
            }
            if outcome != Some(CommitOutcome::Unchanged) {
                let store = FixtureStore::open(&directory);
                store.commit(&update);
                assert_eq!(FixtureStore::open(&directory).read(), update.replacement);
            }
            let _ = tx.send(outcome.map_or(Ok(()), Err));
        }));
        Box::pin(async move { rx.await.unwrap_or(Err(CommitOutcome::Uncertain)) })
    }
}
