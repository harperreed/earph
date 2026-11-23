// ABOUTME: Runs profile and diagnostic artifacts through one bounded worker lifecycle.
// ABOUTME: Drains completion ownership before normal cancellation and shutdown joins.
use super::*;
use earph_core::{diagnostics::*, profile_control::ProfileSaveBinding};
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
enum Task {
    Profile {
        job: Box<ProfileWorkRequest>,
        catalog: Arc<Vec<Arc<ProfileRecord>>>,
        retained: u64,
    },
    Diagnostic {
        job: Arc<DiagnosticWorkRequest>,
        deadline: Instant,
    },
}
struct Work {
    task: Task,
    cancel: Arc<AtomicBool>,
}
enum Output {
    Profile {
        binding: ProfileSaveBinding,
        result: Result<ProfileWorkOutput, EngineError>,
    },
    Diagnostic {
        binding: DiagnosticBinding,
        result: Box<DiagnosticWorkCompletion>,
    },
}
struct Completion {
    id: RequestId,
    output: Output,
}
#[derive(Clone, Copy)]
enum Kind {
    Profile,
    Diagnostic,
}
struct Active {
    id: RequestId,
    cancel: Arc<AtomicBool>,
    kind: Kind,
}
pub(crate) struct ArtifactJobs {
    sender: Option<SyncSender<Work>>,
    receiver: Receiver<Completion>,
    active: Option<Active>,
    join: Option<JoinHandle<()>>,
}
impl ArtifactJobs {
    pub fn spawn(directory: PathBuf) -> Self {
        let (sender, work) = mpsc::sync_channel::<Work>(1);
        let (complete, receiver) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            while let Ok(work) = work.recv() {
                let (id, output) = match work.task {
                    Task::Profile {
                        job,
                        catalog,
                        retained,
                    } => {
                        let result =
                            execute_profile(&job, &catalog, retained, &work.cancel, &directory);
                        (
                            job.request_id,
                            Output::Profile {
                                binding: job.binding,
                                result,
                            },
                        )
                    }
                    Task::Diagnostic { job, deadline } => {
                        let result =
                            diagnostic_artifacts::execute(&directory, &job, &work.cancel, deadline);
                        (
                            job.request_id.clone(),
                            Output::Diagnostic {
                                binding: job.binding.clone(),
                                result: Box::new(result),
                            },
                        )
                    }
                };
                if complete.send(Completion { id, output }).is_err() {
                    break;
                }
            }
        });
        Self {
            sender: Some(sender),
            receiver,
            active: None,
            join: Some(join),
        }
    }
    pub fn is_idle(&self) -> bool {
        self.active.is_none()
    }
    pub fn submit(&mut self, session: &Session, now: u64) -> Result<(), EngineError> {
        if self.active.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "artifact worker is still retiring an operation",
            ));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (id, kind, task) = if let Some(job) = session.diagnostic_work() {
            let remaining = job
                .deadline_monotonic_ns
                .checked_sub(now)
                .filter(|n| *n > 0)
                .ok_or(CANCELLED)?
                .min(DIAGNOSTIC_JOB_DEADLINE_NS);
            let deadline = Instant::now()
                .checked_add(Duration::from_nanos(remaining))
                .ok_or(BUDGET)?;
            (
                job.request_id.clone(),
                Kind::Diagnostic,
                Task::Diagnostic { job, deadline },
            )
        } else {
            let job = session.profile_work().ok_or(INVALID)?;
            (
                job.request_id.clone(),
                Kind::Profile,
                Task::Profile {
                    job: Box::new(job),
                    catalog: session.profile_records()?,
                    retained: preparation_retained_bytes(session)?,
                },
            )
        };
        self.sender
            .as_ref()
            .ok_or(runtime::IO)?
            .try_send(Work {
                task,
                cancel: cancel.clone(),
            })
            .map_err(|_| runtime::IO)?;
        self.active = Some(Active { id, cancel, kind });
        Ok(())
    }
    pub fn cancel(&mut self, session: &mut Session) {
        session.cancel_artifact_work();
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
        }
    }
    pub fn poll(
        &mut self,
        session: &mut Session,
        now: u64,
        directory: &Path,
        token: &str,
    ) -> Result<(), EngineError> {
        session.expire_profile_work(now);
        session.expire_diagnostic_work(now);
        if let Some(active) = &self.active {
            let cancel = match active.kind {
                Kind::Profile => session
                    .profile_work()
                    .is_none_or(|j| j.request_id != active.id),
                Kind::Diagnostic => session.diagnostic_cancel_requested(&active.id),
            };
            if cancel {
                active.cancel.store(true, Ordering::Release);
            }
        }
        loop {
            let completion = match self.receiver.try_recv() {
                Ok(completion) => completion,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.join.as_ref().is_some_and(|j| j.is_finished()) && self.active.is_some()
                    {
                        let active = self.active.take().expect("checked active worker");
                        match active.kind {
                            Kind::Diagnostic => {
                                if let Some(job) = session.diagnostic_work() {
                                    let mut receipt = job.pending_receipt();
                                    receipt.reason =
                                        Some(DiagnosticArtifactReason::StorageUnavailable);
                                    session.complete_diagnostic_work(
                                        &job.request_id,
                                        &job.binding,
                                        now,
                                        DiagnosticWorkCompletion {
                                            receipt,
                                            status: earph_core::control::ReceiptStatus::Failed,
                                            error: Some(runtime::IO),
                                        },
                                    )?;
                                }
                            }
                            Kind::Profile => {
                                if let Some(job) = session.profile_work() {
                                    let _ = session.complete_profile_work(
                                        &job.request_id,
                                        &job.binding,
                                        now,
                                        Err(runtime::IO),
                                        |_| Ok(()),
                                    );
                                }
                            }
                        }
                    }
                    break;
                }
            };
            if self.active.as_ref().is_none_or(|a| a.id != completion.id) {
                continue;
            }
            self.active = None;
            match completion.output {
                Output::Diagnostic { binding, result } => {
                    session.complete_diagnostic_work(&completion.id, &binding, now, *result)?;
                }
                Output::Profile { binding, result } => {
                    let mut persistence_error = None;
                    let _ = session.complete_profile_work(
                        &completion.id,
                        &binding,
                        now,
                        result,
                        |staged| {
                            let bytes =
                                serde_json::to_vec(&staged.checkpoint()).map_err(|_| INVALID)?;
                            runtime::write_checkpoint(
                                directory,
                                &format!("{token}-profile-{}", staged.snapshot().revision),
                                &bytes,
                            )
                            .inspect_err(|e| persistence_error = Some(*e))
                        },
                    );
                    if let Some(error) = persistence_error {
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }
}
impl Drop for ArtifactJobs {
    fn drop(&mut self) {
        if let Some(active) = &self.active {
            active.cancel.store(true, Ordering::Release);
        }
        self.sender = None;
        if let Some(join) = self.join.take() {
            while !join.is_finished() {
                let _ = self.receiver.recv_timeout(Duration::from_millis(10));
            }
            while self.receiver.try_recv().is_ok() {}
            let _ = join.join();
        }
    }
}
