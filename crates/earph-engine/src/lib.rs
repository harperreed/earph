// ABOUTME: Hosts one authoritative engine behind private local IPC.
// ABOUTME: Persists validated queue state before publishing command success.
#![forbid(unsafe_code)]
pub mod assets;
mod directory;
mod graph;
mod host;
mod ipc;
pub mod offline_render;
mod output;
mod output_replay;
mod plan;
mod profiles;
mod render;
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
    fs::{self, File, TryLockError},
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
    _lock: File,
    listener: UnixListener,
    runtime: PathBuf,
    token: String,
    session: Session,
}

impl EngineOwner {
    pub fn snapshot(&self) -> earph_core::session::SessionSnapshot {
        self.session.snapshot()
    }
    pub fn bind(runtime: &Path) -> Result<Self, EngineError> {
        let runtime = runtime::directory(runtime, true)?;
        let lock = runtime::open_lock(&runtime.join("owner.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(EngineError::new(
                    ErrorCode::SourceBusy,
                    "another local owner holds this runtime",
                ));
            }
            Err(TryLockError::Error(_)) => return Err(runtime::IO),
        }
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
        let records = profiles::restore(&runtime, session.profile_references())?;
        session.restore_profile_records(records)?;
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
        })
    }

    pub fn serve(self) -> Result<(), EngineError> {
        let Self {
            _lock,
            listener,
            runtime,
            token,
            session,
        } = self;
        let _owner_lock = _lock;
        host::serve(session, listener, runtime, token)
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
