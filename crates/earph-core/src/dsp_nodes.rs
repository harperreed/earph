// ABOUTME: Defines product-owned equalizer parameters for prepared DSP kernels.
// ABOUTME: Rejects unsupported controls and parameters outside declared processing limits.
use crate::{EngineError, ErrorCode, graph::FiniteGraphScalar};
use serde::{Deserialize, Serialize};

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
}

impl PeqFilter {
    pub fn validate(&self, sample_rate_hz: u32) -> Result<(), EngineError> {
        let (frequency_hz, gain_db, q) = match *self {
            Self::Peaking {
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
