// ABOUTME: Stages URI metadata through the same owner fences and transaction as other controls.
// ABOUTME: Leaves active source, queue, graph jobs, volume, and output authority unchanged.
use super::*;

impl Session {
    pub(super) fn stage_uri(
        &mut self,
        command: &CommandEnvelope,
        uri: &str,
        metadata: &str,
    ) -> Result<ReceiptStatus, EngineError> {
        if self.state.owner_id.is_none() {
            return Err(STALE);
        }
        if self.state.shutdown_requested {
            return Err(STATE);
        }
        let revision = increment(self.state.revision)?;
        let event_sequence = self.next_session_event_sequence(false)?;
        self.sources.stage(uri, metadata)?;
        self.state.revision = revision;
        self.state.event_sequence = event_sequence;
        self.log(Some(command.request_id.clone()));
        Ok(ReceiptStatus::Applied)
    }
}
