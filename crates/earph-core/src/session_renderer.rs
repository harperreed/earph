// ABOUTME: Admits renderer control through the canonical Session and observed ingress peer.
// ABOUTME: Keeps staging separate from exact active and closing source ownership.
use super::*;
use crate::renderer::*;

impl Session {
    /// The peer comes from the owning transport, never a serialized caller assertion.
    pub fn submit_renderer_with_network_effects(
        &mut self,
        command: CommandEnvelope,
        observed_peer: std::net::IpAddr,
        now_ns: u64,
        validate_network: impl FnOnce(&Session, &CommandEnvelope) -> Result<(), EngineError>,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Receipt {
        self.submit_with_ingress(
            command,
            now_ns,
            RendererIngress::Peer(observed_peer),
            |_| Err(STATE),
            validate_network,
            commit,
        )
    }

    pub(super) fn validate_renderer_ingress(
        &self,
        command: &CommandEnvelope,
        observed_peer: Option<std::net::IpAddr>,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        let (controller_id, grant_generation, right) = match &command.payload {
            Command::RendererRequest {
                controller_id,
                grant_generation,
                action,
            } => (controller_id, grant_generation, action.required_right()),
            Command::RendererSubscription {
                controller_id,
                grant_generation,
                ..
            } => (controller_id, grant_generation, RendererRight::Events),
            _ => {
                return if observed_peer.is_none() {
                    Ok(())
                } else {
                    Err(EngineError::new(
                        ErrorCode::AuthDenied,
                        "renderer ingress only accepts scoped requests",
                    ))
                };
            }
        };
        let denied = EngineError::new(
            ErrorCode::AuthDenied,
            "renderer controller authority is unavailable",
        );
        let peer = observed_peer.ok_or(denied)?;
        if !self.renderer.enabled {
            return Err(STATE);
        }
        let authority = self
            .renderer
            .grants
            .iter()
            .find(|g| &g.grant().id == controller_id)
            .ok_or(denied)?;
        if authority.generation != *grant_generation
            || now_ns >= authority.expires_monotonic_ns
            || !authority.grant().peers.contains(&peer)
            || !authority.grant().rights.contains(&right)
        {
            return Err(denied);
        }
        if let Command::RendererSubscription {
            callback_generation,
            ..
        } = &command.payload
        {
            self.renderer_subscription_ingress(
                controller_id,
                *grant_generation,
                *callback_generation,
                now_ns,
            )?;
        }
        Ok(())
    }
    /// Enables Core admission only; hosts must supply qualified trusted ingress separately.
    pub fn enable_renderer_control(&mut self) {
        self.renderer.enabled = true;
        self.renderer.epoch = self.renderer.epoch.max(1);
    }

    pub fn renderer_snapshot(&self) -> RendererSnapshot {
        RendererSnapshot {
            enabled: self.renderer.enabled,
            grant_generation: self.renderer.generation,
            controllers: self
                .renderer
                .grants
                .iter()
                .map(|g| RendererControllerSummary {
                    id: g.grant().id.clone(),
                    generation: g.generation,
                    expires_monotonic_ns: g.expires_monotonic_ns,
                })
                .collect(),
            controller_count: self.renderer.grants.len(),
            staged: self.renderer.staged.as_deref().cloned(),
            active: self.renderer.active.as_deref().cloned(),
            closing: self.renderer.closing.as_deref().cloned(),
            retained_bytes: self.network_retained_bytes().unwrap_or(usize::MAX) as u64,
        }
    }

    pub(super) fn expire_renderer_authority(&mut self, now_ns: u64) -> Result<(), EngineError> {
        self.renderer
            .grants
            .retain(|g| now_ns < g.expires_monotonic_ns);
        if self.renderer.staged.as_ref().is_some_and(|a| {
            !self
                .renderer
                .grants
                .iter()
                .any(|g| g.grant().id == a.controller_id && g.generation == a.grant_generation)
        }) {
            self.renderer.staged = None;
        }
        self.expire_renderer_subscriptions(now_ns);
        self.continue_renderer_retirement()?;
        if self
            .renderer
            .continuation
            .as_ref()
            .is_some_and(|b| !self.renderer_graph_authority_valid(b, now_ns))
        {
            self.renderer.continuation = None;
            self.invalidate_graph_work(EngineError::new(
                ErrorCode::OperationCancelled,
                "renderer graph authority expired",
            ));
        }
        if self.renderer.active.as_ref().is_some_and(|a| {
            a.retirement_request.is_none()
                && self
                    .network
                    .active
                    .as_ref()
                    .is_some_and(|n| n.binding == a.binding)
                && !self.network_authority_valid(&a.binding, now_ns)
        }) {
            self.fail_network_authority(now_ns)?;
        }
        Ok(())
    }

    pub(super) fn apply_renderer_command(
        &mut self,
        c: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        if !self.renderer.enabled {
            return Err(STATE);
        }
        if self.state.owner_id.is_none() || self.state.shutdown_requested {
            return Err(STALE);
        }
        if let Command::RendererRequest {
            controller_id,
            grant_generation,
            action,
        } = &c.payload
        {
            return self.apply_renderer_request(
                c,
                controller_id,
                *grant_generation,
                action,
                now_ns,
            );
        }
        if matches!(
            c.payload,
            Command::RendererCallbackGrantsReplace { .. }
                | Command::RendererCallbackGrantsRevoke { .. }
        ) {
            return self.apply_renderer_callback_command(c, now_ns);
        }
        if matches!(c.payload, Command::RendererSubscription { .. }) {
            return self.apply_renderer_subscription(c, now_ns);
        }
        let revoking = matches!(c.payload, Command::RendererGrantsRevoke { .. });
        let revision = if revoking {
            self.state.revision.saturating_add(1)
        } else {
            increment(self.state.revision)?
        };
        let sequence = if revoking {
            self.state.event_sequence.saturating_add(1)
        } else {
            self.next_session_event_sequence(false)?
        };
        let generation = if revoking {
            self.renderer.generation.saturating_add(1)
        } else {
            increment(self.renderer.generation)?
        };
        let retired;
        match &c.payload {
            Command::RendererGrantsReplace {
                expected_generation,
                grants,
                lifetime_ms,
            } => {
                if *expected_generation != self.renderer.generation {
                    return Err(STALE);
                }
                grants.validate()?;
                for controller in &grants.controllers {
                    if let Some(playback) = &controller.playback {
                        self.validate_renderer_playback(playback)?;
                    }
                }
                if !(1..=crate::network_source::MAX_NETWORK_LIFETIME_MS).contains(lifetime_ms) {
                    return Err(EngineError::new(
                        ErrorCode::AssetInvalid,
                        "invalid renderer grant lifetime",
                    ));
                }
                let expires_monotonic_ns = lifetime_ms
                    .checked_mul(1_000_000)
                    .and_then(|n| now_ns.checked_add(n))
                    .ok_or(crate::network_source::BUDGET)?;
                retired = self.retire_renderer_source(c, None)?;
                self.renderer.grants = (0..grants.controllers.len())
                    .map(|index| {
                        Arc::new(RendererAuthority {
                            generation,
                            expires_monotonic_ns,
                            document: grants.clone(),
                            index,
                        })
                    })
                    .collect();
            }
            Command::RendererGrantsRevoke {
                expected_generation,
                controller_id,
            } => {
                if *expected_generation != self.renderer.generation {
                    return Err(STALE);
                }
                retired = self.retire_renderer_source(c, Some(controller_id))?;
                self.renderer
                    .grants
                    .retain(|g| &g.grant().id != controller_id);
                if self
                    .renderer
                    .staged
                    .as_ref()
                    .is_some_and(|a| &a.controller_id == controller_id)
                {
                    self.renderer.staged = None;
                }
            }
            _ => return Err(STATE),
        }
        self.renderer.generation = generation;
        self.state.revision = revision;
        self.state.event_sequence = sequence;
        self.log(Some(c.request_id.clone()));
        Ok(if retired {
            ReceiptStatus::Accepted
        } else {
            ReceiptStatus::Applied
        })
    }

    pub(super) fn apply_renderer_local_safety(
        &mut self,
        command: &CommandEnvelope,
    ) -> Result<ReceiptStatus, EngineError> {
        let revision = increment(self.state.revision)?;
        let sequence = self.next_session_event_sequence(false)?;
        if !self.retire_renderer_source(command, None)? {
            return Err(STALE);
        }
        if matches!(command.payload, Command::OwnerShutdown) {
            self.state.shutdown_requested = true;
            self.cancel_artifact_work();
        }
        self.cancel_profile_work();
        self.state.revision = revision;
        self.state.event_sequence = sequence;
        self.log(Some(command.request_id.clone()));
        Ok(ReceiptStatus::Accepted)
    }

    fn apply_renderer_request(
        &mut self,
        c: &CommandEnvelope,
        controller_id: &RendererControllerId,
        grant_generation: u64,
        action: &RendererAction,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        match action {
            RendererAction::SetVolume { binding, .. }
            | RendererAction::SetVolumeDb { binding, .. }
            | RendererAction::FactoryDefaults { binding }
            | RendererAction::SetMute { binding, .. }
            | RendererAction::Pause { binding }
            | RendererAction::Resume { binding } => {
                let owner = self.renderer.active.as_ref().ok_or(STALE)?;
                if &owner.controller_id != controller_id
                    || owner.grant_generation != grant_generation
                    || &owner.binding != binding
                    || owner.retirement_request.is_some()
                    || self.network.closing.is_some()
                    || c.session_id != self.state.session_id
                    || !self.network_authority_valid(binding, now_ns)
                {
                    return Err(STALE);
                }
                let mut lowered = c.clone();
                lowered.payload = match action {
                    RendererAction::FactoryDefaults { .. } => Command::VolumeFactoryDefaults,
                    RendererAction::SetMute { muted, .. } => Command::Mute { muted: *muted },
                    RendererAction::Pause { .. } => Command::Pause,
                    RendererAction::Resume { .. } => Command::Resume,
                    _ => {
                        use crate::remote_volume::RemoteVolume;
                        let setting = match action {
                            RendererAction::SetVolume { volume, .. } => {
                                RemoteVolume::from_volume(*volume)?
                            }
                            RendererAction::SetVolumeDb { volume_db, .. } => {
                                RemoteVolume::from_volume_db(*volume_db)?
                            }
                            _ => unreachable!(),
                        };
                        let output = self.state.output.as_ref().ok_or(STATE)?;
                        self.state.volume_policy.validate_software_zero(
                            output.volume_mode,
                            SampleDomain::Pcm,
                            output.fidelity,
                        )?;
                        // Only an already reported finite setting skips lowering after live authority checks.
                        if !self.state.software_gain_zero
                            && setting.volume() != 0
                            && RemoteVolume::from_local(self.state.software_gain_db, false)?
                                == setting
                        {
                            return Ok(ReceiptStatus::Applied);
                        }
                        match setting.millidecibels() {
                            Some(millidecibels) => Command::VolumeSet {
                                millidecibels,
                                ramp_frames: 0,
                            },
                            None => Command::VolumeSilence,
                        }
                    }
                };
                self.apply_command(&lowered, now_ns, |_| Err(STATE))
            }
            RendererAction::StageUri {
                expected_staging_generation,
                uri,
                metadata,
            } => {
                if c.session_id != self.state.session_id
                    || *expected_staging_generation != self.sources.snapshot().generation
                {
                    return Err(STALE);
                }
                if self.renderer.staged.as_ref().is_some_and(|a| {
                    &a.controller_id == controller_id && a.grant_generation == grant_generation
                }) && self
                    .sources
                    .descriptor()
                    .is_some_and(|d| d.uri() == uri && d.metadata() == metadata)
                {
                    return Ok(ReceiptStatus::Applied);
                }
                crate::renderer_events::validate_staged_event(uri, metadata)?;
                let status = self.stage_uri(c, uri, metadata)?;
                self.renderer.staged = self.sources.descriptor().map(|descriptor| {
                    Arc::new(RendererStagedAssociation {
                        controller_id: controller_id.clone(),
                        grant_generation,
                        media_id: descriptor.media_id().clone(),
                        generation: descriptor.generation(),
                    })
                });
                Ok(status)
            }
            RendererAction::Stop { binding: None } => {
                if c.session_id != self.state.session_id
                    || self
                        .renderer
                        .active
                        .iter()
                        .chain(self.renderer.closing.iter())
                        .any(|o| &o.controller_id == controller_id)
                {
                    return Err(STALE);
                }
                Ok(ReceiptStatus::Applied)
            }
            RendererAction::Stop {
                binding: Some(binding),
            } => {
                let owner = self.renderer.active.as_ref().ok_or(STALE)?;
                if &owner.controller_id != controller_id
                    || owner.grant_generation != grant_generation
                    || &owner.binding != binding
                    || self
                        .network
                        .active
                        .as_ref()
                        .is_none_or(|s| &s.binding != binding)
                    || c.session_id != self.state.session_id
                {
                    return Err(STALE);
                }
                if self.network.closing.is_some() {
                    let revision = increment(self.state.revision)?;
                    let sequence = self.next_session_event_sequence(false)?;
                    self.retire_renderer_source(c, Some(controller_id))?;
                    self.state.revision = revision;
                    self.state.event_sequence = sequence;
                    self.log(Some(c.request_id.clone()));
                    return Ok(ReceiptStatus::Accepted);
                }
                self.renderer.continuation = None;
                Arc::make_mut(self.renderer.active.as_mut().ok_or(STALE)?).retirement_request =
                    Some(c.request_id.clone());
                let mut stop = c.clone();
                stop.payload = Command::Stop;
                self.apply_command(&stop, now_ns, |_| Err(STATE))
            }
            RendererAction::Play {
                media_id,
                staging_generation,
                grant_generation: media_grant_generation,
                device_id,
            } => {
                let staged = self.renderer.staged.as_ref().ok_or(STALE)?;
                if &staged.controller_id != controller_id
                    || staged.grant_generation != grant_generation
                    || &staged.media_id != media_id
                    || staged.generation != *staging_generation
                {
                    return Err(STALE);
                }
                let authorization = self
                    .renderer
                    .grants
                    .iter()
                    .find(|g| &g.grant().id == controller_id)
                    .and_then(|g| g.grant().playback.clone())
                    .ok_or(EngineError::new(
                        ErrorCode::AuthDenied,
                        "renderer playback requires local graph authorization",
                    ))?;
                self.validate_renderer_playback(&authorization)?;
                if &authorization.output.device_id != device_id {
                    return Err(STALE);
                }
                if self.renderer.active.as_ref().is_some_and(|owner| {
                    &owner.controller_id == controller_id
                        && owner.grant_generation == grant_generation
                        && &owner.binding.media_id == media_id
                        && owner.binding.grant_generation == *media_grant_generation
                        && c.session_id == self.state.session_id
                        && self
                            .state
                            .output
                            .as_ref()
                            .is_some_and(|o| &o.device_id == device_id)
                        && self.network_authority_valid(&owner.binding, now_ns)
                }) && matches!(
                    self.state.transport,
                    TransportState::Preparing | TransportState::Buffering | TransportState::Playing
                ) {
                    if self.renderer.continuation.is_none() {
                        if !self.state.muted {
                            return Err(EngineError::new(
                                ErrorCode::SafetyPolicyBlocked,
                                "renderer graph retry requires muted output",
                            ));
                        }
                        self.capture_renderer_graph_pin(
                            controller_id,
                            grant_generation,
                            authorization,
                        )?;
                    }
                    return Ok(ReceiptStatus::Applied);
                }
                let start = self.renderer_network_start(c).ok_or(STATE)?;
                let status = self.apply_network_command(&start, now_ns)?;
                let expiry = self
                    .renderer
                    .grants
                    .iter()
                    .find(|g| &g.grant().id == controller_id)
                    .ok_or(STALE)?
                    .expires_monotonic_ns;
                let context = Arc::make_mut(self.network.active.as_mut().ok_or(STALE)?);
                context.expires_monotonic_ns = context.expires_monotonic_ns.min(expiry);
                self.capture_renderer_graph_pin(controller_id, grant_generation, authorization)?;
                self.renderer.active = Some(Arc::new(RendererSourceOwnership {
                    controller_id: controller_id.clone(),
                    grant_generation,
                    binding: self.network.active.as_ref().ok_or(STALE)?.binding.clone(),
                    retirement_request: None,
                }));
                Ok(status)
            }
        }
    }

    pub(super) fn renderer_network_start(
        &self,
        command: &CommandEnvelope,
    ) -> Option<CommandEnvelope> {
        let Command::RendererRequest {
            controller_id,
            action:
                RendererAction::Play {
                    media_id,
                    staging_generation,
                    grant_generation,
                    device_id,
                },
            ..
        } = &command.payload
        else {
            return None;
        };
        let authority = self
            .renderer
            .grants
            .iter()
            .find(|g| &g.grant().id == controller_id)?;
        let mut start = command.clone();
        start.payload = Command::NetworkStartHttpFlac {
            media_id: media_id.clone(),
            staging_generation: *staging_generation,
            grant_generation: *grant_generation,
            device_id: device_id.clone(),
            takeover: authority.grant().rights.contains(&RendererRight::Takeover),
        };
        Some(start)
    }

    pub(super) fn retire_renderer_source(
        &mut self,
        command: &CommandEnvelope,
        controller: Option<&RendererControllerId>,
    ) -> Result<bool, EngineError> {
        let Some(owner) = self.renderer.active.as_ref() else {
            return Ok(false);
        };
        if controller.is_some_and(|id| id != &owner.controller_id)
            || self
                .network
                .active
                .as_ref()
                .is_none_or(|s| s.binding != owner.binding)
        {
            return Ok(false);
        }
        if owner
            .retirement_request
            .as_ref()
            .is_some_and(|id| id != &command.request_id)
            && !matches!(
                command.payload,
                Command::Stop
                    | Command::OwnerShutdown
                    | Command::NetworkGrantsReplace { .. }
                    | Command::NetworkGrantsRevoke { .. }
                    | Command::RendererGrantsReplace { .. }
                    | Command::RendererGrantsRevoke { .. }
            )
        {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "renderer retirement already has an owner",
            ));
        }
        self.renderer.continuation = None;
        Arc::make_mut(self.renderer.active.as_mut().ok_or(STALE)?).retirement_request =
            Some(command.request_id.clone());
        if self.network.closing.is_some() {
            let epochs = self.state.epochs.next_stream()?;
            self.finish_pending(
                ReceiptStatus::Cancelled,
                Some(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "renderer source stopping",
                )),
                None,
            );
            self.invalidate_graph_work(EngineError::new(
                ErrorCode::OperationCancelled,
                "renderer source stopping",
            ));
            self.graph.active = None;
            self.state.epochs = epochs;
            self.state.muted = true;
            self.state.transport = TransportState::Paused;
            self.state.operation = Some(command.request_id.clone());
            self.playback_intent = PlaybackIntent::Paused;
            return Ok(true);
        }
        self.retire_network_command(command, None)?;
        Ok(true)
    }

    pub(super) fn renderer_source_executable(
        &self,
        binding: &crate::network_source::NetworkSourceBinding,
        now_ns: u64,
    ) -> bool {
        if let Some(owner) = self
            .renderer
            .active
            .as_ref()
            .filter(|o| &o.binding == binding)
        {
            return self.renderer.enabled
                && owner.retirement_request.is_none()
                && self.renderer.grants.iter().any(|g| {
                    g.grant().id == owner.controller_id
                        && g.generation == owner.grant_generation
                        && now_ns < g.expires_monotonic_ns
                });
        }
        !self
            .renderer
            .closing
            .as_ref()
            .is_some_and(|o| &o.binding == binding)
    }

    pub(super) fn renderer_owns_active_source(
        &self,
        binding: &crate::network_source::NetworkSourceBinding,
    ) -> bool {
        self.renderer
            .active
            .as_ref()
            .is_some_and(|owner| &owner.binding == binding)
    }

    fn continue_renderer_retirement(&mut self) -> Result<(), EngineError> {
        if self.network.closing.is_some() {
            return Ok(());
        }
        let Some(owner) = &self.renderer.active else {
            return Ok(());
        };
        let Some(request) = &owner.retirement_request else {
            return Ok(());
        };
        let record = self
            .transport_record
            .as_ref()
            .filter(|r| &r.0.request_id == request)
            .cloned()
            .ok_or(STALE)?;
        if self
            .network
            .active
            .as_ref()
            .is_none_or(|s| s.binding != owner.binding)
        {
            return Err(STALE);
        }
        self.retire_network_command(&record.0, None)
    }
}

impl Session {
    pub(super) fn validate_renderer_playback(
        &self,
        authorization: &RendererPlaybackAuthorization,
    ) -> Result<(), EngineError> {
        authorization.shallow_bytes()?;
        authorization.output.validate()?;
        if self.state.output.as_ref() != Some(&authorization.output)
            || self.state.device_epoch != authorization.expected_device_epoch
            || !self.state.output_consented
        {
            return Err(EngineError::new(
                ErrorCode::SafetyPolicyBlocked,
                "renderer playback output requires current local consent",
            ));
        }
        if !matches!(
            authorization.intent.processing,
            crate::graph::GraphProcessingRequest::Float64 { .. }
        ) || authorization.output.fidelity == FidelityPolicy::StrictPreservation
        {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "renderer HTTP playback requires authorized floating processing",
            ));
        }
        authorization.authored_sha256()?;
        Ok(())
    }
    pub fn renderer_graph_continuation(&self) -> Option<Arc<RendererGraphBinding>> {
        self.renderer.continuation.clone()
    }
    pub fn renderer_graph_binding_valid(
        &self,
        binding: &RendererGraphBinding,
        now_ns: u64,
    ) -> bool {
        self.network.closing.is_none() && self.renderer_graph_authority_valid(binding, now_ns)
    }
    fn renderer_graph_authority_valid(&self, binding: &RendererGraphBinding, now_ns: u64) -> bool {
        self.renderer.enabled
            && !self.state.shutdown_requested
            && self.state.owner_id.as_ref() == Some(&binding.owner_id)
            && self.renderer.epoch == binding.renderer_epoch
            && self.renderer.continuation.as_deref() == Some(binding)
            && self
                .validate_renderer_playback(&binding.authorization)
                .is_ok()
            && binding.authorization.authored_sha256().ok() == Some(binding.authored_sha256)
            && self.renderer.grants.iter().any(|g| {
                g.grant().id == binding.controller_id
                    && g.generation == binding.controller_generation
                    && now_ns < g.expires_monotonic_ns
                    && g.grant().playback.as_ref() == Some(&binding.authorization)
            })
            && self
                .renderer
                .active
                .as_ref()
                .is_some_and(|o| o.binding == binding.source && o.retirement_request.is_none())
            && self.network.active.as_ref().is_some_and(|source| {
                source.binding == binding.source && source.source_epochs == binding.source_epochs
            })
            && self.network_authority_valid(&binding.source, now_ns)
    }
    pub fn submit_renderer_graph_with_effects(
        &mut self,
        command: CommandEnvelope,
        binding: Arc<RendererGraphBinding>,
        now_ns: u64,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Receipt {
        self.submit_with_ingress(
            command,
            now_ns,
            RendererIngress::Graph(binding),
            |_| Err(STATE),
            |_, _| Ok(()),
            commit,
        )
    }
    pub(super) fn validate_renderer_graph_submission(
        &self,
        command: &CommandEnvelope,
        binding: &RendererGraphBinding,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if !self.renderer_graph_binding_valid(binding, now_ns) {
            return Err(STALE);
        }
        match &command.payload {
            Command::GraphPrepare { graph, processing }
                if graph == &binding.authorization.intent.graph
                    && processing == &binding.authorization.intent.processing =>
            {
                Ok(())
            }
            Command::GraphCommit { .. } => Ok(()),
            _ => Err(STALE),
        }
    }
    pub(super) fn validate_renderer_graph_phase(
        &self,
        command: &CommandEnvelope,
        binding: &RendererGraphBinding,
    ) -> Result<(), EngineError> {
        match &command.payload {
            Command::GraphPrepare { .. } => {
                if self.state.network_playback.as_ref().is_none_or(|p| {
                    p.binding != binding.source
                        || p.lifecycle != crate::network_source::NetworkLifecycle::Ready
                }) {
                    return Err(STATE);
                }
                Ok(())
            }
            Command::GraphCommit { .. }
                if self.renderer_ready_graph_binding().as_deref() == Some(binding) =>
            {
                Ok(())
            }
            _ => Err(STALE),
        }
    }
}

impl Session {
    fn capture_renderer_graph_pin(
        &mut self,
        controller: &RendererControllerId,
        generation: u64,
        authorization: Arc<RendererPlaybackAuthorization>,
    ) -> Result<(), EngineError> {
        let source = self.network.active.as_ref().ok_or(STALE)?;
        let serial = increment(self.renderer.graph_serial)?;
        self.renderer.continuation = Some(Arc::new(RendererGraphBinding {
            owner_id: source.binding.owner_id.clone(),
            renderer_epoch: self.renderer.epoch,
            attempt_generation: serial,
            controller_id: controller.clone(),
            controller_generation: generation,
            source: source.binding.clone(),
            source_epochs: source.source_epochs,
            authored_sha256: authorization.authored_sha256()?,
            authorization,
        }));
        self.renderer.graph_serial = serial;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_controller_storage_still_charges_its_reserved_slots() {
        let mut s = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
        s.renderer.grants = Vec::with_capacity(8);
        assert_eq!(
            s.network_retained_bytes().unwrap(),
            8 * size_of::<Arc<RendererAuthority>>()
        );
    }

    #[test]
    fn renderer_revocation_removes_authority_at_exhausted_counters() {
        let mut s = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owner").unwrap(),
            ControlLimits::default(),
        );
        s.enable_renderer_control();
        let document = Arc::new(RendererGrantDocument {
            controllers: vec![RendererControllerGrant {
                playback: None,
                id: RendererControllerId::new("controller-a").unwrap(),
                peers: vec!["192.168.1.2".parse().unwrap()],
                rights: vec![RendererRight::Stage],
            }],
        });
        s.renderer.grants.push(Arc::new(RendererAuthority {
            generation: 1,
            expires_monotonic_ns: 1_000_000,
            document,
            index: 0,
        }));
        s.renderer.generation = u64::MAX;
        s.state.revision = u64::MAX;
        let receipt = s.submit(CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new("revoke").unwrap(),
            zone_id: s.state.zone_id.clone(),
            owner_id: s.state.owner_id.clone(),
            session_id: None,
            expected_revision: Some(u64::MAX),
            payload: Command::RendererGrantsRevoke {
                expected_generation: u64::MAX,
                controller_id: RendererControllerId::new("controller-a").unwrap(),
            },
        });
        assert_eq!(receipt.status, ReceiptStatus::Applied);
        assert!(s.renderer.grants.is_empty());
        assert_eq!(s.renderer.generation, u64::MAX);
        assert_eq!(s.state.revision, u64::MAX);
    }
}
