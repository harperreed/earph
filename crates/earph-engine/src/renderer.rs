// ABOUTME: Connects explicitly bound renderer transport to the serial Engine owner.
// ABOUTME: Keeps protocol ingress, graph continuations, and callback retirement under Core authority.
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, QueryResult, Receipt, ReceiptStatus},
    ids::{RequestId, SessionId},
    renderer::{RendererAction, RendererGraphBinding, RendererListener, RendererRight},
    renderer_events::{
        MAX_RENDERER_SUBSCRIPTIONS, RendererDeliveryOutcome, RendererService,
        RendererSubscriptionAction, RendererSubscriptionAdmission, RendererSubscriptionBinding,
    },
    session::{Session, TransportState},
};
use earph_network::upnp::{
    Advertisement, DeviceIdentity, HttpSurface, IncomingRequest, PendingAction,
    PendingSubscription, Service, SsdpEndpoint, UpnpFault,
};
use sha2::{Digest, Sha256};
use std::{net::SocketAddrV4, path::Path, sync::Arc, time::Instant};

/// Multicast scope for discovery, the value that UPnP Device Architecture 1.0 gives.
const DISCOVERY_TTL: u32 = 4;
const DEVICE_NAME: &str = "Earph";
const SERVER: &str = concat!(
    "Earph/",
    env!("CARGO_PKG_VERSION"),
    " UPnP/1.0 Earph/",
    env!("CARGO_PKG_VERSION")
);
const UDN_FILE: &str = "renderer-udn";

/// Explicit library configuration. An owner binds no renderer without one.
pub struct RendererConfiguration {
    address: SocketAddrV4,
    identity: DeviceIdentity,
    discovery: bool,
}

impl RendererConfiguration {
    pub fn new(
        address: SocketAddrV4,
        udn: &str,
        name: &str,
        server: &str,
    ) -> Result<Self, EngineError> {
        let identity = DeviceIdentity::new(udn, name, server)
            .map_err(|_| EngineError::new(ErrorCode::AssetInvalid, "invalid renderer identity"))?;
        Ok(Self {
            address,
            identity,
            discovery: false,
        })
    }
    /// Also advertises and answers SSDP on the selected interface.
    pub fn with_discovery(mut self) -> Self {
        self.discovery = true;
        self
    }
    /// Binds the listener and reports what it serves. A discovery failure closes the listener.
    pub(crate) fn bind(self) -> Result<(HttpSurface, RendererListener), EngineError> {
        let surface = HttpSurface::bind(self.address, self.identity.clone()).map_err(|_| {
            EngineError::new(
                ErrorCode::DeviceUnavailable,
                "cannot bind renderer listener",
            )
        })?;
        let listener = RendererListener {
            address: surface.local_addr(),
            discovery: self.discovery,
        };
        if self.discovery {
            Advertisement::new(listener.address, self.identity)
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
                .and_then(|advertisement| {
                    SsdpEndpoint::bind_multicast(advertisement, DISCOVERY_TTL)
                })
                .and_then(|endpoint| surface.attach_discovery(endpoint))
                .map_err(|_| {
                    EngineError::new(
                        ErrorCode::DeviceUnavailable,
                        "cannot start renderer discovery on the selected interface",
                    )
                })?;
        }
        Ok((surface, listener))
    }
}

/// The renderer that the owner command line selects with `--renderer-listen`.
pub struct RendererListen {
    address: SocketAddrV4,
    discovery: bool,
}

impl RendererListen {
    /// Accepts one concrete IPv4 address with a port. Port 0 lets the system choose the port.
    pub fn parse(listen: &str, discovery: bool) -> Result<Self, EngineError> {
        let address = listen.parse::<SocketAddrV4>().ok().filter(|address| {
            let ip = address.ip();
            !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast()
        });
        Ok(Self {
            address: address.ok_or(EngineError::new(
                ErrorCode::AssetInvalid,
                "--renderer-listen needs one concrete IPv4 address and port, such as 192.0.2.10:49152",
            ))?,
            discovery,
        })
    }
    /// Builds the configuration with this runtime's stable device identity.
    pub(crate) fn configuration(
        self,
        runtime: &Path,
        token: &str,
    ) -> Result<RendererConfiguration, EngineError> {
        let configuration = RendererConfiguration::new(
            self.address,
            &device_udn(runtime, token)?,
            DEVICE_NAME,
            SERVER,
        )?;
        Ok(if self.discovery {
            configuration.with_discovery()
        } else {
            configuration
        })
    }
}

/// Reads this runtime's device name, or creates a random one on first use.
/// Controllers key a renderer by its UDN, so it must survive a restart.
fn device_udn(runtime: &Path, token: &str) -> Result<String, EngineError> {
    if let Some(bytes) = crate::runtime::read(&runtime.join(UDN_FILE), 64)? {
        return String::from_utf8(bytes)
            .map_err(|_| EngineError::new(ErrorCode::AssetInvalid, "invalid renderer identity"));
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| {
        EngineError::new(
            ErrorCode::InternalFault,
            "cannot create the renderer identity",
        )
    })?;
    // RFC 9562 version 4, variant 1.
    random[6] = random[6] & 0x0f | 0x40;
    random[8] = random[8] & 0x3f | 0x80;
    let hex: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let udn = format!(
        "uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    crate::runtime::write_atomic(runtime, UDN_FILE, token, udn.as_bytes())?;
    Ok(udn)
}

pub(crate) enum Admission {
    Read(Vec<(&'static str, String)>),
    Command(Box<CommandEnvelope>),
}

pub(crate) struct RendererRuntime {
    surface: Option<HttpSurface>,
    prefix: String,
    epoch: u64,
    graph_attempt: Option<GraphAttempt>,
    subscriptions: Vec<SubscriptionHandle>,
    retiring: bool,
    listener_joined: bool,
}
struct SubscriptionHandle {
    binding: RendererSubscriptionBinding,
    service: RendererService,
}
struct GraphAttempt {
    binding: Arc<RendererGraphBinding>,
    phase: GraphPhase,
}
#[derive(PartialEq, Eq)]
enum GraphPhase {
    Preparing,
    Committed,
}
impl RendererRuntime {
    pub(crate) fn new(surface: HttpSurface, session: &Session) -> Self {
        let state = session.snapshot();
        let digest = Sha256::digest(
            state
                .owner_id
                .as_ref()
                .expect("owner session")
                .as_str()
                .as_bytes(),
        );
        let owner: String = digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            surface: Some(surface),
            prefix: format!("upnp-{owner}-{:016x}", session.renderer_epoch()),
            epoch: session.renderer_epoch(),
            graph_attempt: None,
            subscriptions: Vec::with_capacity(MAX_RENDERER_SUBSCRIPTIONS),
            retiring: false,
            listener_joined: false,
        }
    }
    fn surface(&self) -> &HttpSurface {
        self.surface.as_ref().expect("owned renderer listener")
    }
    pub(crate) fn local_addr(&self) -> SocketAddrV4 {
        self.surface().local_addr()
    }
    pub(crate) fn try_recv(&self) -> Option<IncomingRequest> {
        if self.retiring {
            None
        } else {
            self.surface().try_recv_incoming()
        }
    }
    fn retire(&mut self, session: &mut Session, now_ns: u64) -> Result<(), EngineError> {
        if self.retiring {
            return Ok(());
        }
        self.retiring = true;
        let retired = session.retire_renderer_epoch(self.epoch, now_ns);
        self.surface().request_shutdown();
        retired
    }
    pub(crate) fn poll(&mut self, session: &mut Session, now_ns: u64) -> Result<(), EngineError> {
        if session.shutdown_requested() {
            self.retire(session, now_ns)?;
        }
        if let Some(stats) = self
            .surface
            .as_mut()
            .expect("owned renderer listener")
            .poll_shutdown()
        {
            self.listener_joined = stats.joined;
            if stats.joined {
                session.set_renderer_listener(None);
            }
            if !self.retiring {
                self.retire(session, now_ns)?;
                if let Some(error) = stats.service_error {
                    eprintln!("renderer listener retired after {error:?}");
                }
            }
        }
        for delivery in session.renderer_deliveries_needing_cancellation(now_ns) {
            self.surface()
                .cancel_delivery(&delivery.binding, delivery.delivery_id);
        }
        while let Some(completion) = self.surface().try_recv_response_completion() {
            let _ = session.acknowledge_renderer_subscription_response(
                &completion.binding,
                completion.success,
                now_ns,
            );
        }
        while let Some(attempt) = self.surface().try_recv_callback_attempt() {
            let authority = session.renderer_delivery_authority(&attempt.delivery, now_ns);
            attempt.authorize(authority, now_ns);
        }
        while let Some(completion) = self.surface().try_recv_delivery_completion() {
            let completed = reconcile_delivery(
                session,
                &completion.delivery,
                completion.outcome,
                completion.transport_joined,
                now_ns,
            );
            // Completion owns the adapter position until Core acknowledges its exact retirement.
            drop(completion);
            completed?;
        }
        for index in (0..self.subscriptions.len()).rev() {
            match session.begin_renderer_delivery(&self.subscriptions[index].binding, now_ns) {
                Ok(delivery) => {
                    if self
                        .surface()
                        .start_delivery(delivery.clone(), now_ns)
                        .is_err()
                        && !self
                            .surface()
                            .cancel_delivery(&delivery.binding, delivery.delivery_id)
                    {
                        session.complete_renderer_delivery(
                            &delivery,
                            RendererDeliveryOutcome::ConnectFailed,
                            true,
                            now_ns,
                        )?;
                    }
                }
                Err(error) if error.code == ErrorCode::AssetInvalid => {
                    self.subscriptions.remove(index);
                }
                Err(_) => {}
            }
        }
        Ok(())
    }
    pub(crate) fn is_idle(&self, session: &Session) -> bool {
        self.listener_joined && session.renderer_subscription_count() == 0
    }
    pub(crate) fn subscription(
        &mut self,
        session: &mut Session,
        pending: PendingSubscription,
        now_ns: u64,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) {
        let admitted = self.subscription_command(session, &pending, now_ns);
        let (command, admission) = match admitted {
            Ok(value) => value,
            Err(status) => {
                pending.respond(Err(status));
                return;
            }
        };
        let result = session.submit_renderer_subscription_with_effects(
            command,
            pending.peer().into(),
            now_ns,
            admission,
            commit,
        );
        if result.receipt.status == ReceiptStatus::Applied {
            if let Some(info) = &result.subscription
                && !self
                    .subscriptions
                    .iter()
                    .any(|handle| handle.binding == info.binding)
            {
                self.subscriptions.push(SubscriptionHandle {
                    binding: info.binding.clone(),
                    service: pending.service(),
                });
            }
            pending.respond(Ok(result.subscription.as_ref()));
        } else {
            pending.respond(Err(result.receipt.error.map_or(500, subscription_fault)));
        }
    }
    fn subscription_command(
        &self,
        session: &Session,
        pending: &PendingSubscription,
        now_ns: u64,
    ) -> Result<(CommandEnvelope, Option<RendererSubscriptionAdmission>), u16> {
        if self.retiring || self.epoch != session.renderer_epoch() || !pending.is_live() {
            return Err(412);
        }
        if let RendererSubscriptionAction::Renew { sid, .. }
        | RendererSubscriptionAction::Unsubscribe { sid } = pending.action()
            && !self
                .subscriptions
                .iter()
                .any(|handle| handle.binding.sid == *sid && handle.service == pending.service())
        {
            return Err(412);
        }
        let controller = session
            .renderer_controller_for_peer(pending.peer().into(), RendererRight::Events, now_ns)
            .map_err(subscription_fault)?;
        let callback_generation = session
            .renderer_callback_generation_for(&controller.id, controller.generation, now_ns)
            .ok_or(412u16)?;
        let admission = if matches!(
            pending.action(),
            RendererSubscriptionAction::Subscribe { .. }
        ) {
            let lease = pending.reserve_lease().map_err(|_| 503u16)?;
            let remaining = pending
                .response_deadline()
                .saturating_duration_since(Instant::now())
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64;
            Some(RendererSubscriptionAdmission {
                lease,
                response_deadline_monotonic_ns: now_ns.saturating_add(remaining),
            })
        } else {
            None
        };
        let command = self.envelope(
            session,
            &pending.request_id().to_string(),
            Command::RendererSubscription {
                controller_id: controller.id,
                grant_generation: controller.generation,
                callback_generation,
                action: pending.action().clone(),
            },
        );
        Ok((command, admission))
    }
    pub(crate) fn next_graph_command(
        &mut self,
        session: &Session,
        now_ns: u64,
    ) -> Option<(CommandEnvelope, Arc<RendererGraphBinding>)> {
        let Some(binding) = session
            .renderer_graph_continuation()
            .filter(|binding| session.renderer_graph_binding_valid(binding, now_ns))
        else {
            self.graph_attempt = None;
            return None;
        };
        if session.network_snapshot().lifecycle
            != Some(earph_core::network_source::NetworkLifecycle::Ready)
        {
            return None;
        }
        if let Some(attempt) = self
            .graph_attempt
            .as_ref()
            .filter(|attempt| attempt.binding == binding)
        {
            if attempt.phase == GraphPhase::Committed {
                return None;
            }
            let ready = session.graph_inspection().prepared?;
            let command = self.envelope(
                session,
                &format!("g{}-commit", binding.attempt_generation),
                Command::GraphCommit {
                    token: ready.token,
                    graph_hash: ready.preview.graph_hash.clone(),
                },
            );
            self.graph_attempt
                .as_mut()
                .expect("current graph attempt")
                .phase = GraphPhase::Committed;
            return Some((command, binding));
        }
        let command = self.envelope(
            session,
            &format!("g{}-prepare", binding.attempt_generation),
            Command::GraphPrepare {
                graph: binding.authorization.intent.graph.clone(),
                processing: binding.authorization.intent.processing,
            },
        );
        self.graph_attempt = Some(GraphAttempt {
            binding: binding.clone(),
            phase: GraphPhase::Preparing,
        });
        Some((command, binding))
    }
    pub(crate) fn admit(
        &self,
        session: &mut Session,
        pending: &PendingAction,
        now_ns: u64,
    ) -> Result<Admission, UpnpFault> {
        if self.retiring || self.epoch != session.renderer_epoch() || !pending.is_live() {
            return Err(UpnpFault::ActionFailed);
        }
        let request = pending.request();
        request.validate_arguments()?;
        let right = match (request.service, request.name()) {
            (_, "SetAVTransportURI") => RendererRight::Stage,
            (Service::RenderingControl, _) => RendererRight::RenderingControl,
            _ => RendererRight::Transport,
        };
        let controller = session
            .renderer_controller_for_peer(pending.peer().into(), right, now_ns)
            .map_err(|error| fault(error, request.service))?;
        if let Some(fault) = request.nonseekable_fault() {
            return Err(fault);
        }
        let renderer = session.renderer_snapshot();
        let state = session.snapshot();
        let unavailable = if request.service == Service::AvTransport {
            UpnpFault::TransitionNotAvailable
        } else {
            UpnpFault::ActionFailed
        };
        let binding = || {
            renderer
                .active
                .as_ref()
                .filter(|owner| {
                    owner.controller_id == controller.id
                        && owner.grant_generation == controller.generation
                        && owner.retirement_request.is_none()
                })
                .map(|owner| owner.binding.clone())
                .ok_or(unavailable)
        };
        let argument = |name| request.argument(name).ok_or(UpnpFault::InvalidArguments);
        let action = match request.name() {
            "SetAVTransportURI" => RendererAction::StageUri {
                expected_staging_generation: {
                    let query = self.envelope(
                        session,
                        &format!("{}-sources", pending.request_id()),
                        Command::SourcesGet {},
                    );
                    let receipt = session.submit(query);
                    match receipt.result {
                        Some(QueryResult::Sources(sources))
                            if receipt.status == ReceiptStatus::Applied =>
                        {
                            sources.generation
                        }
                        _ => {
                            return Err(receipt.error.map_or(UpnpFault::ActionFailed, |error| {
                                fault(error, request.service)
                            }));
                        }
                    }
                },
                uri: argument("CurrentURI")?.to_owned(),
                metadata: argument("CurrentURIMetaData")?.to_owned(),
            },
            "Play" => {
                let staged = renderer
                    .staged
                    .as_ref()
                    .filter(|staged| {
                        staged.controller_id == controller.id
                            && staged.grant_generation == controller.generation
                    })
                    .ok_or(unavailable)?;
                if state.transport == TransportState::Paused
                    && let Ok(active) = binding()
                    && active.media_id == staged.media_id
                {
                    RendererAction::Resume { binding: active }
                } else {
                    RendererAction::Play {
                        media_id: staged.media_id.clone(),
                        staging_generation: staged.generation,
                        grant_generation: session.network_snapshot().grant_generation,
                        device_id: state.output_id.clone().ok_or(unavailable)?,
                    }
                }
            }
            "Stop" => RendererAction::Stop {
                binding: renderer
                    .active
                    .as_ref()
                    .filter(|owner| {
                        owner.controller_id == controller.id
                            && owner.grant_generation == controller.generation
                    })
                    .map(|owner| owner.binding.clone()),
            },
            "Pause" => RendererAction::Pause {
                binding: binding()?,
            },
            "SetMute" => RendererAction::SetMute {
                binding: binding()?,
                muted: matches!(argument("DesiredMute")?, "1" | "true" | "yes"),
            },
            "SetVolume" => RendererAction::SetVolume {
                binding: binding()?,
                volume: argument("DesiredVolume")?
                    .parse()
                    .map_err(|_| UpnpFault::InvalidArguments)?,
            },
            "SetVolumeDB" => RendererAction::SetVolumeDb {
                binding: binding()?,
                volume_db: argument("DesiredVolume")?
                    .parse()
                    .map_err(|_| UpnpFault::InvalidArguments)?,
            },
            "SelectPreset" if argument("PresetName")? == "FactoryDefaults" => {
                RendererAction::FactoryDefaults {
                    binding: binding()?,
                }
            }
            "SelectPreset" => return Err(UpnpFault::InvalidArguments),
            _ => {
                let projection = session.renderer_projection(
                    service(request.service),
                    &controller.id,
                    controller.generation,
                );
                if request.name() == "GetCurrentConnectionInfo"
                    && (!projection.connected || argument("ConnectionID")?.parse::<i32>() != Ok(0))
                {
                    return Err(UpnpFault::InvalidConnection);
                }
                let mut values = Vec::new();
                for &(name, field) in request.output_fields() {
                    let value = match (request.name(), name) {
                        ("GetVolumeDBRange", "MinValue") => {
                            earph_core::remote_volume::MIN_REMOTE_VOLUME_DB.to_string()
                        }
                        ("GetVolumeDBRange", "MaxValue") => "0".to_owned(),
                        _ => {
                            let mut selected = None;
                            projection.fields(|name, value| {
                                if name == field {
                                    selected = Some(value.to_string());
                                }
                            });
                            selected.ok_or(UpnpFault::ActionFailed)?
                        }
                    };
                    values.push((name, value));
                }
                return Ok(Admission::Read(values));
            }
        };
        let mut command = self.envelope(
            session,
            &pending.request_id().to_string(),
            Command::RendererRequest {
                controller_id: controller.id,
                grant_generation: controller.generation,
                action,
            },
        );
        if matches!(
            &command.payload,
            Command::RendererRequest {
                action: RendererAction::Play { .. },
                ..
            }
        ) && renderer.active.as_ref().is_none_or(|owner| {
            Some(&owner.binding.session_id) != state.session_id.as_ref()
                || owner.binding.media_id
                    != renderer.staged.as_ref().expect("Play staging").media_id
        }) {
            command.session_id = Some(
                SessionId::new(format!("{}-{}", self.prefix, pending.request_id()))
                    .map_err(|_| UpnpFault::ActionFailed)?,
            );
        }
        Ok(Admission::Command(Box::new(command)))
    }
    fn envelope(&self, session: &Session, suffix: &str, payload: Command) -> CommandEnvelope {
        let state = session.snapshot();
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(format!("{}-{suffix}", self.prefix))
                .expect("bounded transport request ID"),
            zone_id: state.zone_id,
            owner_id: state.owner_id,
            session_id: state.session_id,
            expected_revision: Some(state.revision),
            payload,
        }
    }
}
fn reconcile_delivery(
    session: &mut Session,
    delivery: &earph_core::renderer_events::RendererDelivery,
    outcome: RendererDeliveryOutcome,
    joined: bool,
    now_ns: u64,
) -> Result<(), EngineError> {
    match session.complete_renderer_delivery(delivery, outcome, joined, now_ns) {
        Err(error)
            if (!joined || outcome == RendererDeliveryOutcome::JoinFailed)
                && error.code == ErrorCode::OutputFault =>
        {
            Ok(())
        }
        completed => completed,
    }
}
impl Drop for RendererRuntime {
    fn drop(&mut self) {
        if let Some(mut surface) = self.surface.take() {
            surface.request_shutdown();
            if surface.poll_shutdown().is_none_or(|stats| !stats.joined) {
                // Preserve the exact runtime and join handle without blocking actor unwinding.
                let _ = Box::leak(Box::new(surface));
                eprintln!("renderer retirement remains owned after actor exit");
            }
        }
    }
}
fn subscription_fault(error: EngineError) -> u16 {
    match error.code {
        ErrorCode::ResourceBudgetExceeded | ErrorCode::SourceBusy => 503,
        ErrorCode::AuthDenied
        | ErrorCode::AuthRequired
        | ErrorCode::AssetInvalid
        | ErrorCode::StaleSession => 412,
        _ => 500,
    }
}

fn service(service: Service) -> RendererService {
    match service {
        Service::AvTransport => RendererService::AvTransport,
        Service::RenderingControl => RendererService::RenderingControl,
        Service::ConnectionManager => RendererService::ConnectionManager,
    }
}
pub(crate) fn receipt_result(receipt: &Receipt, service: Service) -> Result<(), UpnpFault> {
    if matches!(
        receipt.status,
        ReceiptStatus::Applied | ReceiptStatus::Accepted
    ) {
        Ok(())
    } else {
        Err(receipt
            .error
            .map_or(UpnpFault::ActionFailed, |error| fault(error, service)))
    }
}
fn fault(error: EngineError, service: Service) -> UpnpFault {
    match error.code {
        ErrorCode::AuthDenied | ErrorCode::AuthRequired => UpnpFault::ActionNotAuthorized,
        ErrorCode::StaleSession
        | ErrorCode::StaleRevision
        | ErrorCode::SourceBusy
        | ErrorCode::CapabilityUnsupported
            if service == Service::AvTransport =>
        {
            UpnpFault::TransitionNotAvailable
        }
        _ => UpnpFault::ActionFailed,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        control::ControlLimits,
        ids::{OwnerId, RendererControllerId, ZoneId},
        renderer::{RendererControllerGrant, RendererGrantDocument},
        renderer_events::{RendererCallbackGrantDocument, RendererCallbackOrigin},
    };
    use std::{
        io::{Read, Write},
        net::{IpAddr, TcpStream, UdpSocket},
        thread,
        time::Duration,
    };

    #[test]
    fn unjoined_delivery_keeps_core_and_adapter_ownership_until_actual_join() {
        let route = UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let IpAddr::V4(peer) = route.local_addr().unwrap().ip() else {
            unreachable!()
        };
        assert!(peer.is_private());
        let clock = Instant::now();
        let now = || u64::try_from(clock.elapsed().as_nanos()).unwrap();
        let surface = HttpSurface::bind(
            SocketAddrV4::new(peer, 0),
            DeviceIdentity::new(
                "uuid:00000000-0000-0000-0000-000000000005",
                "Retirement fixture",
                "Earph/1 UPnP/1.0 Earph/1",
            )
            .unwrap(),
        )
        .unwrap();
        let address = surface.local_addr();
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("callback-retirement").unwrap(),
            ControlLimits::default(),
        );
        session.enable_renderer_control();
        let mut renderer = RendererRuntime::new(surface, &session);
        let controller = RendererControllerId::new("fixture").unwrap();
        let grant = renderer.envelope(
            &session,
            "grant",
            Command::RendererGrantsReplace {
                expected_generation: 0,
                lifetime_ms: 60000,
                grants: Arc::new(RendererGrantDocument {
                    controllers: vec![RendererControllerGrant {
                        id: controller.clone(),
                        peers: vec![peer.into()],
                        rights: vec![RendererRight::Events],
                        playback: None,
                    }],
                }),
            },
        );
        assert_eq!(session.submit(grant).status, ReceiptStatus::Applied);
        let callback = renderer.envelope(
            &session,
            "callback",
            Command::RendererCallbackGrantsReplace {
                controller_id: controller,
                expected_controller_generation: 1,
                expected_generation: 0,
                lifetime_ms: 60000,
                grants: Arc::new(RendererCallbackGrantDocument {
                    origins: vec![RendererCallbackOrigin {
                        origin: format!("http://{peer}:9"),
                        pins: vec![peer.into()],
                    }],
                }),
            },
        );
        assert_eq!(session.submit(callback).status, ReceiptStatus::Applied);
        let client = thread::spawn(move || {
            let mut stream = TcpStream::connect(address).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            write!(stream,"SUBSCRIBE /upnp/avt/events HTTP/1.1\r\nHost: {address}\r\nNT: upnp:event\r\nCALLBACK: <http://{peer}:9/notify>\r\nTIMEOUT: Second-60\r\nConnection: close\r\n\r\n").unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        });
        let end = Instant::now() + Duration::from_secs(5);
        let pending = loop {
            if let Some(IncomingRequest::Subscription(pending)) = renderer.try_recv() {
                break pending;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        };
        renderer.subscription(&mut session, pending, now(), |_| Ok(()));
        client.join().unwrap();
        let response = loop {
            if let Some(response) = renderer.surface().try_recv_response_completion() {
                break response;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        };
        assert!(response.success);
        session
            .acknowledge_renderer_subscription_response(&response.binding, true, now())
            .unwrap();
        let delivery = session
            .begin_renderer_delivery(&response.binding, now())
            .unwrap();
        renderer
            .surface()
            .start_delivery(delivery.clone(), now())
            .unwrap();
        let attempt = loop {
            if let Some(attempt) = renderer.surface().try_recv_callback_attempt() {
                break attempt;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        };
        // The actual sender task owns this attempt and has not joined.
        let retained = reconcile_delivery(
            &mut session,
            &delivery,
            RendererDeliveryOutcome::JoinFailed,
            false,
            now(),
        );
        assert!(
            retained.is_ok(),
            "uncertain cleanup must keep the serial owner alive: {retained:?}"
        );
        assert_eq!(session.renderer_subscription_count(), 1);
        assert!(renderer.surface().stats().reserved_bytes > 4 * 1024 * 1024);
        attempt.authorize(session.renderer_delivery_authority(&delivery, now()), now());
        let completion = loop {
            if let Some(completion) = renderer.surface().try_recv_delivery_completion() {
                break completion;
            }
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        };
        assert!(completion.transport_joined);
        reconcile_delivery(
            &mut session,
            &completion.delivery,
            completion.outcome,
            completion.transport_joined,
            now(),
        )
        .unwrap();
        drop(completion);
        drop(delivery);
        assert_eq!(session.renderer_subscription_count(), 0);
        assert!(renderer.surface().stats().reserved_bytes < 1024 * 1024);
        let shutdown = renderer.envelope(&session, "shutdown", Command::OwnerShutdown);
        assert_eq!(session.submit(shutdown).status, ReceiptStatus::Applied);
        while !renderer.is_idle(&session) {
            renderer.poll(&mut session, now()).unwrap();
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
    }
}
