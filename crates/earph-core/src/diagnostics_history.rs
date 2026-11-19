// ABOUTME: Defines bounded owner-scoped event pages and safe current recovery facts.
// ABOUTME: Preserves sparse history without publishing raw event IDs or private State snapshots.
use super::*;
use crate::session::TransportState;

pub const MAX_EVENTS_PAGE: usize = 128;
pub const MAX_DIAGNOSTIC_HISTORY: usize = 1024;
pub const MAX_EVENTS_READ_BYTES: usize = 256 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventCursor {
    pub owner_id: OwnerId,
    pub after_sequence: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventHistoryScope {
    RetainedStateEvents,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticEvent {
    pub sequence: u64,
    pub revision: u64,
    pub epochs: Epochs,
    pub monotonic_ns: u64,
    pub transport: TransportState,
    pub muted: bool,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<ErrorCode>,
}
/// Recovery covers these public state facts; it is not a serialized private State.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticSnapshot {
    pub cut: DiagnosticCut,
    pub transport: TransportState,
    pub muted: bool,
    pub software_gain_db: f64,
    #[serde(deserialize_with = "nullable")]
    pub error: Option<ErrorCode>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsReport {
    pub schema_version: u32,
    pub cut: DiagnosticCut,
    pub scope: EventHistoryScope,
    pub complete_audit_log: bool,
    pub gap: bool,
    pub events: Vec<DiagnosticEvent>,
    pub snapshot: DiagnosticSnapshot,
    pub next_sequence: u64,
    pub has_more: bool,
}
pub type EventsRead = DiagnosticRead<EventsReport>;
impl EventsRead {
    /// Checks request correlation without confusing a recovery snapshot with historical replay.
    pub fn validate_cursor(&self, cursor: Option<&EventCursor>) -> Result<(), EngineError> {
        self.validate()?;
        let report = &self.report;
        match cursor {
            None if !report.events.is_empty() || report.gap || report.has_more => Err(INVALID),
            Some(cursor) if self.binding.owner_id.as_ref() != Some(&cursor.owner_id) => {
                Err(INVALID)
            }
            Some(cursor) if cursor.after_sequence > report.cut.event_sequence => {
                if !report.gap || !report.events.is_empty() || report.has_more {
                    Err(INVALID)
                } else {
                    Ok(())
                }
            }
            Some(cursor)
                if report
                    .events
                    .iter()
                    .any(|event| event.sequence <= cursor.after_sequence) =>
            {
                Err(INVALID)
            }
            _ => Ok(()),
        }
    }
}
impl EventsReport {
    pub(crate) fn validate_history(&self, limit: usize) -> Result<(), EngineError> {
        if self.schema_version != 1
            || self.complete_audit_log
            || self.snapshot.cut != self.cut
            || self.events.capacity() > limit
            || self.events.len() > limit
            || !self.snapshot.software_gain_db.is_finite()
            || !(-120.0..=0.0).contains(&self.snapshot.software_gain_db)
            || self
                .events
                .windows(2)
                .any(|p| p[0].sequence >= p[1].sequence)
            || self.events.iter().any(|e| {
                e.sequence == 0
                    || e.sequence > self.cut.event_sequence
                    || e.revision > self.cut.revision
                    || e.monotonic_ns > self.cut.monotonic_ns
                    || e.epochs.session > self.cut.epochs.session
                    || e.epochs.session == self.cut.epochs.session
                        && e.epochs.stream > self.cut.epochs.stream
            })
            || self.next_sequence
                != self
                    .events
                    .last()
                    .map_or(self.cut.event_sequence, |e| e.sequence)
            || self.has_more
                && (self.events.len() != limit || self.next_sequence >= self.cut.event_sequence)
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
impl DiagnosticReport for EventsReport {
    const MAX_READ_BYTES: usize = MAX_EVENTS_READ_BYTES;
    fn cut(&self) -> DiagnosticCut {
        self.cut
    }
    fn validate(&self) -> Result<(), EngineError> {
        self.validate_history(MAX_EVENTS_PAGE)
    }
}
