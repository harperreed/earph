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

fn is_applied_snapshot(record: &RequestRecord) -> bool {
    !record.in_flight
        && record.presentation == Presentation::Applied
        && record.request.is_snapshot_query()
}

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
    /// Tracks the current send; an Accepted operation can remain pending afterward.
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
    pub(crate) fn pause_ack_pending(&self) -> bool {
        let Some(observed) = self.latest_state.as_ref().filter(|_| self.attached) else {
            return false;
        };
        let Some(owner) = observed.state.owner_id.as_ref() else {
            return false;
        };
        self.records.iter().any(|record| {
            let request = record.request.envelope();
            !record.owner_changed
                && record.presentation == Presentation::Accepted
                && matches!(request.payload, Command::Pause)
                && request.owner_id.as_ref() == Some(owner)
                && request.zone_id == observed.state.zone_id
                && request.session_id == observed.state.session_id
        })
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
        Self::resume(runtime, PresentationModel::default()).map_err(|(error, _)| error)
    }
    pub(crate) fn resume(
        runtime: String,
        mut model: PresentationModel,
    ) -> Result<Self, (DeliveryFailure, Box<PresentationModel>)> {
        let dispatcher = match Dispatcher::attach(runtime) {
            Ok(dispatcher) => dispatcher,
            Err(error) => return Err((error, Box::new(model))),
        };
        model.attached = true;
        Ok(Self {
            dispatcher: Some(dispatcher),
            model,
        })
    }
    pub fn model(&self) -> &PresentationModel {
        &self.model
    }
    /// Retires an adopted internal snapshot while preserving the latest observed State.
    /// Sent work, failures, uncertainty, discovery, and mutation receipts remain retained.
    pub fn retire_observation(&mut self, id: &RequestId) -> bool {
        let Some(index) = self.model.records.iter().position(|record| {
            &record.request.envelope().request_id == id && is_applied_snapshot(record)
        }) else {
            return false;
        };
        self.model.records.remove(index);
        true
    }
    pub fn submit(&mut self, request: Request) -> Result<Submission, SubmitError> {
        if self.model.record(&request.envelope().request_id).is_some() {
            return Err(SubmitError::DuplicateIdentity);
        }
        if request.lane() == Lane::Ordinary
            && matches!(request.envelope().payload, Command::State)
            && let Some(existing) = self.model.records.iter().find(|entry| {
                entry.in_flight
                    && entry.request.lane() == Lane::Ordinary
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
                    .enumerate()
                    .filter(|(_, entry)| {
                        !entry.in_flight
                            && entry.presentation.settled()
                            && (!ordinary_full || entry.request.lane() == Lane::Ordinary)
                    })
                    .min_by_key(|(_, entry)| !is_applied_snapshot(entry))
                    .map(|(index, _)| index)
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

    fn observe_core(
        model: &mut PresentationModel,
        core: &mut earph_core::session::Session,
        request: Request,
        now: u64,
    ) {
        if model.record(&request.envelope().request_id).is_none() {
            model.next_ordinal += 1;
            model.records.push_back(RequestRecord {
                request: request.clone(),
                presentation: Presentation::Queued,
                in_flight: true,
                owner_changed: false,
                outcome: None,
                ordinal: model.next_ordinal,
            });
        }
        let receipt = core.submit_graph(request.envelope().clone(), now);
        model.observe(Completion {
            request,
            outcome: Ok(crate::decode_reply(
                serde_json::to_vec(&serde_json::json!({"receipt":receipt})).unwrap(),
            )
            .unwrap()),
        });
    }

    fn accepted_pause() -> (earph_core::session::Session, PresentationModel, Request) {
        let mut core = crate::network_fixture::ready(None);
        let mut model = PresentationModel {
            attached: true,
            ..Default::default()
        };
        observe_core(&mut model, &mut core, query("pause-before"), 50);
        let pause = Request::mutation(
            RequestId::new("pause-awaiting-worker").unwrap(),
            &model.latest_state().unwrap().state,
            Command::Pause,
        )
        .unwrap();
        observe_core(&mut model, &mut core, pause.clone(), 60);
        observe_core(&mut model, &mut core, query("pause-accepted-state"), 60);
        assert_eq!(
            model
                .record(&pause.envelope().request_id)
                .unwrap()
                .presentation,
            Presentation::Accepted
        );
        assert_eq!(
            model.latest_state().unwrap().state.transport,
            crate::TransportView::Paused
        );
        (core, model, pause)
    }

    #[test]
    fn accepted_pause_waits_for_its_actual_worker_acknowledgement() {
        use earph_core::network_source::{NetworkEvent, NetworkEventKind, NetworkLifecycle};
        let (mut core, mut model, pause) = accepted_pause();
        assert!(
            model.pause_ack_pending(),
            "Accepted Pause still awaits the worker"
        );
        model
            .records
            .iter_mut()
            .find(|record| record.request.envelope().request_id == pause.envelope().request_id)
            .unwrap()
            .in_flight = true;
        assert!(
            model.pause_ack_pending(),
            "Reconciliation must not make Pause complete"
        );
        observe_core(&mut model, &mut core, pause.clone(), 60);
        assert!(model.pause_ack_pending());
        let state = core.snapshot();
        let active = core.network_source().unwrap();
        let mut observation = state.observation.network().cloned().unwrap();
        observation.output_epochs = state.epochs;
        observation.lifecycle = NetworkLifecycle::Paused;
        core.advance_network(NetworkEvent {
            binding: active.binding.clone(),
            source_epochs: active.source_epochs,
            output_epochs: state.epochs,
            graph_epoch: state.graph_epoch,
            device_epoch: state.device_epoch,
            monotonic_ns: 61,
            kind: NetworkEventKind::Paused { observation },
        })
        .unwrap();
        observe_core(&mut model, &mut core, pause.clone(), 62);
        assert_eq!(
            model
                .record(&pause.envelope().request_id)
                .unwrap()
                .presentation,
            Presentation::Applied
        );
        assert!(!model.pause_ack_pending());
    }

    #[test]
    fn detached_and_replaced_owners_do_not_inherit_a_pending_pause() {
        let (_core, mut model, pause) = accepted_pause();
        assert!(model.pause_ack_pending());
        model.attached = false;
        assert!(!model.pause_ack_pending());
        model.attached = true;
        assert!(model.pause_ack_pending());
        let mut replacement = earph_core::session::Session::new_owner(
            ZoneId::new("main").unwrap(),
            earph_core::ids::OwnerId::new("replacement-owner").unwrap(),
            earph_core::control::ControlLimits::default(),
        );
        observe_core(&mut model, &mut replacement, query("replacement-state"), 70);
        let retained = model.record(&pause.envelope().request_id).unwrap();
        assert_eq!(retained.presentation, Presentation::Accepted);
        assert!(retained.owner_changed);
        assert!(!model.pause_ack_pending());
    }

    fn query(id: &str) -> Request {
        Request::query(
            RequestId::new(id).unwrap(),
            ZoneId::new("main").unwrap(),
            Command::State,
        )
        .unwrap()
    }
    fn settle(client: &mut DesktopClient, id: &RequestId) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            client.poll();
            if !client.model().record(id).unwrap().in_flight {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }
    struct Owner {
        runtime: std::path::PathBuf,
        serve: Option<std::thread::JoinHandle<Result<(), EngineError>>>,
    }
    impl Owner {
        fn start() -> Self {
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let runtime =
                std::path::PathBuf::from(format!("/tmp/erm-{}-{time:x}", std::process::id()));
            let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
            Self {
                runtime,
                serve: Some(std::thread::spawn(move || owner.serve())),
            }
        }
        fn client(&self) -> crate::OwnerClient {
            crate::OwnerClient::attach(self.runtime.to_str().unwrap().into()).unwrap()
        }
    }
    impl Drop for Owner {
        fn drop(&mut self) {
            // Cleanup must survive failures in the projection under test.
            let bytes = earph_engine::request(&self.runtime, query("resume-cleanup-state").bytes())
                .unwrap();
            let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let state = &reply["receipt"]["result"]["value"];
            let request = earph_core::control::CommandEnvelope {
                schema_version: 1,
                request_id: RequestId::new("resume-cleanup-shutdown").unwrap(),
                zone_id: serde_json::from_value(state["zone_id"].clone()).unwrap(),
                owner_id: serde_json::from_value(state["owner_id"].clone()).unwrap(),
                session_id: serde_json::from_value(state["session_id"].clone()).unwrap(),
                expected_revision: Some(state["revision"].as_u64().unwrap()),
                payload: Command::OwnerShutdown,
            };
            earph_engine::request(&self.runtime, &serde_json::to_vec(&request).unwrap()).unwrap();
            self.serve.take().unwrap().join().unwrap().unwrap();
        }
    }

    #[test]
    fn resume_preserves_owner_b_when_an_uncertain_owner_a_read_replays() {
        let a = Owner::start();
        let b = Owner::start();
        let old = query("resume-old-a");
        let Reply::Receipt(a_reply) = a.client().request(&old).unwrap() else {
            panic!("A state")
        };
        let missing = format!("{}-x", a.runtime.to_str().unwrap());
        let mut client = DesktopClient::attach(missing).unwrap();
        client.submit(old.clone()).unwrap();
        settle(&mut client, &old.envelope().request_id);
        assert_eq!(
            client
                .model()
                .record(&old.envelope().request_id)
                .unwrap()
                .presentation,
            Presentation::Uncertain
        );
        let (model, shutdown) = client.disconnect();
        shutdown.join().unwrap();
        let mut client = DesktopClient::resume(b.runtime.to_str().unwrap().into(), model)
            .unwrap_or_else(|(e, _)| panic!("{e:?}"));
        let fresh = query("resume-fresh-b");
        client.submit(fresh.clone()).unwrap();
        settle(&mut client, &fresh.envelope().request_id);
        let b_state = client.model().latest_state().unwrap().state.clone();
        assert_ne!(b_state.owner_id, a_reply.state().unwrap().owner_id);
        let mutation = Request::mutation(
            RequestId::new("resume-b-mute").unwrap(),
            &b_state,
            Command::Mute { muted: true },
        )
        .unwrap();
        client.submit(mutation.clone()).unwrap();
        settle(&mut client, &mutation.envelope().request_id);
        let ordinal = client.model.next_ordinal;
        let (model, shutdown) = client.disconnect();
        shutdown.join().unwrap();
        let mut client = DesktopClient::resume(a.runtime.to_str().unwrap().into(), model)
            .unwrap_or_else(|(e, _)| panic!("{e:?}"));
        client.reconcile(&old.envelope().request_id).unwrap();
        settle(&mut client, &old.envelope().request_id);
        assert_eq!(
            client.model().latest_state().unwrap().state.owner_id,
            b_state.owner_id
        );
        assert!(
            !client
                .model()
                .record(&mutation.envelope().request_id)
                .unwrap()
                .owner_changed
        );
        assert_eq!(client.model.next_ordinal, ordinal);
        let old_record = client.model().record(&old.envelope().request_id).unwrap();
        assert_eq!(old_record.request.bytes(), old.bytes());
        let Some(Ok(Reply::Receipt(replay))) = &old_record.outcome else {
            panic!("actual replay")
        };
        assert_eq!(
            replay.state().unwrap().owner_id,
            a_reply.state().unwrap().owner_id
        );
        let (model, shutdown) = client.disconnect();
        shutdown.join().unwrap();
        let (error, retained) = DesktopClient::resume("x".repeat(61), model).err().unwrap();
        assert!(matches!(error, DeliveryFailure::Local { .. }));
        assert_eq!(retained.next_ordinal, ordinal);
        assert!(retained.record(&old.envelope().request_id).is_some());
        assert!(!retained.attached);
    }

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
