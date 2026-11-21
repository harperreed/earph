// ABOUTME: Binds local saved intent to current owner-observed native device identities.
// ABOUTME: Persists bounded associations without granting output or activation authority.
use super::*;
use crate::{
    profile_control::*,
    profiles::{MAX_PROFILE_ASSOCIATIONS, ProfileAssociation, ProfileObjectHash},
};

impl Session {
    pub(super) fn observed_profile_device(&self) -> Result<&BackendObservation, EngineError> {
        let unavailable = EngineError::new(
            ErrorCode::DeviceUnavailable,
            "profile association requires the current opened backend",
        );
        if self.state.session_id.is_none()
            || self.state.source != Some(SourceKind::LocalFile)
            || self.local_boundary.is_some()
            || !matches!(
                self.state.transport,
                TransportState::Buffering | TransportState::Playing | TransportState::Paused
            )
        {
            return Err(unavailable);
        }
        let output = self.state.output.as_ref().ok_or(unavailable)?;
        let backend = self
            .state
            .playback
            .as_ref()
            .and_then(|p| p.backend.as_ref())
            .ok_or(unavailable)?;
        if backend.backend != output.backend {
            return Err(unavailable);
        }
        Ok(backend)
    }
    pub(super) fn profile_native_identity(&self) -> Option<ProfileObjectHash> {
        let backend = self.state.playback.as_ref()?.backend.as_ref()?;
        (backend.backend == OutputBackend::CoreAudio
            && self.state.output.as_ref()?.backend == backend.backend)
            .then_some(backend.device_identity_sha256)
            .flatten()
    }
    pub(super) fn validate_profile_associations(&self, loaded: bool) -> Result<(), EngineError> {
        let associations = &self.profiles.associations;
        if associations.capacity() > MAX_PROFILE_ASSOCIATIONS {
            return Err(BUDGET);
        }
        let records = if loaded {
            Some(self.profile_records()?)
        } else {
            None
        };
        for (index, association) in associations.iter().enumerate() {
            association.retained_capacity_bytes()?;
            if association.backend != OutputBackend::CoreAudio
                || associations[..index].contains(association)
                || !self
                    .profiles
                    .references
                    .iter()
                    .any(|r| r.id == association.profile_id)
            {
                return Err(INVALID);
            }
            if let Some(records) = &records {
                let record = records
                    .iter()
                    .find(|r| r.profile.id == association.profile_id)
                    .ok_or(INVALID)?;
                if association.backend != record.profile.output.backend
                    || association.route != record.profile.output.route
                {
                    return Err(INVALID);
                }
            }
        }
        Ok(())
    }
    pub(super) fn apply_profile_bind(
        &mut self,
        command: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        let Command::ProfilesBind {
            id,
            expected_profile_revision,
            profile_sha256,
            expected_device_epoch,
        } = &command.payload
        else {
            return Err(STATE);
        };
        if command.session_id.is_none()
            || command.session_id != self.state.session_id
            || *expected_device_epoch != self.state.device_epoch
        {
            return Err(STALE);
        }
        let graph = self.graph_inspection();
        if self.profile_worker_busy()
            || self.profile_work().is_some()
            || graph.pending_request_id.is_some()
            || graph.prepared.is_some()
        {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "another operation owns the profile preparation slot",
            ));
        }
        let record = self.profile_record(&ProfileCatalogReference {
            id: id.clone(),
            revision: *expected_profile_revision,
            profile_sha256: *profile_sha256,
        })?;
        let backend = self.observed_profile_device()?;
        let identity = if backend.backend == OutputBackend::CoreAudio {
            backend.device_identity_sha256
        } else {
            None
        }
        .ok_or(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "selected backend has no stable native identity for profile association",
        ))?;
        let output = self.state.output.as_ref().ok_or(STATE)?;
        if record.profile.output.backend != output.backend
            || record.profile.output.route != output.route
            || record.profile.output.device_id != output.device_id
        {
            return Err(EngineError::new(
                ErrorCode::DeviceUnavailable,
                "profile association targets another output",
            ));
        }
        let association = ProfileAssociation {
            backend: output.backend,
            route: output.route,
            device_identity_sha256: identity,
            profile_id: id.clone(),
        };
        if !self.profiles.associations.contains(&association) {
            if self.profiles.associations.len() == MAX_PROFILE_ASSOCIATIONS {
                return Err(BUDGET);
            }
            self.profiles
                .associations
                .try_reserve_exact(1)
                .map_err(|_| BUDGET)?;
            self.profiles.associations.push(association);
        }
        self.validate_profile_associations(true)?;
        self.state.revision = increment(self.state.revision)?;
        self.state.event_sequence = self.next_session_event_sequence(false)?;
        self.state.monotonic_ns = now_ns;
        self.validate_profile_storage(Some(command))?;
        if serde_json::to_vec(&self.checkpoint())
            .map_err(|_| INVALID)?
            .len()
            > 2 * 1024 * 1024
        {
            return Err(BUDGET);
        }
        self.log(Some(command.request_id.clone()));
        Ok(ReceiptStatus::Applied)
    }
    pub(super) fn profile_bound_result(&self, id: &ProfileId) -> Option<QueryResult> {
        let output = self.state.output.as_ref()?;
        let identity = self.profile_native_identity()?;
        self.profiles
            .associations
            .iter()
            .find(|a| {
                &a.profile_id == id
                    && a.backend == output.backend
                    && a.route == output.route
                    && a.device_identity_sha256 == identity
            })
            .cloned()
            .map(QueryResult::ProfileBound)
    }
}
