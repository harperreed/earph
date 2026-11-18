// ABOUTME: Hosts one authoritative engine behind private local IPC.
// ABOUTME: Persists validated queue state before publishing command success.
#![forbid(unsafe_code)]
mod directory;
mod runtime;
mod transport;

use earph_core::{
    EngineError, ErrorCode,
    control::{ControlLimits, decode_command},
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
        let session = match runtime::read(&runtime.join("queue.json"), 2 * transport::MAX_BYTES)? {
            Some(bytes) => {
                let checkpoint: SessionCheckpoint =
                    serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
                Session::restore_owner(zone, owner, ControlLimits::default(), checkpoint)?
            }
            None => Session::new_owner(zone, owner, ControlLimits::default()),
        };
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

    pub fn serve(mut self) -> Result<(), EngineError> {
        loop {
            let (mut stream, _) = match self.listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(UNAVAILABLE),
            };
            let mut persistence_error = None;
            let reply = match transport::read_frame(&mut stream)
                .and_then(|bytes| decode_command(&bytes, &ControlLimits::default()))
            {
                Ok(command) => {
                    let runtime = &self.runtime;
                    let token = &self.token;
                    let receipt = self.session.submit_with_queue_expansion(
                        command,
                        directory::paths,
                        |staged| {
                            let bytes =
                                serde_json::to_vec(&staged.checkpoint()).map_err(|_| INVALID)?;
                            let result = runtime::write_atomic(
                                runtime,
                                "queue.json",
                                &format!("{token}-{}", staged.snapshot().revision),
                                &bytes,
                            );
                            if let Err(error) = result {
                                persistence_error = Some(error);
                            }
                            result
                        },
                    );
                    serde_json::to_vec(&serde_json::json!({ "receipt": receipt }))
                }
                Err(error) => serde_json::to_vec(&serde_json::json!({ "error": error })),
            }
            .map_err(|_| INVALID)?;
            // A disconnected controller cannot undo a committed operation.
            let _ = transport::write_frame(&mut stream, &reply);
            if let Some(error) = persistence_error {
                return Err(error);
            }
            if self.session.shutdown_requested() {
                return Ok(());
            }
        }
    }
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
