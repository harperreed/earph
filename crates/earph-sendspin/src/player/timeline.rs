// ABOUTME: Reduces ordered player events into bounded owned PCM and clear decisions.
// ABOUTME: Retains allocation credits and protocol history across consumer handoff.
use super::*;
use crate::timing::Binding;
use std::collections::VecDeque;

/// Identity facts supplied by the sole source owner. Output epochs remain Engine-owned.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PlayerBinding {
    clock: Binding,
    owner_generation: u64,
    source_generation: u64,
}
impl PlayerBinding {
    pub fn new(clock: Binding, owner_generation: u64, source_generation: u64) -> Self {
        Self {
            clock,
            owner_generation,
            source_generation,
        }
    }
    pub fn clock(self) -> Binding {
        self.clock
    }
    pub fn owner_generation(self) -> u64 {
        self.owner_generation
    }
    pub fn source_generation(self) -> u64 {
        self.source_generation
    }
}
impl fmt::Debug for PlayerBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PlayerBinding([REDACTED])")
    }
}

/// Exact server microseconds multiplied by 48000. Each source frame adds 1000000 ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServerPosition(i128);
impl ServerPosition {
    pub const TICKS_PER_MICROSECOND: i128 = 48000;
    pub const TICKS_PER_FRAME: i128 = 1000000;
    pub fn new(timestamp_us: i64, frames: usize) -> Result<Self, PlayerError> {
        check_timestamp(timestamp_us)?;
        let offset = i128::try_from(frames)
            .map_err(|_| PlayerError::NumericRange)?
            .checked_mul(Self::TICKS_PER_FRAME)
            .ok_or(PlayerError::NumericRange)?;
        Self::checked(i128::from(timestamp_us) * Self::TICKS_PER_MICROSECOND + offset)
    }
    fn checked(ticks: i128) -> Result<Self, PlayerError> {
        let limit = i128::from(MAX_TIME_US) * Self::TICKS_PER_MICROSECOND;
        if !(-limit..=limit).contains(&ticks) {
            return Err(PlayerError::NumericRange);
        }
        Ok(Self(ticks))
    }
    pub fn ticks(self) -> i128 {
        self.0
    }
    pub fn microseconds_floor(self) -> i64 {
        self.0.div_euclid(Self::TICKS_PER_MICROSECOND) as i64
    }
    pub fn add_frames(self, frames: usize) -> Result<Self, PlayerError> {
        let offset = i128::try_from(frames)
            .map_err(|_| PlayerError::NumericRange)?
            .checked_mul(Self::TICKS_PER_FRAME)
            .ok_or(PlayerError::NumericRange)?;
        Self::checked(
            self.0
                .checked_add(offset)
                .ok_or(PlayerError::NumericRange)?,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineLimits {
    wire_bytes: usize,
    decoded_frames: usize,
    chunks: usize,
}
impl Default for TimelineLimits {
    fn default() -> Self {
        Self {
            wire_bytes: MAX_OUTSTANDING_WIRE_BYTES,
            decoded_frames: MAX_DECODED_FRAMES,
            chunks: MAX_CHUNKS,
        }
    }
}
impl TimelineLimits {
    /// Local callers may only tighten the release ceilings.
    pub fn new(
        wire_bytes: usize,
        decoded_frames: usize,
        chunks: usize,
    ) -> Result<Self, PlayerError> {
        if wire_bytes == 0
            || wire_bytes > MAX_OUTSTANDING_WIRE_BYTES
            || decoded_frames == 0
            || decoded_frames > MAX_DECODED_FRAMES
            || chunks == 0
            || chunks > MAX_CHUNKS
        {
            return Err(PlayerError::LocalBudget);
        }
        Ok(Self {
            wire_bytes,
            decoded_frames,
            chunks,
        })
    }
    pub fn wire_bytes(self) -> usize {
        self.wire_bytes
    }
    pub fn decoded_frames(self) -> usize {
        self.decoded_frames
    }
    pub fn chunks(self) -> usize {
        self.chunks
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimelineUsage {
    pub wire_bytes: usize,
    pub allocated_frames: usize,
    pub allocated_bytes: usize,
    pub descriptors: usize,
}

/// One PCM allocation and its nonclone release lease.
/// After moving samples, retain this lease until all downstream storage is released.
pub struct PcmChunk {
    id: u64,
    binding: PlayerBinding,
    generation: u64,
    format: StreamFormat,
    format_revision: u64,
    timestamp: i64,
    original_frames: usize,
    trimmed_frames: usize,
    position: ServerPosition,
    arrival: ReceiveStamp,
    send_ahead_us: u32,
    wire_bytes: usize,
    samples: Option<Vec<i16>>,
}
impl fmt::Debug for PcmChunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PcmChunk")
            .field("frames", &self.frames())
            .field("wire_bytes", &self.wire_bytes)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl PcmChunk {
    pub fn binding(&self) -> PlayerBinding {
        self.binding
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn format(&self) -> StreamFormat {
        self.format
    }
    pub fn format_revision(&self) -> u64 {
        self.format_revision
    }
    pub fn server_timestamp_us(&self) -> i64 {
        self.timestamp
    }
    pub fn original_frames(&self) -> usize {
        self.original_frames
    }
    pub fn trimmed_frames(&self) -> usize {
        self.trimmed_frames
    }
    pub fn frames(&self) -> usize {
        self.original_frames - self.trimmed_frames
    }
    pub fn position(&self) -> ServerPosition {
        self.position
    }
    pub fn end_position(&self) -> ServerPosition {
        self.position
            .add_frames(self.frames())
            .expect("admitted position")
    }
    pub fn arrival(&self) -> ReceiveStamp {
        self.arrival
    }
    pub fn send_ahead_us(&self) -> u32 {
        self.send_ahead_us
    }
    pub fn wire_bytes(&self) -> usize {
        self.wire_bytes
    }
    /// Empty after extraction; the admitted frame count and release lease remain unchanged.
    pub fn samples(&self) -> &[i16] {
        self.samples.as_deref().unwrap_or(&[])
    }
    /// Move the admitted allocation exactly once, preserving this chunk's identity and metadata.
    /// Release downstream storage before returning this lease through `release_chunk`.
    pub fn take_samples(&mut self) -> Option<Vec<i16>> {
        self.samples.take()
    }
}

struct Entry {
    id: u64,
    generation: u64,
    timestamp: i64,
    end: ServerPosition,
    completion_end: ServerPosition,
    wire_bytes: usize,
    counted: bool,
    valid: bool,
    allocation_frames: usize,
    allocation_bytes: usize,
    pending: Option<PcmChunk>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CutoffEvidence {
    RetainedChunk,
    NotRetained,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearOutcome {
    pub generation: u64,
    pub discarded_chunks: usize,
    pub cutoff: CutoffEvidence,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelineAction {
    Started,
    Queued,
    NoAudio,
    Discarded,
    Cleared(ClearOutcome),
    Ended,
}

/// One serialized source timeline. It owns neither output nor an independently updated clock.
pub struct PlayerTimeline {
    binding: PlayerBinding,
    limits: TimelineLimits,
    participating: bool,
    active: bool,
    format: Option<StreamFormat>,
    format_revision: u64,
    generation: u64,
    serial: u64,
    entries: VecDeque<Entry>,
    previous_end: Option<ServerPosition>,
    replacement_floor: Option<ServerPosition>,
    last_arrival: Option<ReceiveStamp>,
    output_delay_ms: Option<u32>,
    last_server_now: Option<ServerPosition>,
}
impl fmt::Debug for PlayerTimeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerTimeline")
            .field("active", &self.active)
            .field("generation", &self.generation)
            .field("usage", &self.usage())
            .finish_non_exhaustive()
    }
}
impl PlayerTimeline {
    pub fn new(binding: PlayerBinding, limits: TimelineLimits) -> Self {
        Self {
            binding,
            limits,
            participating: false,
            active: false,
            format: None,
            format_revision: 0,
            generation: 0,
            serial: 0,
            entries: VecDeque::with_capacity(limits.chunks),
            previous_end: None,
            replacement_floor: None,
            last_arrival: None,
            output_delay_ms: None,
            last_server_now: None,
        }
    }
    pub fn binding(&self) -> PlayerBinding {
        self.binding
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn format(&self) -> Option<StreamFormat> {
        self.format
    }
    pub fn stream_active(&self) -> bool {
        self.active
    }
    /// Include this retained descriptor storage in the output owner's preparation budget.
    pub fn metadata_capacity_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<Entry>()
    }
    pub fn usage(&self) -> TimelineUsage {
        let mut usage = TimelineUsage {
            descriptors: self.entries.len(),
            ..TimelineUsage::default()
        };
        for entry in &self.entries {
            if entry.counted {
                usage.wire_bytes += entry.wire_bytes;
            }
            usage.allocated_frames += entry.allocation_frames;
            usage.allocated_bytes += entry.allocation_bytes;
        }
        usage
    }
    /// Apply activation in the same delivery order as subsequent lifecycle messages.
    pub fn set_participation(&mut self, player: bool, playback: bool) -> Result<(), PlayerError> {
        let participating = player && playback;
        if self.participating && !participating {
            self.cancel()?;
        }
        self.participating = participating;
        Ok(())
    }
    /// `accept_audio` is a caller decision. It does not suppress lifecycle validation.
    /// Partial clear takes the actual unretractable server endpoint reported by the output owner.
    pub fn apply(
        &mut self,
        event: PlayerEvent<'_>,
        expected: PlayerBinding,
        accept_audio: bool,
        irretractable_until: Option<ServerPosition>,
    ) -> Result<TimelineAction, PlayerError> {
        if expected != self.binding {
            return Err(PlayerError::BindingMismatch);
        }
        if self.last_arrival.is_some_and(|last| event.arrival() < last) {
            return Err(PlayerError::ArrivalOrder);
        }
        self.last_arrival = Some(event.arrival());
        if !self.participating {
            return Ok(TimelineAction::Discarded);
        }
        match event {
            PlayerEvent::Start {
                server_transmitted_us,
                format,
                ..
            } => {
                check_timestamp(server_transmitted_us)?;
                let revision = self
                    .format_revision
                    .checked_add(1)
                    .ok_or(PlayerError::CounterOverflow)?;
                if !self.active {
                    self.reset()?;
                }
                self.active = true;
                self.format = Some(format);
                self.format_revision = revision;
                Ok(TimelineAction::Started)
            }
            PlayerEvent::Audio { frame, arrival } => {
                validate_audio(frame)?;
                if !self.active {
                    return Err(PlayerError::InactiveStream);
                }
                self.admit(frame, arrival, accept_audio)
            }
            PlayerEvent::Clear {
                server_transmitted_us,
                from_timestamp_us,
                ..
            } => {
                check_timestamp(server_transmitted_us)?;
                if !self.active {
                    return Err(PlayerError::InactiveStream);
                }
                let outcome = if let Some(cutoff) = from_timestamp_us {
                    self.partial_clear(cutoff, irretractable_until)?
                } else {
                    let discarded_chunks = self.entries.iter().filter(|entry| entry.valid).count();
                    self.reset()?;
                    ClearOutcome {
                        generation: self.generation,
                        discarded_chunks,
                        cutoff: CutoffEvidence::NotRetained,
                    }
                };
                Ok(TimelineAction::Cleared(outcome))
            }
            PlayerEvent::End { .. } => {
                if !self.active {
                    return Err(PlayerError::InactiveStream);
                }
                self.reset()?;
                self.active = false;
                self.format = None;
                Ok(TimelineAction::Ended)
            }
        }
    }
    fn admit(
        &mut self,
        frame: PlayerAudioFrame<'_>,
        arrival: ReceiveStamp,
        accept: bool,
    ) -> Result<TimelineAction, PlayerError> {
        let frames = validate_audio(frame)?;
        let wire_bytes = frame.payload.len() + 13;
        let mut position = ServerPosition::new(frame.server_timestamp_us, 0)?;
        if frames > 0
            && self.replacement_floor.is_none()
            && let Some(previous) = self.previous_end
        {
            let difference = position.0 - previous.0;
            // Integer server timestamps can truncate a submicrosecond frame boundary.
            if difference.abs() < ServerPosition::TICKS_PER_MICROSECOND {
                position = previous;
            } else if difference < 0 {
                return Err(PlayerError::Overlap);
            }
        }
        let end = position.add_frames(frames)?;
        let completion_end = ServerPosition::new(frame.server_timestamp_us, frames)?;
        let trimmed_frames = self.replacement_floor.map_or(0, |floor| {
            if floor <= position {
                0
            } else {
                ((floor.0 - position.0 + ServerPosition::TICKS_PER_FRAME - 1)
                    / ServerPosition::TICKS_PER_FRAME)
                    .min(frames as i128) as usize
            }
        });
        let retained_frames = if accept { frames - trimmed_frames } else { 0 };
        let usage = self.usage();
        if wire_bytes > self.limits.wire_bytes.saturating_sub(usage.wire_bytes)
            || retained_frames
                > self
                    .limits
                    .decoded_frames
                    .saturating_sub(usage.allocated_frames)
            || self.entries.len() >= self.limits.chunks
        {
            return Err(PlayerError::LocalBudget);
        }
        let id = self
            .serial
            .checked_add(1)
            .ok_or(PlayerError::CounterOverflow)?;
        let mut samples = Vec::new();
        if retained_frames > 0 {
            samples
                .try_reserve_exact(retained_frames * 2)
                .map_err(|_| PlayerError::LocalBudget)?;
            // Charge allocator capacity before filling the allocation or transferring it.
            if samples.capacity().div_ceil(2) > self.limits.decoded_frames - usage.allocated_frames
            {
                return Err(PlayerError::LocalBudget);
            }
            samples.extend(
                frame.payload[trimmed_frames * 4..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|sample| i16::from_le_bytes(*sample)),
            );
        }
        let allocation_frames = samples.capacity().div_ceil(2);
        let allocation_bytes = samples.capacity() * 2;
        let pending = (retained_frames > 0).then_some(PcmChunk {
            id,
            binding: self.binding,
            generation: self.generation,
            format: self.format.expect("active stream has format"),
            format_revision: self.format_revision,
            timestamp: frame.server_timestamp_us,
            original_frames: frames,
            trimmed_frames,
            position: position.add_frames(trimmed_frames)?,
            arrival,
            send_ahead_us: frame.send_ahead_us,
            wire_bytes,
            samples: Some(samples),
        });
        self.entries.push_back(Entry {
            id,
            generation: self.generation,
            timestamp: frame.server_timestamp_us,
            end,
            completion_end,
            wire_bytes,
            counted: true,
            valid: true,
            allocation_frames,
            allocation_bytes,
            pending,
        });
        self.serial = id;
        if frames > 0 {
            self.previous_end = Some(self.previous_end.map_or(end, |previous| previous.max(end)));
        }
        if frames > 0 && self.replacement_floor.is_some_and(|floor| end >= floor) {
            self.replacement_floor = None;
        }
        Ok(if !accept {
            TimelineAction::Discarded
        } else if retained_frames == 0 {
            TimelineAction::NoAudio
        } else {
            TimelineAction::Queued
        })
    }
    pub fn take_chunk(&mut self) -> Option<PcmChunk> {
        self.entries
            .iter_mut()
            .find_map(|entry| entry.pending.take())
    }
    /// A clear revokes only discarded chunks. Earlier straddling chunks remain current.
    pub fn is_current(&self, chunk: &PcmChunk) -> bool {
        chunk.binding == self.binding
            && self.entries.iter().any(|entry| {
                entry.id == chunk.id && entry.generation == chunk.generation && entry.valid
            })
    }
    /// Return the nonclone lease only after moved downstream sample storage has been released.
    pub fn release_chunk(&mut self, chunk: PcmChunk) -> Result<(), PlayerError> {
        if chunk.binding != self.binding {
            return Err(PlayerError::BindingMismatch);
        }
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == chunk.id && entry.generation == chunk.generation)
            .ok_or(PlayerError::UnknownChunk)?;
        if entry.pending.is_some() || entry.allocation_bytes == 0 {
            return Err(PlayerError::UnknownChunk);
        }
        entry.allocation_bytes = 0;
        entry.allocation_frames = 0;
        drop(chunk);
        self.prune();
        Ok(())
    }
    /// Release only protocol accounting whose exact completion time has passed.
    /// PCM allocations stay charged until consumed or explicitly discarded.
    pub fn advance_server_time(
        &mut self,
        now: ServerPosition,
        output_delay_ms: u32,
    ) -> Result<(), PlayerError> {
        if output_delay_ms > 5000 {
            return Err(PlayerError::NumericRange);
        }
        if self
            .output_delay_ms
            .is_some_and(|delay| delay != output_delay_ms)
        {
            return Err(PlayerError::OutputDelayChanged);
        }
        if self.last_server_now.is_some_and(|last| now < last) {
            return Err(PlayerError::ServerTimeOrder);
        }
        self.output_delay_ms = Some(output_delay_ms);
        self.last_server_now = Some(now);
        let delay = i128::from(output_delay_ms) * 1000 * ServerPosition::TICKS_PER_MICROSECOND;
        for entry in &mut self.entries {
            if entry.completion_end.0 - delay <= now.0 {
                entry.counted = false;
            }
        }
        self.prune();
        Ok(())
    }
    fn partial_clear(
        &mut self,
        cutoff: i64,
        irretractable: Option<ServerPosition>,
    ) -> Result<ClearOutcome, PlayerError> {
        check_timestamp(cutoff)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(PlayerError::CounterOverflow)?;
        let evidence = if self
            .entries
            .iter()
            .any(|entry| entry.valid && entry.timestamp == cutoff)
        {
            CutoffEvidence::RetainedChunk
        } else {
            CutoffEvidence::NotRetained
        };
        let mut discarded_chunks = 0;
        for entry in &mut self.entries {
            if entry.valid && entry.timestamp >= cutoff {
                discarded_chunks += 1;
                Self::discard(entry);
            }
        }
        self.prune();
        self.previous_end = self
            .entries
            .iter()
            .filter(|entry| entry.valid)
            .map(|entry| entry.end)
            .max();
        self.replacement_floor = self
            .previous_end
            .into_iter()
            .chain(irretractable)
            .chain(self.replacement_floor)
            .max();
        self.generation = generation;
        Ok(ClearOutcome {
            generation,
            discarded_chunks,
            cutoff: evidence,
        })
    }
    fn discard(entry: &mut Entry) {
        entry.counted = false;
        entry.valid = false;
        if entry.pending.take().is_some() {
            entry.allocation_bytes = 0;
            entry.allocation_frames = 0;
        }
    }
    fn prune(&mut self) {
        self.entries
            .retain(|entry| entry.counted || entry.allocation_bytes > 0);
    }
    fn reset(&mut self) -> Result<(), PlayerError> {
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(PlayerError::CounterOverflow)?;
        for entry in &mut self.entries {
            Self::discard(entry);
        }
        self.prune();
        self.previous_end = None;
        self.replacement_floor = None;
        self.generation = generation;
        Ok(())
    }
    /// Cancellation bypasses admission capacity and revokes future source work.
    pub fn cancel(&mut self) -> Result<(), PlayerError> {
        self.reset()?;
        self.active = false;
        self.format = None;
        self.participating = false;
        Ok(())
    }
}
