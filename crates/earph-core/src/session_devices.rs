// ABOUTME: Pins one owner discovery receipt and settles it without changing playback intent.
// ABOUTME: Admits bounded immutable results through the shared command replay history.
use super::*;
use crate::devices::*;

type Record = Arc<(CommandEnvelope, Receipt)>;
#[derive(Clone)]
struct Pending {
    job: DeviceQueryJob,
    record: Option<Record>,
}
#[derive(Clone, Default)]
pub(super) struct DeviceControl {
    backend: Option<OutputBackend>,
    pending: Option<Pending>,
}

impl Session {
    pub fn enable_device_discovery(&mut self, backend: OutputBackend) {
        self.devices.backend = Some(backend);
    }
    pub fn device_query_job(&self) -> Option<DeviceQueryJob> {
        self.devices.pending.as_ref().map(|p| p.job.clone())
    }
    pub(super) fn device_capabilities(&self) -> DeviceCapabilities {
        DeviceCapabilities::new(self.devices.backend.into_iter().collect())
    }
    pub(super) fn device_receipt(&self, request: &RequestId) -> Option<&Record> {
        self.devices
            .pending
            .as_ref()
            .filter(|p| &p.job.request_id == request)
            .and_then(|p| p.record.as_ref())
    }
    pub(super) fn retain_device_record(&mut self, record: Record) {
        if let Some(pending) = &mut self.devices.pending
            && pending.job.request_id == record.0.request_id
        {
            pending.record = Some(record);
        }
    }
    pub(super) fn admit_device_query(
        &mut self,
        command: &CommandEnvelope,
    ) -> Result<ReceiptStatus, EngineError> {
        if command.owner_id.is_some() && command.owner_id != self.state.owner_id {
            return Err(STALE);
        }
        if self.state.shutdown_requested {
            return Err(STATE);
        }
        let query = match &command.payload {
            Command::DevicesList { backend } => DeviceQuery {
                backend: *backend,
                endpoint: None,
            },
            Command::DevicesInspect { backend, endpoint } => DeviceQuery {
                backend: *backend,
                endpoint: Some(endpoint.clone()),
            },
            _ => return Err(DEVICE_INVALID),
        };
        query.validate()?;
        if self.devices.backend != Some(query.backend) {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "owner has no configured discovery helper for this backend",
            ));
        }
        if self.devices.pending.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "a device query is still active or retiring",
            ));
        }
        loop {
            let retained: usize = self
                .receipts
                .iter()
                .filter_map(|record| match &record.1.result {
                    Some(QueryResult::Devices(result)) => Some(result.retained_bytes),
                    _ => None,
                })
                .sum();
            if retained
                .checked_add(MAX_DEVICE_RESULT_BYTES)
                .is_some_and(|bytes| bytes <= MAX_RETAINED_DEVICE_BYTES)
            {
                break;
            }
            let index = self
                .receipts
                .iter()
                .position(|record| matches!(record.1.result, Some(QueryResult::Devices(_))))
                .ok_or(DEVICE_BUDGET)?;
            self.receipts.remove(index);
        }
        self.devices.pending = Some(Pending {
            job: DeviceQueryJob {
                request_id: command.request_id.clone(),
                owner_id: self.state.owner_id.clone().ok_or(STALE)?,
                query,
            },
            record: None,
        });
        Ok(ReceiptStatus::Accepted)
    }
    pub fn complete_device_query(
        &mut self,
        job: &DeviceQueryJob,
        now_ns: u64,
        observation: DeviceObservation,
    ) -> Result<(), EngineError> {
        let pending = self.devices.pending.as_ref().ok_or(STALE)?;
        if pending.job != *job || self.state.owner_id.as_ref() != Some(&job.owner_id) {
            return Err(STALE);
        }
        let retained = observation.retained_capacity_bytes(&job.query)?;
        let result = DeviceQueryResult {
            owner_id: job.owner_id.clone(),
            backend: job.query.backend,
            completed_monotonic_ns: now_ns,
            read_boundary: job.query.backend.into(),
            retained_bytes: retained - size_of::<DeviceObservation>()
                + size_of::<DeviceQueryResult>()
                + job.owner_id.capacity_bytes(),
            observation,
        };
        if result.retained_bytes > MAX_DEVICE_RESULT_BYTES {
            return Err(DEVICE_BUDGET);
        }
        let (status, error) = match &result.observation {
            DeviceObservation::Failed { error, .. } => (
                if error.code == ErrorCode::OperationCancelled {
                    ReceiptStatus::Cancelled
                } else {
                    ReceiptStatus::Failed
                },
                Some(EngineError::new(
                    error.code,
                    "read-only device query failed",
                )),
            ),
            _ => (ReceiptStatus::Applied, None),
        };
        let prior = pending.record.as_ref().ok_or(STALE)?;
        let mut updated = (**prior).clone();
        updated.1.status = status;
        updated.1.error = error;
        updated.1.result = Some(QueryResult::Devices(Arc::new(result)));
        let updated = Arc::new(updated);
        if let Some(slot) = self
            .receipts
            .iter_mut()
            .find(|record| Arc::ptr_eq(record, prior))
        {
            *slot = updated;
        } else {
            if self.receipts.len() == self.limits.receipts {
                self.receipts.pop_front();
            }
            self.receipts.push_back(updated);
        }
        self.devices.pending = None;
        Ok(())
    }
    pub fn fail_device_query(
        &mut self,
        job: &DeviceQueryJob,
        now_ns: u64,
        error: EngineError,
    ) -> Result<(), EngineError> {
        self.complete_device_query(
            job,
            now_ns,
            DeviceObservation::Failed {
                error: error.into(),
                cleanup_confirmed: true,
            },
        )
    }
}
