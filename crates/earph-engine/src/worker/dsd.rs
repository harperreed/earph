// ABOUTME: Owns one admitted DSD reader, packer, pending carrier block, and native output.
// ABOUTME: Tracks accepted source bits and closes each native instance without PCM gain or drain.
use super::*;
use earph_core::dop::*;
use earph_media::dsd::DsdFile;

const INVALID: EngineError =
    EngineError::new(ErrorCode::AssetInvalid, "invalid DSD source frontier");
const INCOMPATIBLE: EngineError = EngineError::new(
    ErrorCode::FormatUnsupported,
    "DSD queue requires one supported source format",
);
const CANCELLED: EngineError =
    EngineError::new(ErrorCode::OperationCancelled, "DSD source cancelled");
const UNQUALIFIED: EngineError = EngineError::new(
    ErrorCode::SafetyPolicyBlocked,
    "DSD endpoint qualification is unavailable",
);

/// No production constructor exists until concrete endpoint qualification is implemented.
pub(super) enum SubmissionAuthority {
    #[cfg(test)]
    SoftwareCapture,
}
fn public_submission_authority(_: &SessionSnapshot) -> Result<SubmissionAuthority, EngineError> {
    Err(UNQUALIFIED)
}
struct Track {
    entry: QueueEntry,
    file: DsdFile,
    format: DsdSourceFormat,
    total: u64,
    decoded: u64,
    sequence: u64,
}
impl Track {
    fn open(
        entry: QueueEntry,
        epochs: Epochs,
        cancel: &AtomicBool,
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        if cancel.load(Ordering::Acquire) {
            return Err(CANCELLED);
        }
        let admitted = (|| {
            if !earph_media::is_dsd(Path::new(&entry.path))? {
                return Err(INCOMPATIBLE);
            }
            let file = DsdFile::open(Path::new(&entry.path), epochs, MemoryBudget::default())?;
            let format =
                DsdSourceFormat::new(file.bit_rate_hz(), earph_core::audio::ChannelLayout::Stereo)?;
            let total = file.total_samples();
            Ok(Self {
                entry: entry.clone(),
                file,
                format,
                total,
                decoded: 0,
                sequence: 0,
            })
        })();
        match admitted {
            Ok(track) => {
                if cancel.load(Ordering::Acquire) {
                    return Err(CANCELLED);
                }
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::DsdReady {
                        format: track.format,
                        total_source_bits: track.total,
                    },
                )?;
                Ok(track)
            }
            Err(error) => {
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::Failed { code: error.code },
                )?;
                Err(error)
            }
        }
    }
}
struct Pending {
    block: DopBlock,
    offset: usize,
}
impl Pending {
    fn accept(&mut self, frames: usize) -> Result<bool, EngineError> {
        let end = self.offset.checked_add(frames).ok_or(INVALID)?;
        if frames == 0 || end > self.block.frame_count() as usize {
            return Err(INVALID);
        }
        self.offset = end;
        Ok(end == self.block.frame_count() as usize)
    }
}

#[cfg(test)]
mod pending_tests {
    use super::*;
    #[test]
    fn partial_acceptance_keeps_bytes_and_refuses_impossible_retry_counts() {
        let stream = DopStream {
            bit_rate_hz: 2_822_400,
            channels: earph_core::audio::ChannelLayout::Stereo,
            epochs: Epochs::default(),
            source_sample_position: 0,
        };
        let mut packer = DopPacker::new(stream, MemoryBudget::default()).unwrap();
        let source = earph_core::audio::DsdBlock::new(
            (1..=12).collect(),
            2_822_400,
            stream.channels,
            stream.epochs,
        )
        .unwrap();
        let mut pending = Pending {
            block: packer.push(&source).unwrap().unwrap(),
            offset: 0,
        };
        let bytes = pending.block.payload().to_vec();
        assert!(!pending.accept(1).unwrap());
        assert_eq!(pending.offset, 1);
        assert_eq!(pending.block.payload(), bytes);
        assert_eq!(
            (pending.block.sequence(), pending.block.frame_position()),
            (0, 0)
        );
        for impossible in [0, 3] {
            assert!(pending.accept(impossible).is_err());
            assert_eq!(pending.offset, 1);
            assert_eq!(pending.block.payload(), bytes);
        }
        assert!(pending.accept(2).unwrap());
        assert_eq!(pending.offset, 3);
        assert!(pending.accept(1).is_err());
    }
}
struct EntrySpan {
    entry: QueueEntry,
    total: u64,
    start_frame: u64,
    source_start: u64,
    end_frame: u64,
}
#[derive(Clone)]
struct SourceCursor {
    entry: QueueEntry,
    total: u64,
    position: u64,
}
pub(super) struct DsdPlayback {
    // Native output and all source resources retire before the reservation's final field.
    output: Option<output::DsdOutput>,
    current: Track,
    next: Option<Track>,
    packer: Option<DopPacker>,
    pending: Option<Pending>,
    pub(super) settings: Settings,
    pub(super) cancel: Arc<AtomicBool>,
    format: DsdSourceFormat,
    source_epochs: Epochs,
    output_epochs: Epochs,
    backend: Option<BackendObservation>,
    accepted: SourceCursor,
    spans: Vec<EntrySpan>,
    last_boundary: Option<DsdOutputBoundary>,
    finished: bool,
    started: bool,
    view: Arc<Mutex<PlanView>>,
    progress: Arc<Mutex<Option<(Epochs, LocalPlaybackObservation)>>>,
    reservation: QueueReservation,
}
impl Drop for DsdPlayback {
    fn drop(&mut self) {
        let _ = self.close(DsdCloseReason::Cancelled);
    }
}
impl DsdPlayback {
    pub(super) fn prepare(
        settings: Settings,
        entries: Vec<QueueEntry>,
        cancel: Arc<AtomicBool>,
        view: Arc<Mutex<PlanView>>,
        progress: Arc<Mutex<Option<(Epochs, LocalPlaybackObservation)>>>,
        events: &SyncSender<Event>,
    ) -> Result<Self, EngineError> {
        if cancel.load(Ordering::Acquire) {
            return Err(CANCELLED);
        }
        settings.snapshot.validate_dsd_passthrough()?;
        if settings.pcm_processing {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "DSD cannot prepare a PCM graph",
            ));
        }
        if settings
            .snapshot
            .output
            .as_ref()
            .is_none_or(|o| o.backend != earph_core::control::OutputBackend::Alsa)
            || !cfg!(target_os = "linux")
        {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "typed DSD output is unavailable on this backend",
            ));
        }
        let epochs = settings.snapshot.epochs;
        let selected = settings.snapshot.active_entry_id.as_ref().ok_or(FAILED)?;
        let index = entries
            .iter()
            .position(|e| &e.id == selected)
            .ok_or(FAILED)?;
        let metadata = entries
            .iter()
            .try_fold(
                size_of::<Self>()
                    .checked_add(
                        entries
                            .capacity()
                            .checked_mul(size_of::<QueueEntry>())
                            .ok_or(INVALID)?,
                    )
                    .ok_or(INVALID)? as u64,
                |n, e| {
                    n.checked_add(e.path.capacity() as u64)?
                        .checked_add(e.id.as_str().len() as u64)
                },
            )
            .ok_or(INVALID)?;
        let mut expected = None;
        for entry in &entries[index..] {
            let track = Track::open(entry.clone(), epochs, &cancel, events)?;
            if expected.is_some_and(|format| format != track.format) {
                item(
                    events,
                    epochs,
                    &entry.id,
                    PreparationStatus::Failed {
                        code: INCOMPATIBLE.code,
                    },
                )?;
                return Err(INCOMPATIBLE);
            }
            expected = Some(track.format);
        }
        let current = Track::open(entries[index].clone(), epochs, &cancel, events)?;
        let format = current.format;
        let accepted = SourceCursor {
            entry: current.entry.clone(),
            total: current.total,
            position: 0,
        };
        let spans = Vec::with_capacity(256);
        let budget = settings
            .snapshot
            .profile_storage_bytes
            .checked_add(metadata)
            .and_then(|n| n.checked_add(DSD_WORKING_BYTES as u64))
            .and_then(|n| n.checked_add((spans.capacity() * size_of::<EntrySpan>()) as u64))
            .ok_or(INVALID)?;
        MemoryBudget::default().admit(usize::try_from(budget).map_err(|_| INVALID)?)?;
        let mut source = Self {
            output: None,
            current,
            next: None,
            packer: None,
            pending: None,
            settings,
            cancel,
            format,
            source_epochs: epochs,
            output_epochs: epochs,
            backend: None,
            accepted,
            spans,
            last_boundary: None,
            finished: false,
            started: false,
            view: view.clone(),
            progress,
            reservation: QueueReservation { view, epochs },
        };
        source.reset_source(events)?;
        let observation = LocalPlaybackObservation::from(source.observe()?);
        emit(
            events,
            Event::Ready {
                epochs,
                observation,
            },
        )?;
        Ok(source)
    }
    pub(super) fn epochs(&self) -> Epochs {
        self.settings.snapshot.epochs
    }
    fn stream(&self) -> DopStream {
        DopStream {
            bit_rate_hz: self.format.bit_rate_hz(),
            channels: self.format.channels(),
            epochs: self.source_epochs,
            source_sample_position: self.accepted.position,
        }
    }
    fn admit_memory(&self, adapter_bytes: u64) -> Result<(), EngineError> {
        let mut metadata = size_of::<Self>()
            .checked_add(
                self.spans
                    .capacity()
                    .checked_mul(size_of::<EntrySpan>())
                    .ok_or(INVALID)?,
            )
            .ok_or(INVALID)?;
        if let Some(observation) = self.settings.snapshot.observation.dsd() {
            metadata = metadata
                .checked_add(std::mem::size_of_val(observation))
                .and_then(|n| n.checked_add(observation.source_entry_id.as_str().len()))
                .and_then(|n| {
                    n.checked_add(
                        observation
                            .last_boundary
                            .as_ref()
                            .map_or(0, |boundary| boundary.source_entry_id.as_str().len()),
                    )
                })
                .ok_or(INVALID)?;
        }
        let entries = std::iter::once(&self.current.entry)
            .chain(self.next.iter().map(|track| &track.entry))
            .chain(std::iter::once(&self.accepted.entry))
            .chain(self.spans.iter().map(|span| &span.entry));
        for entry in entries {
            metadata = metadata
                .checked_add(entry.path.capacity())
                .and_then(|n| n.checked_add(entry.id.as_str().len()))
                .ok_or(INVALID)?;
        }
        let retained = self
            .settings
            .snapshot
            .profile_storage_bytes
            .checked_add(DSD_WORKING_BYTES as u64)
            .and_then(|n| n.checked_add(adapter_bytes))
            .and_then(|n| n.checked_add(metadata as u64))
            .ok_or(INVALID)?;
        MemoryBudget::default().admit(usize::try_from(retained).map_err(|_| INVALID)?)
    }
    fn reset_source(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(CANCELLED);
        }
        let mut current = Track::open(
            self.accepted.entry.clone(),
            self.epochs(),
            &self.cancel,
            events,
        )?;
        if current.format != self.format || current.total != self.accepted.total {
            return Err(INCOMPATIBLE);
        }
        current.file.seek(self.accepted.position, self.epochs())?;
        current.decoded = self.accepted.position;
        self.current = current;
        self.source_epochs = self.epochs();
        self.output_epochs = self.epochs();
        self.packer = Some(DopPacker::new(self.stream(), MemoryBudget::default())?);
        self.pending = None;
        self.next = None;
        self.backend = None;
        self.finished = false;
        self.started = false;
        self.spans.clear();
        self.spans.push(EntrySpan {
            entry: self.current.entry.clone(),
            total: self.current.total,
            start_frame: 0,
            source_start: self.accepted.position,
            end_frame: (self.current.total - self.accepted.position) / 16,
        });
        self.prefetch(events)
    }
    fn prefetch(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        if self.next.is_some() || self.finished {
            return Ok(());
        }
        let (next, changed) = {
            let mut view = self.view.lock().map_err(|_| FAILED)?;
            let next = view.following(&self.current.entry.id);
            let reserved = next.as_ref().map(|e| e.id.clone());
            let active = Some(self.current.entry.id.clone());
            let changed = view.active != active
                || view.reserved != reserved
                || view.epochs != Some(self.epochs());
            view.active = active;
            view.epochs = Some(self.epochs());
            self.reservation.epochs = self.epochs();
            view.reserved = reserved;
            (next, changed)
        };
        if changed {
            emit(
                events,
                Event::Reserved {
                    epochs: self.epochs(),
                    active: self.current.entry.id.clone(),
                    next: next.as_ref().map(|e| e.id.clone()),
                },
            )?;
        }
        if let Some(entry) = next {
            let track = Track::open(entry.clone(), self.source_epochs, &self.cancel, events)?;
            if track.format != self.format {
                item(
                    events,
                    self.epochs(),
                    &entry.id,
                    PreparationStatus::Failed {
                        code: INCOMPATIBLE.code,
                    },
                )?;
                return Err(INCOMPATIBLE);
            }
            self.next = Some(track);
        }
        self.admit_memory(
            self.backend
                .as_ref()
                .map_or(0, |backend| backend.audio_storage_bytes),
        )
    }
    fn cursor(&self, frames: u64) -> Result<SourceCursor, EngineError> {
        let span = self
            .spans
            .iter()
            .find(|span| frames > span.start_frame && frames <= span.end_frame)
            .or_else(|| self.spans.first().filter(|_| frames == 0))
            .ok_or(INVALID)?;
        let position = frames
            .checked_sub(span.start_frame)
            .and_then(|n| n.checked_mul(16))
            .and_then(|n| n.checked_add(span.source_start))
            .ok_or(INVALID)?;
        if position > span.total {
            return Err(INVALID);
        }
        Ok(SourceCursor {
            entry: span.entry.clone(),
            total: span.total,
            position,
        })
    }
    pub(super) fn failure_observation(&self) -> DsdPlaybackObservation {
        let accepted = &self.accepted;
        let decoded = if self.current.entry.id == accepted.entry.id {
            self.current.decoded.max(accepted.position)
        } else {
            accepted.total
        };
        DsdPlaybackObservation {
            source_format: self.format,
            total_source_bits: accepted.total,
            source_entry_id: accepted.entry.id.clone(),
            source_position_bits: accepted.position,
            decoded_position_bits: decoded,
            source_epochs: self.source_epochs,
            carrier_format: DopCarrierFormat::from_source(self.format),
            output: self.output.as_ref().and_then(|output| {
                self.backend.as_ref().map(|backend| DsdOutputObservation {
                    output_epochs: self.output_epochs,
                    submitted_carrier_frames: output.submitted(),
                    estimated_consumed_carrier_frames: None,
                    backend_delay_carrier_frames: None,
                    consumption_basis: "unavailable",
                    backend: backend.clone(),
                })
            }),
            last_boundary: self.last_boundary.clone(),
            qualification: DsdQualification::Unverified,
            completion_basis: self.finished.then_some(DsdCompletionBasis::SourceSubmitted),
        }
    }
    pub(super) fn observe(&mut self) -> Result<DsdPlaybackObservation, EngineError> {
        let mut observation = self.failure_observation();
        if let Some(output) = &mut self.output {
            observation.output = Some(output.observation(self.output_epochs)?);
        }
        observation.validate()?;
        Ok(observation)
    }
    pub(super) fn close(&mut self, reason: DsdCloseReason) -> Result<(), EngineError> {
        let mut result = Ok(());
        if let Some(mut output) = self.output.take() {
            let submitted = output.submitted();
            if let Ok(cursor) = self.cursor(submitted) {
                self.accepted = cursor;
            } else {
                result = Err(INVALID);
            }
            self.last_boundary = Some(DsdOutputBoundary {
                output_epochs: self.output_epochs,
                submitted_carrier_frames: submitted,
                source_entry_id: self.accepted.entry.id.clone(),
                source_position_bits: self.accepted.position,
                reason,
            });
            if let Err(error) = output.stop() {
                if let Some(boundary) = &mut self.last_boundary {
                    boundary.reason = DsdCloseReason::Failed;
                }
                result = Err(error);
            }
        }
        self.current.file.cancel();
        if let Some(next) = &mut self.next {
            next.file.cancel();
        }
        if let Some(packer) = &mut self.packer {
            packer.cancel();
        }
        self.next = None;
        self.packer = None;
        self.pending = None;
        self.backend = None;
        result
    }
    pub(super) fn matches_settings(&self, settings: &Settings) -> bool {
        let before = &self.settings.snapshot;
        let next = &settings.snapshot;
        next.source == Some(SourceKind::LocalFile)
            && before.owner_id == next.owner_id
            && before.session_id == next.session_id
            && before.device_epoch == next.device_epoch
            && before.graph_epoch == next.graph_epoch
            && before.epochs.session == next.epochs.session
            && (before.epochs.stream == next.epochs.stream
                || before.epochs.stream.checked_add(1) == Some(next.epochs.stream))
    }
    pub(super) fn reposition(
        &mut self,
        settings: Settings,
        position: Option<u64>,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        if !self.matches_settings(&settings) {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "DSD control belongs to another source",
            ));
        }
        if let Some(position) = position
            && (!position.is_multiple_of(16) || position > self.accepted.total)
        {
            return Err(INVALID);
        }
        self.close(if position.is_some() {
            DsdCloseReason::Seek
        } else {
            DsdCloseReason::Pause
        })?;
        self.settings = settings;
        if let Some(position) = position {
            self.accepted.position = position;
        }
        self.reset_source(events)?;
        let observation = LocalPlaybackObservation::from(self.observe()?);
        emit(
            events,
            Event::Paused {
                epochs: self.epochs(),
                observation,
            },
        )
    }
    pub(super) fn apply_settings(
        &mut self,
        settings: Settings,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        if !self.matches_settings(&settings) {
            return Err(EngineError::new(
                ErrorCode::StaleSession,
                "DSD settings belong to another source",
            ));
        }
        let closed = settings.snapshot.muted || !settings.playing;
        if closed {
            self.close(DsdCloseReason::Mute)?;
        }
        self.settings = settings;
        // Safety closure precedes refusal of a prospective incompatible cap or gain.
        self.settings.snapshot.validate_dsd_passthrough()?;
        if self.settings.pcm_processing {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "DSD cannot activate PCM processing",
            ));
        }
        if closed {
            self.reset_source(events)?;
        }
        Ok(())
    }
    pub(super) fn step(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            self.close(DsdCloseReason::Cancelled)?;
            return Ok(true);
        }
        if self.finished {
            return Ok(true);
        }
        if self.finish_at_end(events)? {
            return Ok(true);
        }
        if !self.settings.playing || self.settings.snapshot.muted {
            self.prefetch(events)?;
            return Ok(false);
        }
        let authority = public_submission_authority(&self.settings.snapshot)?;
        self.step_authorized(events, &authority)
    }
    pub(super) fn step_authorized(
        &mut self,
        events: &SyncSender<Event>,
        _authority: &SubmissionAuthority,
    ) -> Result<bool, EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            self.close(DsdCloseReason::Cancelled)?;
            return Ok(true);
        }
        if self.finished {
            return Ok(true);
        }
        if self.finish_at_end(events)? {
            return Ok(true);
        }
        if !self.settings.playing || self.settings.snapshot.muted {
            return Ok(false);
        }
        self.settings.snapshot.validate_dsd_passthrough()?;
        if self.settings.pcm_processing {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "DSD cannot activate PCM processing",
            ));
        }
        self.prefetch(events)?;
        if self.fill_pending(events)? {
            return Ok(true);
        }
        if self.pending.is_none() {
            return Ok(false);
        }
        if self.output.is_none() {
            if self.cancel.load(Ordering::Acquire) {
                return Err(CANCELLED);
            }
            let config = self.settings.snapshot.output.as_ref().ok_or(FAILED)?;
            let mut output = output::DsdOutput::open(config, self.stream())?;
            let observed = output.observation(self.output_epochs)?;
            self.admit_memory(observed.backend.audio_storage_bytes)?;
            self.backend = Some(observed.backend);
            self.output = Some(output);
        }

        if self.cancel.load(Ordering::Acquire) {
            self.close(DsdCloseReason::Cancelled)?;
            return Ok(true);
        }
        if let Some(pending) = &mut self.pending {
            let output = self.output.as_mut().ok_or(INVALID)?;
            match output.write(
                &pending.block,
                pending.offset,
                pending.block.frame_count() as usize - pending.offset,
            )? {
                Some(frames) => {
                    let complete = pending.accept(frames)?;
                    self.accepted =
                        self.cursor(self.output.as_ref().ok_or(INVALID)?.submitted())?;
                    if !self.started && frames > 0 {
                        self.started = true;
                        emit(
                            events,
                            Event::Started {
                                epochs: self.epochs(),
                            },
                        )?;
                    }
                    if complete {
                        self.pending = None;
                    }
                    let observation = LocalPlaybackObservation::from(self.observe()?);
                    if let Ok(mut latest) = self.progress.try_lock() {
                        *latest = Some((self.epochs(), observation));
                    }
                }
                None => output.wait()?,
            }
        }
        Ok(false)
    }
    pub(super) fn fill_pending(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if self.pending.is_none() {
            let period = self
                .settings
                .snapshot
                .output
                .as_ref()
                .ok_or(FAILED)?
                .period_frames as usize;
            let limit = 4096.min(period.checked_mul(2).ok_or(INVALID)?);
            if self.cancel.load(Ordering::Acquire) {
                return Err(CANCELLED);
            }
            match self.current.file.next_block(limit)? {
                Some(frame) => {
                    if frame.sequence != self.current.sequence
                        || frame.source_sample_position != self.current.decoded
                    {
                        return Err(INVALID);
                    }
                    let source_bits = u64::try_from(frame.block.payload.len() / 2)
                        .map_err(|_| INVALID)?
                        .checked_mul(8)
                        .ok_or(INVALID)?;
                    self.current.decoded = self
                        .current
                        .decoded
                        .checked_add(source_bits)
                        .ok_or(INVALID)?;
                    self.current.sequence = self.current.sequence.checked_add(1).ok_or(INVALID)?;
                    self.pending = self
                        .packer
                        .as_mut()
                        .ok_or(INVALID)?
                        .push(&frame.block)?
                        .map(|block| Pending { block, offset: 0 });
                }
                None => {
                    if let Some(next) = self.next.take() {
                        let start = self.spans.last().ok_or(INVALID)?.end_frame;
                        let end = start.checked_add(next.total / 16).ok_or(INVALID)?;
                        if self.spans.len() == 256 {
                            return Err(EngineError::new(
                                ErrorCode::ResourceBudgetExceeded,
                                "DSD entry spans exceed queue capacity",
                            ));
                        }
                        self.spans.push(EntrySpan {
                            entry: next.entry.clone(),
                            total: next.total,
                            start_frame: start,
                            source_start: 0,
                            end_frame: end,
                        });
                        self.current = next;
                        self.prefetch(events)?;
                        return Ok(false);
                    }
                    self.packer.as_mut().ok_or(INVALID)?.finish()?;
                    self.close(DsdCloseReason::SourceSubmitted)?;
                    self.finished = true;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    fn finish_at_end(&mut self, events: &SyncSender<Event>) -> Result<bool, EngineError> {
        if self.accepted.position != self.accepted.total
            || self.current.entry.id != self.accepted.entry.id
            || self.pending.is_some()
        {
            return Ok(false);
        }
        self.prefetch(events)?;
        if self.next.is_some() {
            return Ok(false);
        }
        if self.current.file.next_block(2)?.is_some() {
            return Err(INVALID);
        }
        self.packer.as_mut().ok_or(INVALID)?.finish()?;
        self.close(DsdCloseReason::SourceSubmitted)?;
        self.finished = true;
        Ok(true)
    }
}
