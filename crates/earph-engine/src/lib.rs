// ABOUTME: Hosts one authoritative engine behind private local IPC.
// ABOUTME: Persists validated queue state before publishing command success.
#![forbid(unsafe_code)]
pub mod assets;
mod build_provenance;
pub use build_provenance::build_provenance;
mod devices;
mod directory;
pub use devices::{DeviceDiscoveryHelper, run_helper as run_device_query_helper};
mod graph;
mod host;
mod ipc;
pub mod network_decode;
mod network_source;
pub mod offline_render;
mod output;
mod output_replay;
mod plan;
mod profiles;
mod render;
mod renderer;
pub mod sendspin_credentials;
mod sendspin_runtime;
pub use sendspin_runtime::SendspinConfiguration;
pub mod sendspin_schedule;
pub use renderer::{RendererConfiguration, RendererListen};
mod runtime;
pub mod stream_render;
mod transport;
mod worker;

use earph_core::{
    EngineError, ErrorCode,
    control::ControlLimits,
    ids::{OwnerId, ZoneId},
    session::{Session, SessionCheckpoint},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, TryLockError},
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixListener,
    },
    path::{Component, Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid local owner document");
const UNAVAILABLE: EngineError =
    EngineError::new(ErrorCode::DeviceUnavailable, "local owner is unavailable");

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    schema_version: u32,
    socket: String,
}

pub struct EngineOwner {
    _lock: runtime::OwnerLock,
    listener: UnixListener,
    runtime: PathBuf,
    token: String,
    session: Session,
    device_helper: Option<DeviceDiscoveryHelper>,
    renderer: Option<renderer::RendererRuntime>,
    sendspin: Option<sendspin_runtime::SendspinRuntime>,
}

impl EngineOwner {
    /// Enables the Sendspin player source for one server.
    /// No connection opens until a controller starts a Sendspin session.
    pub fn with_sendspin(
        mut self,
        configuration: SendspinConfiguration,
    ) -> Result<Self, EngineError> {
        if self.sendspin.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "Sendspin server already configured",
            ));
        }
        let owner = self.session.snapshot().owner_id.ok_or(INVALID)?;
        self.sendspin = Some(sendspin_runtime::SendspinRuntime::new(
            configuration,
            owner,
        )?);
        self.session.enable_sendspin();
        Ok(self)
    }
    /// Binds trusted ingress before enabling Core admission. Grants remain explicit local commands.
    pub fn with_renderer(
        mut self,
        configuration: RendererConfiguration,
    ) -> Result<Self, EngineError> {
        if self.renderer.is_some() {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "renderer listener already bound",
            ));
        }
        let (renderer, listener) = configuration.bind()?;
        self.session.enable_renderer_control();
        self.session.set_renderer_listener(Some(listener));
        self.renderer = Some(renderer::RendererRuntime::new(renderer, &self.session));
        Ok(self)
    }
    /// Starts the renderer that the owner command line selects.
    /// It uses the device identity that this runtime keeps across restarts.
    pub fn with_renderer_listen(self, listen: RendererListen) -> Result<Self, EngineError> {
        let (configuration, created_udn) = listen.configuration(&self.runtime)?;
        let owner = self.with_renderer(configuration)?;
        if let Some(udn) = created_udn {
            renderer::store_udn(&owner.runtime, &owner.token, &udn)?;
        }
        Ok(owner)
    }
    /// Reports where the renderer listens and whether it runs discovery.
    pub fn renderer_listener(&self) -> Option<earph_core::renderer::RendererListener> {
        self.session.renderer_snapshot().listener
    }
    pub fn renderer_address(&self) -> Option<std::net::SocketAddrV4> {
        self.renderer.as_ref().map(|renderer| renderer.local_addr())
    }
    /// Enables discovery only when this host explicitly supplies the production engine helper.
    pub fn with_device_discovery(mut self, helper: DeviceDiscoveryHelper) -> Self {
        if let Some(backend) = helper.backend() {
            self.session.enable_device_discovery(backend);
            self.device_helper = Some(helper);
        }
        self
    }
    pub fn snapshot(&self) -> earph_core::session::SessionSnapshot {
        self.session.snapshot()
    }
    pub fn bind(runtime: &Path) -> Result<Self, EngineError> {
        let runtime = runtime::directory(runtime, true)?;
        let lock =
            match runtime::OwnerLock::try_acquire(runtime::open_lock(&runtime.join("owner.lock"))?)
            {
                Ok(lock) => lock,
                Err(TryLockError::WouldBlock) => {
                    return Err(EngineError::new(
                        ErrorCode::SourceBusy,
                        "another local owner holds this runtime",
                    ));
                }
                Err(TryLockError::Error(_)) => return Err(runtime::IO),
            };
        let generation = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| runtime::IO)?
            .as_nanos();
        let mut bound = None;
        for attempt in 0..64 {
            let token = format!("o-{:x}-{generation:x}-{attempt:x}", std::process::id());
            let socket = format!("{token}.sock");
            match UnixListener::bind(runtime.join(&socket)) {
                Ok(listener) => {
                    bound = Some((token, socket, listener));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {}
                Err(_) => return Err(runtime::IO),
            }
        }
        let (token, socket, listener) = bound.ok_or(runtime::IO)?;
        fs::set_permissions(runtime.join(&socket), fs::Permissions::from_mode(0o600))
            .map_err(|_| runtime::IO)?;
        let zone = ZoneId::new("main")?;
        let owner = OwnerId::new(token.clone())?;
        let mut session = match runtime::read_checkpoint(&runtime, &token)? {
            Some(bytes) => {
                let checkpoint: SessionCheckpoint =
                    serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
                Session::restore_owner(zone, owner, ControlLimits::default(), checkpoint)?
            }
            None => Session::new_owner(zone, owner, ControlLimits::default()),
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        session.enable_local_playback();
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        session.enable_network_playback();
        let records = profiles::restore(&runtime, session.profile_references())?;
        session.restore_profile_records(records)?;
        session = session.with_diagnostic_build(build_provenance())?;
        let endpoint = serde_json::to_vec(&Endpoint {
            schema_version: 1,
            socket,
        })
        .map_err(|_| INVALID)?;
        runtime::write_atomic(&runtime, "endpoint.json", &token, &endpoint)?;
        Ok(Self {
            _lock: lock,
            listener,
            runtime,
            token,
            session,
            device_helper: None,
            renderer: None,
            sendspin: None,
        })
    }

    pub fn serve(self) -> Result<(), EngineError> {
        let Self {
            _lock,
            listener,
            runtime,
            token,
            session,
            device_helper,
            renderer,
            sendspin,
        } = self;
        let _owner_lock = _lock;
        host::serve(
            session,
            listener,
            runtime,
            token,
            device_helper,
            renderer,
            sendspin,
        )
    }
}

/// Returns the shared local command and response frame limit in bytes.
pub const fn control_frame_limit() -> usize {
    transport::MAX_BYTES
}

/// Sends one shared command document to the existing owner.
pub fn request(runtime: &Path, command: &[u8]) -> Result<Vec<u8>, EngineError> {
    let runtime = runtime::directory(runtime, false)?;
    let bytes = runtime::read(&runtime.join("endpoint.json"), 256)?.ok_or(UNAVAILABLE)?;
    let endpoint: Endpoint = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    let components: Vec<_> = Path::new(&endpoint.socket).components().collect();
    if endpoint.schema_version != 1
        || components.len() != 1
        || !matches!(components[0], Component::Normal(_))
        || !endpoint.socket.starts_with("o-")
        || !endpoint.socket.ends_with(".sock")
        || endpoint.socket.len() > 64
    {
        return Err(INVALID);
    }
    let socket = runtime.join(endpoint.socket);
    let metadata = fs::symlink_metadata(&socket).map_err(|_| UNAVAILABLE)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o7777 != 0o600
    {
        return Err(EngineError::new(
            ErrorCode::AuthDenied,
            "owner socket is not private",
        ));
    }
    let mut stream = transport::connect(&socket)?;
    transport::write_frame(&mut stream, command)?;
    transport::read_frame(&mut stream)
}
