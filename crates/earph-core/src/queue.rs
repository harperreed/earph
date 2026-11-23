// ABOUTME: Owns the bounded ordered queue for local file playback.
// ABOUTME: Keeps preparation results and selection attached to stable entry identities.
use crate::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    ids::{QueueEntryId, increment},
};
use serde::{Deserialize, Serialize};

const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid queue entry");
const FULL: EngineError =
    EngineError::new(ErrorCode::ResourceBudgetExceeded, "local queue is full");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreparationStatus {
    Unprepared,
    Ready {
        format: PcmFormat,
        total_frames: u64,
    },
    DsdReady {
        format: crate::dop::DsdSourceFormat,
        total_source_bits: u64,
    },
    Failed {
        code: ErrorCode,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueEntry {
    pub id: QueueEntryId,
    pub path: String,
    pub status: PreparationStatus,
}

/// A portable view. Restore it through `LocalQueue::restore` before use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSnapshot {
    pub entries: Vec<QueueEntry>,
    pub selected: Option<QueueEntryId>,
    counter: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueuePage {
    pub entries: Vec<QueueEntry>,
    pub selected: Option<QueueEntryId>,
    pub offset: usize,
    pub total: usize,
}

#[derive(Debug, Default, Clone)]
pub struct LocalQueue {
    entries: Vec<QueueEntry>,
    selected: Option<QueueEntryId>,
    counter: u64,
}

impl LocalQueue {
    pub(crate) fn page(&self, offset: usize, limit: usize) -> Result<QueuePage, EngineError> {
        if limit == 0 || limit > 64 || offset > self.entries.len() {
            return Err(INVALID);
        }
        let end = offset.saturating_add(limit).min(self.entries.len());
        Ok(QueuePage {
            entries: self.entries[offset..end].to_vec(),
            selected: self.selected.clone(),
            offset,
            total: self.entries.len(),
        })
    }
    pub fn snapshot(&self) -> QueueSnapshot {
        QueueSnapshot {
            entries: self.entries.clone(),
            selected: self.selected.clone(),
            counter: self.counter,
        }
    }

    pub fn restore(snapshot: QueueSnapshot) -> Result<Self, EngineError> {
        if snapshot.entries.len() > 256 {
            return Err(FULL);
        }
        for (index, entry) in snapshot.entries.iter().enumerate() {
            validate_path(&entry.path)?;
            let number = entry
                .id
                .as_str()
                .strip_prefix("local-")
                .and_then(|id| id.parse::<u64>().ok())
                .ok_or(INVALID)?;
            if number == 0
                || number > snapshot.counter
                || entry.id.as_str() != format!("local-{number}")
                || snapshot.entries[..index]
                    .iter()
                    .any(|prior| prior.id == entry.id)
            {
                return Err(INVALID);
            }
        }
        if snapshot
            .selected
            .as_ref()
            .is_some_and(|id| !snapshot.entries.iter().any(|entry| &entry.id == id))
        {
            return Err(INVALID);
        }
        let entries = snapshot
            .entries
            .into_iter()
            .map(|mut entry| {
                entry.status = PreparationStatus::Unprepared;
                entry
            })
            .collect();
        Ok(Self {
            entries,
            selected: snapshot.selected,
            counter: snapshot.counter,
        })
    }

    pub fn append(&mut self, path: impl Into<String>) -> Result<QueueEntryId, EngineError> {
        let path = path.into();
        validate_path(&path)?;
        if self.entries.len() >= 256 {
            return Err(FULL);
        }
        let counter = increment(self.counter)?;
        let id = QueueEntryId::new(format!("local-{counter}"))?;
        self.entries.push(QueueEntry {
            id: id.clone(),
            path,
            status: PreparationStatus::Unprepared,
        });
        self.counter = counter;
        Ok(id)
    }

    pub fn entries(&self) -> &[QueueEntry] {
        &self.entries
    }

    pub fn selected(&self) -> Option<&QueueEntryId> {
        self.selected.as_ref()
    }

    pub fn remove(&mut self, id: &QueueEntryId) -> Result<(), EngineError> {
        let index = self.index(id)?;
        self.entries.remove(index);
        if self.selected.as_ref() == Some(id) {
            self.selected = None;
        }
        Ok(())
    }

    /// Moves an entry to its final zero-based index in the queue.
    pub fn move_entry(&mut self, id: &QueueEntryId, new_index: usize) -> Result<(), EngineError> {
        let index = self.index(id)?;
        if new_index >= self.entries.len() {
            return Err(INVALID);
        }
        let entry = self.entries.remove(index);
        self.entries.insert(new_index, entry);
        Ok(())
    }

    pub fn select(&mut self, id: Option<QueueEntryId>) -> Result<(), EngineError> {
        if let Some(id) = &id {
            self.index(id)?;
        }
        self.selected = id;
        Ok(())
    }

    pub fn update_status(
        &mut self,
        id: &QueueEntryId,
        status: PreparationStatus,
    ) -> Result<(), EngineError> {
        let index = self.index(id)?;
        self.entries[index].status = status;
        Ok(())
    }

    fn index(&self, id: &QueueEntryId) -> Result<usize, EngineError> {
        self.entries
            .iter()
            .position(|entry| &entry.id == id)
            .ok_or(INVALID)
    }
}

pub(crate) fn validate_path(path: &str) -> Result<(), EngineError> {
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        Err(INVALID)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_entries_require_fresh_preparation() {
        let mut queue = LocalQueue::default();
        let id = queue.append("file.wav").unwrap();
        queue
            .update_status(
                &id,
                PreparationStatus::Failed {
                    code: ErrorCode::AssetInvalid,
                },
            )
            .unwrap();
        let restored = LocalQueue::restore(queue.snapshot()).unwrap();
        assert_eq!(restored.entries()[0].id, id);
        assert_eq!(restored.entries()[0].status, PreparationStatus::Unprepared);
    }

    #[test]
    fn exhausted_identity_counter_rejects_append_without_mutation() {
        let mut queue = LocalQueue::default();
        let selected = queue.append("kept.wav").unwrap();
        queue.select(Some(selected.clone())).unwrap();
        queue.counter = u64::MAX;
        let before = queue.entries().to_vec();
        assert_eq!(
            queue.append("rejected.wav").unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(queue.entries(), before);
        assert_eq!(queue.selected(), Some(&selected));
        assert_eq!(queue.counter, u64::MAX);
    }

    #[test]
    fn rejected_path_does_not_consume_an_identity() {
        let mut queue = LocalQueue::default();
        assert!(queue.append("").is_err());
        assert_eq!(queue.counter, 0);
        queue.append("accepted.wav").unwrap();
        assert_eq!(queue.counter, 1);
    }
}
