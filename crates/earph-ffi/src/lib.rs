// ABOUTME: Exposes a bounded control client without owning an engine or output.
// ABOUTME: Preserves shared command bytes, owner replies, and failure uncertainty.
use earph_core::{EngineError, ErrorCode};
use std::{fmt, path::Path, sync::Arc};

uniffi::setup_scaffolding!();

const MAX_RETAINED_RUNTIME_BYTES: usize = 60;

#[derive(Debug, uniffi::Error)]
pub enum ClientError {
    Engine { code: String, detail: String },
}
impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self::Engine { code, detail } = self;
        write!(f, "{code}: {detail}")
    }
}
impl std::error::Error for ClientError {}
impl From<EngineError> for ClientError {
    fn from(error: EngineError) -> Self {
        let value = serde_json::to_value(error.code)
            .expect("Core's unit error variants serialize without fallible values");
        let code = value
            .as_str()
            .expect("Core error codes serialize as strings")
            .to_owned();
        Self::Engine {
            code,
            detail: error.message.to_owned(),
        }
    }
}

/// A control client only; releasing it never stops an attached owner.
#[derive(uniffi::Object)]
pub struct LocalClient {
    runtime: Box<str>,
}

#[uniffi::export]
impl LocalClient {
    /// Retains a bounded path without creating an owner or contacting an endpoint.
    #[uniffi::constructor]
    pub fn new(runtime_directory: String) -> Result<Arc<Self>, ClientError> {
        if runtime_directory.is_empty() {
            return Err(EngineError::new(ErrorCode::AssetInvalid, "runtime path is empty").into());
        }
        if runtime_directory.len() > MAX_RETAINED_RUNTIME_BYTES {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "client runtime path exceeds storage limit",
            )
            .into());
        }
        // A boxed string drops excess caller capacity before the client retains the path.
        Ok(Arc::new(Self {
            runtime: runtime_directory.into_boxed_str(),
        }))
    }

    /// Forwards one synchronous JSON control frame; run this outside a foreign UI thread.
    pub fn request(&self, command: Vec<u8>) -> Result<Vec<u8>, ClientError> {
        if command.is_empty() || command.len() > earph_engine::control_frame_limit() {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "IPC frame exceeds byte limit",
            )
            .into());
        }
        earph_engine::request(Path::new(&*self.runtime), &command).map_err(Into::into)
    }
}
