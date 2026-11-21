// ABOUTME: Owns diagnostic job admission, candidate pins, cancellation and historical terminal receipts.
// ABOUTME: Retains unresolved effects until the single artifact worker reports actual completion.
use super::*;
impl Session {
    pub fn artifact_worker_busy(&self) -> bool {
        self.profile_worker_busy() || self.diagnostics.work.is_some()
    }
    pub fn diagnostic_work(&self) -> Option<Arc<DiagnosticWorkRequest>> {
        self.diagnostics.work.clone()
    }
    pub fn diagnostic_cancel_requested(&self, id: &RequestId) -> bool {
        self.diagnostics
            .work
            .as_ref()
            .is_none_or(|w| &w.request_id != id)
            || self.diagnostics.cancel_requested
            || self.state.shutdown_requested
    }
    pub fn expire_diagnostic_work(&mut self, now: u64) {
        if self
            .diagnostics
            .work
            .as_ref()
            .is_some_and(|w| now >= w.deadline_monotonic_ns)
        {
            self.diagnostics.cancel_requested = true;
        }
    }
    pub fn cancel_artifact_work(&mut self) {
        self.cancel_profile_work();
        if self.diagnostics.work.is_some() {
            self.diagnostics.cancel_requested = true;
        }
    }
    pub(in crate::session) fn diagnostic_record(
        &self,
        id: &RequestId,
    ) -> Option<&DiagnosticRecord> {
        self.diagnostics
            .pending_record
            .as_ref()
            .filter(|r| &r.0.request_id == id)
            .or_else(|| {
                self.diagnostics
                    .terminal_record
                    .as_ref()
                    .filter(|r| &r.0.request_id == id)
            })
    }
    pub(in crate::session) fn retain_diagnostic_record(&mut self, record: DiagnosticRecord) {
        if self
            .diagnostics
            .work
            .as_ref()
            .is_some_and(|w| w.request_id == record.0.request_id)
            && record.1.status == ReceiptStatus::Accepted
        {
            self.diagnostics.pending_record = Some(record);
        }
    }
    pub(in crate::session) fn diagnostic_artifact_reply(
        &self,
        id: &RequestId,
    ) -> Option<QueryResult> {
        self.diagnostics
            .work
            .as_ref()
            .filter(|w| &w.request_id == id)
            .map(|w| QueryResult::DiagnosticArtifact(Arc::new(w.pending_receipt())))
    }
    // The dispatch hook returns an error only before transferring the job to its worker.
    pub(in crate::session) fn refused_diagnostic_dispatch(&self, now: u64) -> Option<QueryResult> {
        let mut receipt = self.diagnostics.work.as_ref()?.pending_receipt();
        receipt.effect = DiagnosticFilesystemEffect::Unchanged;
        receipt.completed_monotonic_ns = Some(now);
        receipt.reason = Some(DiagnosticArtifactReason::StorageUnavailable);
        Some(QueryResult::DiagnosticArtifact(Arc::new(receipt)))
    }
    pub(in crate::session) fn diagnostic_cancel_reply(
        &self,
        id: &RequestId,
    ) -> Option<QueryResult> {
        self.diagnostics
            .cancel_reply
            .as_ref()
            .filter(|(request, _)| request == id)
            .map(|(_, reply)| QueryResult::DiagnosticCancel(reply.clone()))
    }
    pub(in crate::session) fn admit_diagnostic_artifact(
        &mut self,
        command: &CommandEnvelope,
        now: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        if self.state.shutdown_requested || self.artifact_worker_busy() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "artifact worker is occupied or shutting down",
            ));
        }
        let (operation, directory, expected_sha256) = match &command.payload {
            Command::DiagnosticsExport {
                directory,
                expected_sha256,
            } => (
                DiagnosticArtifactOperation::Export,
                directory,
                expected_sha256,
            ),
            Command::DiagnosticsInspect {
                directory,
                expected_sha256,
            } => (
                DiagnosticArtifactOperation::Inspect,
                directory,
                expected_sha256,
            ),
            _ => return Err(STATE),
        };
        validate_diagnostic_directory(directory)?;
        if command.owner_id != self.state.owner_id {
            return Err(STALE);
        }
        if operation == DiagnosticArtifactOperation::Export
            && command.expected_revision != Some(self.state.revision)
        {
            return Err(EngineError::new(
                ErrorCode::StaleRevision,
                "refresh the owner revision before export",
            ));
        }
        let preview = if operation == DiagnosticArtifactOperation::Export {
            let candidate = self.diagnostics.candidate.as_ref().ok_or(EngineError::new(
                ErrorCode::AssetInvalid,
                "review a current diagnostic preview before export",
            ))?;
            if candidate.read.report.sha256 != *expected_sha256 {
                return Err(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "expected diagnostic preview does not match",
                ));
            }
            Some(candidate.read.clone())
        } else {
            None
        };
        let binding = self.diagnostic_binding();
        binding.validate()?;
        self.diagnostics.work = Some(Arc::new(DiagnosticWorkRequest {
            request_id: command.request_id.clone(),
            binding,
            operation,
            directory: directory.clone(),
            expected_sha256: *expected_sha256,
            preview,
            deadline_monotonic_ns: now
                .checked_add(DIAGNOSTIC_JOB_DEADLINE_NS)
                .ok_or(DIAGNOSTIC_BUDGET)?,
        }));
        self.diagnostics
            .work
            .as_ref()
            .ok_or(STATE)?
            .pending_receipt()
            .validate_for_status(ReceiptStatus::Accepted)?;
        self.diagnostics.cancel_requested = false;
        self.diagnostics.cancel_recorded = false;
        Ok(ReceiptStatus::Accepted)
    }
    pub(in crate::session) fn apply_diagnostic_cancel(
        &mut self,
        command: &CommandEnvelope,
    ) -> Result<ReceiptStatus, EngineError> {
        let Command::DiagnosticsCancel { request_id } = &command.payload else {
            return Err(STATE);
        };
        let state = if self
            .diagnostics
            .work
            .as_ref()
            .is_some_and(|w| &w.request_id == request_id)
        {
            self.diagnostics.cancel_requested = true;
            self.diagnostics.cancel_recorded = true;
            DiagnosticCancelState::Requested
        } else if self
            .diagnostics
            .terminal_record
            .as_ref()
            .is_some_and(|r| &r.0.request_id == request_id)
            || self.receipts.iter().any(|r| {
                &r.0.request_id == request_id
                    && r.0.payload.starts_diagnostic_work()
                    && r.1.status != ReceiptStatus::Accepted
            })
        {
            DiagnosticCancelState::AlreadyTerminal
        } else {
            DiagnosticCancelState::UnknownOperation
        };
        self.diagnostics.cancel_reply = Some((
            command.request_id.clone(),
            Arc::new(DiagnosticCancelReceipt {
                schema_version: 1,
                binding: self.diagnostic_binding(),
                request_id: request_id.clone(),
                state,
            }),
        ));
        Ok(ReceiptStatus::Applied)
    }
    pub fn complete_diagnostic_work(
        &mut self,
        id: &RequestId,
        binding: &DiagnosticBinding,
        now: u64,
        mut result: DiagnosticWorkCompletion,
    ) -> Result<(), EngineError> {
        let job = self
            .diagnostics
            .work
            .as_ref()
            .filter(|w| &w.request_id == id && &w.binding == binding)
            .cloned()
            .ok_or(STALE)?;
        if job
            .deadline_monotonic_ns
            .checked_sub(DIAGNOSTIC_JOB_DEADLINE_NS)
            .is_none_or(|created| now < created)
        {
            return Err(STALE);
        }
        let prior = self
            .diagnostics
            .pending_record
            .as_ref()
            .filter(|r| &r.0.request_id == id)
            .cloned()
            .ok_or(STALE)?;
        result.receipt.completed_monotonic_ns = Some(now);
        let valid = result.receipt.request_id == job.request_id
            && result.receipt.binding == job.binding
            && result.receipt.operation == job.operation
            && result.receipt.directory == job.directory
            && result.receipt.expected_sha256 == job.expected_sha256
            && result.receipt.deadline_monotonic_ns == job.deadline_monotonic_ns
            && result.receipt.validate_for_status(result.status).is_ok()
            && (result.error.is_some()
                == matches!(
                    result.status,
                    ReceiptStatus::Failed | ReceiptStatus::Cancelled
                ));
        if !valid {
            result.receipt = job.pending_receipt();
            result.receipt.effect = if job.operation == DiagnosticArtifactOperation::Export {
                DiagnosticFilesystemEffect::InspectRequired
            } else {
                DiagnosticFilesystemEffect::Unchanged
            };
            result.receipt.completed_monotonic_ns = Some(now);
            result.receipt.reason = Some(DiagnosticArtifactReason::InvalidObject);
            result.status = ReceiptStatus::Failed;
            result.error = Some(EngineError::new(
                ErrorCode::AssetInvalid,
                "invalid diagnostic worker completion; inspect the explicit destination",
            ));
        }
        let mut receipt = prior.1.clone();
        receipt.status = result.status;
        receipt.error = result.error;
        receipt.result = Some(QueryResult::DiagnosticArtifact(Arc::new(result.receipt)));
        let record = Arc::new((prior.0.clone(), receipt));
        for slot in &mut self.receipts {
            if &slot.0.request_id == id {
                *slot = record.clone();
            }
        }
        self.diagnostics.pending_record = None;
        self.diagnostics.work = None;
        self.diagnostics.cancel_requested = false;
        self.diagnostics.terminal_record = Some(record);
        self.expire_diagnostic_candidate(now);
        Ok(())
    }
}
