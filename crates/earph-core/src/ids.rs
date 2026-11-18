// ABOUTME: Defines opaque identities and generation counters for the engine.
// ABOUTME: Rejects malformed identifiers and counter wrap before state changes.
use crate::{EngineError, ErrorCode};
use serde::{Deserialize, Serialize};

macro_rules! identity {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, EngineError> {
                let value = value.into();
                if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                    return Err(EngineError::new(ErrorCode::AssetInvalid, "invalid opaque identifier"));
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str { &self.0 }
        }
        impl TryFrom<String> for $name {
            type Error = EngineError;
            fn try_from(value: String) -> Result<Self, Self::Error> { Self::new(value) }
        }
        impl From<$name> for String { fn from(id: $name) -> Self { id.0 } }
    )+};
}
identity!(
    ZoneId,
    SessionId,
    DeviceId,
    ProfileId,
    GraphId,
    NodeId,
    PreparedGraphId,
    RequestId,
    OwnerId,
    QueueEntryId
);

impl NodeId {
    /// Reports retained UTF-8 storage capacity for graph memory admission.
    pub fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl PreparedGraphId {
    /// Reports retained token storage before command receipt admission.
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl ProfileId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl DeviceId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl RequestId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl OwnerId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl SessionId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}
impl ZoneId {
    pub(crate) fn capacity_bytes(&self) -> usize {
        self.0.capacity()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Epochs {
    pub session: u64,
    pub stream: u64,
}
impl Epochs {
    pub fn next_session(self) -> Result<Self, EngineError> {
        Ok(Self {
            session: increment(self.session)?,
            stream: 0,
        })
    }
    pub fn next_stream(self) -> Result<Self, EngineError> {
        Ok(Self {
            stream: increment(self.stream)?,
            ..self
        })
    }
}
pub(crate) fn increment(value: u64) -> Result<u64, EngineError> {
    value.checked_add(1).ok_or(EngineError::new(
        ErrorCode::ResourceBudgetExceeded,
        "counter exhausted",
    ))
}
