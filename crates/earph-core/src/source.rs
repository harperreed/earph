// ABOUTME: Bounds untrusted URI metadata without admitting a playable source.
// ABOUTME: Exposes staged identities and byte counts while keeping raw input in owner memory.
use crate::{
    EngineError, ErrorCode,
    ids::{StagedMediaId, increment},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub const MAX_STAGED_URI_BYTES: usize = 8192;
pub const MAX_STAGED_METADATA_BYTES: usize = 65536;
const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid staged URI metadata");
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "staged URI metadata exceeds storage limits",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAdmission {
    Unadmitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedUriSummary {
    pub media_id: StagedMediaId,
    pub admission: SourceAdmission,
    pub uri_bytes: usize,
    pub metadata_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StagingDocument")]
pub struct SourceStagingSnapshot {
    pub generation: u64,
    pub staged: Option<StagedUriSummary>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StagingDocument {
    generation: u64,
    #[serde(deserialize_with = "Option::deserialize")]
    staged: Option<StagedUriSummary>,
}

impl TryFrom<StagingDocument> for SourceStagingSnapshot {
    type Error = EngineError;

    fn try_from(document: StagingDocument) -> Result<Self, Self::Error> {
        if let Some(media) = &document.staged
            && (document.generation == 0
                || media.media_id.as_str() != format!("staged-{}", document.generation)
                || !(1..=MAX_STAGED_URI_BYTES).contains(&media.uri_bytes)
                || media.metadata_bytes > MAX_STAGED_METADATA_BYTES)
        {
            return Err(INVALID);
        }
        Ok(Self {
            generation: document.generation,
            staged: document.staged,
        })
    }
}

#[derive(Clone)]
pub struct StagedUriDescriptor {
    generation: u64,
    media_id: StagedMediaId,
    uri: Box<str>,
    metadata: Box<str>,
}

#[derive(Clone, Default)]
pub(crate) struct SourceStaging {
    generation: u64,
    staged: Option<Arc<StagedUriDescriptor>>,
}

pub(crate) fn validate_storage(uri: &String, metadata: &String) -> Result<(), EngineError> {
    if uri.capacity() > MAX_STAGED_URI_BYTES || metadata.capacity() > MAX_STAGED_METADATA_BYTES {
        return Err(BUDGET);
    }
    Ok(())
}

impl std::fmt::Debug for StagedUriDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StagedUriDescriptor([redacted])")
    }
}
impl StagedUriDescriptor {
    pub fn media_id(&self) -> &StagedMediaId {
        &self.media_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn uri(&self) -> &str {
        &self.uri
    }
    pub fn metadata(&self) -> &str {
        &self.metadata
    }
    pub(crate) fn retained_bytes(&self) -> usize {
        size_of::<Self>() + 16 + 128 + self.uri.len() + self.metadata.len()
    }
}
impl SourceStaging {
    pub(crate) fn descriptor(&self) -> Option<Arc<StagedUriDescriptor>> {
        self.staged.clone()
    }

    pub(crate) fn snapshot(&self) -> SourceStagingSnapshot {
        SourceStagingSnapshot {
            generation: self.generation,
            staged: self.staged.as_ref().map(|media| StagedUriSummary {
                media_id: media.media_id.clone(),
                admission: SourceAdmission::Unadmitted,
                uri_bytes: media.uri.len(),
                metadata_bytes: media.metadata.len(),
            }),
        }
    }

    pub(crate) fn stage(&mut self, uri: &str, metadata: &str) -> Result<(), EngineError> {
        if (uri.is_empty() && !metadata.is_empty())
            || uri.trim() != uri
            || uri.chars().any(char::is_control)
        {
            return Err(INVALID);
        }
        let generation = increment(self.generation)?;
        let staged = if uri.is_empty() {
            None
        } else {
            Some(Arc::new(StagedUriDescriptor {
                generation,
                media_id: StagedMediaId::new(format!("staged-{generation}"))?,
                uri: uri.into(),
                metadata: metadata.into(),
            }))
        };
        self.generation = generation;
        self.staged = staged;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_exhaustion_preserves_the_previous_descriptor() {
        let mut staging = SourceStaging::default();
        staging.stage("untrusted:original", "metadata").unwrap();
        staging.generation = u64::MAX;
        let before = staging.snapshot();
        for (uri, metadata) in [("untrusted:replacement", ""), ("", "")] {
            assert_eq!(
                staging.stage(uri, metadata).unwrap_err().code,
                ErrorCode::ResourceBudgetExceeded
            );
            assert_eq!(staging.snapshot(), before);
        }
    }
}
