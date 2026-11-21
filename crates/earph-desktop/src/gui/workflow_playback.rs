// ABOUTME: Composes one explicit Play into fenced preparation, resume, and Unmute requests.
// ABOUTME: Cancels unsent continuations without replaying uncertain audio commands.
use super::*;
use crate::TransportView;
use earph_core::{
    control::{SourceKind, VolumeObservation},
    ids::{Epochs, QueueEntryId},
};
#[cfg(all(test, target_os = "linux"))]
#[path = "workflow_playback_tests.rs"]
mod tests;

#[derive(Clone, Copy)]
pub(super) enum PlayMutation {
    Stop,
    Select,
    Start,
    Resume,
    Unmute,
}
enum PlayStage {
    Preflight(PlayMutation),
    Waiting {
        phase: PlayMutation,
        request: Option<RequestId>,
    },
}
struct PlayTarget {
    session: SessionId,
    epochs: Epochs,
}
pub(super) struct LocalPlay {
    basis: Arc<StateView>,
    entry: QueueEntryId,
    target: Option<PlayTarget>,
    stage: PlayStage,
    deadline: Instant,
    takeover: bool,
}
pub(super) struct StopTarget {
    owner: Option<OwnerId>,
    zone: ZoneId,
    session: Option<SessionId>,
    session_epoch: u64,
}
impl StopTarget {
    pub fn observed(state: &StateView) -> Self {
        Self {
            owner: state.owner_id.clone(),
            zone: state.zone_id.clone(),
            session: state.session_id.clone(),
            session_epoch: state.epochs.session,
        }
    }
    pub fn matches(&self, state: &StateView) -> bool {
        self.owner == state.owner_id
            && self.zone == state.zone_id
            && self.session == state.session_id
            && self.session_epoch == state.epochs.session
    }
}
impl LocalPlay {
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn stop_target(&self) -> Option<StopTarget> {
        let target = self.target.as_ref()?;
        Some(StopTarget {
            owner: self.basis.owner_id.clone(),
            zone: self.basis.zone_id.clone(),
            session: Some(target.session.clone()),
            session_epoch: target.epochs.session,
        })
    }
}
impl GuiController {
    pub fn volume_edit_available(&self) -> bool {
        self.control_edit_available()
            && self.state().is_some_and(|state| {
                state.output_consented
                    && state.dsd_playback.is_none()
                    && state.output.as_ref().is_some_and(|output| {
                        output.volume_mode == earph_core::safety::VolumeMode::Software
                    })
            })
    }
    pub(super) fn set_volume(
        &mut self,
        basis: Arc<StateView>,
        millidecibels: i32,
        ramp_frames: u32,
    ) -> Result<(), String> {
        if !self.volume_edit_available() {
            return Err("Software volume is unavailable for the current output".into());
        }
        let observed = VolumeObservation {
            source: basis.source,
            active_entry_id: basis.active_entry_id.clone(),
            epochs: basis.epochs,
            device_epoch: basis.device_epoch,
            graph_epoch: basis.graph_epoch,
            output: basis
                .output
                .clone()
                .ok_or("Observed volume has no output")?,
            output_consented: basis.output_consented,
            muted: basis.muted,
            software_gain_zero: basis.software_gain_zero,
            volume_policy: earph_core::safety::VolumePolicy::new(
                basis.volume_policy.software_cap_db,
                basis.volume_policy.hardware_cap_db,
            )
            .map_err(|error| error.to_string())?,
            local_volume_policy: earph_core::safety::VolumePolicy::new(
                basis.local_volume_policy.software_cap_db,
                basis.local_volume_policy.hardware_cap_db,
            )
            .map_err(|error| error.to_string())?,
        };
        let observed_revision = basis.revision;
        let task = Task::VolumePreflight {
            basis,
            command: Command::VolumeSetGuarded {
                observed: Box::new(observed),
                observed_revision,
                millidecibels,
                ramp_frames,
            },
        };
        if self.pending.is_some() || self.ordinary_in_flight() {
            self.waiting_edit = Some(task);
            Ok(())
        } else {
            self.start_query(Command::State, task)
        }
    }
    pub(super) fn complete_volume_preflight(
        &mut self,
        basis: Arc<StateView>,
        command: Command,
        receipt: crate::ReceiptView,
    ) -> Result<(), String> {
        let Some(ResultView::State(state)) = receipt.result else {
            return Err("Volume preflight returned no Engine state".into());
        };
        if !same_review_context(&basis, &state)
            || basis.muted != state.muted
            || basis.software_gain_zero != state.software_gain_zero
            || basis.volume_policy != state.volume_policy
            || basis.local_volume_policy != state.local_volume_policy
            || basis.output_consented != state.output_consented
            || !state.output_consented
            || state.dsd_playback.is_some()
            || state
                .output
                .as_ref()
                .is_none_or(|output| output.volume_mode != earph_core::safety::VolumeMode::Software)
        {
            return Err(
                "Volume output, source, mute, or policy changed; review the current output".into(),
            );
        }
        if !self.control_edit_available() {
            return Err("A pending or uncertain action prevents changing volume".into());
        }
        let request = Request::mutation(request_id()?, &state, command)
            .map_err(|error| format!("{error:?}"))?;
        let id = self.submit(request)?;
        self.pending = Some((
            id,
            Task::Mutation {
                draft_epoch: self.draft_epoch,
            },
        ));
        Ok(())
    }
    pub fn play_local_available(&self) -> bool {
        self.local_play.is_none()
            && self.queue_edit_available()
            && self
                .queue
                .as_ref()
                .is_some_and(|queue| !queue.entries.is_empty())
            && self
                .state()
                .is_some_and(|state| state.output.is_some() && state.output_consented)
    }
    pub fn play_local_pending(&self) -> bool {
        self.local_play.is_some()
    }
    pub fn playback_action_status(&self) -> &'static str {
        match self.local_play.as_ref().map(|play| &play.stage) {
            Some(
                PlayStage::Preflight(PlayMutation::Unmute)
                | PlayStage::Waiting {
                    phase: PlayMutation::Unmute,
                    ..
                },
            ) => "Starting playback",
            Some(
                PlayStage::Preflight(PlayMutation::Resume)
                | PlayStage::Waiting {
                    phase: PlayMutation::Resume,
                    ..
                },
            ) => "Resuming playback",
            Some(_) => "Preparing playback",
            None => match self.state().map(|state| (state.transport, state.muted)) {
                Some((TransportView::Playing, true)) => "Muted",
                Some((TransportView::Playing, false)) => "Playing",
                Some((TransportView::Paused, _)) => "Paused",
                Some((TransportView::Failed, _)) => "Playback unavailable",
                Some((TransportView::Preparing | TransportView::Buffering, _)) => {
                    "Preparing playback"
                }
                Some(_) => "Ready to play",
                None => "Not connected",
            },
        }
    }
    pub(super) fn play_local(
        &mut self,
        basis: Arc<StateView>,
        takeover: bool,
        entry_id: Option<QueueEntryId>,
    ) -> Result<(), String> {
        if !self.play_local_available() {
            return Err("Wait for the current request and approve an output before playing a nonempty queue".into());
        }
        if self.state().is_none_or(|state| {
            !same_review_context(&basis, state)
                || basis.output_consented != state.output_consented
                || basis.volume_policy != state.volume_policy
                || basis.local_volume_policy != state.local_volume_policy
        }) {
            return Err("Playback context changed; refresh before playing".into());
        }
        let queue = self.queue.as_ref().unwrap();
        let active_entry = basis
            .active_entry_id
            .as_ref()
            .filter(|_| basis.source == Some(SourceKind::LocalFile) && basis.session_id.is_some());
        let entry = entry_id
            .as_ref()
            .or(active_entry)
            .or(queue
                .selected
                .as_ref()
                .or_else(|| queue.entries.first().map(|entry| &entry.id)))
            .unwrap()
            .clone();
        if !queue.entries.iter().any(|candidate| candidate.id == entry) {
            return Err("The selected file is no longer in the current queue".into());
        }
        if basis.session_id.is_some() && basis.source != Some(SourceKind::LocalFile) && !takeover {
            return Err(
                "Another source owns playback; explicit takeover approval is required".into(),
            );
        }
        let same_entry = basis.source == Some(SourceKind::LocalFile)
            && basis.session_id.is_some()
            && basis.active_entry_id.as_ref() == Some(&entry);
        if same_entry && basis.transport == TransportView::Playing && !basis.muted {
            return Ok(());
        }
        let command = if !same_entry
            && (basis.source == Some(SourceKind::LocalFile) || takeover)
            && basis.session_id.is_some()
        {
            Some((Command::Stop, PlayMutation::Stop))
        } else if !same_entry
            && queue
                .selected
                .as_ref()
                .or_else(|| queue.entries.first().map(|e| &e.id))
                != Some(&entry)
        {
            Some((
                Command::QueueSelect {
                    id: Some(entry.clone()),
                },
                PlayMutation::Select,
            ))
        } else if same_entry && basis.transport == TransportView::Paused {
            Some((Command::Resume, PlayMutation::Resume))
        } else if same_entry
            && matches!(
                basis.transport,
                TransportView::Preparing | TransportView::Buffering | TransportView::Playing
            )
        {
            None
        } else {
            Some((
                Command::Start {
                    source: SourceKind::LocalFile,
                    device_id: basis.output.as_ref().unwrap().device_id.clone(),
                    takeover: takeover || basis.source == Some(SourceKind::LocalFile),
                    seekable: true,
                },
                PlayMutation::Start,
            ))
        };
        let target = same_entry.then(|| PlayTarget {
            session: basis.session_id.clone().unwrap(),
            epochs: basis.epochs,
        });
        let stage = command.as_ref().map_or(
            PlayStage::Waiting {
                phase: PlayMutation::Start,
                request: None,
            },
            |(_, phase)| PlayStage::Preflight(*phase),
        );
        self.local_play = Some(LocalPlay {
            basis: basis.clone(),
            entry,
            target,
            stage,
            deadline: Instant::now() + Duration::from_secs(30),
            takeover,
        });
        if let Some((command, phase)) = command
            && let Err(error) = self.capture_mutation(basis, command, Some(phase))
        {
            self.cancel_local_play();
            return Err(error);
        }
        self.refresh = true;
        Ok(())
    }
    pub(super) fn cancel_local_play(&mut self) {
        self.local_play = None;
        if self
            .waiting_edit
            .as_ref()
            .is_some_and(|task| matches!(task, Task::Preflight { play: Some(_), .. }))
        {
            self.waiting_edit = None;
        }
        if let Some((_, task)) = self.pending.as_mut()
            && matches!(task, Task::Preflight { play: Some(_), .. })
        {
            *task = Task::Before;
        }
    }
    pub(super) fn local_play_submitted(
        &mut self,
        phase: PlayMutation,
        state: &StateView,
        request: RequestId,
        session: Option<SessionId>,
    ) -> Result<(), String> {
        let play = self
            .local_play
            .as_mut()
            .ok_or("Playback intent was cancelled before submission")?;
        let epochs = if matches!(phase, PlayMutation::Start) {
            state
                .epochs
                .next_session()
                .map_err(|error| error.to_string())?
        } else {
            state.epochs
        };
        play.target = session.map(|session| PlayTarget { session, epochs });
        if matches!(
            phase,
            PlayMutation::Start | PlayMutation::Resume | PlayMutation::Unmute
        ) && play.target.is_none()
        {
            return Err("Playback request has no source session".into());
        }
        play.stage = PlayStage::Waiting {
            phase,
            request: Some(request),
        };
        Ok(())
    }
    fn play_request_error(&self, id: &RequestId) -> String {
        let Some(record) = self.model().record(id) else {
            return "Playback request is no longer retained; refresh before playing again".into();
        };
        match record.outcome.as_ref() {
            Some(Ok(Reply::Receipt(receipt))) => receipt
                .error
                .as_ref()
                .map(|error| error.message.clone())
                .unwrap_or_else(|| {
                    format!(
                        "Playback {:?}; inspect the retained request",
                        record.presentation
                    )
                }),
            Some(Ok(Reply::OwnerError(error))) => error.error.message.clone(),
            Some(Err(DeliveryFailure::Local { detail, .. })) => detail.clone(),
            Some(Err(DeliveryFailure::Protocol(error))) => error.detail.into(),
            None => format!(
                "Playback {:?}; inspect the retained request",
                record.presentation
            ),
        }
    }
    pub(super) fn expire_local_play(&mut self, now: Instant) -> bool {
        if self
            .local_play
            .as_ref()
            .is_none_or(|play| now < play.deadline)
        {
            return false;
        }
        let has_target = self
            .local_play
            .as_ref()
            .is_some_and(|play| play.target.is_some());
        let cleanup = if has_target {
            self.safety_command(Command::Stop)
        } else {
            self.cancel_local_play();
            Ok(())
        };
        self.refresh = true;
        self.error = Some(match cleanup {
            Ok(()) => "Playback preparation timed out; its automatic start was cancelled".into(),
            Err(error) => {
                format!("Playback preparation timed out; Stop could not be submitted: {error}")
            }
        });
        true
    }
    pub(super) fn poll_local_play(&mut self, now: Instant) {
        if self.expire_local_play(now) {
            return;
        }
        let Some(play) = self.local_play.take() else {
            return;
        };
        let mut minimum_revision = play.basis.revision;
        if let PlayStage::Waiting {
            phase,
            request: Some(id),
        } = &play.stage
        {
            let Some(record) = self.model().record(id) else {
                self.cancel_local_play();
                self.error = Some(self.play_request_error(id));
                return;
            };
            if record.owner_changed {
                self.cancel_local_play();
                self.error = Some("Playback owner changed; the request remains retained".into());
                return;
            }
            if !record.in_flight {
                if !matches!(
                    record.presentation,
                    Presentation::Queued | Presentation::Accepted | Presentation::Applied
                ) {
                    let error = self.play_request_error(id);
                    self.cancel_local_play();
                    self.error = Some(error);
                    return;
                }
                if matches!(phase, PlayMutation::Unmute)
                    && record.presentation == Presentation::Applied
                {
                    return;
                }
                if let Some(Ok(Reply::Receipt(receipt))) = &record.outcome {
                    minimum_revision = receipt.revision;
                }
            }
        }
        let Some(state) = self.state().cloned() else {
            self.local_play = Some(play);
            return;
        };
        if state.owner_id != play.basis.owner_id
            || state.zone_id != play.basis.zone_id
            || state.output != play.basis.output
            || state.device_epoch != play.basis.device_epoch
            || state.graph_epoch != play.basis.graph_epoch
            || state.volume_policy != play.basis.volume_policy
            || state.local_volume_policy != play.basis.local_volume_policy
            || !state.output_consented
        {
            self.cancel_local_play();
            self.error = Some(
                "Playback output, owner, or volume policy changed; review the current output before playing".into(),
            );
            return;
        }
        let PlayStage::Waiting { phase, request } = &play.stage else {
            self.local_play = Some(play);
            return;
        };
        if request.as_ref().is_some_and(|id| {
            self.model().record(id).is_none_or(|record| {
                record.in_flight || record.presentation == Presentation::Queued
            })
        }) || state.revision < minimum_revision
        {
            self.local_play = Some(play);
            return;
        }
        let Some(target) = play.target.as_ref() else {
            if matches!(phase, PlayMutation::Select) {
                self.continue_local_play(play, state, PlayMutation::Start);
                return;
            }
            self.local_play = Some(play);
            return;
        };
        if matches!(phase, PlayMutation::Stop) {
            if state.epochs.session != target.epochs.session {
                self.error = Some("Playback source changed during Stop".into());
                return;
            }
            if state.transport == TransportView::Idle && state.session_id.is_none() {
                let applied = request.as_ref().is_some_and(|id| {
                    self.model()
                        .record(id)
                        .is_some_and(|r| r.presentation == Presentation::Applied)
                });
                if applied {
                    self.continue_local_play(play, state, PlayMutation::Select);
                    return;
                }
            } else if state.session_id.as_ref() != Some(&target.session)
                || state.source != play.basis.source
            {
                self.error = Some("Playback source changed during Stop".into());
                return;
            }
            self.local_play = Some(play);
            return;
        }
        if state.session_id.as_ref() != Some(&target.session)
            || state.epochs != target.epochs
            || state.source != Some(SourceKind::LocalFile)
            || state.active_entry_id.as_ref() != Some(&play.entry)
        {
            self.cancel_local_play();
            self.error = Some("Playback source changed; no automatic Unmute was sent".into());
            return;
        }
        if let Some(error) = state.error {
            self.cancel_local_play();
            self.error = Some(format!("Playback preparation failed: {error:?}"));
            return;
        }
        if matches!(phase, PlayMutation::Unmute) {
            self.local_play = Some(play);
            return;
        }
        let ready = state
            .playback
            .as_ref()
            .is_some_and(|p| p.source_entry_id == play.entry && p.backend.is_some())
            || state
                .dsd_playback
                .as_ref()
                .is_some_and(|p| p.source_entry_id == play.entry);
        if !ready
            || self.pending.is_some()
            || self.waiting_edit.is_some()
            || self.ordinary_in_flight()
        {
            self.local_play = Some(play);
            return;
        }
        if !state.muted {
            return;
        }
        let mut play = play;
        play.stage = PlayStage::Preflight(PlayMutation::Unmute);
        self.local_play = Some(play);
        if let Err(error) = self.capture_mutation(
            Arc::new(state),
            Command::Mute { muted: false },
            Some(PlayMutation::Unmute),
        ) {
            self.cancel_local_play();
            self.error = Some(error);
        }
    }
    fn continue_local_play(&mut self, mut play: LocalPlay, state: StateView, phase: PlayMutation) {
        if self.pending.is_some() || self.waiting_edit.is_some() || self.ordinary_in_flight() {
            self.local_play = Some(play);
            return;
        }
        if matches!(phase, PlayMutation::Start) && !same_review_context(&play.basis, &state) {
            self.error = Some("Playback source changed after selecting the file".into());
            return;
        }
        let command = match phase {
            PlayMutation::Select => Command::QueueSelect {
                id: Some(play.entry.clone()),
            },
            PlayMutation::Start => Command::Start {
                source: SourceKind::LocalFile,
                device_id: state.output.as_ref().unwrap().device_id.clone(),
                takeover: play.takeover,
                seekable: true,
            },
            _ => unreachable!(),
        };
        let basis = Arc::new(state);
        play.basis = basis.clone();
        play.target = None;
        play.stage = PlayStage::Preflight(phase);
        self.local_play = Some(play);
        if let Err(error) = self.capture_mutation(basis, command, Some(phase)) {
            self.cancel_local_play();
            self.error = Some(error);
        }
    }
}
