// ABOUTME: Decides whether the Sendspin head chunk waits, pads, drops, or writes, and paces idle silence.
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
    /// Times a chunk took an anchor. A wait and a dropped chunk take none.
    pub anchors: u64,
    /// The first locked error after the latest anchor.
    pub start_error_us: Option<i64>,
    pub last_error_us: Option<i64>,
    pub max_abs_error_us: u64,
    pub inserted_silence_frames: u64,
    pub dropped_frames: u64,
    /// The head start of the first chunk after the latest reset: its target minus the time it was first seen.
    /// The output delay is not subtracted. A reset keeps the value until the next first chunk.
    pub first_lead_us: Option<i64>,
    /// The smallest first lead so far.
    pub min_first_lead_us: Option<i64>,
    /// The time from the first sight of that chunk to its first decision.
    pub first_wait_us: Option<u64>,
    /// The longest first wait so far.
    pub max_first_wait_us: u64,
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
    head: Head,
}

/// How far the first chunk after a reset has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Head {
    /// No chunk was seen since the reset.
    Unseen,
    /// The first chunk was seen at this local time and awaits its first decision.
    Seen {
        at_us: i64,
    },
    Decided,
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
                anchors: 0,
                start_error_us: None,
                last_error_us: None,
                max_abs_error_us: 0,
                inserted_silence_frames: 0,
                dropped_frames: 0,
                first_lead_us: None,
                min_first_lead_us: None,
                first_wait_us: None,
                max_first_wait_us: 0,
                mapping_revision: None,
                delay_known: false,
                correction: Correction::None,
                qualification: Qualification::Unqualified,
            },
            head: Head::Unseen,
        })
    }

    /// Anchor again at the next chunk, after a clear or an underrun. Cumulative statistics stay.
    pub fn reset(&mut self) {
        self.stats.anchored = false;
        self.stats.start_error_us = None;
        self.head = Head::Unseen;
    }

    /// Records the head start of the first chunk since the last reset.
    /// Call it when the chunk first has a target, even while no decision can run.
    /// A later call changes nothing until the next reset. A value outside the range saturates.
    pub fn sight(&mut self, target_us: i64, now_us: i64) {
        see(&mut self.stats, &mut self.head, target_us, now_us);
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
        let mut head = self.head;
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
            if stats.anchored {
                // A long gap starts a new anchor attempt with this chunk.
                head = Head::Unseen;
            }
            stats.anchored = false;
            stats.start_error_us = None;
            see(&mut stats, &mut head, chunk.target_us, chunk.now_us);
            if let Head::Seen { at_us } = head {
                let wait = chunk.now_us.saturating_sub(at_us).max(0).unsigned_abs();
                stats.first_wait_us = Some(wait);
                stats.max_first_wait_us = stats.max_first_wait_us.max(wait);
                head = Head::Decided;
            }
            anchor(&mut stats, lead, rate, chunk)?
        };
        self.stats = stats;
        self.head = head;
        Ok(decision)
    }
}

/// Records the first lead when no chunk was seen since the reset.
fn see(stats: &mut ScheduleSnapshot, head: &mut Head, target_us: i64, now_us: i64) {
    if *head != Head::Unseen {
        return;
    }
    let lead = target_us.saturating_sub(now_us);
    stats.first_lead_us = Some(lead);
    stats.min_first_lead_us = Some(
        stats
            .min_first_lead_us
            .map_or(lead, |least| least.min(lead)),
    );
    *head = Head::Seen { at_us: now_us };
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
        stats.anchors = add(stats.anchors, 1)?;
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
    stats.anchors = add(stats.anchors, 1)?;
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

/// Paces the silence that keeps a running output from running dry while no audio is due.
/// It estimates the frames that the output holds from the accepted frames and the local clock.
/// An output without a clock accepts frames faster than it plays them, so only this estimate bounds it.
/// Every value stays far inside the `i128` range, so the arithmetic saturates and never fails.
#[derive(Clone, Copy, Debug)]
pub struct SilencePacer {
    sample_rate: i128,
    depth_frames: u64,
    /// When an output at the nominal rate has played every accepted frame, in frame-microseconds.
    horizon: Option<i128>,
}
impl SilencePacer {
    /// Idle silence keeps `depth_frames` in the output.
    pub fn new(sample_rate: u32, depth_frames: u64) -> Result<Self, ScheduleError> {
        if sample_rate == 0 {
            return Err(ScheduleError::ZeroSampleRate);
        }
        Ok(Self {
            sample_rate: sample_rate.into(),
            depth_frames,
            horizon: None,
        })
    }

    /// The output dropped its frames, after a suspend or an underrun recovery.
    pub fn reset(&mut self) {
        self.horizon = None;
    }

    /// The output accepted these frames of audio or silence at `now_us`.
    pub fn accepted(&mut self, now_us: i64, frames: u64) {
        let now = self.frame_micros(now_us);
        let played = self.horizon.map_or(now, |horizon| horizon.max(now));
        self.horizon = Some(played.saturating_add(i128::from(frames) * MICROS_PER_SECOND));
    }

    /// A running output reported that it holds these frames at `now_us`.
    /// A zero report corrects nothing: an output without a clock reports zero right after a write.
    pub fn correct(&mut self, now_us: i64, held_frames: u64) {
        if held_frames > 0 {
            let held = i128::from(held_frames) * MICROS_PER_SECOND;
            self.horizon = Some(self.frame_micros(now_us).saturating_add(held));
        }
    }

    /// Returns the silence frames that fill the output to its depth at `now_us`.
    /// The silence also fits before `before_us`. A waiting head chunk sets that limit:
    /// more silence would delay it past its target.
    /// Idle silence ends `contiguous` playback and makes the next chunk anchor again.
    /// It then waits until the output holds less than two thirds of the depth,
    /// so a short dip between two chunks moves no audio.
    pub fn owed(&self, now_us: i64, before_us: Option<i64>, contiguous: bool) -> u64 {
        let now = self.frame_micros(now_us);
        let left = self
            .horizon
            .map_or(0, |horizon| horizon.saturating_sub(now));
        // A held fraction counts as a whole frame, so the estimate never exceeds the depth.
        let held = (left.max(0) + MICROS_PER_SECOND - 1) / MICROS_PER_SECOND;
        let held = u64::try_from(held).unwrap_or(u64::MAX);
        if contiguous && u128::from(held) * 3 >= u128::from(self.depth_frames) * 2 {
            return 0;
        }
        let owed = self.depth_frames.saturating_sub(held);
        let Some(before_us) = before_us else {
            return owed;
        };
        let fit = self.frame_micros(before_us).saturating_sub(now).max(0) / MICROS_PER_SECOND;
        owed.min(u64::try_from(fit).unwrap_or(u64::MAX))
    }

    fn frame_micros(&self, time_us: i64) -> i128 {
        i128::from(time_us) * self.sample_rate
    }
}

/// The scheduler refuses a reported device latency above this limit. Admitted, not measured.
pub const DEVICE_LATENCY_LIMIT_US: i64 = 1_000_000;

/// Decides whether the schedule counts the latency that the output device reports.
/// That latency lies behind the frames that the output holds. It is a device report, not a measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatencyAdmission {
    reported_frames: Option<u64>,
    applied: bool,
    lead_time_ms: u64,
}
impl LatencyAdmission {
    /// `reported_frames` counts frames at `sample_rate`. `None` means that the device reports none.
    /// A report within the limit applies. A longer one is kept for the report and adds nothing.
    pub fn new(sample_rate: u32, reported_frames: Option<u64>) -> Self {
        let rate = u128::from(sample_rate);
        let micros = u128::from(reported_frames.unwrap_or(0)) * MICROS_PER_SECOND as u128;
        let applied = reported_frames.is_some()
            && rate > 0
            && micros <= DEVICE_LATENCY_LIMIT_US as u128 * rate;
        Self {
            reported_frames,
            applied,
            // The limit keeps an applied latency far inside `u64` milliseconds.
            lead_time_ms: if applied {
                micros.div_ceil(rate * 1_000) as u64
            } else {
                0
            },
        }
    }
    /// The latency that the device reports, applied or not.
    pub fn reported_frames(self) -> Option<u64> {
        self.reported_frames
    }
    /// Whether `delay_frames` adds the reported latency.
    pub fn applied(self) -> bool {
        self.applied
    }
    /// Returns the scheduler's delay: the frames that the output holds plus the applied latency.
    /// An unknown hold stays unknown.
    pub fn delay_frames(self, held_frames: Option<u64>) -> Option<u64> {
        let latency = self.reported_frames.filter(|_| self.applied).unwrap_or(0);
        held_frames.map(|held| held.saturating_add(latency))
    }
    /// The whole milliseconds that the applied latency adds to the lead time this player requires.
    /// The schedule writes each chunk that much earlier, so the server must send it that much earlier.
    pub fn lead_time_ms(self) -> u64 {
        self.lead_time_ms
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
