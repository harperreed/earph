// ABOUTME: Owns separate callback authority and response-gated renderer subscriptions.
// ABOUTME: Retains initial, latest, and closing deliveries through exact task acknowledgements.
use super::*;
use crate::{
    renderer::{RendererControllerSummary, RendererRight},
    renderer_events::*,
};

impl Session {
    pub fn renderer_callback_generation(&self) -> u64 {
        self.renderer.events.generation
    }
    pub fn renderer_callback_generation_for(
        &self,
        controller: &RendererControllerId,
        controller_generation: u64,
        now_ns: u64,
    ) -> Option<u64> {
        self.renderer
            .events
            .callbacks
            .iter()
            .find(|authority| {
                authority.controller_id == *controller
                    && authority.controller_generation == controller_generation
                    && self.callback_authority_live(authority, now_ns)
            })
            .map(|authority| authority.generation)
    }
    pub fn renderer_epoch(&self) -> u64 {
        self.renderer.epoch
    }
    pub fn renderer_subscription_count(&self) -> usize {
        self.renderer.events.subscriptions.len()
    }
    pub fn renderer_controller_for_peer(
        &self,
        peer: std::net::IpAddr,
        right: RendererRight,
        now_ns: u64,
    ) -> Result<RendererControllerSummary, EngineError> {
        let mut owners = self.renderer.grants.iter().filter(|g| {
            self.renderer.enabled
                && now_ns < g.expires_monotonic_ns
                && g.grant().peers.contains(&peer)
                && g.grant().rights.contains(&right)
        });
        let owner = owners.next().ok_or(EngineError::new(
            ErrorCode::AuthDenied,
            "renderer peer has no controller authority",
        ))?;
        if owners.next().is_some() {
            return Err(EngineError::new(
                ErrorCode::AuthDenied,
                "renderer peer has ambiguous controller authority",
            ));
        }
        Ok(RendererControllerSummary {
            id: owner.grant().id.clone(),
            generation: owner.generation,
            expires_monotonic_ns: owner.expires_monotonic_ns,
        })
    }
    pub(super) fn apply_renderer_callback_command(
        &mut self,
        c: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        let (controller_id, controller_generation, expected_generation) = match &c.payload {
            Command::RendererCallbackGrantsReplace {
                controller_id,
                expected_controller_generation,
                expected_generation,
                ..
            }
            | Command::RendererCallbackGrantsRevoke {
                controller_id,
                expected_controller_generation,
                expected_generation,
            } => (
                controller_id,
                *expected_controller_generation,
                *expected_generation,
            ),
            _ => return Err(STATE),
        };
        if expected_generation != self.renderer.events.generation {
            return Err(STALE);
        }
        let controller = self
            .renderer
            .grants
            .iter()
            .find(|g| {
                g.grant().id == *controller_id
                    && g.generation == controller_generation
                    && now_ns < g.expires_monotonic_ns
            })
            .ok_or(STALE)?;
        let revoke = matches!(c.payload, Command::RendererCallbackGrantsRevoke { .. });
        let generation = if revoke {
            self.renderer.events.generation.saturating_add(1)
        } else {
            increment(self.renderer.events.generation)?
        };
        let replacement = if let Command::RendererCallbackGrantsReplace {
            grants,
            lifetime_ms,
            ..
        } = &c.payload
        {
            if !controller.grant().rights.contains(&RendererRight::Events)
                || !(1..=crate::network_source::MAX_NETWORK_LIFETIME_MS).contains(lifetime_ms)
            {
                return Err(INVALID);
            }
            grants.validate(&controller.grant().peers)?;
            let expires = lifetime_ms
                .checked_mul(1_000_000)
                .and_then(|n| now_ns.checked_add(n))
                .ok_or(BUDGET)?
                .min(controller.expires_monotonic_ns);
            Some(Arc::new(CallbackAuthority {
                controller_id: controller_id.clone(),
                controller_generation,
                generation,
                expires_monotonic_ns: expires,
                document: grants.clone(),
            }))
        } else {
            None
        };
        let revision = if revoke {
            self.state.revision.saturating_add(1)
        } else {
            increment(self.state.revision)?
        };
        let sequence = if revoke {
            self.state.event_sequence.saturating_add(1)
        } else {
            self.next_session_event_sequence(false)?
        };
        self.renderer
            .events
            .callbacks
            .retain(|a| a.controller_id != *controller_id);
        if let Some(replacement) = replacement {
            self.renderer.events.callbacks.push(replacement);
        }
        self.renderer.events.generation = generation;
        self.expire_renderer_subscriptions(now_ns);
        self.state.revision = revision;
        self.state.event_sequence = sequence;
        self.log(Some(c.request_id.clone()));
        Ok(ReceiptStatus::Applied)
    }
    pub fn submit_renderer_subscription_with_effects(
        &mut self,
        command: CommandEnvelope,
        observed_peer: std::net::IpAddr,
        now_ns: u64,
        admission: Option<RendererSubscriptionAdmission>,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> RendererSubscriptionResult {
        let id = command.request_id.clone();
        self.renderer.events.admission = admission;
        let receipt = self.submit_renderer_with_network_effects(
            command,
            observed_peer,
            now_ns,
            |_, _| Ok(()),
            commit,
        );
        self.renderer.events.admission = None;
        let subscription = if receipt.status == ReceiptStatus::Applied {
            self.renderer
                .events
                .results
                .iter()
                .rev()
                .find(|r| r.request_id == id)
                .and_then(|r| r.info.clone())
        } else {
            None
        };
        RendererSubscriptionResult {
            receipt,
            subscription,
        }
    }
    pub(super) fn renderer_subscription_ingress(
        &self,
        controller: &RendererControllerId,
        generation: u64,
        callback_generation: u64,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if self.renderer.events.callbacks.iter().any(|a| {
            a.controller_id == *controller
                && a.controller_generation == generation
                && a.generation == callback_generation
                && self.callback_authority_live(a, now_ns)
        }) {
            Ok(())
        } else {
            Err(EngineError::new(
                ErrorCode::AuthDenied,
                "renderer callback authority is unavailable",
            ))
        }
    }
    fn callback_authority_live(&self, authority: &CallbackAuthority, now_ns: u64) -> bool {
        self.renderer.enabled
            && !self.state.shutdown_requested
            && now_ns < authority.expires_monotonic_ns
            && self.renderer.events.callbacks.iter().any(|a| {
                a.controller_id == authority.controller_id
                    && a.controller_generation == authority.controller_generation
                    && a.generation == authority.generation
                    && Arc::ptr_eq(&a.document, &authority.document)
            })
            && self.renderer.grants.iter().any(|g| {
                g.grant().id == authority.controller_id
                    && g.generation == authority.controller_generation
                    && now_ns < g.expires_monotonic_ns
                    && g.grant().rights.contains(&RendererRight::Events)
            })
    }
    fn subscription_live(&self, record: &Subscription, now_ns: u64) -> bool {
        self.state.owner_id.as_ref() == Some(&record.info.binding.owner_id)
            && self.renderer.epoch == record.info.binding.renderer_epoch
            && !record.closing
            && self.callback_authority_live(&record.authority, now_ns)
            && if record.renewable {
                now_ns < record.info.expires_monotonic_ns
            } else {
                (record.initial.is_some() || record.in_flight.as_ref().is_some_and(|d| d.initial))
                    && record
                        .initial_deadline_ns
                        .map_or(now_ns < record.response_deadline_ns, |deadline| {
                            now_ns < deadline
                        })
            }
    }
    fn subscription_expiry(
        &self,
        requested: u32,
        authority: &CallbackAuthority,
        now_ns: u64,
    ) -> Result<(u32, u64), EngineError> {
        if requested == 0 {
            return Err(INVALID);
        }
        let expiry = now_ns
            .checked_add(u64::from(requested.min(1800)) * 1_000_000_000)
            .ok_or(BUDGET)?
            .min(authority.expires_monotonic_ns);
        let seconds = expiry.saturating_sub(now_ns) / 1_000_000_000;
        if seconds == 0 {
            return Err(PRECONDITION);
        }
        Ok((seconds as u32, expiry))
    }
    pub(super) fn apply_renderer_subscription(
        &mut self,
        c: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        let Command::RendererSubscription {
            controller_id,
            grant_generation,
            callback_generation,
            action,
        } = &c.payload
        else {
            return Err(STATE);
        };
        if c.session_id != self.state.session_id {
            return Err(STALE);
        }
        self.renderer_subscription_ingress(
            controller_id,
            *grant_generation,
            *callback_generation,
            now_ns,
        )?;
        let authority = self
            .renderer
            .events
            .callbacks
            .iter()
            .find(|a| a.controller_id == *controller_id && a.generation == *callback_generation)
            .cloned()
            .ok_or(PRECONDITION)?;
        let revision = increment(self.state.revision)?;
        let sequence = self.next_session_event_sequence(false)?;
        let result =
            match action {
                RendererSubscriptionAction::Subscribe {
                    service,
                    callbacks,
                    timeout_seconds,
                } => {
                    if self.renderer.events.subscriptions.len() >= MAX_RENDERER_SUBSCRIPTIONS
                        || callbacks.is_empty()
                        || callbacks.len() > 8
                    {
                        return Err(BUDGET);
                    }
                    for callback in callbacks {
                        let origin = callback_origin(callback)?;
                        if !authority.document.origins.iter().any(|o| {
                            callback_origin(&o.origin).is_ok_and(|granted| granted == origin)
                        }) {
                            return Err(EngineError::new(
                                ErrorCode::AuthDenied,
                                "renderer callback destination is not granted",
                            ));
                        }
                    }
                    let admission = self.renderer.events.admission.take().ok_or(BUDGET)?;
                    let slots = admission.lease.event_slot_bytes();
                    if admission.lease.lease_id() == 0
                        || admission.lease.service() != *service
                        || slots.0 != service.event_slot_bytes()
                        || slots.1 != service.event_slot_bytes()
                        || self
                            .renderer
                            .events
                            .subscriptions
                            .iter()
                            .any(|s| s.lease.lease_id() == admission.lease.lease_id())
                    {
                        return Err(BUDGET);
                    }
                    let position = self.free_renderer_delivery_position().ok_or(BUDGET)?;
                    let response_deadline = admission
                        .response_deadline_monotonic_ns
                        .min(now_ns.checked_add(RENDERER_RESPONSE_NS).ok_or(BUDGET)?);
                    if response_deadline <= now_ns {
                        return Err(PRECONDITION);
                    }
                    let (timeout_seconds, expires_monotonic_ns) =
                        self.subscription_expiry(*timeout_seconds, &authority, now_ns)?;
                    let serial = increment(self.renderer.events.serial)?;
                    let initial_delivery_id = increment(self.renderer.events.delivery_serial)?;
                    let owner_id = self.state.owner_id.clone().ok_or(STALE)?;
                    let sid = subscription_sid(&owner_id, self.renderer.epoch, serial);
                    let info = RendererSubscriptionInfo {
                        binding: RendererSubscriptionBinding {
                            owner_id,
                            renderer_epoch: self.renderer.epoch,
                            sid,
                            generation: serial,
                            controller_id: controller_id.clone(),
                            controller_generation: *grant_generation,
                            callback_generation: *callback_generation,
                        },
                        timeout_seconds,
                        expires_monotonic_ns,
                    };
                    let initial = Arc::new(RendererEventSnapshot {
                        controller_id: controller_id.clone(),
                        controller_generation: *grant_generation,
                        projection: self.renderer_projection(
                            *service,
                            controller_id,
                            *grant_generation,
                        ),
                    });
                    initial.encoded_size()?;
                    let callbacks: Arc<[String]> = callbacks.clone().into();
                    self.renderer
                        .events
                        .subscriptions
                        .push(Arc::new(Subscription {
                            info: info.clone(),
                            service: *service,
                            authority,
                            callbacks,
                            lease: admission.lease,
                            initial_delivery_id,
                            initial: Some(initial.clone()),
                            latest: None,
                            observed: initial,
                            in_flight: None,
                            position: Some(position),
                            response_deadline_ns: response_deadline,
                            confirmed: false,
                            initial_deadline_ns: None,
                            renewable: true,
                            closing: false,
                            next_sequence: 1,
                            next_send_ns: now_ns,
                        }));
                    self.renderer.events.serial = serial;
                    self.renderer.events.delivery_serial = initial_delivery_id;
                    Some(info)
                }
                RendererSubscriptionAction::Renew {
                    sid,
                    timeout_seconds,
                } => {
                    let index = self.subscription_index(
                        sid,
                        controller_id,
                        *grant_generation,
                        *callback_generation,
                        now_ns,
                    )?;
                    let (timeout, expiry) =
                        self.subscription_expiry(*timeout_seconds, &authority, now_ns)?;
                    let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
                    record.info.timeout_seconds = timeout;
                    record.info.expires_monotonic_ns = expiry;
                    Some(record.info.clone())
                }
                RendererSubscriptionAction::Unsubscribe { sid } => {
                    let index = self.subscription_index(
                        sid,
                        controller_id,
                        *grant_generation,
                        *callback_generation,
                        now_ns,
                    )?;
                    let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
                    record.renewable = false;
                    record.latest = None;
                    if record.initial.is_none()
                        && !record.in_flight.as_ref().is_some_and(|d| d.initial)
                    {
                        record.closing = true;
                    }
                    self.expire_renderer_subscriptions(now_ns);
                    None
                }
            };
        self.renderer
            .events
            .results
            .retain(|r| r.request_id != c.request_id);
        if self.renderer.events.results.len() == self.limits.receipts {
            self.renderer.events.results.pop_front();
        }
        self.renderer
            .events
            .results
            .push_back(Arc::new(SubscriptionResultRecord {
                request_id: c.request_id.clone(),
                info: result,
            }));
        self.state.revision = revision;
        self.state.event_sequence = sequence;
        self.log(Some(c.request_id.clone()));
        Ok(ReceiptStatus::Applied)
    }
    fn subscription_index(
        &self,
        sid: &str,
        controller: &RendererControllerId,
        controller_generation: u64,
        callback_generation: u64,
        now_ns: u64,
    ) -> Result<usize, EngineError> {
        self.renderer
            .events
            .subscriptions
            .iter()
            .position(|s| {
                s.info.binding.sid == sid
                    && s.info.binding.controller_id == *controller
                    && s.info.binding.controller_generation == controller_generation
                    && s.info.binding.callback_generation == callback_generation
                    && s.renewable
                    && self.subscription_live(s, now_ns)
            })
            .ok_or(PRECONDITION)
    }
    fn free_renderer_delivery_position(&self) -> Option<u8> {
        (0..MAX_RENDERER_DELIVERIES as u8).find(|position| {
            !self
                .renderer
                .events
                .subscriptions
                .iter()
                .any(|s| s.position == Some(*position))
        })
    }
    pub fn renderer_projection(
        &self,
        service: RendererService,
        controller: &RendererControllerId,
        generation: u64,
    ) -> RendererProjection {
        let staged = self
            .renderer
            .staged
            .as_ref()
            .filter(|s| s.controller_id == *controller && s.grant_generation == generation);
        let active = self.renderer.active.as_ref().filter(|s| {
            s.controller_id == *controller
                && s.grant_generation == generation
                && s.retirement_request.is_none()
        });
        let descriptor = staged
            .and_then(|association| {
                self.sources
                    .descriptor()
                    .filter(|d| d.generation() == association.generation)
            })
            .or_else(|| {
                active.and_then(|owner| {
                    self.network
                        .active
                        .as_ref()
                        .filter(|n| n.binding == owner.binding)
                        .map(|n| n.descriptor.clone())
                })
            });
        let transport_state = if active.is_some() {
            match self.state.transport {
                TransportState::Playing => "PLAYING",
                TransportState::Paused => "PAUSED_PLAYBACK",
                TransportState::Preparing
                | TransportState::Buffering
                | TransportState::Reconfiguring
                | TransportState::Draining => "TRANSITIONING",
                _ => "STOPPED",
            }
        } else if descriptor.is_some() {
            "STOPPED"
        } else {
            "NO_MEDIA_PRESENT"
        };
        let measured = active.and_then(|owner| {
            self.state
                .network_playback
                .as_ref()
                .filter(|p| p.binding == owner.binding)
        });
        let duration_seconds = measured.and_then(|p| {
            p.total_frames
                .map(|frames| frames / u64::from(p.format.sample_rate_hz()))
        });
        let position_seconds = measured.and_then(|p| {
            p.audible_source_position_frames
                .map(|frames| frames / u64::from(p.format.sample_rate_hz()))
        });
        let now_ns = self.network.admission_ns.max(self.state.monotonic_ns);
        let authority = self.renderer.grants.iter().find(|g| {
            g.grant().id == *controller
                && g.generation == generation
                && now_ns < g.expires_monotonic_ns
        });
        let transport =
            authority.is_some_and(|g| g.grant().rights.contains(&RendererRight::Transport));
        let can_play = authority.is_some_and(|g| {
            g.grant()
                .playback
                .as_ref()
                .is_some_and(|p| self.validate_renderer_playback(p).is_ok())
                && (self.state.session_id.is_none()
                    || active.is_some()
                    || g.grant().rights.contains(&RendererRight::Takeover))
        }) && self
            .network
            .grants
            .as_ref()
            .is_some_and(|(expiry, _)| now_ns < *expiry);
        let transport_actions = if !transport || descriptor.is_none() {
            ""
        } else if active.is_some() {
            if self.graph_activation().is_some()
                || !active.is_some_and(|o| self.network_authority_valid(&o.binding, now_ns))
            {
                "Stop"
            } else if can_play {
                "Play,Stop,Pause"
            } else {
                "Stop,Pause"
            }
        } else if can_play {
            "Play,Stop"
        } else {
            "Stop"
        };
        RendererProjection {
            service,
            transport_actions,
            presets: if self.renderer_presets().is_empty() {
                ""
            } else {
                "FactoryDefaults"
            },
            transport_state,
            transport_status: if self.state.error.is_some() {
                "ERROR_OCCURRED"
            } else {
                "OK"
            },
            duration_seconds,
            position_seconds,
            muted: self.state.muted,
            volume: crate::remote_volume::RemoteVolume::from_local(
                self.state.software_gain_db,
                self.state.software_gain_zero,
            )
            .expect("canonical gain remains in its validated range"),
            connected: active.is_some(),
            descriptor,
        }
    }
    pub(super) fn capture_renderer_events(&mut self) {
        for index in 0..self.renderer.events.subscriptions.len() {
            let record = &self.renderer.events.subscriptions[index];
            if !record.renewable || record.closing {
                continue;
            }
            let projection = self.renderer_projection(
                record.service,
                &record.info.binding.controller_id,
                record.info.binding.controller_generation,
            );
            if record.observed.projection == projection {
                continue;
            }
            let snapshot = Arc::new(RendererEventSnapshot {
                controller_id: record.info.binding.controller_id.clone(),
                controller_generation: record.info.binding.controller_generation,
                projection,
            });
            debug_assert!(
                snapshot.encoded_size().is_ok(),
                "renderer mutation admission must bound every canonical event"
            );
            let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
            record.observed = snapshot.clone();
            record.latest = Some(snapshot);
        }
    }
    pub(super) fn expire_renderer_subscriptions(&mut self, now_ns: u64) {
        let mut records = std::mem::take(&mut self.renderer.events.subscriptions);
        records.retain_mut(|record| {
            let response_expired = !record.confirmed && now_ns >= record.response_deadline_ns;
            let initial_expired = record
                .initial_deadline_ns
                .is_some_and(|deadline| now_ns >= deadline);
            if !self.subscription_live(record, now_ns) || response_expired {
                let record = Arc::make_mut(record);
                record.closing = true;
                record.renewable = false;
                record.latest = None;
                record.initial = None;
                return record.in_flight.is_some();
            }
            if initial_expired && record.in_flight.is_none() {
                let record = Arc::make_mut(record);
                record.initial = None;
                record.position = None;
            }
            record.renewable || record.initial.is_some() || record.in_flight.is_some()
        });
        self.renderer.events.subscriptions = records;
        let mut callbacks = std::mem::take(&mut self.renderer.events.callbacks);
        callbacks.retain(|authority| {
            self.renderer.enabled
                && !self.state.shutdown_requested
                && now_ns < authority.expires_monotonic_ns
                && self.renderer.grants.iter().any(|g| {
                    g.grant().id == authority.controller_id
                        && g.generation == authority.controller_generation
                        && now_ns < g.expires_monotonic_ns
                })
        });
        self.renderer.events.callbacks = callbacks;
    }
    /// Call only after the actual subscription response has completed TCP write shutdown.
    pub fn acknowledge_renderer_subscription_response(
        &mut self,
        binding: &RendererSubscriptionBinding,
        completed: bool,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        self.expire_renderer_subscriptions(now_ns);
        let index = self
            .renderer
            .events
            .subscriptions
            .iter()
            .position(|s| s.info.binding == *binding && !s.confirmed)
            .ok_or(PRECONDITION)?;
        if !completed {
            self.renderer.events.subscriptions.remove(index);
            return Ok(());
        }
        let deadline = now_ns.checked_add(RENDERER_DELIVERY_NS).ok_or(BUDGET)?;
        let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
        record.confirmed = true;
        record.initial_deadline_ns = Some(deadline);
        Ok(())
    }
    pub fn begin_renderer_delivery(
        &mut self,
        binding: &RendererSubscriptionBinding,
        now_ns: u64,
    ) -> Result<Arc<RendererDelivery>, EngineError> {
        self.expire_renderer_subscriptions(now_ns);
        let index = self
            .renderer
            .events
            .subscriptions
            .iter()
            .position(|s| s.info.binding == *binding && self.subscription_live(s, now_ns))
            .ok_or(PRECONDITION)?;
        let record = &self.renderer.events.subscriptions[index];
        if !record.confirmed || record.in_flight.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "renderer delivery awaits response completion or task retirement",
            ));
        }
        let initial = record.initial.is_some();
        if !initial
            && (!record.renewable || now_ns < record.next_send_ns || record.latest.is_none())
        {
            return Err(STATE);
        }
        let position = record
            .position
            .or_else(|| self.free_renderer_delivery_position())
            .ok_or(BUDGET)?;
        let serial = if initial {
            record.initial_delivery_id
        } else {
            increment(self.renderer.events.delivery_serial)?
        };
        let deadline = if initial {
            record.initial_deadline_ns.ok_or(PRECONDITION)?
        } else {
            now_ns.checked_add(RENDERER_DELIVERY_NS).ok_or(BUDGET)?
        };
        let snapshot = if initial {
            record.initial.as_ref()
        } else {
            record.latest.as_ref()
        }
        .ok_or(STATE)?
        .clone();
        let sequence = if initial { 0 } else { record.next_sequence };
        let delivery = Arc::new(RendererDelivery {
            binding: binding.clone(),
            delivery_id: serial,
            position,
            sequence,
            initial,
            snapshot,
            callbacks: record.callbacks.clone(),
            grants: record.authority.document.clone(),
            deadline_monotonic_ns: deadline.min(record.authority.expires_monotonic_ns),
            lease: record.lease.clone(),
        });
        let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
        if initial {
            record.initial = None;
        } else {
            record.latest = None;
            record.next_sequence = if sequence == u32::MAX {
                1
            } else {
                sequence + 1
            };
        }
        record.position = Some(position);
        record.in_flight = Some(delivery.clone());
        record.next_send_ns = if record.service == RendererService::ConnectionManager {
            now_ns
        } else {
            now_ns.saturating_add(RENDERER_MODERATION_NS)
        };
        if !initial {
            self.renderer.events.delivery_serial = serial;
        }
        Ok(delivery)
    }
    pub fn renderer_delivery_authority(
        &self,
        delivery: &RendererDelivery,
        now_ns: u64,
    ) -> Option<RendererCallbackAuthorityView> {
        let record = self
            .renderer
            .events
            .subscriptions
            .iter()
            .find(|s| s.info.binding == delivery.binding)?;
        if !self.subscription_live(record, now_ns)
            || now_ns >= delivery.deadline_monotonic_ns
            || !record
                .in_flight
                .as_ref()
                .is_some_and(|d| same_delivery(d, delivery))
        {
            return None;
        }
        Some(RendererCallbackAuthorityView {
            binding: delivery.binding.clone(),
            delivery_id: delivery.delivery_id,
            callbacks: delivery.callbacks.clone(),
            grants: delivery.grants.clone(),
            expires_monotonic_ns: delivery
                .deadline_monotonic_ns
                .min(record.authority.expires_monotonic_ns),
        })
    }
    pub fn renderer_deliveries_needing_cancellation(
        &self,
        now_ns: u64,
    ) -> Vec<Arc<RendererDelivery>> {
        self.renderer
            .events
            .subscriptions
            .iter()
            .filter_map(|s| s.in_flight.as_ref())
            .filter(|d| self.renderer_delivery_authority(d, now_ns).is_none())
            .cloned()
            .collect()
    }
    pub fn complete_renderer_delivery(
        &mut self,
        delivery: &RendererDelivery,
        outcome: RendererDeliveryOutcome,
        transport_joined: bool,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        let index = self
            .renderer
            .events
            .subscriptions
            .iter()
            .position(|s| {
                s.info.binding == delivery.binding
                    && s.in_flight
                        .as_ref()
                        .is_some_and(|d| same_delivery(d, delivery))
            })
            .ok_or(PRECONDITION)?;
        let record = Arc::make_mut(&mut self.renderer.events.subscriptions[index]);
        if !transport_joined || outcome == RendererDeliveryOutcome::JoinFailed {
            record.closing = true;
            record.renewable = false;
            record.latest = None;
            record.initial = None;
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "renderer transport join remains outstanding",
            ));
        }
        record.in_flight = None;
        record.position = None;
        if outcome == RendererDeliveryOutcome::InvalidSid {
            record.closing = true;
            record.renewable = false;
            record.latest = None;
            record.initial = None;
        }
        self.expire_renderer_subscriptions(now_ns);
        Ok(())
    }
    /// Invalidate the old listener epoch before the host cancels its exact transport tasks.
    pub fn retire_renderer_epoch(
        &mut self,
        expected_epoch: u64,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if self.renderer.epoch != expected_epoch {
            return Err(STALE);
        }
        self.renderer.continuation = None;
        self.renderer.grants.clear();
        self.renderer.staged = None;
        self.renderer.generation = self.renderer.generation.saturating_add(1);
        self.state.revision = self.state.revision.saturating_add(1);
        self.state.event_sequence = self.state.event_sequence.saturating_add(1);
        self.invalidate_graph_work(EngineError::new(
            ErrorCode::OperationCancelled,
            "renderer listener retired",
        ));
        self.renderer.events.callbacks.clear();
        self.expire_renderer_subscriptions(now_ns);
        self.renderer.epoch = increment(self.renderer.epoch)?;
        self.expire_network_authority(now_ns)?;
        self.log(None);
        Ok(())
    }
}
fn same_delivery(a: &RendererDelivery, b: &RendererDelivery) -> bool {
    a.binding == b.binding
        && a.delivery_id == b.delivery_id
        && a.sequence == b.sequence
        && a.initial == b.initial
        && a.position == b.position
        && a.deadline_monotonic_ns == b.deadline_monotonic_ns
        && Arc::ptr_eq(&a.snapshot, &b.snapshot)
        && Arc::ptr_eq(&a.callbacks, &b.callbacks)
        && Arc::ptr_eq(&a.grants, &b.grants)
        && Arc::ptr_eq(&a.lease, &b.lease)
}
fn subscription_sid(owner: &OwnerId, epoch: u64, serial: u64) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(b"earph.renderer.subscription.v1\0");
    digest.update(owner.as_str().as_bytes());
    digest.update(epoch.to_le_bytes());
    digest.update(serial.to_le_bytes());
    let mut bytes: [u8; 32] = digest.finalize().into();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "uuid:{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::{RendererControllerGrant, RendererGrantDocument};
    struct Slots([String; 2]);
    impl RendererSubscriptionLease for Slots {
        fn lease_id(&self) -> u64 {
            1
        }
        fn service(&self) -> RendererService {
            RendererService::RenderingControl
        }
        fn event_slot_bytes(&self) -> (usize, usize) {
            (self.0[0].capacity(), self.0[1].capacity())
        }
    }
    fn command(s: &Session, id: &str, payload: Command) -> CommandEnvelope {
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(id).unwrap(),
            zone_id: s.state.zone_id.clone(),
            owner_id: s.state.owner_id.clone(),
            session_id: s.state.session_id.clone(),
            expected_revision: Some(s.state.revision),
            payload,
        }
    }
    fn configured() -> (Session, RendererControllerId, std::net::IpAddr) {
        let mut s = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("sequence-owner").unwrap(),
            ControlLimits::default(),
        );
        s.enable_renderer_control();
        let controller = RendererControllerId::new("controller").unwrap();
        let peer = "192.168.1.2".parse().unwrap();
        let grant = command(
            &s,
            "controller",
            Command::RendererGrantsReplace {
                expected_generation: 0,
                lifetime_ms: 60000,
                grants: Arc::new(RendererGrantDocument {
                    controllers: vec![RendererControllerGrant {
                        id: controller.clone(),
                        peers: vec![peer],
                        rights: vec![RendererRight::Events],
                        playback: None,
                    }],
                }),
            },
        );
        assert_eq!(s.submit(grant).status, ReceiptStatus::Applied);
        let callback = command(
            &s,
            "callback",
            Command::RendererCallbackGrantsReplace {
                controller_id: controller.clone(),
                expected_controller_generation: 1,
                expected_generation: 0,
                lifetime_ms: 60000,
                grants: Arc::new(RendererCallbackGrantDocument {
                    origins: vec![RendererCallbackOrigin {
                        origin: "http://private.example:80".into(),
                        pins: vec![peer],
                    }],
                }),
            },
        );
        assert_eq!(s.submit(callback).status, ReceiptStatus::Applied);
        (s, controller, peer)
    }
    fn subscription(
        s: &mut Session,
        controller: RendererControllerId,
        peer: std::net::IpAddr,
    ) -> RendererSubscriptionResult {
        let subscribe = command(
            s,
            "subscribe",
            Command::RendererSubscription {
                controller_id: controller,
                grant_generation: 1,
                callback_generation: 1,
                action: RendererSubscriptionAction::Subscribe {
                    service: RendererService::RenderingControl,
                    callbacks: vec!["http://private.example:80/events".into()],
                    timeout_seconds: 60,
                },
            },
        );
        s.submit_renderer_subscription_with_effects(
            subscribe,
            peer,
            0,
            Some(RendererSubscriptionAdmission {
                lease: Arc::new(Slots([
                    String::with_capacity(2048),
                    String::with_capacity(2048),
                ])),
                response_deadline_monotonic_ns: 15_000_000_000,
            }),
            |_| Ok(()),
        )
    }
    #[test]
    fn dispatch_wraps_from_maximum_to_one_without_reissuing_initial_zero() {
        let (mut s, controller, peer) = configured();
        let info = subscription(&mut s, controller, peer).subscription.unwrap();
        s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
            .unwrap();
        let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
        assert_eq!(initial.sequence, 0);
        s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Delivered, true, 0)
            .unwrap();
        Arc::make_mut(&mut s.renderer.events.subscriptions[0]).next_sequence = u32::MAX;
        let mute = command(&s, "first-change", Command::Mute { muted: false });
        s.submit(mute);
        let maximum = s
            .begin_renderer_delivery(&info.binding, 5_000_000_000)
            .unwrap();
        assert_eq!(maximum.sequence, u32::MAX);
        s.complete_renderer_delivery(
            &maximum,
            RendererDeliveryOutcome::Delivered,
            true,
            5_000_000_000,
        )
        .unwrap();
        let mute = command(&s, "second-change", Command::Mute { muted: true });
        s.submit(mute);
        let wrapped = s
            .begin_renderer_delivery(&info.binding, 10_000_000_000)
            .unwrap();
        assert_eq!(wrapped.sequence, 1);
        assert!(!wrapped.initial);
    }
    #[test]
    fn exhausted_delivery_identity_rejects_subscription_before_admission() {
        let (mut s, controller, peer) = configured();
        s.renderer.events.delivery_serial = u64::MAX;
        let before = s.snapshot();
        let result = subscription(&mut s, controller, peer);
        assert_eq!(result.receipt.status, ReceiptStatus::Rejected);
        assert_eq!(
            result.receipt.error.unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(s.snapshot(), before);
        assert_eq!(s.renderer_subscription_count(), 0);
    }
    #[test]
    fn admitted_initial_retains_its_delivery_identity_at_counter_exhaustion() {
        let (mut s, controller, peer) = configured();
        s.renderer.events.delivery_serial = u64::MAX - 1;
        let result = subscription(&mut s, controller, peer);
        assert_eq!(result.receipt.status, ReceiptStatus::Applied);
        let info = result.subscription.unwrap();
        assert_eq!(s.renderer.events.delivery_serial, u64::MAX);
        s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
            .unwrap();
        let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
        assert_eq!(initial.delivery_id, u64::MAX);
        assert_eq!(initial.sequence, 0);
    }
}
