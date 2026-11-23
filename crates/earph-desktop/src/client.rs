// ABOUTME: Builds immutable shared command envelopes and calls the actual LocalClient.
// ABOUTME: Preserves owner receipts, local errors, and exact reconciliation bytes.
use crate::{
    DeviceObservationView, ProjectionError, Reply, ReplyStatus, ResultView, StateView, decode_reply,
};
use earph_core::{
    control::{Command, CommandEnvelope},
    ids::{OwnerId, RequestId, SessionId, ZoneId},
};
use earph_ffi::{ClientError, LocalClient};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Ordinary,
    Safety,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError {
    NotQuery,
    NotDeviceQuery,
    NotDiagnosticQuery,
    NotDiagnosticArtifact,
    NotMutation,
    NotStart,
    MissingOwner,
    FrameLimit,
    Encoding,
}

#[derive(Debug, Clone)]
pub struct Request {
    envelope: Arc<CommandEnvelope>,
    bytes: Arc<[u8]>,
    lane: Lane,
}
impl Request {
    pub fn query(id: RequestId, zone: ZoneId, payload: Command) -> Result<Self, RequestError> {
        if !payload.is_query() {
            return Err(RequestError::NotQuery);
        }
        Self::query_envelope(id, zone, None, payload)
    }
    /// Binds native discovery to the observed owner without capturing playback state.
    pub fn device_query(
        id: RequestId,
        state: &StateView,
        payload: Command,
    ) -> Result<Self, RequestError> {
        if !matches!(
            payload,
            Command::DevicesList { .. } | Command::DevicesInspect { .. }
        ) {
            return Err(RequestError::NotDeviceQuery);
        }
        let owner = state.owner_id.clone().ok_or(RequestError::MissingOwner)?;
        Self::query_envelope(id, state.zone_id.clone(), Some(owner), payload)
    }
    /// Binds a diagnostic observation to the currently observed owner without changing state.
    pub fn diagnostic_query(
        id: RequestId,
        state: &StateView,
        payload: Command,
    ) -> Result<Self, RequestError> {
        if !payload.is_diagnostic() {
            return Err(RequestError::NotDiagnosticQuery);
        }
        let owner = state.owner_id.clone().ok_or(RequestError::MissingOwner)?;
        Self::query_envelope(id, state.zone_id.clone(), Some(owner), payload)
    }
    /// Captures owner scope and only the revision required for reviewed export.
    pub fn diagnostic_artifact(
        id: RequestId,
        state: &StateView,
        payload: Command,
    ) -> Result<Self, RequestError> {
        if !payload.is_diagnostic_artifact() {
            return Err(RequestError::NotDiagnosticArtifact);
        }
        let owner = state.owner_id.clone().ok_or(RequestError::MissingOwner)?;
        let revision =
            matches!(payload, Command::DiagnosticsExport { .. }).then_some(state.revision);
        Self::encode(CommandEnvelope {
            schema_version: 1,
            request_id: id,
            zone_id: state.zone_id.clone(),
            owner_id: Some(owner),
            session_id: None,
            expected_revision: revision,
            payload,
        })
    }
    fn query_envelope(
        id: RequestId,
        zone: ZoneId,
        owner_id: Option<OwnerId>,
        payload: Command,
    ) -> Result<Self, RequestError> {
        Self::encode(CommandEnvelope {
            schema_version: 1,
            request_id: id,
            zone_id: zone,
            owner_id,
            session_id: None,
            expected_revision: None,
            payload,
        })
    }
    pub fn mutation(
        id: RequestId,
        state: &StateView,
        payload: Command,
    ) -> Result<Self, RequestError> {
        if payload.is_query() {
            return Err(RequestError::NotMutation);
        }
        Self::targeted(id, state, state.session_id.clone(), payload)
    }
    /// Reads current state on the independent safety lane before fencing a Stop.
    pub(crate) fn safety_state(id: RequestId, state: &StateView) -> Result<Self, RequestError> {
        let mut request = Self::targeted(id, state, state.session_id.clone(), Command::State)?;
        request.lane = Lane::Safety;
        Ok(request)
    }
    /// Captures current owner fences with the caller's new session ID.
    /// Reconciliation retains this exact envelope without changing the observed State.
    pub fn start(
        id: RequestId,
        state: &StateView,
        session_id: SessionId,
        payload: Command,
    ) -> Result<Self, RequestError> {
        if !matches!(
            payload,
            Command::Start { .. } | Command::NetworkStartHttpFlac { .. }
        ) {
            return Err(RequestError::NotStart);
        }
        Self::targeted(id, state, Some(session_id), payload)
    }
    fn targeted(
        id: RequestId,
        state: &StateView,
        session_id: Option<SessionId>,
        payload: Command,
    ) -> Result<Self, RequestError> {
        let owner = state.owner_id.clone().ok_or(RequestError::MissingOwner)?;
        Self::encode(CommandEnvelope {
            schema_version: 1,
            request_id: id,
            zone_id: state.zone_id.clone(),
            owner_id: Some(owner),
            session_id,
            expected_revision: Some(state.revision),
            payload,
        })
    }
    fn encode(envelope: CommandEnvelope) -> Result<Self, RequestError> {
        let bytes = serde_json::to_vec(&envelope).map_err(|_| RequestError::Encoding)?;
        if bytes.len() > earph_engine::control_frame_limit() {
            return Err(RequestError::FrameLimit);
        }
        // This selects client scheduling only. The owner still admits every safety command.
        let lane = if matches!(
            envelope.payload,
            Command::Stop
                | Command::Pause
                | Command::Mute { muted: true }
                | Command::VolumeSilence
                | Command::VolumeFactoryDefaults
                | Command::VolumeCaps { .. }
                | Command::OwnerShutdown
                | Command::NetworkGrantsRevoke { .. }
                | Command::RendererGrantsRevoke { .. }
        ) || matches!(&envelope.payload, Command::RendererRequest { action, .. } if action.is_safety())
        {
            Lane::Safety
        } else {
            Lane::Ordinary
        };
        Ok(Self {
            envelope: Arc::new(envelope),
            bytes: bytes.into(),
            lane,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn envelope(&self) -> &CommandEnvelope {
        &self.envelope
    }
    pub fn lane(&self) -> Lane {
        self.lane
    }
    pub(crate) fn is_snapshot_query(&self) -> bool {
        matches!(
            self.envelope.payload,
            Command::State
                | Command::Capabilities
                | Command::QueueGet { .. }
                | Command::NetworkGet {}
                | Command::RendererGet {}
                | Command::ProfilesList { .. }
                | Command::ProfilesGet { .. }
                | Command::ProfilesResolve { .. }
                | Command::GraphInspect { .. }
                | Command::SignalPathGet {}
                | Command::MetricsGet {}
                | Command::EventsGet { .. }
                | Command::DiagnosticsPreview {}
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeliveryFailure {
    Local { code: String, detail: String },
    Protocol(ProjectionError),
}
impl From<ClientError> for DeliveryFailure {
    fn from(error: ClientError) -> Self {
        let ClientError::Engine { code, detail } = error;
        Self::Local { code, detail }
    }
}

#[derive(Clone)]
pub struct OwnerClient {
    client: Arc<LocalClient>,
}
impl OwnerClient {
    /// Constructs an attach-only handle. No owner lookup or engine start occurs here.
    pub fn attach(runtime_directory: String) -> Result<Self, DeliveryFailure> {
        Ok(Self {
            client: LocalClient::new(runtime_directory)?,
        })
    }
    /// Performs synchronous IPC. Call this on a worker, never the UI thread.
    pub fn request(&self, request: &Request) -> Result<Reply, DeliveryFailure> {
        let reply = decode_reply(self.client.request(request.bytes().to_vec())?)
            .map_err(DeliveryFailure::Protocol)?;
        if let Reply::Receipt(receipt) = &reply
            && !receipt_matches(request, receipt)
        {
            return Err(DeliveryFailure::Protocol(ProjectionError {
                detail: "receipt identity or query result does not match the request",
                bytes: reply.bytes().into(),
            }));
        }
        Ok(reply)
    }
}

fn receipt_matches(request: &Request, receipt: &crate::ReceiptView) -> bool {
    let envelope = request.envelope();
    let identity_matches = receipt.request_id == envelope.request_id
        && receipt.zone_id == envelope.zone_id
        && receipt.session_id == envelope.session_id;
    let result_matches = match (&envelope.payload, &receipt.result) {
        (payload, Some(ResultView::Renderer(_))) if payload.is_renderer() => {
            receipt.status == ReplyStatus::Applied
        }
        (payload, _) if payload.is_renderer() => {
            receipt.status != ReplyStatus::Applied && receipt.result.is_none()
        }
        (_, Some(ResultView::Renderer(_))) => false,
        (
            Command::DiagnosticsExport {
                directory,
                expected_sha256,
            }
            | Command::DiagnosticsInspect {
                directory,
                expected_sha256,
            },
            Some(ResultView::DiagnosticArtifact(value)),
        ) => {
            use earph_core::diagnostics::DiagnosticArtifactOperation;
            envelope.owner_id.is_some()
                && matches!(
                    (&envelope.payload, value.operation),
                    (
                        Command::DiagnosticsExport { .. },
                        DiagnosticArtifactOperation::Export
                    ) | (
                        Command::DiagnosticsInspect { .. },
                        DiagnosticArtifactOperation::Inspect
                    )
                )
                && value.directory == *directory
                && value.expected_sha256 == *expected_sha256
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (Command::DiagnosticsCancel { request_id }, Some(ResultView::DiagnosticCancel(value))) => {
            envelope.owner_id.is_some()
                && value.request_id == *request_id
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (payload, _) if payload.is_diagnostic_artifact() => {
            receipt.status == ReplyStatus::Rejected && receipt.result.is_none()
        }
        (Command::SignalPathGet {}, Some(ResultView::SignalPath(value))) => {
            receipt.status == ReplyStatus::Applied
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (Command::MetricsGet {}, Some(ResultView::Metrics(value))) => {
            receipt.status == ReplyStatus::Applied
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (Command::EventsGet { cursor }, Some(ResultView::Events(value))) => {
            receipt.status == ReplyStatus::Applied
                && value.validate_cursor(cursor.as_ref()).is_ok()
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (Command::DiagnosticsPreview {}, Some(ResultView::DiagnosticPreview(value))) => {
            receipt.status == ReplyStatus::Applied
                && diagnostic_target_matches(&value.binding, envelope)
        }
        (payload, _) if payload.is_diagnostic() => {
            receipt.status != ReplyStatus::Applied && receipt.result.is_none()
        }
        (
            Command::SourcesGet {} | Command::SourceStageUri { .. },
            Some(ResultView::Sources(value)),
        ) => {
            receipt.status == ReplyStatus::Applied
                && match &envelope.payload {
                    Command::SourceStageUri { uri, metadata } => {
                        if uri.is_empty() {
                            value.generation > 0 && value.staged.is_none()
                        } else {
                            value.staged.as_ref().is_some_and(|staged| {
                                staged.uri_bytes == uri.len()
                                    && staged.metadata_bytes == metadata.len()
                            })
                        }
                    }
                    _ => true,
                }
        }
        (
            Command::NetworkGet {}
            | Command::NetworkGrantsReplace { .. }
            | Command::NetworkGrantsRevoke { .. },
            Some(ResultView::Network(_)),
        ) => receipt.status == ReplyStatus::Applied,
        (
            Command::NetworkGet {}
            | Command::NetworkGrantsReplace { .. }
            | Command::NetworkGrantsRevoke { .. },
            _,
        ) => receipt.status != ReplyStatus::Applied && receipt.result.is_none(),
        (Command::NetworkStartHttpFlac { .. }, _) => receipt.result.is_none(),
        (_, Some(ResultView::Network(_))) => false,
        (Command::SourcesGet {} | Command::SourceStageUri { .. }, _) => {
            receipt.status != ReplyStatus::Applied && receipt.result.is_none()
        }
        (
            Command::DevicesList { backend } | Command::DevicesInspect { backend, .. },
            Some(ResultView::Devices(value)),
        ) => {
            value.backend == *backend
                && match (&envelope.payload, &value.observation) {
                    (Command::DevicesList { .. }, DeviceObservationView::List { .. }) => {
                        receipt.status == ReplyStatus::Applied
                    }
                    (
                        Command::DevicesInspect { endpoint, .. },
                        DeviceObservationView::Inspection {
                            endpoint: observed, ..
                        },
                    ) => receipt.status == ReplyStatus::Applied && endpoint == observed,
                    (_, DeviceObservationView::Failed { .. }) => {
                        matches!(receipt.status, ReplyStatus::Failed | ReplyStatus::Cancelled)
                    }
                    _ => false,
                }
        }
        (Command::DevicesList { .. } | Command::DevicesInspect { .. }, _) => {
            receipt.status != ReplyStatus::Applied && receipt.result.is_none()
        }
        _ => {
            receipt.status != ReplyStatus::Applied
                || match (&envelope.payload, &receipt.result) {
                    (Command::State, Some(ResultView::State(_)))
                    | (Command::Capabilities, Some(ResultView::Capabilities(_)))
                    | (Command::QueueGet { .. }, Some(ResultView::Queue(_)))
                    | (Command::GraphInspect { .. }, Some(ResultView::GraphInspection(_)))
                    | (Command::ProfilesList { .. }, Some(ResultView::Profiles(_)))
                    | (Command::ProfilesGet { .. }, Some(ResultView::Profile(_)))
                    | (Command::ProfilesResolve { .. }, Some(ResultView::ProfileResolution(_))) => {
                        true
                    }
                    (command, _) => !command.is_query(),
                }
        }
    };
    identity_matches && result_matches
}

fn diagnostic_target_matches(
    binding: &earph_core::diagnostics::DiagnosticBinding,
    envelope: &CommandEnvelope,
) -> bool {
    envelope
        .owner_id
        .as_ref()
        .is_none_or(|owner| binding.owner_id.as_ref() == Some(owner))
        && envelope
            .session_id
            .as_ref()
            .is_none_or(|session| binding.session_id.as_ref() == Some(session))
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::network_fixture;
    use earph_core::{control::ControlLimits, session::Session};
    fn receipt(owner: &mut Session, request: &Request) -> crate::ReceiptView {
        let receipt = owner.submit(request.envelope().clone());
        let Reply::Receipt(receipt) =
            decode_reply(serde_json::to_vec(&serde_json::json!({"receipt":receipt})).unwrap())
                .unwrap()
        else {
            panic!("receipt")
        };
        receipt
    }
    #[test]
    fn network_revoke_keeps_typed_result_after_exact_retirement_and_replay() {
        let mut owner = network_fixture::ready(None);
        let state: StateView =
            serde_json::from_value(serde_json::to_value(owner.snapshot()).unwrap()).unwrap();
        let request = Request::mutation(
            RequestId::new("retire-exact").unwrap(),
            &state,
            Command::NetworkGrantsRevoke {
                expected_generation: 1,
            },
        )
        .unwrap();
        let accepted = owner.submit_with_network_effects(
            request.envelope().clone(),
            40,
            |_| unreachable!(),
            |_, _| Ok(()),
            |_| Ok(()),
        );
        assert_eq!(
            accepted.status,
            earph_core::control::ReceiptStatus::Accepted
        );
        assert!(receipt_matches(&request, &receipt(&mut owner, &request)));
        let closing = owner.closing_network_source().unwrap();
        owner
            .complete_network_retirement(
                earph_core::network_source::NetworkRetirementAck {
                    binding: closing.context.binding.clone(),
                    source_epochs: closing.context.source_epochs,
                    output_epochs: closing.output_epochs,
                    graph_epoch: closing.graph_epoch,
                    device_epoch: closing.device_epoch,
                    observation: None,
                },
                50,
            )
            .unwrap();
        let settled = receipt(&mut owner, &request);
        assert_eq!(settled.status, ReplyStatus::Applied);
        assert!(matches!(settled.result, Some(ResultView::Network(_))));
        assert!(receipt_matches(&request, &settled));
    }
    #[test]
    fn network_query_result_must_match_the_exact_typed_request() {
        let mut owner = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("network-consumer").unwrap(),
            ControlLimits::default(),
        );
        let query = Request::query(
            RequestId::new("query").unwrap(),
            ZoneId::new("main").unwrap(),
            Command::NetworkGet {},
        )
        .unwrap();
        let observed = receipt(&mut owner, &query);
        assert!(receipt_matches(&query, &observed));
        let wrong = Request::query(
            RequestId::new("query").unwrap(),
            ZoneId::new("main").unwrap(),
            Command::SourcesGet {},
        )
        .unwrap();
        assert!(!receipt_matches(&wrong, &observed));
        let wrong = Request::query(
            RequestId::new("query").unwrap(),
            ZoneId::new("main").unwrap(),
            Command::State,
        )
        .unwrap();
        assert!(!receipt_matches(&wrong, &observed));
        let state: StateView =
            serde_json::from_value(serde_json::to_value(owner.snapshot()).unwrap()).unwrap();
        let wrong =
            Request::mutation(RequestId::new("query").unwrap(), &state, Command::Stop).unwrap();
        assert!(!receipt_matches(&wrong, &observed));
        let mut missing = observed;
        missing.result = None;
        assert!(!receipt_matches(&query, &missing));
    }
}
