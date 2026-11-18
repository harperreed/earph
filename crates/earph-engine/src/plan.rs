// ABOUTME: Publishes committed queue configuration and observed playback reservations.
// ABOUTME: Keeps filesystem work outside the short reservation gate.
use earph_core::{
    EngineError, ErrorCode,
    control::Command,
    ids::{Epochs, QueueEntryId},
    queue::{QueueEntry, QueueSnapshot},
};

#[derive(Default)]
pub(crate) struct PlanView {
    pub entries: Vec<QueueEntry>,
    pub active: Option<QueueEntryId>,
    pub reserved: Option<QueueEntryId>,
    pub epochs: Option<Epochs>,
}
impl PlanView {
    pub fn validate_edit(&self, command: &Command) -> Result<(), EngineError> {
        if !matches!(
            command,
            Command::QueueRemove { .. } | Command::QueueMove { .. } | Command::QueueSelect { .. }
        ) {
            return Ok(());
        }
        let Some(active) = &self.active else {
            return Ok(());
        };
        let active_index = self
            .entries
            .iter()
            .position(|entry| &entry.id == active)
            .ok_or(EngineError::new(
                ErrorCode::InternalFault,
                "active queue entry disappeared",
            ))?;
        let protected = self
            .reserved
            .as_ref()
            .and_then(|id| self.entries.iter().position(|entry| &entry.id == id))
            .unwrap_or(active_index);
        let blocked = EngineError::new(
            ErrorCode::SourceBusy,
            "the active and reserved next entries cannot change",
        );
        match command {
            Command::QueueRemove { id } | Command::QueueMove { id, .. } => {
                if self
                    .entries
                    .iter()
                    .position(|entry| &entry.id == id)
                    .is_some_and(|index| index <= protected)
                {
                    return Err(blocked);
                }
                if matches!(command, Command::QueueMove { new_index, .. } if *new_index <= protected)
                {
                    return Err(blocked);
                }
            }
            Command::QueueSelect { id } if id.as_ref() != Some(active) => return Err(blocked),
            _ => {}
        }
        Ok(())
    }
    pub fn publish(&mut self, queue: QueueSnapshot) {
        self.entries = queue.entries;
    }
    pub fn following(&self, id: &QueueEntryId) -> Option<QueueEntry> {
        self.entries
            .iter()
            .position(|entry| &entry.id == id)
            .and_then(|index| self.entries.get(index + 1))
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::queue::LocalQueue;

    #[test]
    fn an_edit_is_rechecked_when_playback_reserves_a_formerly_editable_entry() {
        let mut queue = LocalQueue::default();
        let first = queue.append("first.wav").unwrap();
        let second = queue.append("second.wav").unwrap();
        let third = queue.append("third.wav").unwrap();
        let mut view = PlanView {
            entries: queue.snapshot().entries,
            active: Some(first),
            reserved: Some(second.clone()),
            epochs: None,
        };
        let edit = Command::QueueRemove { id: third.clone() };
        assert!(view.validate_edit(&edit).is_ok());
        view.active = Some(second);
        view.reserved = Some(third);
        assert_eq!(
            view.validate_edit(&edit).unwrap_err().code,
            ErrorCode::SourceBusy
        );
    }

    #[test]
    fn publishing_a_committed_queue_preserves_the_observed_reservations() {
        let mut queue = LocalQueue::default();
        let first = queue.append("first.wav").unwrap();
        let second = queue.append("second.wav").unwrap();
        let mut view = PlanView {
            entries: queue.snapshot().entries,
            active: Some(first.clone()),
            reserved: Some(second.clone()),
            epochs: None,
        };
        queue.append("third.wav").unwrap();
        view.publish(queue.snapshot());
        assert_eq!(view.active, Some(first));
        assert_eq!(view.reserved, Some(second.clone()));
        assert_eq!(view.following(&second).unwrap().path, "third.wav");
    }
}
