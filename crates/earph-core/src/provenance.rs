// ABOUTME: Defines bounded compiled-package facts without reading the environment or claiming executable identity.
// ABOUTME: Keeps unavailable facts explicit and separates Engine records from harness records.
use crate::{EngineError, ErrorCode, profiles::ProfileObjectHash, serde_helpers::nullable};
use serde::{Deserialize, Serialize};

pub const MAX_BUILD_PROVENANCE_BYTES: usize = 2048;
pub const BUILD_INPUT_SET_VERSION: u32 = 1;
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid compiled build provenance");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct GitCommit([u8; 40]);
impl TryFrom<String> for GitCommit {
    type Error = EngineError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 40
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(INVALID);
        }
        Ok(Self(value.as_bytes().try_into().map_err(|_| INVALID)?))
    }
}
impl From<GitCommit> for String {
    fn from(value: GitCommit) -> Self {
        String::from_utf8(value.0.to_vec()).expect("validated ASCII commit")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildFieldReason {
    NotProvided,
    UnsupportedValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "T: Deserialize<'de>"))]
pub struct BuildField<T> {
    #[serde(deserialize_with = "nullable")]
    pub value: Option<T>,
    #[serde(deserialize_with = "nullable")]
    pub unavailable_reason: Option<BuildFieldReason>,
}
impl<T> BuildField<T> {
    pub const fn available(value: T) -> Self {
        Self {
            value: Some(value),
            unavailable_reason: None,
        }
    }
    pub const fn unavailable(reason: BuildFieldReason) -> Self {
        Self {
            value: None,
            unavailable_reason: Some(reason),
        }
    }
    fn valid(&self) -> bool {
        self.value.is_some() != self.unavailable_reason.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildProducer {
    EnginePackage,
    Harness,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RustChannel {
    Stable,
    Beta,
    Nightly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustCompiler {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub channel: RustChannel,
    pub commit: BuildField<GitCommit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildTarget {
    #[serde(rename = "x86_64-unknown-linux-gnu")]
    X86_64UnknownLinuxGnu,
    #[serde(rename = "aarch64-unknown-linux-gnu")]
    Aarch64UnknownLinuxGnu,
    #[serde(rename = "aarch64-apple-darwin")]
    Aarch64AppleDarwin,
    #[serde(rename = "x86_64-apple-darwin")]
    X86_64AppleDarwin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoProfileClass {
    Debug,
    Release,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompiledBuildRecord {
    pub producer: BuildProducer,
    pub input_set_version: u32,
    pub commit: BuildField<GitCommit>,
    pub source_dirty: BuildField<bool>,
    pub dependency_lock_sha256: ProfileObjectHash,
    pub source_inputs_sha256: ProfileObjectHash,
    pub compiler: BuildField<RustCompiler>,
    pub target: BuildField<BuildTarget>,
    pub cargo_profile_class: BuildField<CargoProfileClass>,
}
impl CompiledBuildRecord {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.input_set_version != BUILD_INPUT_SET_VERSION
            || !self.commit.valid()
            || !self.source_dirty.valid()
            || !self.compiler.valid()
            || !self.target.valid()
            || !self.cargo_profile_class.valid()
            || self
                .compiler
                .value
                .as_ref()
                .is_some_and(|c| c.major == 0 || !c.commit.valid())
        {
            return Err(INVALID);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildUnavailableReason {
    NotProvided,
    InvalidRecord,
    UnsupportedRecord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunningBuildProvenance {
    pub schema_version: u32,
    pub state: BuildState,
    #[serde(deserialize_with = "nullable")]
    pub unavailable_reason: Option<BuildUnavailableReason>,
    #[serde(deserialize_with = "nullable")]
    pub record: Option<CompiledBuildRecord>,
}
impl RunningBuildProvenance {
    pub const fn unavailable(reason: BuildUnavailableReason) -> Self {
        Self {
            schema_version: 1,
            state: BuildState::Unavailable,
            unavailable_reason: Some(reason),
            record: None,
        }
    }
    pub fn available(record: CompiledBuildRecord) -> Result<Self, EngineError> {
        record.validate()?;
        Ok(Self {
            schema_version: 1,
            state: BuildState::Available,
            unavailable_reason: None,
            record: Some(record),
        })
    }
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.schema_version != 1 {
            return Err(INVALID);
        }
        match (self.state, self.unavailable_reason, &self.record) {
            (BuildState::Available, None, Some(record)) => record.validate(),
            (BuildState::Unavailable, Some(_), None) => Ok(()),
            _ => Err(INVALID),
        }
    }
    pub fn validate_for_engine(&self) -> Result<(), EngineError> {
        self.validate()?;
        if self
            .record
            .as_ref()
            .is_some_and(|record| record.producer != BuildProducer::EnginePackage)
        {
            return Err(INVALID);
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, EngineError> {
        if bytes.len() > MAX_BUILD_PROVENANCE_BYTES {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "compiled build provenance exceeds its bound",
            ));
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        value.validate()?;
        Ok(value)
    }
    /// Every retained member has fixed storage; no producer text or spare heap capacity is retained.
    pub fn retained_capacity_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}
