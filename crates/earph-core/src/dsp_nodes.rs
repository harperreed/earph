// ABOUTME: Defines product-owned equalizer parameters and resampler presets.
// ABOUTME: Rejects unsupported controls and parameters outside declared processing limits.
use crate::{EngineError, ErrorCode, graph::FiniteGraphScalar};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct FirSha256([u8; 64]);
impl FirSha256 {
    pub fn new(value: String) -> Result<Self, EngineError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "correction identity requires a lowercase SHA-256",
            ));
        }
        Ok(Self(value.as_bytes().try_into().map_err(|_| {
            EngineError::new(ErrorCode::AssetInvalid, "invalid correction identity")
        })?))
    }
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated ASCII identity")
    }
}
impl TryFrom<String> for FirSha256 {
    type Error = EngineError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<FirSha256> for String {
    fn from(value: FirSha256) -> Self {
        value.as_str().into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirPurpose {
    Correction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "FirReferenceFields")]
pub struct FirAssetReference {
    pub sha256: FirSha256,
    pub sample_rate_hz: u32,
    pub channels: crate::audio::ChannelLayout,
    pub taps_per_channel: usize,
    pub purpose: FirPurpose,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FirReferenceFields {
    sha256: FirSha256,
    sample_rate_hz: u32,
    channels: crate::audio::ChannelLayout,
    taps_per_channel: usize,
    purpose: FirPurpose,
}
impl TryFrom<FirReferenceFields> for FirAssetReference {
    type Error = EngineError;
    fn try_from(value: FirReferenceFields) -> Result<Self, Self::Error> {
        let result = Self {
            sha256: value.sha256,
            sample_rate_hz: value.sample_rate_hz,
            channels: value.channels,
            taps_per_channel: value.taps_per_channel,
            purpose: value.purpose,
        };
        result.validate()?;
        Ok(result)
    }
}
impl FirAssetReference {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.sample_rate_hz == 0 || !(1..=262144).contains(&self.taps_per_channel) {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "correction dimensions exceed processing limits",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SrcPreset {
    Quality,
    Responsive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PeqFilter {
    Peaking {
        frequency_hz: FiniteGraphScalar,
        q: FiniteGraphScalar,
        gain_db: FiniteGraphScalar,
    },
    LowShelf {
        frequency_hz: FiniteGraphScalar,
        gain_db: FiniteGraphScalar,
    },
    HighShelf {
        frequency_hz: FiniteGraphScalar,
        gain_db: FiniteGraphScalar,
    },
    LowShelfQ {
        frequency_hz: FiniteGraphScalar,
        q: FiniteGraphScalar,
        gain_db: FiniteGraphScalar,
    },
    HighShelfQ {
        frequency_hz: FiniteGraphScalar,
        q: FiniteGraphScalar,
        gain_db: FiniteGraphScalar,
    },
}

impl PeqFilter {
    pub fn validate(&self, sample_rate_hz: u32) -> Result<(), EngineError> {
        let (frequency_hz, gain_db, q) = match *self {
            Self::Peaking {
                frequency_hz,
                gain_db,
                q,
            }
            | Self::LowShelfQ {
                frequency_hz,
                gain_db,
                q,
            }
            | Self::HighShelfQ {
                frequency_hz,
                gain_db,
                q,
            } => (frequency_hz, gain_db, Some(q)),
            Self::LowShelf {
                frequency_hz,
                gain_db,
            }
            | Self::HighShelf {
                frequency_hz,
                gain_db,
            } => (frequency_hz, gain_db, None),
        };
        if !(10.0..=0.45 * f64::from(sample_rate_hz)).contains(&frequency_hz.get())
            || !(-24.0..=24.0).contains(&gain_db.get())
            || q.is_some_and(|q| !(0.1..=20.0).contains(&q.get()))
        {
            return Err(EngineError::new(
                ErrorCode::GraphInvalid,
                "equalizer parameters exceed processing limits",
            ));
        }
        Ok(())
    }
}
