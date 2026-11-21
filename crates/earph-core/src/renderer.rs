// ABOUTME: Defines bounded renderer control grants and redacted source ownership facts.
// ABOUTME: Keeps controller authority separate from media fetch grants and protocol listeners.
use crate::{
    EngineError, ErrorCode,
    ids::*,
    network_source::{BUDGET, NetworkSourceBinding},
};
use serde::{Deserialize, Serialize};
use std::{net::IpAddr, sync::Arc};

pub const MAX_RENDERER_CONTROLLERS: usize = 8;
const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid renderer grants");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererRight {
    Stage,
    Transport,
    RenderingControl,
    Takeover,
    Events,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum RendererAction {
    StageUri {
        expected_staging_generation: u64,
        uri: String,
        metadata: String,
    },
    Play {
        media_id: StagedMediaId,
        staging_generation: u64,
        grant_generation: u64,
        device_id: DeviceId,
    },
    Stop {
        #[serde(deserialize_with = "crate::serde_helpers::nullable")]
        binding: Option<NetworkSourceBinding>,
    },
    SetVolume {
        binding: NetworkSourceBinding,
        volume: u16,
    },
    SetVolumeDb {
        binding: NetworkSourceBinding,
        volume_db: i16,
    },
    FactoryDefaults {
        binding: NetworkSourceBinding,
    },
    SetMute {
        binding: NetworkSourceBinding,
        muted: bool,
    },
    Pause {
        binding: NetworkSourceBinding,
    },
    Resume {
        binding: NetworkSourceBinding,
    },
}

impl std::fmt::Debug for RendererAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RendererAction")
            .field("kind", &std::mem::discriminant(self))
            .finish_non_exhaustive()
    }
}

impl RendererAction {
    /// Reserves safety delivery for exact zero and presets which always mute.
    pub fn is_safety(&self) -> bool {
        match self {
            Self::Stop { .. }
            | Self::FactoryDefaults { .. }
            | Self::SetMute { muted: true, .. } => true,
            Self::SetVolume { volume, .. } => *volume == 0,
            Self::SetVolumeDb { volume_db, .. } => {
                crate::remote_volume::RemoteVolume::from_volume_db(*volume_db)
                    .is_ok_and(|volume| volume.volume() == 0)
            }
            Self::StageUri { .. }
            | Self::Play { .. }
            | Self::SetMute { muted: false, .. }
            | Self::Pause { .. }
            | Self::Resume { .. } => false,
        }
    }
    pub(crate) fn required_right(&self) -> RendererRight {
        match self {
            Self::StageUri { .. } => RendererRight::Stage,
            Self::Play { .. } | Self::Stop { .. } | Self::Pause { .. } | Self::Resume { .. } => {
                RendererRight::Transport
            }
            Self::SetVolume { .. }
            | Self::SetVolumeDb { .. }
            | Self::FactoryDefaults { .. }
            | Self::SetMute { .. } => RendererRight::RenderingControl,
        }
    }

    pub(crate) fn retained_bytes(&self) -> Result<usize, EngineError> {
        match self {
            Self::StageUri { uri, metadata, .. } => {
                crate::source::validate_storage(uri, metadata)?;
                Ok(uri.capacity() + metadata.capacity())
            }
            Self::Play {
                media_id,
                device_id,
                ..
            } => {
                if media_id.capacity_bytes() > 128 || device_id.capacity_bytes() > 128 {
                    return Err(BUDGET);
                }
                Ok(media_id.capacity_bytes() + device_id.capacity_bytes())
            }
            Self::Stop { binding } => {
                if let Some(binding) = binding {
                    binding.validate()?;
                }
                Ok(binding.as_ref().map_or(0, |b| {
                    b.owner_id.capacity_bytes()
                        + b.session_id.capacity_bytes()
                        + b.media_id.capacity_bytes()
                }))
            }
            Self::SetVolume { binding, .. }
            | Self::SetVolumeDb { binding, .. }
            | Self::FactoryDefaults { binding }
            | Self::SetMute { binding, .. }
            | Self::Pause { binding }
            | Self::Resume { binding } => {
                binding.validate()?;
                Ok(binding.owner_id.capacity_bytes()
                    + binding.session_id.capacity_bytes()
                    + binding.media_id.capacity_bytes())
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererControllerGrant {
    pub id: RendererControllerId,
    pub peers: Vec<IpAddr>,
    pub rights: Vec<RendererRight>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub playback: Option<Arc<RendererPlaybackAuthorization>>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererGrantDocument {
    pub controllers: Vec<RendererControllerGrant>,
}
impl std::fmt::Debug for RendererControllerGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RendererControllerGrant([redacted])")
    }
}
impl std::fmt::Debug for RendererGrantDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RendererGrantDocument([redacted])")
    }
}

impl RendererGrantDocument {
    pub(crate) fn shallow_bytes(&self) -> Result<usize, EngineError> {
        if self.controllers.capacity() > MAX_RENDERER_CONTROLLERS {
            return Err(BUDGET);
        }
        let mut bytes = size_of::<Self>()
            + 16
            + self.controllers.capacity() * size_of::<RendererControllerGrant>();
        for controller in &self.controllers {
            if controller.id.capacity_bytes() > 128
                || controller.peers.capacity() > 8
                || controller.rights.capacity() > 8
            {
                return Err(BUDGET);
            }
            bytes = bytes
                .checked_add(
                    controller.id.capacity_bytes()
                        + controller.peers.capacity() * size_of::<IpAddr>()
                        + controller.rights.capacity() * size_of::<RendererRight>(),
                )
                .ok_or(BUDGET)?;
        }
        // Admission counts shared playback allocations separately through AuthorityBytes.
        for controller in &self.controllers {
            if let Some(playback) = &controller.playback {
                playback.shallow_bytes()?;
            }
        }
        Ok(bytes)
    }

    pub fn retained_bytes(&self) -> Result<usize, EngineError> {
        let mut bytes = self.shallow_bytes()?;
        let mut authorizations = std::collections::BTreeSet::new();
        let mut intents = std::collections::BTreeSet::new();
        let mut graphs = std::collections::BTreeSet::new();
        for playback in self.controllers.iter().filter_map(|c| c.playback.as_ref()) {
            if authorizations.insert(Arc::as_ptr(playback) as usize) {
                bytes = bytes.checked_add(playback.shallow_bytes()?).ok_or(BUDGET)?;
            }
            if intents.insert(Arc::as_ptr(&playback.intent) as usize) {
                bytes = bytes
                    .checked_add(size_of::<crate::graph_control::GraphIntent>() + 16)
                    .ok_or(BUDGET)?;
            }
            if graphs.insert(Arc::as_ptr(&playback.intent.graph) as usize) {
                bytes = bytes
                    .checked_add(playback.intent.graph.retained_capacity_bytes()? + 16)
                    .ok_or(BUDGET)?;
            }
        }
        Ok(bytes)
    }
    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        self.retained_bytes()?;
        if self.controllers.is_empty() {
            return Err(INVALID);
        }
        for (index, controller) in self.controllers.iter().enumerate() {
            if self.controllers[..index]
                .iter()
                .any(|c| c.id == controller.id)
                || controller.peers.is_empty()
                || controller.rights.is_empty()
                || controller.peers.iter().enumerate().any(|(i, p)| {
                    p.is_unspecified() || p.is_multicast() || controller.peers[..i].contains(p)
                })
                || controller
                    .rights
                    .iter()
                    .enumerate()
                    .any(|(i, r)| controller.rights[..i].contains(r))
                || controller.rights.contains(&RendererRight::Takeover)
                    && !controller.rights.contains(&RendererRight::Transport)
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererStagedAssociation {
    pub controller_id: RendererControllerId,
    pub grant_generation: u64,
    pub media_id: StagedMediaId,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererSourceOwnership {
    pub controller_id: RendererControllerId,
    pub grant_generation: u64,
    pub binding: NetworkSourceBinding,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub retirement_request: Option<RequestId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererControllerSummary {
    pub id: RendererControllerId,
    pub generation: u64,
    pub expires_monotonic_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RendererSnapshotDocument")]
pub struct RendererSnapshot {
    pub enabled: bool,
    pub grant_generation: u64,
    pub controllers: Vec<RendererControllerSummary>,
    pub controller_count: usize,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub staged: Option<RendererStagedAssociation>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub active: Option<RendererSourceOwnership>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    pub closing: Option<RendererSourceOwnership>,
    pub retained_bytes: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RendererSnapshotDocument {
    enabled: bool,
    grant_generation: u64,
    controllers: Vec<RendererControllerSummary>,
    controller_count: usize,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    staged: Option<RendererStagedAssociation>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    active: Option<RendererSourceOwnership>,
    #[serde(deserialize_with = "crate::serde_helpers::nullable")]
    closing: Option<RendererSourceOwnership>,
    retained_bytes: u64,
}

impl TryFrom<RendererSnapshotDocument> for RendererSnapshot {
    type Error = EngineError;

    fn try_from(d: RendererSnapshotDocument) -> Result<Self, Self::Error> {
        let generation_valid = |generation| generation > 0 && generation <= d.grant_generation;
        if d.controllers.len() > MAX_RENDERER_CONTROLLERS
            || d.controller_count != d.controllers.len()
            || d.retained_bytes > crate::network_source::MAX_NETWORK_AUTHORITY_BYTES as u64
            || !d.enabled
                && (d.grant_generation != 0
                    || d.controller_count != 0
                    || d.staged.is_some()
                    || d.active.is_some()
                    || d.closing.is_some())
            || d.controllers.iter().enumerate().any(|(i, c)| {
                !generation_valid(c.generation)
                    || c.expires_monotonic_ns == 0
                    || d.controllers[..i].iter().any(|p| p.id == c.id)
            })
            || d.staged.as_ref().is_some_and(|s| {
                !generation_valid(s.grant_generation)
                    || s.generation == 0
                    || s.media_id.as_str() != format!("staged-{}", s.generation)
            })
            || d.active
                .iter()
                .chain(d.closing.iter())
                .any(|o| !generation_valid(o.grant_generation) || o.binding.validate().is_err())
            || d.active
                .as_ref()
                .zip(d.closing.as_ref())
                .is_some_and(|(a, c)| a.binding == c.binding)
        {
            return Err(INVALID);
        }
        Ok(Self {
            enabled: d.enabled,
            grant_generation: d.grant_generation,
            controllers: d.controllers,
            controller_count: d.controller_count,
            staged: d.staged,
            active: d.active,
            closing: d.closing,
            retained_bytes: d.retained_bytes,
        })
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RendererPlaybackAuthorization {
    pub intent: Arc<crate::graph_control::GraphIntent>,
    pub output: crate::control::OutputConfiguration,
    pub expected_device_epoch: u64,
}
impl std::fmt::Debug for RendererPlaybackAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RendererPlaybackAuthorization([redacted])")
    }
}
impl RendererPlaybackAuthorization {
    pub fn authored_sha256(&self) -> Result<[u8; 32], EngineError> {
        use sha2::{Digest, Sha256};
        self.intent.graph.validate_structure()?;
        let mut digest = Sha256::new();
        digest.update(b"earph.renderer.authored-intent.v1\0");
        // Stream serialized intent into the digest, without another retained graph allocation.
        struct HashWriter<'a>(&'a mut Sha256);
        impl std::io::Write for HashWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(HashWriter(&mut digest), &self.intent).map_err(|_| INVALID)?;
        Ok(digest.finalize().into())
    }
    pub(crate) fn shallow_bytes(&self) -> Result<usize, EngineError> {
        if self.output.endpoint.capacity() > 256
            || self.output.device_id.capacity_bytes() > 128
            || self.output.profile_id.capacity_bytes() > 128
        {
            return Err(BUDGET);
        }
        self.intent.graph.validate_storage()?;
        Ok(size_of::<Self>()
            + 16
            + self.output.endpoint.capacity()
            + self.output.device_id.capacity_bytes()
            + self.output.profile_id.capacity_bytes())
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct RendererGraphBinding {
    pub owner_id: OwnerId,
    pub renderer_epoch: u64,
    pub attempt_generation: u64,
    pub controller_id: RendererControllerId,
    pub controller_generation: u64,
    pub source: NetworkSourceBinding,
    pub source_epochs: crate::ids::Epochs,
    pub authored_sha256: [u8; 32],
    pub authorization: Arc<RendererPlaybackAuthorization>,
}
impl std::fmt::Debug for RendererGraphBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RendererGraphBinding([redacted])")
    }
}

#[derive(Clone, Default)]
pub(crate) struct RendererControl {
    pub enabled: bool,
    pub epoch: u64,
    pub graph_serial: u64,
    pub events: crate::renderer_events::RendererEventControl,
    pub continuation: Option<Arc<RendererGraphBinding>>,
    pub generation: u64,
    pub grants: Vec<Arc<RendererAuthority>>,
    pub staged: Option<Arc<RendererStagedAssociation>>,
    pub active: Option<Arc<RendererSourceOwnership>>,
    pub closing: Option<Arc<RendererSourceOwnership>>,
}

pub(crate) struct RendererAuthority {
    pub generation: u64,
    pub expires_monotonic_ns: u64,
    pub document: Arc<RendererGrantDocument>,
    pub index: usize,
}

impl RendererAuthority {
    pub(crate) fn grant(&self) -> &RendererControllerGrant {
        &self.document.controllers[self.index]
    }
}
