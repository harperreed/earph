// ABOUTME: Projects canonical software gain onto one stateless renderer volume grid.
// ABOUTME: Uses bounded integer conversions with quieter ties and a distinct silence sentinel.
use crate::{EngineError, ErrorCode};

pub const MAX_REMOTE_VOLUME: u16 = 30700;
pub const MIN_REMOTE_VOLUME_DB: i16 = -32767;
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "renderer volume is outside its declared range",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteVolume {
    volume: u16,
}
impl RemoteVolume {
    pub fn from_volume(volume: u16) -> Result<Self, EngineError> {
        if volume > MAX_REMOTE_VOLUME {
            return Err(INVALID);
        }
        Ok(Self { volume })
    }
    pub fn from_volume_db(volume_db: i16) -> Result<Self, EngineError> {
        if !(MIN_REMOTE_VOLUME_DB..=0).contains(&volume_db) {
            return Err(INVALID);
        }
        Ok(Self::nearest(i64::from(volume_db), 1, true))
    }
    /// Observes the finite setting without changing it to fit the remote grid.
    pub fn from_local(gain_db: f64, zero: bool) -> Result<Self, EngineError> {
        if !gain_db.is_finite() || !(-120.0..=0.0).contains(&gain_db) {
            return Err(INVALID);
        }
        if zero {
            return Ok(Self { volume: 0 });
        }
        let millidecibels = (gain_db * 1000.0).round() as i64;
        let mut best = Self::nearest(millidecibels * 256, 1000, false);
        let mut distance = (f64::from(best.volume_db()) / 256.0 - gain_db).abs();
        // Profile caps can retain fractions of a millidecibel. Inspect only the seed's neighbors.
        for volume in
            best.volume.saturating_sub(1).max(1)..=(best.volume + 1).min(MAX_REMOTE_VOLUME)
        {
            let candidate = Self { volume };
            let candidate_distance = (f64::from(candidate.volume_db()) / 256.0 - gain_db).abs();
            if candidate_distance < distance
                || candidate_distance == distance && volume < best.volume
            {
                best = candidate;
                distance = candidate_distance;
            }
        }
        Ok(best)
    }
    pub fn volume(self) -> u16 {
        self.volume
    }
    pub fn volume_db(self) -> i16 {
        if self.volume == 0 {
            return MIN_REMOTE_VOLUME_DB;
        }
        (-30720 + nearest_quieter(i64::from(self.volume - 1) * 30720, 30699)) as i16
    }
    pub fn millidecibels(self) -> Option<i32> {
        (self.volume != 0).then(|| nearest_quieter(i64::from(self.volume_db()) * 1000, 256) as i32)
    }
    fn nearest(target: i64, scale: i64, include_silence: bool) -> Self {
        let mut lower = 1;
        let mut upper = MAX_REMOTE_VOLUME;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            if i64::from(Self { volume: middle }.volume_db()) * scale < target {
                lower = middle + 1;
            } else {
                upper = middle;
            }
        }
        let loud = Self { volume: lower };
        let quiet = Self {
            volume: lower.saturating_sub(1).max(u16::from(!include_silence)),
        };
        let loud_distance = (i64::from(loud.volume_db()) * scale - target).abs();
        let quiet_distance = (i64::from(quiet.volume_db()) * scale - target).abs();
        if loud_distance < quiet_distance {
            loud
        } else {
            quiet
        }
    }
}
fn nearest_quieter(numerator: i64, denominator: i64) -> i64 {
    numerator.div_euclid(denominator)
        + i64::from(numerator.rem_euclid(denominator) * 2 > denominator)
}
