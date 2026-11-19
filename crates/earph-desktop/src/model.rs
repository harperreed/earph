// ABOUTME: Presents bounded request histories without confusing acceptance with application.
// ABOUTME: Preserves uncertain envelopes and observed snapshots across independent control lanes.
use crate::{
    Completion, DeliveryFailure, DispatchError, Dispatcher, Lane, Reply, ReplyStatus, Request,
    ResultView, StateView, WorkerShutdown,
};
use earph_core::{ErrorCode, control::Command, ids::RequestId};
use std::{collections::VecDeque, sync::Arc, time::Instant};

const HISTORY_LIMIT: usize = 16;
// Keep space for one in-flight and one queued safety request even during ordinary uncertainty.
const ORDINARY_HISTORY_LIMIT: usize = HISTORY_LIMIT - 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presentation {
    Queued,
    Accepted,
    Applied,
    Rejected,
    Cancelled,
    Failed,
    OwnerError,
    Uncertain,
}
impl Presentation {
    fn settled(self) -> bool {
        matches!(
            self,
            Self::Applied | Self::Rejected | Self::Cancelled | Self::Failed | Self::OwnerError
        )
    }
}

#[derive(Debug)]
pub struct RequestRecord {
    pub request: Request,
    pub presentation: Presentation,
    pub in_flight: bool,
    pub owner_changed: bool,
    pub outcome: Option<Result<Reply, DeliveryFailure>>,
    ordinal: u64,
}

#[derive(Debug)]
pub struct ObservedState {
    pub state: Arc<StateView>,
    pub request_id: RequestId,
    pub received_at: Instant,
    ordinal: u64,
}

#[derive(Debug, Default)]
pub struct PresentationModel {
    records: VecDeque<RequestRecord>,
    latest_state: Option<ObservedState>,
    next_ordinal: u64,
    pub attached: bool,
}
impl PresentationModel {
    pub fn records(&self) -> &VecDeque<RequestRecord> {
        &self.records
    }
    pub fn record(&self, id: &RequestId) -> Option<&RequestRecord> {
        self.records
            .iter()
            .find(|entry| &entry.request.envelope().request_id == id)
    }
    pub fn latest_state(&self) -> Option<&ObservedState> {
        self.latest_state.as_ref()
    }
    fn observe(&mut self, completion: Completion) {
        let id = &completion.request.envelope().request_id;
        let Some(index) = self
            .records
            .iter()
            .position(|entry| &entry.request.envelope().request_id == id)
        else {
            return;
        };
        if self.records[index].request.bytes() != completion.request.bytes() {
            return;
        }
        let ordinal = self.records[index].ordinal;
        if let Ok(Reply::Receipt(receipt)) = &completion.outcome
            && let Some(ResultView::State(state)) = &receipt.result
        {
            let fresh = self.latest_state.as_ref().is_none_or(|current| {
                ordinal > current.ordinal
                    && (state.owner_id != current.state.owner_id
                        || (state.revision, state.event_sequence, state.monotonic_ns)
                            >= (
                                current.state.revision,
                                current.state.event_sequence,
                                current.state.monotonic_ns,
                            ))
            });
            if fresh {
                for record in &mut self.records {
                    if record
                        .request
                        .envelope()
                        .owner_id
                        .as_ref()
                        .is_some_and(|owner| Some(owner) != state.owner_id.as_ref())
                    {
                        record.owner_changed = true;
                    }
                }
                self.latest_state = Some(ObservedState {
                    state: state.clone(),
                    request_id: receipt.request_id.clone(),
                    received_at: Instant::now(),
                    ordinal,
                });
            }
        }
        let record = &mut self.records[index];
        record.in_flight = false;
        record.presentation = match &completion.outcome {
            Err(_) => Presentation::Uncertain,
            Ok(Reply::OwnerError(error))
                if matches!(
                    error.error.code,
                    ErrorCode::DeviceUnavailable | ErrorCode::InternalFault
                ) =>
            {
                Presentation::Uncertain
            }
            Ok(Reply::OwnerError(_)) => Presentation::OwnerError,
            Ok(Reply::Receipt(receipt)) => match receipt.status {
                ReplyStatus::Accepted => Presentation::Accepted,
                ReplyStatus::Applied => Presentation::Applied,
                ReplyStatus::Rejected => Presentation::Rejected,
                ReplyStatus::Cancelled => Presentation::Cancelled,
                ReplyStatus::Failed => Presentation::Failed,
            },
        };
        record.outcome = Some(completion.outcome);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submission {
    Admitted,
    Coalesced(RequestId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitError {
    DuplicateIdentity,
    HistoryFull,
    OrdinalExhausted,
    UnknownRequest,
    NotReconcilable,
    OwnerChanged,
    Dispatch(DispatchError),
}

pub struct DesktopClient {
    dispatcher: Option<Dispatcher>,
    model: PresentationModel,
}
impl DesktopClient {
    pub fn attach(runtime: String) -> Result<Self, DeliveryFailure> {
        // Attachment constructs a handle and workers; it does not prove that an owner is available.
        let dispatcher = Dispatcher::attach(runtime)?;
        Ok(Self {
            dispatcher: Some(dispatcher),
            model: PresentationModel {
                attached: true,
                ..Default::default()
            },
        })
    }
    pub fn model(&self) -> &PresentationModel {
        &self.model
    }
    pub fn submit(&mut self, request: Request) -> Result<Submission, SubmitError> {
        if self.model.record(&request.envelope().request_id).is_some() {
            return Err(SubmitError::DuplicateIdentity);
        }
        if matches!(request.envelope().payload, Command::State)
            && let Some(existing) = self.model.records.iter().find(|entry| {
                entry.in_flight
                    && entry.request.envelope().zone_id == request.envelope().zone_id
                    && matches!(entry.request.envelope().payload, Command::State)
            })
        {
            return Ok(Submission::Coalesced(
                existing.request.envelope().request_id.clone(),
            ));
        }
        let ordinal = self
            .model
            .next_ordinal
            .checked_add(1)
            .ok_or(SubmitError::OrdinalExhausted)?;
        let ordinary_full = request.lane() == Lane::Ordinary
            && self
                .model
                .records
                .iter()
                .filter(|entry| entry.request.lane() == Lane::Ordinary)
                .count()
                >= ORDINARY_HISTORY_LIMIT;
        let eviction = if ordinary_full || self.model.records.len() == HISTORY_LIMIT {
            Some(
                self.model
                    .records
                    .iter()
                    .position(|entry| {
                        !entry.in_flight
                            && entry.presentation.settled()
                            && (!ordinary_full || entry.request.lane() == Lane::Ordinary)
                    })
                    .ok_or(SubmitError::HistoryFull)?,
            )
        } else {
            None
        };
        self.dispatcher
            .as_ref()
            .ok_or(SubmitError::Dispatch(DispatchError::Disconnected))?
            .try_submit(request.clone())
            .map_err(SubmitError::Dispatch)?;
        if let Some(index) = eviction {
            self.model.records.remove(index);
        }
        self.model.next_ordinal = ordinal;
        self.model.records.push_back(RequestRecord {
            request,
            presentation: Presentation::Queued,
            in_flight: true,
            owner_changed: false,
            outcome: None,
            ordinal,
        });
        Ok(Submission::Admitted)
    }
    pub fn reconcile(&mut self, id: &RequestId) -> Result<(), SubmitError> {
        let index = self
            .model
            .records
            .iter()
            .position(|entry| &entry.request.envelope().request_id == id)
            .ok_or(SubmitError::UnknownRequest)?;
        let record = &self.model.records[index];
        if record.owner_changed {
            return Err(SubmitError::OwnerChanged);
        }
        if record.in_flight
            || !matches!(
                record.presentation,
                Presentation::Accepted | Presentation::Uncertain
            )
        {
            return Err(SubmitError::NotReconcilable);
        }
        self.dispatcher
            .as_ref()
            .ok_or(SubmitError::Dispatch(DispatchError::Disconnected))?
            .try_submit(record.request.clone())
            .map_err(SubmitError::Dispatch)?;
        self.model.records[index].in_flight = true;
        Ok(())
    }
    pub fn poll(&mut self) -> usize {
        let mut count = 0;
        if let Some(dispatcher) = &self.dispatcher {
            // Bound one UI drain even if producers continue completing requests.
            for _ in 0..4 {
                let Some(completion) = dispatcher.try_next() else {
                    break;
                };
                self.model.observe(completion);
                count += 1;
            }
        }
        count
    }
    pub fn disconnect(mut self) -> (PresentationModel, WorkerShutdown) {
        self.model.attached = false;
        for record in &mut self.model.records {
            if record.in_flight {
                record.in_flight = false;
                record.presentation = Presentation::Uncertain;
            }
        }
        let shutdown = self
            .dispatcher
            .take()
            .expect("attached client owns dispatcher")
            .disconnect();
        (std::mem::take(&mut self.model), shutdown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{EngineError, ids::ZoneId};

    #[test]
    fn owner_serialization_failure_preserves_uncertainty_and_the_original_request() {
        let request = Request::query(
            RequestId::new("serialize-failure").unwrap(),
            ZoneId::new("main").unwrap(),
            Command::Capabilities,
        )
        .unwrap();
        let expected = request.bytes().to_vec();
        let mut model = PresentationModel::default();
        model.records.push_back(RequestRecord {
            request: request.clone(),
            presentation: Presentation::Queued,
            in_flight: true,
            owner_changed: false,
            outcome: None,
            ordinal: 1,
        });
        let error = EngineError::new(ErrorCode::InternalFault, "cannot serialize control receipt");
        let bytes = serde_json::to_vec(&serde_json::json!({"error": error})).unwrap();
        model.observe(Completion {
            request,
            outcome: Ok(crate::decode_reply(bytes).unwrap()),
        });
        let record = model.records.front().unwrap();
        assert_eq!(record.presentation, Presentation::Uncertain);
        assert!(!record.presentation.settled());
        assert_eq!(record.request.bytes(), expected);
    }
}
