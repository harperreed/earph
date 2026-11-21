// ABOUTME: Enforces approval, epochs, bounded storage, and output lifecycle.
// ABOUTME: Reports submitted frames separately from backend consumption estimates.
use crate::{native, *};
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmBlock, PcmRepresentation},
    dop::{DopBlock, DopStream},
    ids::Epochs,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePcmState {
    Open,
    Setup,
    Prepared,
    Running,
    Xrun,
    Draining,
    Paused,
    Suspended,
    Disconnected,
    Unknown(u32),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingAvailability {
    Available,
    Pending,
    InvalidDelay,
    Unavailable,
}
#[derive(Debug, Clone, Copy)]
pub struct TimingObservation {
    pub epochs: Epochs,
    pub state: OutputState,
    pub accepted_frames: u64,
    pub client_rate_hz: u32,
    pub pcm_state: NativePcmState,
    pub signed_delay_frames: i64,
    pub availability: TimingAvailability,
    pub timestamp_mode: Option<u32>,
    pub timestamp_type: Option<u32>,
    pub native_accuracy_ns: Option<u64>,
    pub native_update_age_ns: Option<u64>,
}
fn timing_availability(state: NativePcmState, delay: i64, accepted: u64) -> TimingAvailability {
    match state {
        NativePcmState::Running | NativePcmState::Draining => {
            if u64::try_from(delay).is_ok_and(|delay| delay <= accepted) {
                TimingAvailability::Available
            } else {
                TimingAvailability::InvalidDelay
            }
        }
        NativePcmState::Prepared | NativePcmState::Paused => TimingAvailability::Pending,
        _ => TimingAvailability::Unavailable,
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn signed_delay_and_native_state_control_timing_availability() {
        assert_eq!(
            timing_availability(NativePcmState::Prepared, 0, 0),
            TimingAvailability::Pending
        );
        assert_eq!(
            timing_availability(NativePcmState::Running, 0, 64),
            TimingAvailability::Available
        );
        assert_eq!(
            timing_availability(NativePcmState::Running, -1, 64),
            TimingAvailability::InvalidDelay
        );
        assert_eq!(
            timing_availability(NativePcmState::Draining, 65, 64),
            TimingAvailability::InvalidDelay
        );
        assert_eq!(
            timing_availability(NativePcmState::Xrun, -32, 64),
            TimingAvailability::Unavailable
        );
    }
}

pub struct AlsaOutput {
    pcm: Option<native::Pcm>,
    report: OpenReport,
    epochs: Epochs,
    state: OutputState,
    scratch: Vec<u8>,
    submitted_frames: u64,
    next_position: Option<u64>,
}
impl AlsaOutput {
    pub fn open(request: OpenRequest, epochs: Epochs) -> Result<Self, AlsaError> {
        request.validate()?;
        let bytes = request
            .format
            .payload_bytes(request.period_frames.into())
            .map_err(|_| {
                AlsaError::new(
                    ErrorCode::ResourceBudgetExceeded,
                    "PCM period exceeds addressable storage",
                )
            })?;
        let scratch = vec![0; bytes];
        let pcm = native::Pcm::open(&request)?;
        let observed = pcm.configure(&request)?;
        let identity = pcm.identity()?;
        let report = OpenReport {
            schema_version: 1,
            pcm_type: pcm.type_name(),
            identity,
            observed,
            alsa_version: native::library_version(),
            evidence_level: "opened",
            downstream_transformations: if request.route == Route::DirectHardware {
                "none_in_alsa_hw"
            } else {
                "unknown"
            },
            payload_preservation: "unverified",
            requested: request,
        };
        Ok(Self {
            pcm: Some(pcm),
            report,
            epochs,
            state: OutputState::Prepared,
            scratch,
            submitted_frames: 0,
            next_position: None,
        })
    }
    pub fn report(&self) -> &OpenReport {
        &self.report
    }
    pub fn state(&self) -> OutputState {
        self.state
    }
    pub fn submitted_frames(&self) -> u64 {
        self.submitted_frames
    }
    /// Reads one coherent native state/delay snapshot without changing stream settings.
    pub fn timing_observation(&mut self) -> Result<TimingObservation, AlsaError> {
        let pcm = self.pcm.as_ref().ok_or(AlsaError::new(
            ErrorCode::OutputFault,
            "output has no active backend clock",
        ))?;
        let (pcm_state, signed_delay_frames, timestamp_mode, timestamp_type) =
            pcm.timing_status()?;
        Ok(TimingObservation {
            epochs: self.epochs,
            state: self.state,
            accepted_frames: self.submitted_frames,
            client_rate_hz: self.report.requested.format.sample_rate_hz(),
            pcm_state,
            signed_delay_frames,
            availability: timing_availability(
                pcm_state,
                signed_delay_frames,
                self.submitted_frames,
            ),
            timestamp_mode,
            timestamp_type,
            native_accuracy_ns: None,
            native_update_age_ns: None,
        })
    }
    /// Actual Rust-owned audio capacity, excluding metadata, allocator overhead, and ALSA storage.
    pub fn audio_storage_bytes(&self) -> Result<u64, AlsaError> {
        u64::try_from(self.scratch.capacity()).map_err(|_| {
            AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "audio storage count exceeds u64",
            )
        })
    }

    pub fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
        permission: SignalPermission,
    ) -> Result<WriteProgress, AlsaError> {
        self.validate_write(permission)?;
        let header = block.header();
        if header.epochs != self.epochs {
            return Err(AlsaError::new(
                ErrorCode::StaleSession,
                "PCM belongs to a different output epoch",
            ));
        }
        if header.format != self.report.requested.format {
            return Err(AlsaError::new(
                ErrorCode::FormatUnsupported,
                "PCM format differs from the opened stream",
            ));
        }
        if header.frame_count > u64::from(self.report.requested.period_frames) {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "PCM block exceeds the prepared period",
            ));
        }
        if frames == 0
            || offset
                .checked_add(frames)
                .is_none_or(|end| end as u64 > header.frame_count)
        {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "invalid PCM write span",
            ));
        }
        let position = header
            .frame_position
            .checked_add(offset as u64)
            .ok_or(AlsaError::new(
                ErrorCode::AssetInvalid,
                "PCM frame position overflow",
            ))?;
        if self
            .next_position
            .is_some_and(|expected| expected != position)
        {
            return Err(AlsaError::new(
                ErrorCode::AssetInvalid,
                "PCM span is not contiguous with accepted frames",
            ));
        }
        block
            .pack_le(&mut self.scratch)
            .map_err(|_| AlsaError::new(ErrorCode::AssetInvalid, "cannot pack PCM block"))?;
        let frame_bytes = header
            .format
            .payload_bytes(1)
            .map_err(|_| AlsaError::new(ErrorCode::InternalFault, "invalid frame size"))?;
        let start = offset * frame_bytes;
        let result = self.submit(start, frames, frame_bytes)?;
        if let WriteProgress::Accepted { frames } = result {
            self.next_position = Some(position + frames as u64);
        }
        Ok(result)
    }

    fn validate_write(&self, permission: SignalPermission) -> Result<(), AlsaError> {
        if !matches!(self.state, OutputState::Prepared | OutputState::Running) {
            return Err(AlsaError::new(
                ErrorCode::OutputFault,
                "output is stopped or failed; open a new stream explicitly",
            ));
        }
        if permission != SignalPermission::Granted {
            return Err(AlsaError::new(
                ErrorCode::SafetyPolicyBlocked,
                "signal emission requires explicit approval for the selected route",
            ));
        }
        Ok(())
    }

    fn submit(
        &mut self,
        start: usize,
        frames: usize,
        frame_bytes: usize,
    ) -> Result<WriteProgress, AlsaError> {
        self.submitted_frames
            .checked_add(frames as u64)
            .ok_or(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "submitted frame counter exhausted",
            ))?;
        let end = start + frames * frame_bytes;
        let result = self.pcm.as_ref().expect("active stream owns PCM").write(
            &self.scratch[start..end],
            frames,
            frame_bytes,
        );
        match result {
            Ok(WriteProgress::Accepted { frames }) => {
                self.submitted_frames += frames as u64;
                self.state = OutputState::Running;
                Ok(WriteProgress::Accepted { frames })
            }
            Ok(WriteProgress::WouldBlock) => Ok(WriteProgress::WouldBlock),
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }

    fn fail(&mut self) {
        self.state = OutputState::Failed;
        self.pcm.take();
    }
    pub fn progress(&mut self) -> Result<OutputProgress, AlsaError> {
        let Some(pcm) = &self.pcm else {
            return Err(AlsaError::new(
                ErrorCode::OutputFault,
                "output has no active backend clock",
            ));
        };
        match pcm.delay() {
            Ok(delay) if delay <= self.submitted_frames => Ok(OutputProgress {
                state: self.state,
                submitted_frames: self.submitted_frames,
                backend_delay_frames: delay,
                estimated_consumed_frames: self.submitted_frames - delay,
                measurement_boundary: "alsa_client_estimate",
            }),
            result => {
                let error = result.err().unwrap_or(AlsaError::new(
                    ErrorCode::OutputFault,
                    "ALSA delay exceeds submitted frames",
                ));
                self.fail();
                Err(error)
            }
        }
    }
    pub fn stop(&mut self) -> Result<(), AlsaError> {
        let result = self.pcm.as_ref().map_or(Ok(()), native::Pcm::stop);
        if result.is_err() || self.state == OutputState::Failed {
            self.state = OutputState::Failed;
        } else {
            self.state = OutputState::Stopped;
        }
        self.pcm.take();
        result
    }
    /// Drops pending frames and reprepares the same endpoint for an explicit new epoch.
    pub fn suspend(&mut self, epochs: Epochs) -> Result<OutputProgress, AlsaError> {
        let progress = self.progress()?;
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| {
                pcm.stop()?;
                pcm.prepare()
            });
        if let Err(error) = result {
            self.fail();
            return Err(error);
        }
        self.epochs = epochs;
        self.state = OutputState::Prepared;
        self.submitted_frames = 0;
        self.next_position = None;
        Ok(progress)
    }
    pub fn finish(&mut self, permission: SignalPermission) -> Result<DrainProgress, AlsaError> {
        if permission != SignalPermission::Granted {
            return Err(AlsaError::new(
                ErrorCode::SafetyPolicyBlocked,
                "draining requires current signal approval",
            ));
        }
        if self.state == OutputState::Stopped {
            return Ok(DrainProgress::Complete);
        }
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| {
                if self.state == OutputState::Draining {
                    pcm.drain_progress()
                } else {
                    pcm.drain()
                }
            });
        match result {
            Ok(DrainProgress::Complete) => {
                self.state = OutputState::Stopped;
                self.pcm.take();
                Ok(DrainProgress::Complete)
            }
            Ok(progress) => {
                self.state = OutputState::Draining;
                Ok(progress)
            }
            Err(error) => {
                self.fail();
                Err(error)
            }
        }
    }
    pub fn wait_ready(&mut self, timeout_ms: u32) -> Result<bool, AlsaError> {
        if timeout_ms == 0 || timeout_ms > 20 {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "ALSA wait must be bounded to 1..20 milliseconds",
            ));
        }
        if self.state == OutputState::Draining {
            // Completed hardware drain can wake poll with POLLERR in the valid SETUP state.
            // Observe completion without restarting the hardware drain and its silence padding.
            std::thread::sleep(std::time::Duration::from_millis(u64::from(timeout_ms)));
            return Ok(false);
        }
        let result = self
            .pcm
            .as_ref()
            .ok_or(AlsaError::new(ErrorCode::OutputFault, "output is not open"))
            .and_then(|pcm| pcm.wait_ready(timeout_ms));
        if result.is_err() {
            self.fail();
        }
        result
    }
}

const INVALID_DOP_SPAN: AlsaError = AlsaError::new(
    ErrorCode::AssetInvalid,
    "DoP span differs from the pending stream boundary",
);

#[derive(Debug, Clone, PartialEq, Eq)]
struct DopCursor {
    source_bits: u64,
    sequence: u64,
    marker: u8,
    pending: Option<(u64, u64)>,
}
impl DopCursor {
    fn new(stream: DopStream) -> Self {
        Self {
            source_bits: stream.source_sample_position,
            sequence: 0,
            marker: 0x05,
            pending: None,
        }
    }
    fn validate(
        &self,
        block: &DopBlock,
        offset: usize,
        frames: usize,
        period: u32,
    ) -> Result<(), AlsaError> {
        if block.frame_count() > u64::from(period) {
            return Err(AlsaError::new(
                ErrorCode::ResourceBudgetExceeded,
                "DoP block exceeds the prepared carrier period",
            ));
        }
        let end = offset
            .checked_add(frames)
            .filter(|end| *end as u64 <= block.frame_count())
            .ok_or(INVALID_DOP_SPAN)?;
        if frames == 0 {
            return Err(INVALID_DOP_SPAN);
        }
        let source_bits = block
            .frame_position()
            .checked_add(offset as u64)
            .and_then(|position| position.checked_mul(16))
            .ok_or(INVALID_DOP_SPAN)?;
        block
            .frame_position()
            .checked_add(end as u64)
            .and_then(|position| position.checked_mul(16))
            .ok_or(INVALID_DOP_SPAN)?;
        self.sequence.checked_add(1).ok_or(INVALID_DOP_SPAN)?;
        if block.sequence() != self.sequence
            || source_bits != self.source_bits
            || block.payload()[offset * 6 + 2] != self.marker
        {
            return Err(INVALID_DOP_SPAN);
        }
        match self.pending {
            Some(span) if span != (block.frame_position(), block.frame_count()) => {
                Err(INVALID_DOP_SPAN)
            }
            None if offset != 0 => Err(INVALID_DOP_SPAN),
            _ => Ok(()),
        }
    }
    fn begin(&mut self, block: &DopBlock) {
        self.pending = Some((block.frame_position(), block.frame_count()));
    }
    fn accept(&mut self, frames: usize) {
        if frames == 0 {
            return;
        }
        let (position, count) = self
            .pending
            .expect("validated DoP write owns a pending span");
        self.source_bits += frames as u64 * 16;
        if !frames.is_multiple_of(2) {
            self.marker = if self.marker == 0x05 { 0xfa } else { 0x05 };
        }
        if self.source_bits / 16 == position + count {
            self.sequence += 1;
            self.pending = None;
        }
    }
}

/// Submits opaque stereo DoP carrier bytes with an explicit stream boundary.
/// Accepted counts and progress use carrier frames, while the cursor uses source bits.
/// This type exposes no PCM processing, drain, or suspend operation.
/// ```compile_fail
/// use earph_alsa::{DopOutput, SignalPermission};
/// use earph_core::audio::PcmBlock;
/// fn wrong(output: &mut DopOutput, block: &PcmBlock) {
///     output.write(block, 0, 1, SignalPermission::Granted).unwrap();
/// }
/// ```
/// ```compile_fail
/// use earph_alsa::{AlsaOutput, SignalPermission};
/// use earph_core::dop::DopBlock;
/// fn wrong(output: &mut AlsaOutput, block: &DopBlock) {
///     output.write(block, 0, 1, SignalPermission::Granted).unwrap();
/// }
/// ```
pub struct DopOutput {
    output: AlsaOutput,
    cursor: DopCursor,
}
impl DopOutput {
    pub fn open(request: OpenRequest, stream: DopStream) -> Result<Self, AlsaError> {
        request.validate()?;
        if !stream.source_sample_position.is_multiple_of(16) {
            return Err(INVALID_DOP_SPAN);
        }
        if !matches!(stream.bit_rate_hz, 2_822_400 | 5_644_800)
            || stream.channels != ChannelLayout::Stereo
            || request.format.channels() != stream.channels
            || request.format.representation() != PcmRepresentation::Signed24
            || request.format.sample_rate_hz() != stream.bit_rate_hz / 16
        {
            return Err(AlsaError::new(
                ErrorCode::FormatUnsupported,
                "DoP requires an exact stereo packed signed24 carrier matching DSD64 or DSD128",
            ));
        }
        Ok(Self {
            output: AlsaOutput::open(request, stream.epochs)?,
            cursor: DopCursor::new(stream),
        })
    }
    pub fn report(&self) -> &OpenReport {
        self.output.report()
    }
    pub fn state(&self) -> OutputState {
        self.output.state()
    }
    /// Native accepted carrier frames, independent of reader and packer progress.
    pub fn submitted_frames(&self) -> u64 {
        self.output.submitted_frames()
    }
    /// Actual period scratch capacity, charged separately from codec working storage.
    pub fn audio_storage_bytes(&self) -> Result<u64, AlsaError> {
        self.output.audio_storage_bytes()
    }
    pub fn progress(&mut self) -> Result<OutputProgress, AlsaError> {
        self.output.progress()
    }
    /// Close without drain, filler, or replay of the pending suffix.
    pub fn stop(&mut self) -> Result<(), AlsaError> {
        self.output.stop()
    }
    pub fn wait_ready(&mut self, timeout_ms: u32) -> Result<bool, AlsaError> {
        self.output.wait_ready(timeout_ms)
    }
    pub fn write(
        &mut self,
        block: &DopBlock,
        offset: usize,
        frames: usize,
        permission: SignalPermission,
    ) -> Result<WriteProgress, AlsaError> {
        self.output.validate_write(permission)?;
        if block.epochs() != self.output.epochs {
            return Err(AlsaError::new(
                ErrorCode::StaleSession,
                "DoP belongs to a different output epoch",
            ));
        }
        let format = self.output.report.requested.format;
        if block.carrier_rate_hz() != format.sample_rate_hz()
            || block.channels() != format.channels()
        {
            return Err(AlsaError::new(
                ErrorCode::FormatUnsupported,
                "DoP carrier differs from the opened stream",
            ));
        }
        self.cursor.validate(
            block,
            offset,
            frames,
            self.output.report.requested.period_frames,
        )?;
        let pending = self.cursor.pending.is_some();
        if pending {
            if &self.output.scratch[..block.payload().len()] != block.payload() {
                return Err(INVALID_DOP_SPAN);
            }
        } else {
            self.output.scratch[..block.payload().len()].copy_from_slice(block.payload());
        }
        let progress = self.output.submit(offset * 6, frames, 6)?;
        if !pending {
            self.cursor.begin(block);
        }
        if let WriteProgress::Accepted { frames } = progress {
            self.cursor.accept(frames);
        }
        Ok(progress)
    }
}
#[cfg(test)]
mod dop_cursor_tests {
    use super::*;
    use earph_core::{
        audio::ChannelLayout,
        dop::{DopBlock, DopStream},
    };

    fn stream() -> DopStream {
        DopStream {
            bit_rate_hz: 2_822_400,
            channels: ChannelLayout::Stereo,
            epochs: Epochs::default(),
            source_sample_position: 0,
        }
    }
    fn block(sequence: u64, position: u64, marker: u8) -> DopBlock {
        let other = if marker == 0x05 { 0xfa } else { 0x05 };
        DopBlock::new(
            vec![
                1, 2, marker, 3, 4, marker, 5, 6, other, 7, 8, other, 9, 10, marker, 11, 12, marker,
            ],
            176_400,
            ChannelLayout::Stereo,
            Epochs::default(),
            sequence,
            position,
        )
        .unwrap()
    }
    #[test]
    fn cursor_retains_zero_and_partial_progress_until_the_complete_block() {
        // This test supplies progress directly. It makes no native short-write or EAGAIN claim.
        let mut cursor = DopCursor::new(stream());
        let first = block(0, 0, 0x05);
        cursor.validate(&first, 0, 3, 64).unwrap();
        cursor.begin(&first);
        let before = cursor.clone();
        cursor.accept(0);
        assert_eq!(cursor, before);
        cursor.accept(1);
        assert_eq!(
            (cursor.source_bits, cursor.sequence, cursor.marker),
            (16, 0, 0xfa)
        );
        assert!(cursor.pending.is_some());
        let before = cursor.clone();
        assert!(cursor.validate(&first, 0, 1, 64).is_err());
        assert!(cursor.validate(&block(1, 1, 0xfa), 0, 1, 64).is_err());
        assert_eq!(cursor, before);
        cursor.validate(&first, 1, 2, 64).unwrap();
        cursor.accept(1);
        assert_eq!(
            (cursor.source_bits, cursor.sequence, cursor.marker),
            (32, 0, 0x05)
        );
        cursor.validate(&first, 2, 1, 64).unwrap();
        cursor.accept(1);
        assert_eq!(
            (cursor.source_bits, cursor.sequence, cursor.marker),
            (48, 1, 0xfa)
        );
        assert!(cursor.pending.is_none());
        assert!(cursor.validate(&first, 0, 3, 64).is_err());
        cursor.validate(&block(1, 3, 0xfa), 0, 3, 64).unwrap();
    }
    #[test]
    fn cursor_rejects_overflow_budgets_and_wrong_boundaries_without_progress() {
        let mut cursor = DopCursor::new(stream());
        let first = block(0, 0, 0x05);
        for (offset, frames, period) in [
            (usize::MAX, 2, 64),
            (0, 0, 64),
            (0, 4, 64),
            (0, 1, 2),
            (1, 1, 64),
        ] {
            let before = cursor.clone();
            assert!(cursor.validate(&first, offset, frames, period).is_err());
            assert_eq!(cursor, before);
        }
        cursor.sequence = u64::MAX;
        let last_sequence = block(u64::MAX, 0, 0x05);
        assert!(cursor.validate(&last_sequence, 0, 1, 64).is_err());
        assert_eq!(cursor.sequence, u64::MAX);
        assert!(
            DopBlock::new(
                vec![1, 2, 5, 3, 4, 5],
                176_400,
                ChannelLayout::Stereo,
                Epochs::default(),
                0,
                u64::MAX / 16
            )
            .is_err()
        );
    }
}
