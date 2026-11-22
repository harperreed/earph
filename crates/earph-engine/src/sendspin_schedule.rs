// ABOUTME: Decides whether the Sendspin head chunk waits, pads, drops, or writes against its local target time.
// ABOUTME: Uses checked integer arithmetic over caller-supplied facts; it reads no clock and performs no I/O.

//! Rounding rules. All comparisons use exact integers in frame-microseconds
//! (`microseconds * sample_rate`), so the reported delay is never rounded first.
//! - Wait time rounds up to a whole microsecond. The wake time then lies inside the pad window.
//! - Pad silence rounds to the nearest frame. A tie rounds up.
//! - Late frames round up, so the first written frame is never behind its target.
//! - A recorded error rounds to the nearest microsecond. A tie rounds away from zero.

/// While anchoring, a longer lead waits and a shorter positive lead becomes silence.
pub const ANCHOR_PAD_LIMIT_US: i64 = 5_000;
/// While locked, a timestamp gap up to this length becomes silence. A longer gap anchors again.
pub const SHORT_GAP_LIMIT_US: i64 = 100_000;
const MICROS_PER_SECOND: i128 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    ZeroSampleRate,
    Overflow,
}

/// Facts about the head chunk at one decision. Both times share one local clock domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkFacts {
    /// Local time of the chunk's first frame, from `UsableMapping::port_target`.
    pub target_us: i64,
    /// Revision of the mapping that produced `target_us`.
    pub mapping_revision: u64,
    pub frames: u64,
    pub now_us: i64,
    /// Frames the output still holds before a new write becomes audible. `None` means unknown.
    pub delay_frames: Option<u64>,
    /// Whole frames between the previous chunk's end and this chunk's start.
    /// The caller computes this from the timeline positions. Anchoring ignores it.
    pub gap_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Keep the chunk and decide again at `until_us`. This time never exceeds the target.
    Wait { until_us: i64 },
    /// Write the silence, then the chunk without its first `skip_frames` frames.
    Write {
        prepend_silence_frames: u64,
        skip_frames: u64,
    },
    /// Discard the whole chunk and decide again with the next one.
    DropChunk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Correction {
    None,
}
impl Correction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Qualification {
    Unqualified,
}
impl Qualification {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unqualified => "unqualified",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleSnapshot {
    pub anchored: bool,
    /// The first locked error after the latest anchor.
    pub start_error_us: Option<i64>,
    pub last_error_us: Option<i64>,
    pub max_abs_error_us: u64,
    pub inserted_silence_frames: u64,
    pub dropped_frames: u64,
    /// Mapping revision of the last decision.
    pub mapping_revision: Option<u64>,
    /// Whether the last decision had a reported output delay.
    pub delay_known: bool,
    pub correction: Correction,
    pub qualification: Qualification,
}

/// Anchors the first chunk to its target, then measures the error of contiguous playback.
#[derive(Clone, Copy, Debug)]
pub struct Scheduler {
    sample_rate: u32,
    stats: ScheduleSnapshot,
}

impl Scheduler {
    pub fn new(sample_rate: u32) -> Result<Self, ScheduleError> {
        if sample_rate == 0 {
            return Err(ScheduleError::ZeroSampleRate);
        }
        Ok(Self {
            sample_rate,
            stats: ScheduleSnapshot {
                anchored: false,
                start_error_us: None,
                last_error_us: None,
                max_abs_error_us: 0,
                inserted_silence_frames: 0,
                dropped_frames: 0,
                mapping_revision: None,
                delay_known: false,
                correction: Correction::None,
                qualification: Qualification::Unqualified,
            },
        })
    }

    /// Anchor again at the next chunk, after a clear or an underrun. Cumulative statistics stay.
    pub fn reset(&mut self) {
        self.stats.anchored = false;
        self.stats.start_error_us = None;
    }

    pub fn snapshot(&self) -> ScheduleSnapshot {
        self.stats
    }

    /// Decide what to do with the head chunk. An error leaves the scheduler unchanged.
    pub fn decide(&mut self, chunk: ChunkFacts) -> Result<Decision, ScheduleError> {
        let rate = i128::from(self.sample_rate);
        // An unknown delay counts as zero.
        let delay = mul(
            i128::from(chunk.delay_frames.unwrap_or(0)),
            MICROS_PER_SECOND,
        )?;
        // Time until a frame written now reaches its target, in frame-microseconds.
        // The delay enters here and nowhere else.
        let lead = sub(
            mul(sub(chunk.target_us.into(), chunk.now_us.into())?, rate)?,
            delay,
        )?;
        let gap = mul(i128::from(chunk.gap_frames), MICROS_PER_SECOND)?;
        // Work on a copy, so a failed decision commits nothing.
        let mut stats = self.stats;
        stats.mapping_revision = Some(chunk.mapping_revision);
        stats.delay_known = chunk.delay_frames.is_some();
        let decision = if stats.anchored && gap <= mul(SHORT_GAP_LIMIT_US.into(), rate)? {
            // The chunk's first frame leaves after the delay and the gap silence.
            let error = narrow(round_nearest(sub(gap, lead)?, rate)?)?;
            stats.inserted_silence_frames = add(stats.inserted_silence_frames, chunk.gap_frames)?;
            stats.start_error_us.get_or_insert(error);
            stats.last_error_us = Some(error);
            stats.max_abs_error_us = stats.max_abs_error_us.max(error.unsigned_abs());
            Decision::Write {
                prepend_silence_frames: chunk.gap_frames,
                skip_frames: 0,
            }
        } else {
            stats.anchored = false;
            stats.start_error_us = None;
            anchor(&mut stats, lead, rate, chunk)?
        };
        self.stats = stats;
        Ok(decision)
    }
}

/// Place one chunk against its own target. A `Write` locks the scheduler.
fn anchor(
    stats: &mut ScheduleSnapshot,
    lead: i128,
    rate: i128,
    chunk: ChunkFacts,
) -> Result<Decision, ScheduleError> {
    let pad_limit = mul(ANCHOR_PAD_LIMIT_US.into(), rate)?;
    if lead > pad_limit {
        // Wake when the lead fits the pad window. Rounding up guarantees progress.
        let wait_us = ceil_div(sub(lead, pad_limit)?, rate)?;
        let until_us = narrow(
            i128::from(chunk.now_us)
                .checked_add(wait_us)
                .ok_or(ScheduleError::Overflow)?,
        )?;
        return Ok(Decision::Wait { until_us });
    }
    if lead > 0 {
        let silence = frames(round_nearest(lead, MICROS_PER_SECOND)?)?;
        stats.inserted_silence_frames = add(stats.inserted_silence_frames, silence)?;
        stats.anchored = true;
        return Ok(Decision::Write {
            prepend_silence_frames: silence,
            skip_frames: 0,
        });
    }
    let late = ceil_div(
        lead.checked_neg().ok_or(ScheduleError::Overflow)?,
        MICROS_PER_SECOND,
    )?;
    if late >= i128::from(chunk.frames) {
        stats.dropped_frames = add(stats.dropped_frames, chunk.frames)?;
        return Ok(Decision::DropChunk);
    }
    let skip_frames = frames(late)?;
    stats.dropped_frames = add(stats.dropped_frames, skip_frames)?;
    stats.anchored = true;
    Ok(Decision::Write {
        prepend_silence_frames: 0,
        skip_frames,
    })
}

/// Turns successive mapped server times into a non-decreasing sequence.
/// `PlayerTimeline::advance_server_time` rejects a decreasing value, and a new mapping can step back.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicClamp {
    highest: Option<i64>,
}
impl MonotonicClamp {
    /// Return `value`, or the highest earlier value when `value` is lower.
    pub fn clamp(&mut self, value: i64) -> i64 {
        let clamped = self.highest.map_or(value, |highest| highest.max(value));
        self.highest = Some(clamped);
        clamped
    }
}

fn mul(left: i128, right: i128) -> Result<i128, ScheduleError> {
    left.checked_mul(right).ok_or(ScheduleError::Overflow)
}
fn sub(left: i128, right: i128) -> Result<i128, ScheduleError> {
    left.checked_sub(right).ok_or(ScheduleError::Overflow)
}
fn add(left: u64, right: u64) -> Result<u64, ScheduleError> {
    left.checked_add(right).ok_or(ScheduleError::Overflow)
}
fn narrow(value: i128) -> Result<i64, ScheduleError> {
    i64::try_from(value).map_err(|_| ScheduleError::Overflow)
}
fn frames(value: i128) -> Result<u64, ScheduleError> {
    u64::try_from(value).map_err(|_| ScheduleError::Overflow)
}
/// Divide a nonnegative value by a positive divisor and round up.
fn ceil_div(value: i128, divisor: i128) -> Result<i128, ScheduleError> {
    let quotient = value
        .checked_div_euclid(divisor)
        .ok_or(ScheduleError::Overflow)?;
    let remainder = value
        .checked_rem_euclid(divisor)
        .ok_or(ScheduleError::Overflow)?;
    quotient
        .checked_add(i128::from(remainder != 0))
        .ok_or(ScheduleError::Overflow)
}
/// Divide by a positive divisor and round to nearest. A tie rounds away from zero.
fn round_nearest(value: i128, divisor: i128) -> Result<i128, ScheduleError> {
    let half = divisor / 2;
    let biased = if value < 0 {
        value.checked_sub(half)
    } else {
        value.checked_add(half)
    };
    // Integer division truncates toward zero, which completes the rounding for both signs.
    biased
        .and_then(|biased| biased.checked_div(divisor))
        .ok_or(ScheduleError::Overflow)
}

#[cfg(test)]
mod tests;
