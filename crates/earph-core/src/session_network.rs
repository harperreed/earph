// ABOUTME: Applies network source authority through checked Session transactions.
// ABOUTME: Retains exact cleanup ownership after revocation without retaining admission.
use super::*;
use crate::network_source::*;
use std::collections::BTreeSet;

impl Session {
    pub fn enable_network_playback(&mut self) {
        self.network.enabled = true;
    }
    pub fn staged_uri_descriptor(
        &self,
        media_id: &StagedMediaId,
        generation: u64,
    ) -> Result<Arc<crate::source::StagedUriDescriptor>, EngineError> {
        let descriptor = self.sources.descriptor().ok_or(STALE)?;
        if descriptor.media_id() != media_id || descriptor.generation() != generation {
            return Err(STALE);
        }
        Ok(descriptor)
    }
    pub fn network_source(&self) -> Option<Arc<NetworkSourceContext>> {
        self.network
            .active
            .as_ref()
            .filter(|s| {
                !self.renderer_owns_active_source(&s.binding)
                    || self.network_authority_valid(&s.binding, self.network.admission_ns)
            })
            .cloned()
    }
    pub fn network_grants(&self) -> Option<(u64, u64, Arc<NetworkGrantDocument>)> {
        self.network
            .grants
            .as_ref()
            .map(|(expiry, grants)| (self.network.generation, *expiry, grants.clone()))
    }
    pub fn closing_network_source(&self) -> Option<NetworkClosingSource> {
        self.network.closing.clone()
    }
    pub fn network_snapshot(&self) -> NetworkSnapshot {
        NetworkSnapshot {
            enabled: self.network.enabled,
            grant_generation: self.network.generation,
            expires_monotonic_ns: self.network.grants.as_ref().map(|g| g.0),
            origin_count: self
                .network
                .grants
                .as_ref()
                .map_or(0, |g| g.1.origins.len()),
            source_generation: self.network.source_generation,
            retained_bytes: self.network_retained_bytes().unwrap_or(usize::MAX) as u64,
            lifecycle: if self.network.active.as_ref().is_some_and(|s| {
                self.renderer_owns_active_source(&s.binding)
                    && !self.network_authority_valid(&s.binding, self.network.admission_ns)
            }) {
                Some(NetworkLifecycle::Retiring)
            } else if self.network.active.is_some() {
                Some(match self.state.transport {
                    _ if self.state.network_playback.is_none() => NetworkLifecycle::Preparing,
                    TransportState::Preparing => NetworkLifecycle::Preparing,
                    TransportState::Playing => NetworkLifecycle::Playing,
                    TransportState::Paused => NetworkLifecycle::Paused,
                    _ => self
                        .state
                        .network_playback
                        .as_ref()
                        .map_or(NetworkLifecycle::Preparing, |p| p.lifecycle),
                })
            } else if self.network.closing.is_some() {
                Some(NetworkLifecycle::Retiring)
            } else {
                self.state.network_playback.as_ref().map(|p| p.lifecycle)
            },
            active: self.network.active.as_ref().map(|s| s.binding.clone()),
            closing: self
                .network
                .closing
                .as_ref()
                .map(|s| s.context.binding.clone()),
            seek: false,
            profiles: false,
            strict_preservation: false,
        }
    }
    pub fn network_authority_valid(&self, binding: &NetworkSourceBinding, now_ns: u64) -> bool {
        self.network.active.as_ref().is_some_and(|s| {
            &s.binding == binding
                && self.renderer_source_executable(binding, now_ns.max(self.network.admission_ns))
                && self.state.owner_id.as_ref() == Some(&s.binding.owner_id)
                && self.state.session_id.as_ref() == Some(&binding.session_id)
                && self.state.source == Some(SourceKind::HttpFlac)
                && self.state.output_consented
                && !self.state.shutdown_requested
                && self.network.generation == binding.grant_generation
                && now_ns.max(self.network.admission_ns) < s.expires_monotonic_ns
                && self
                    .network
                    .grants
                    .as_ref()
                    .is_some_and(|g| Arc::ptr_eq(&g.1, &s.grants))
        })
    }
    pub(super) fn validate_network_control(
        &self,
        command: &Command,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if matches!(
            command,
            Command::ProfilesPrepare { .. }
                | Command::ProfilesApply { .. }
                | Command::ProfilesBind { .. }
                | Command::Seek { .. }
        ) {
            return Err(STATE);
        }
        if !matches!(
            command,
            Command::Stop | Command::OwnerShutdown | Command::Mute { muted: true }
        ) && self
            .network
            .active
            .as_ref()
            .is_none_or(|s| !self.network_authority_valid(&s.binding, now_ns))
        {
            return Err(STATE);
        }
        Ok(())
    }
    pub(super) fn close_network_at(
        &mut self,
        epochs: Epochs,
        reason: Option<ErrorCode>,
    ) -> Result<(), EngineError> {
        if self.network.active.is_none() {
            return Ok(());
        }
        if self.network.closing.is_some() {
            // A takeover cannot publish readiness until the outgoing worker retires.
            // Its unprepared replacement owns no native or transport resources yet.
            if self.state.network_playback.is_some() {
                return Err(EngineError::new(
                    ErrorCode::SourceBusy,
                    "network retirement still owns the worker",
                ));
            }
            self.network.active = None;
            return Ok(());
        }
        let active = self.network.active.take().ok_or(STALE)?;
        self.network.closing_failure_observed = false;
        self.network.closing_frontier = Some(self.state.epochs);
        self.network.closing_activation =
            self.graph_activation().map(|a| NetworkActivationBounds {
                request_id: a.request_id,
                previous_output_epochs: a.previous_binding.epochs,
                activation_output_epochs: a.epochs,
                graph_epoch: self.state.graph_epoch,
                device_epoch: a.previous_binding.device_epoch,
            });
        self.network.closing = Some(NetworkClosingSource {
            context: active,
            output_epochs: epochs,
            graph_epoch: self.state.graph_epoch,
            device_epoch: self.state.device_epoch,
            reason,
        });
        if self.renderer.active.as_ref().is_some_and(|o| {
            self.network
                .closing
                .as_ref()
                .is_some_and(|c| o.binding == c.context.binding)
        }) {
            self.renderer.closing = self.renderer.active.take();
        }
        if let Some(p) = &mut self.state.network_playback {
            p.lifecycle = if reason.is_some() {
                NetworkLifecycle::Failed
            } else {
                NetworkLifecycle::Retiring
            };
        }
        Ok(())
    }
    pub fn expire_network_authority(&mut self, now_ns: u64) -> Result<(), EngineError> {
        if now_ns < self.network.admission_ns {
            return Err(crate::network_source::INVALID);
        }
        self.network.admission_ns = now_ns;
        self.expire_renderer_authority(now_ns)?;
        if self.network.grants.as_ref().is_none_or(|g| now_ns < g.0) {
            return Ok(());
        }
        // Removing authority is unconditional, including at counter exhaustion.
        self.network.grants = None;
        self.fail_network_authority(now_ns)
    }

    pub(super) fn fail_network_authority(&mut self, now_ns: u64) -> Result<(), EngineError> {
        let renderer_deferred = self.network.closing.is_some()
            && self
                .network
                .active
                .as_ref()
                .is_some_and(|source| self.renderer_owns_active_source(&source.binding));
        if renderer_deferred
            && (self
                .renderer
                .active
                .as_ref()
                .is_some_and(|owner| owner.retirement_request.is_some())
                || self.state.transport == TransportState::Failed
                    && self.state.error.is_some()
                    && self.state.operation.is_none())
        {
            return Ok(());
        }
        if self.network.active.is_some() {
            self.state.muted = true;
            self.state.transport = TransportState::Failed;
            self.state.error.get_or_insert(ErrorCode::AuthDenied);
            let epochs = self.state.epochs.next_stream().unwrap_or(self.state.epochs);
            if !renderer_deferred {
                self.close_network_at(self.state.epochs, self.state.error)?;
            }
            self.state.epochs = epochs;
            self.invalidate_graph_work(EngineError::new(
                ErrorCode::AuthDenied,
                "network authority expired",
            ));
            self.graph.active = None;
            self.state.revision = self.state.revision.saturating_add(1);
            self.state.event_sequence = self.state.event_sequence.saturating_add(1);
            self.state.monotonic_ns = now_ns.max(self.state.monotonic_ns);
            self.finish_pending(
                ReceiptStatus::Failed,
                Some(EngineError::new(
                    ErrorCode::AuthDenied,
                    "network authority expired",
                )),
                None,
            );
            self.log(None);
        }
        Ok(())
    }
    pub(super) fn apply_network_command(
        &mut self,
        c: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        if !self.network.enabled || !self.local_playback_enabled {
            return Err(STATE);
        }
        if self.state.owner_id.is_none() || self.state.shutdown_requested {
            return Err(STALE);
        }
        let revision = increment(self.state.revision)?;
        let sequence = self.next_session_event_sequence(false)?;
        let status = match &c.payload {
            Command::NetworkGrantsReplace {
                expected_generation,
                grants,
                lifetime_ms,
            } => {
                if *expected_generation != self.network.generation {
                    return Err(STALE);
                }
                grants.validate()?;
                if !(1..=MAX_NETWORK_LIFETIME_MS).contains(lifetime_ms) {
                    return Err(crate::network_source::INVALID);
                }
                let expiry = lifetime_ms
                    .checked_mul(1_000_000)
                    .and_then(|n| now_ns.checked_add(n))
                    .ok_or(crate::network_source::BUDGET)?;
                let generation = increment(self.network.generation)?;
                let retired = self.network.active.is_some();
                if retired {
                    self.retire_network_command(c, None)?;
                }
                self.network.generation = generation;
                self.network.grants = Some((expiry, grants.clone()));
                if retired {
                    ReceiptStatus::Accepted
                } else {
                    ReceiptStatus::Applied
                }
            }
            Command::NetworkGrantsRevoke {
                expected_generation,
            } => {
                if *expected_generation != self.network.generation {
                    return Err(STALE);
                }
                let generation = increment(self.network.generation)?;
                let retired = self.network.active.is_some();
                if retired {
                    self.retire_network_command(c, None)?;
                }
                self.network.generation = generation;
                self.network.grants = None;
                if retired {
                    ReceiptStatus::Accepted
                } else {
                    ReceiptStatus::Applied
                }
            }
            Command::NetworkStartHttpFlac {
                media_id,
                staging_generation,
                grant_generation,
                device_id,
                takeover,
            } => {
                if self.network.closing.is_some() || self.artifact_worker_busy() {
                    return Err(EngineError::new(
                        ErrorCode::SourceBusy,
                        "network preparation or retirement is busy",
                    ));
                }
                let session_id = c.session_id.clone().ok_or(STALE)?;
                if self.state.session_id.as_ref() == Some(&session_id) {
                    return Err(STALE);
                }
                if self.state.session_id.is_some() && !takeover {
                    return Err(EngineError::new(
                        ErrorCode::SourceBusy,
                        "explicit takeover required",
                    ));
                }
                let descriptor = self.staged_uri_descriptor(media_id, *staging_generation)?;
                let (expiry, grants) = self.network.grants.as_ref().ok_or(EngineError::new(
                    ErrorCode::AuthDenied,
                    "network grants are required",
                ))?;
                if *grant_generation != self.network.generation || now_ns >= *expiry {
                    return Err(EngineError::new(
                        ErrorCode::AuthDenied,
                        "network grants changed or expired",
                    ));
                }
                let output = self.state.output.as_ref().ok_or(STATE)?;
                if &output.device_id != device_id {
                    return Err(STALE);
                }
                if !self.state.output_consented {
                    return Err(EngineError::new(
                        ErrorCode::SafetyPolicyBlocked,
                        "approve the current output before preparing network playback",
                    ));
                }
                if output.fidelity == FidelityPolicy::StrictPreservation {
                    return Err(EngineError::new(
                        ErrorCode::FidelityPolicyConflict,
                        "network playback requires flexible PCM fidelity",
                    ));
                }
                let generation = increment(self.network.source_generation)?;
                let epochs = self.state.epochs.next_session()?;
                let context = Arc::new(NetworkSourceContext {
                    binding: NetworkSourceBinding {
                        owner_id: self.state.owner_id.clone().ok_or(STALE)?,
                        session_id: session_id.clone(),
                        media_id: media_id.clone(),
                        source_generation: generation,
                        grant_generation: *grant_generation,
                    },
                    source_epochs: epochs,
                    descriptor,
                    grants: grants.clone(),
                    expires_monotonic_ns: *expiry,
                });
                self.close_network_at(self.state.epochs, None)?;
                self.finish_pending(
                    ReceiptStatus::Cancelled,
                    Some(EngineError::new(
                        ErrorCode::OperationCancelled,
                        "source superseded",
                    )),
                    None,
                );
                self.invalidate_graph_work(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "source superseded",
                ));
                self.graph.active = None;
                self.cancel_profile_work();
                self.network.source_generation = generation;
                self.network.active = Some(context);
                self.state.session_id = Some(session_id);
                self.state.source = Some(SourceKind::HttpFlac);
                self.state.epochs = epochs;
                self.state.transport = TransportState::Preparing;
                self.state.muted = true;
                self.state.seekable = false;
                self.state.active_entry_id = None;
                self.state.reserved_next_entry_id = None;
                self.state.playback = None;
                self.state.network_playback = None;
                self.state.audible_position_frames = None;
                self.state.decoded_position_frames = 0;
                self.state.error = None;
                self.state.operation = Some(c.request_id.clone());
                self.playback_intent = PlaybackIntent::Playing;
                ReceiptStatus::Accepted
            }
            _ => return Err(STATE),
        };
        self.state.revision = revision;
        self.state.event_sequence = sequence;
        self.log(Some(c.request_id.clone()));
        Ok(status)
    }
    pub(super) fn retire_network_command(
        &mut self,
        c: &CommandEnvelope,
        reason: Option<ErrorCode>,
    ) -> Result<(), EngineError> {
        if self
            .network
            .active
            .as_ref()
            .is_some_and(|source| self.renderer_owns_active_source(&source.binding))
        {
            if self.network.closing.is_some() {
                if !self.retire_renderer_source(c, None)? {
                    return Err(STALE);
                }
                return Ok(());
            }
            Arc::make_mut(self.renderer.active.as_mut().ok_or(STALE)?).retirement_request =
                Some(c.request_id.clone());
        }
        let epochs = self.state.epochs.next_stream()?;
        self.close_network_at(self.state.epochs, reason)?;
        if self.state.operation.as_ref() != Some(&c.request_id) {
            self.finish_pending(
                ReceiptStatus::Cancelled,
                Some(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "network authority revoked",
                )),
                None,
            );
        }
        self.invalidate_graph_work(EngineError::new(
            ErrorCode::OperationCancelled,
            "network authority revoked",
        ));
        self.graph.active = None;
        self.state.epochs = epochs;
        self.state.muted = true;
        self.state.transport = TransportState::Paused;
        self.state.operation = Some(c.request_id.clone());
        self.playback_intent = PlaybackIntent::Paused;
        Ok(())
    }
    pub(super) fn retain_network_rejection(
        &mut self,
        command: CommandEnvelope,
        receipt: Receipt,
    ) -> Receipt {
        if self.validate_network_retention(self, &command).is_ok() {
            if self.receipts.len() == self.limits.receipts {
                self.receipts.pop_front();
            }
            self.receipts
                .push_back(Arc::new((command, receipt.clone())));
        }
        receipt
    }
    pub fn network_retained_bytes(&self) -> Result<usize, EngineError> {
        let mut counter = AuthorityBytes::default();
        counter.session(self)?;
        Ok(counter.bytes)
    }
    pub(super) fn validate_network_retention(
        &self,
        candidate: &Session,
        command: &CommandEnvelope,
    ) -> Result<(), EngineError> {
        let mut count = AuthorityBytes::default();
        count.session(self)?;
        count.session(candidate)?;
        count.command(command)?;
        // Retained ordinary failures cannot consume the safety receipt reserve.
        let ceiling = if command.payload.is_query()
            || matches!(
                command.payload,
                Command::NetworkGrantsRevoke { .. }
                    | Command::RendererGrantsRevoke { .. }
                    | Command::RendererRequest {
                        action: crate::renderer::RendererAction::Stop { .. },
                        ..
                    }
                    | Command::Stop
                    | Command::OwnerShutdown
                    | Command::Mute { muted: true }
            ) {
            MAX_NETWORK_AUTHORITY_BYTES
        } else {
            let receipt_reserve = self
                .limits
                .receipts
                .checked_mul(
                    size_of::<(CommandEnvelope, Receipt)>()
                        + 2 * size_of::<usize>()
                        + 13 * 128
                        + crate::renderer::MAX_RENDERER_CONTROLLERS
                            * (size_of::<crate::renderer::RendererControllerSummary>() + 128)
                        + 4 * 128,
                )
                .ok_or(crate::network_source::BUDGET)?;
            MAX_NETWORK_AUTHORITY_BYTES.saturating_sub(receipt_reserve)
        };
        if count.bytes > ceiling {
            return Err(crate::network_source::BUDGET);
        }
        if count.bytes as u64
            + candidate.profile_storage_bytes()
            + candidate
                .profile_environment_bytes()?
                .saturating_sub(candidate.network_retained_bytes()? as u64)
            > 256 * 1024 * 1024
        {
            return Err(crate::network_source::BUDGET);
        }
        Ok(())
    }
}
#[derive(Default)]
struct AuthorityBytes {
    bytes: usize,
    records: BTreeSet<usize>,
    documents: BTreeSet<usize>,
    descriptors: BTreeSet<usize>,
    contexts: BTreeSet<usize>,
    renderer_authorities: BTreeSet<usize>,
    renderer_records: BTreeSet<usize>,
    renderer_vectors: BTreeSet<usize>,
}
impl AuthorityBytes {
    fn renderer_document(
        &mut self,
        document: &Arc<crate::renderer::RendererGrantDocument>,
    ) -> Result<(), EngineError> {
        if self.documents.insert(Arc::as_ptr(document) as usize) {
            self.add(document.retained_bytes()?)?;
        }
        Ok(())
    }
    fn add(&mut self, bytes: usize) -> Result<(), EngineError> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(crate::network_source::BUDGET)?;
        Ok(())
    }
    fn document(&mut self, document: &Arc<NetworkGrantDocument>) -> Result<(), EngineError> {
        if self.documents.insert(Arc::as_ptr(document) as usize) {
            self.add(document.retained_bytes()?)?;
        }
        Ok(())
    }
    fn descriptor(
        &mut self,
        descriptor: &Arc<crate::source::StagedUriDescriptor>,
    ) -> Result<(), EngineError> {
        if self.descriptors.insert(Arc::as_ptr(descriptor) as usize) {
            self.add(descriptor.retained_bytes())?;
        }
        Ok(())
    }
    fn context(&mut self, context: &Arc<NetworkSourceContext>) -> Result<(), EngineError> {
        if self.contexts.insert(Arc::as_ptr(context) as usize) {
            self.add(size_of::<NetworkSourceContext>() + 16 + 512)?;
        }
        self.document(&context.grants)?;
        self.descriptor(&context.descriptor)
    }
    fn command(&mut self, command: &CommandEnvelope) -> Result<(), EngineError> {
        let mut bytes = command.network_capacity_bytes()?;
        if let Command::NetworkGrantsReplace { grants, .. } = &command.payload {
            bytes -= grants.retained_bytes()?;
            self.document(grants)?;
        }
        if let Command::RendererGrantsReplace { grants, .. } = &command.payload {
            bytes -= grants.retained_bytes()?;
            self.renderer_document(grants)?;
        }
        self.add(bytes)
    }
    fn session(&mut self, session: &Session) -> Result<(), EngineError> {
        if session.renderer.grants.capacity() != 0
            && self
                .renderer_vectors
                .insert(session.renderer.grants.as_ptr() as usize)
        {
            self.add(
                session.renderer.grants.capacity()
                    * size_of::<Arc<crate::renderer::RendererAuthority>>(),
            )?;
        }
        if let Some(association) = &session.renderer.staged
            && self
                .renderer_records
                .insert(Arc::as_ptr(association) as usize)
        {
            self.add(
                size_of::<crate::renderer::RendererStagedAssociation>()
                    + 16
                    + association.controller_id.capacity_bytes()
                    + association.media_id.capacity_bytes(),
            )?;
        }
        for owner in session
            .renderer
            .active
            .iter()
            .chain(session.renderer.closing.iter())
        {
            if self.renderer_records.insert(Arc::as_ptr(owner) as usize) {
                self.add(
                    size_of::<crate::renderer::RendererSourceOwnership>()
                        + 16
                        + owner.controller_id.capacity_bytes()
                        + owner.binding.owner_id.capacity_bytes()
                        + owner.binding.session_id.capacity_bytes()
                        + owner.binding.media_id.capacity_bytes()
                        + owner
                            .retirement_request
                            .as_ref()
                            .map_or(0, RequestId::capacity_bytes),
                )?;
            }
        }
        for authority in &session.renderer.grants {
            if self
                .renderer_authorities
                .insert(Arc::as_ptr(authority) as usize)
            {
                self.add(size_of::<crate::renderer::RendererAuthority>() + 16)?;
            }
            self.renderer_document(&authority.document)?;
        }
        if let Some(d) = session.sources.descriptor() {
            self.descriptor(&d)?;
        }
        if let Some((_, d)) = &session.network.grants {
            self.document(d)?;
        }
        if let Some(c) = &session.network.active {
            self.context(c)?;
        }
        if let Some(c) = &session.network.closing {
            self.context(&c.context)?;
        }
        if let Some(bounds) = &session.network.closing_activation {
            self.add(size_of::<NetworkActivationBounds>() + bounds.request_id.capacity_bytes())?;
        }
        for r in session
            .receipts
            .iter()
            .chain(session.transport_record.iter())
        {
            if self.records.insert(Arc::as_ptr(r) as usize) {
                self.command(&r.0)?;
            }
        }
        Ok(())
    }
}

impl Session {
    fn validate_network_observation(
        &self,
        p: &NetworkPlaybackObservation,
        binding: &NetworkSourceBinding,
        source_epochs: Epochs,
        output_epochs: Epochs,
        graph_epoch: u64,
        device_epoch: u64,
    ) -> Result<(), EngineError> {
        p.validate()?;
        if &p.binding != binding
            || p.source_epochs != source_epochs
            || p.output_epochs != output_epochs
            || p.graph_epoch != graph_epoch
            || p.device_epoch != device_epoch
        {
            return Err(STALE);
        }
        if !matches!(
            p.format.representation(),
            PcmRepresentation::Signed16 | PcmRepresentation::Signed24
        ) || p.format.channels().count() > 2
        {
            return Err(EngineError::new(
                ErrorCode::FormatUnsupported,
                "network PCM format is unsupported",
            ));
        }
        if let Some(prior) = &self.state.network_playback
            && prior.binding == p.binding
            && (prior.format != p.format
                || prior.total_frames.is_some() && prior.total_frames != p.total_frames
                || prior.source_position_frames > p.source_position_frames
                || prior.output_epochs == p.output_epochs
                    && prior.submitted_frames > p.submitted_frames)
        {
            return Err(crate::network_source::INVALID);
        }
        Ok(())
    }
    fn closing_fence_valid(&self, actual: &NetworkExecutionFence) -> bool {
        let Some(closing) = &self.network.closing else {
            return false;
        };
        let Some(frontier) = self.network.closing_frontier else {
            return false;
        };
        let source = closing.context.source_epochs;
        if actual.binding != closing.context.binding
            || actual.source_epochs != source
            || actual.output_epochs.session != source.session
            || frontier.session != source.session
            || actual.output_epochs.stream < source.stream
            || actual.output_epochs.stream > frontier.stream
            || actual.device_epoch != closing.device_epoch
        {
            return false;
        }
        if let Some(bounds) = &self.network.closing_activation {
            actual.graph_epoch == bounds.graph_epoch
                || bounds.graph_epoch.checked_add(1) == Some(actual.graph_epoch)
                    && actual.output_epochs == bounds.activation_output_epochs
        } else {
            actual.graph_epoch == closing.graph_epoch
        }
    }
    pub fn advance_network(&mut self, event: NetworkEvent) -> Result<(), EngineError> {
        self.expire_network_authority(event.monotonic_ns)?;
        if let NetworkEventKind::Failed { code } = &event.kind
            && self.closing_fence_valid(&NetworkExecutionFence {
                binding: event.binding.clone(),
                source_epochs: event.source_epochs,
                output_epochs: event.output_epochs,
                graph_epoch: event.graph_epoch,
                device_epoch: event.device_epoch,
            })
        {
            if event.monotonic_ns < self.state.monotonic_ns {
                return Err(STALE);
            }
            if *code != ErrorCode::OperationCancelled && !self.network.closing_failure_observed {
                self.network.closing_failure_observed = true;
                self.network.closing.as_mut().ok_or(STALE)?.reason = Some(*code);
                if self.network.active.is_none() {
                    self.state.error = Some(*code);
                    self.state.transport = TransportState::Failed;
                    self.state.muted = true;
                }
                if *code == ErrorCode::OutputFault {
                    self.state.output_consented = false;
                    self.state.volume_policy.disarm();
                }
                self.state.revision = self.state.revision.saturating_add(1);
                self.state.event_sequence = self.state.event_sequence.saturating_add(1);
                self.state.monotonic_ns = event.monotonic_ns;
                self.log(None);
            }
            return Ok(());
        }
        let active = self.network.active.as_ref().ok_or(STALE)?;
        let failed = matches!(event.kind, NetworkEventKind::Failed { .. });
        let valid_output = if failed {
            event.output_epochs.session == active.source_epochs.session
                && event.output_epochs.stream >= active.source_epochs.stream
                && event.output_epochs.stream <= self.state.epochs.stream
        } else {
            self.state.epochs == event.output_epochs
        };
        if (!failed && !self.network_authority_valid(&event.binding, event.monotonic_ns))
            || active.binding != event.binding
            || self.network.closing.is_some()
            || active.source_epochs != event.source_epochs
            || !valid_output
            || self.state.graph_epoch != event.graph_epoch
            || self.state.device_epoch != event.device_epoch
            || event.monotonic_ns < self.state.monotonic_ns
        {
            return Err(STALE);
        }
        let terminal = matches!(
            event.kind,
            NetworkEventKind::Completed { .. } | NetworkEventKind::Failed { .. }
        );
        let progress = matches!(event.kind, NetworkEventKind::Progress { .. });
        let prepared = matches!(event.kind, NetworkEventKind::Prepared { .. });
        let paused = matches!(event.kind, NetworkEventKind::Paused { .. });
        let mut next = self.clone();
        next.state.revision = if progress {
            next.state.revision
        } else if terminal {
            next.state.revision.saturating_add(1)
        } else {
            increment(next.state.revision)?
        };
        next.state.event_sequence = if terminal {
            next.state.event_sequence.saturating_add(1)
        } else {
            next.next_session_event_sequence(false)?
        };
        next.state.monotonic_ns = event.monotonic_ns;
        let mut settle = None;
        match event.kind {
            NetworkEventKind::Prepared { observation }
            | NetworkEventKind::Progress { observation }
            | NetworkEventKind::Paused { observation }
            | NetworkEventKind::Completed { observation } => {
                self.validate_network_observation(
                    &observation,
                    &event.binding,
                    event.source_epochs,
                    event.output_epochs,
                    event.graph_epoch,
                    event.device_epoch,
                )?;
                if observation.replacement_boundary.as_ref()
                    != self
                        .state
                        .network_playback
                        .as_ref()
                        .and_then(|p| p.replacement_boundary.as_ref())
                {
                    return Err(crate::network_source::INVALID);
                }
                let lifecycle = observation.lifecycle;
                if (prepared && lifecycle != NetworkLifecycle::Ready)
                    || (paused && lifecycle != NetworkLifecycle::Paused)
                    || (!terminal
                        && self.graph.active.is_none()
                        && observation.submitted_frames != 0)
                {
                    return Err(crate::network_source::INVALID);
                }
                next.state.decoded_position_frames = observation.source_position_frames;
                next.state.audible_position_frames = observation.audible_source_position_frames;
                next.state.network_playback = Some(observation);
                if terminal {
                    next.state.network_playback.as_mut().ok_or(STALE)?.lifecycle =
                        NetworkLifecycle::Completed;
                    next.state.muted = true;
                    next.state.transport = TransportState::Draining;
                    next.close_network_at(event.output_epochs, None)?;
                    next.state.network_playback.as_mut().ok_or(STALE)?.lifecycle =
                        NetworkLifecycle::Completed;
                    next.invalidate_graph_work(EngineError::new(
                        ErrorCode::OperationCancelled,
                        "network source ended",
                    ));
                    next.graph.active = None;
                } else if !progress {
                    next.state.transport = if next.wants_playback() {
                        TransportState::Buffering
                    } else {
                        TransportState::Paused
                    };
                    if lifecycle == NetworkLifecycle::Paused || !next.wants_playback() {
                        settle = Some((ReceiptStatus::Applied, None, None));
                    }
                }
            }
            NetworkEventKind::Started { activation_frame } => {
                if next.graph.active.is_none() || next.state.transport != TransportState::Buffering
                {
                    return Err(STATE);
                }
                next.state.transport = next.playback_intent.transport();
                let playing = next.wants_playback();
                if let Some(p) = &mut next.state.network_playback {
                    p.lifecycle = if playing {
                        NetworkLifecycle::Playing
                    } else {
                        NetworkLifecycle::Paused
                    };
                }
                settle = Some((ReceiptStatus::Applied, None, Some(activation_frame)));
            }
            NetworkEventKind::Failed { code } => {
                next.state.error.get_or_insert(code);
                let error = EngineError::new(
                    next.state.error.ok_or(STALE)?,
                    "network source failed; explicit recovery is required",
                );
                next.state.muted = true;
                next.state.transport = TransportState::Failed;
                next.close_network_at(event.output_epochs, Some(error.code))?;
                next.network.closing_failure_observed = code != ErrorCode::OperationCancelled;
                next.invalidate_graph_work(error);
                next.graph.active = None;
                if code == ErrorCode::OutputFault {
                    next.state.output_consented = false;
                    next.state.volume_policy.disarm();
                }
                settle = Some((ReceiptStatus::Failed, Some(error), None));
            }
        }
        if let Some((status, error, frame)) = settle {
            next.finish_pending(status, error, frame);
        }
        next.log(None);
        *self = next;
        Ok(())
    }
    pub fn complete_network_retirement(
        &mut self,
        ack: NetworkRetirementAck,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        self.expire_network_authority(now_ns)?;
        if !self.closing_fence_valid(&NetworkExecutionFence {
            binding: ack.binding.clone(),
            source_epochs: ack.source_epochs,
            output_epochs: ack.output_epochs,
            graph_epoch: ack.graph_epoch,
            device_epoch: ack.device_epoch,
        }) || now_ns < self.state.monotonic_ns
        {
            return Err(STALE);
        }
        if let Some(p) = &ack.observation {
            self.validate_network_observation(
                p,
                &ack.binding,
                ack.source_epochs,
                ack.output_epochs,
                ack.graph_epoch,
                ack.device_epoch,
            )?;
        }
        self.network.closing.take().ok_or(STALE)?;
        if self
            .renderer
            .closing
            .as_ref()
            .is_some_and(|o| o.binding == ack.binding)
        {
            self.renderer.closing = None;
        }
        self.network.closing_failure_observed = false;
        self.network.closing_activation = None;
        self.network.closing_frontier = None;
        if self.network.active.is_none() && self.state.source == Some(SourceKind::HttpFlac) {
            if let Some(p) = ack.observation {
                self.state.network_playback = Some(p);
            }
            if let Some(p) = &mut self.state.network_playback {
                p.lifecycle = if self.state.error.is_some() {
                    NetworkLifecycle::Failed
                } else {
                    NetworkLifecycle::Completed
                };
            }
            self.state.session_id = None;
            self.state.source = None;
            self.state.muted = true;
            self.state.seekable = false;
            self.state.transport = if self.state.error.is_some() {
                TransportState::Failed
            } else {
                TransportState::Idle
            };
            let error = self.state.error.map(|code| {
                EngineError::new(code, "network source failed; explicit recovery is required")
            });
            self.finish_pending(
                if error.is_some() {
                    ReceiptStatus::Failed
                } else {
                    ReceiptStatus::Applied
                },
                error,
                None,
            );
        }
        self.state.monotonic_ns = now_ns;
        self.expire_renderer_authority(now_ns)?;
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.event_sequence = self.state.event_sequence.saturating_add(1);
        self.log(None);
        Ok(())
    }
}

impl Session {
    /// Captures measured failure fences without adopting PCM or settling retirement.
    pub fn fail_network_graph_activation(
        &mut self,
        id: &RequestId,
        error: EngineError,
        actual: NetworkExecutionFence,
    ) -> Result<(), EngineError> {
        let pending = self.graph_activation().filter(|a| &a.request_id == id);
        let (context, bounds) = if let Some(activation) = &pending {
            (
                self.network.active.clone().ok_or(STALE)?,
                NetworkActivationBounds {
                    request_id: activation.request_id.clone(),
                    previous_output_epochs: activation.previous_binding.epochs,
                    activation_output_epochs: activation.epochs,
                    graph_epoch: self.state.graph_epoch,
                    device_epoch: activation.previous_binding.device_epoch,
                },
            )
        } else {
            let closing = self.network.closing.as_ref().ok_or(STALE)?;
            let bounds = self
                .network
                .closing_activation
                .clone()
                .filter(|b| &b.request_id == id)
                .ok_or(STALE)?;
            (closing.context.clone(), bounds)
        };
        if actual.binding != context.binding
            || actual.source_epochs != context.source_epochs
            || !bounds.admits(&actual)
        {
            return Err(STALE);
        }
        if pending.is_some() {
            self.close_network_at(actual.output_epochs, Some(error.code))?;
        }
        let closing = self.network.closing.as_mut().ok_or(STALE)?;
        closing.output_epochs = actual.output_epochs;
        closing.graph_epoch = actual.graph_epoch;
        closing.device_epoch = actual.device_epoch;
        if error.code != ErrorCode::OperationCancelled && !self.network.closing_failure_observed {
            self.network.closing_failure_observed = true;
            closing.reason = Some(error.code);
            if self.network.active.is_none() {
                self.state.error = Some(error.code);
                self.state.muted = true;
                self.state.transport = TransportState::Failed;
            }
        }
        if error.code == ErrorCode::OutputFault {
            self.state.output_consented = false;
            self.state.volume_policy.disarm();
        }
        if pending.is_some() {
            self.fail_graph_activation(id, error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Session {
        let mut s = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owner").unwrap(),
            ControlLimits::default(),
        );
        s.enable_local_playback();
        s.enable_network_playback();
        s.state.output = Some(OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "silent-fixture".into(),
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        });
        s.state.output_consented = true;
        s.sources.stage("http://private.example/a", "").unwrap();
        s.network.generation = 1;
        s.network.grants = Some((
            1_000_000,
            Arc::new(NetworkGrantDocument {
                origins: vec![NetworkOriginGrant {
                    origin: "http://private.example".into(),
                    pins: vec!["192.168.1.2".parse().unwrap()],
                    roots_der: vec![],
                }],
            }),
        ));
        s
    }
    fn c(s: &Session, payload: Command) -> CommandEnvelope {
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new("mutation").unwrap(),
            zone_id: s.state.zone_id.clone(),
            owner_id: s.state.owner_id.clone(),
            session_id: Some(SessionId::new("fresh").unwrap()),
            expected_revision: Some(s.state.revision),
            payload,
        }
    }
    #[test]
    fn exhausted_source_revision_event_and_epoch_counters_reject_start_atomically() {
        for counter in ["source", "revision", "event", "session"] {
            let mut s = fixture();
            match counter {
                "source" => s.network.source_generation = u64::MAX,
                "revision" => s.state.revision = u64::MAX,
                "event" => s.state.event_sequence = u64::MAX,
                _ => s.state.epochs.session = u64::MAX,
            }
            let before = s.snapshot();
            let c = c(
                &s,
                Command::NetworkStartHttpFlac {
                    media_id: StagedMediaId::new("staged-1").unwrap(),
                    staging_generation: 1,
                    grant_generation: 1,
                    device_id: DeviceId::new("dac").unwrap(),
                    takeover: false,
                },
            );
            let r = s.submit_with_network_effects(
                c,
                1,
                |_| unreachable!(),
                |_, _| panic!("exhausted start reached policy"),
                |_| panic!("exhausted start committed"),
            );
            assert_eq!(
                r.error.unwrap().code,
                ErrorCode::ResourceBudgetExceeded,
                "{counter}"
            );
            assert_eq!(s.snapshot(), before);
            assert!(s.network.active.is_none());
        }
    }
    #[test]
    fn saturated_authority_history_keeps_explicit_revocation_available() {
        let mut s = fixture();
        let start = c(
            &s,
            Command::NetworkStartHttpFlac {
                media_id: StagedMediaId::new("staged-1").unwrap(),
                staging_generation: 1,
                grant_generation: 1,
                device_id: DeviceId::new("dac").unwrap(),
                takeover: false,
            },
        );
        s.submit_with_network_effects(start, 1, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()));
        for i in 0..16 {
            let document = Arc::new(NetworkGrantDocument {
                origins: vec![NetworkOriginGrant {
                    origin: "http://private.example".into(),
                    pins: vec!["192.168.1.2".parse().unwrap()],
                    roots_der: vec![vec![7; 1024 * 1024 - 16384]],
                }],
            });
            let mut command = c(
                &s,
                Command::NetworkGrantsReplace {
                    expected_generation: 0,
                    grants: document,
                    lifetime_ms: 1,
                },
            );
            command.request_id = RequestId::new(format!("fill-{i}")).unwrap();
            command.session_id = s.state.session_id.clone();
            s.submit_with_network_effects(
                command,
                2,
                |_| unreachable!(),
                |_, _| panic!("stale grants reached policy"),
                |_| Ok(()),
            );
        }
        let mut command = c(
            &s,
            Command::NetworkGrantsReplace {
                expected_generation: 0,
                grants: Arc::new(NetworkGrantDocument {
                    origins: vec![NetworkOriginGrant {
                        origin: "http://private.example".into(),
                        pins: vec!["192.168.1.2".parse().unwrap()],
                        roots_der: vec![vec![]],
                    }],
                }),
                lifetime_ms: 1,
            },
        );
        command.request_id = RequestId::new("fill-remainder").unwrap();
        command.session_id = s.state.session_id.clone();
        let remaining = MAX_NETWORK_AUTHORITY_BYTES
            - s.network_retained_bytes().unwrap()
            - command.network_capacity_bytes().unwrap();
        if remaining <= 1024 * 1024 {
            if let Command::NetworkGrantsReplace { grants, .. } = &mut command.payload {
                Arc::make_mut(grants).origins[0].roots_der[0] = vec![7; remaining];
            }
            s.submit_with_network_effects(
                command,
                2,
                |_| unreachable!(),
                |_, _| panic!("stale grants reached policy"),
                |_| Ok(()),
            );
        }
        let mut revoke = c(
            &s,
            Command::NetworkGrantsRevoke {
                expected_generation: 1,
            },
        );
        revoke.request_id = RequestId::new("revoke-full-history").unwrap();
        revoke.session_id = s.state.session_id.clone();
        let r =
            s.submit_with_network_effects(revoke, 3, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()));
        assert_eq!(r.status, ReceiptStatus::Accepted);
        assert!(s.network.active.is_none());
        assert!(s.network_retained_bytes().unwrap() <= MAX_NETWORK_AUTHORITY_BYTES);
    }
    #[test]
    fn retention_allowance_covers_the_actual_record_layout_and_cloned_receipt_ids() {
        let s = fixture();
        let command = c(
            &s,
            Command::NetworkGrantsRevoke {
                expected_generation: 1,
            },
        );
        let identifiers = command.request_id.capacity_bytes() * 2
            + command.zone_id.capacity_bytes() * 2
            + command.owner_id.as_ref().unwrap().capacity_bytes()
            + command.session_id.as_ref().unwrap().capacity_bytes() * 2;
        let required =
            size_of::<(CommandEnvelope, Receipt)>() + 2 * size_of::<usize>() + identifiers;
        assert!(command.network_capacity_bytes().unwrap() >= required);
    }
    #[test]
    fn exhausted_grant_counter_does_not_replace_the_immutable_document() {
        let mut s = fixture();
        s.network.generation = u64::MAX;
        let doc = s.network.grants.as_ref().unwrap().1.clone();
        let mut command = c(
            &s,
            Command::NetworkGrantsReplace {
                expected_generation: u64::MAX,
                grants: doc.clone(),
                lifetime_ms: 1,
            },
        );
        command.session_id = None;
        let r = s.submit_with_network_effects(
            command,
            1,
            |_| unreachable!(),
            |_, _| panic!("exhausted generation reached policy"),
            |_| Ok(()),
        );
        assert_eq!(r.error.unwrap().code, ErrorCode::ResourceBudgetExceeded);
        assert!(Arc::ptr_eq(&s.network.grants.as_ref().unwrap().1, &doc));
        assert_eq!(s.network.generation, u64::MAX);
    }
    #[test]
    fn expiry_and_exact_cleanup_still_revoke_at_terminal_counter_exhaustion() {
        let mut s = fixture();
        let c = c(
            &s,
            Command::NetworkStartHttpFlac {
                media_id: StagedMediaId::new("staged-1").unwrap(),
                staging_generation: 1,
                grant_generation: 1,
                device_id: DeviceId::new("dac").unwrap(),
                takeover: false,
            },
        );
        assert_eq!(
            s.submit_with_network_effects(c, 1, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
                .status,
            ReceiptStatus::Accepted
        );
        s.state.epochs.stream = u64::MAX;
        s.state.revision = u64::MAX;
        s.state.event_sequence = u64::MAX;
        s.expire_network_authority(1_000_000).unwrap();
        assert!(s.network.active.is_none());
        assert!(s.state.muted);
        assert!(s.network.grants.is_none());
        let closing = s.closing_network_source().unwrap();
        s.complete_network_retirement(
            NetworkRetirementAck {
                binding: closing.context.binding.clone(),
                source_epochs: closing.context.source_epochs,
                output_epochs: closing.output_epochs,
                graph_epoch: closing.graph_epoch,
                device_epoch: closing.device_epoch,
                observation: None,
            },
            1_000_001,
        )
        .unwrap();
        assert!(s.network.closing.is_none());
        assert_eq!(s.state.revision, u64::MAX);
        assert_eq!(s.state.event_sequence, u64::MAX);
    }
}
