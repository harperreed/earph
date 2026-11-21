// ABOUTME: Owns explicitly admitted outbound Sendspin connection profiles.
// ABOUTME: Keeps credentials, source authority, and output outside application startup.
mod clock;
pub use clock::{ClockConfiguration, ClockObservation, MonotonicDomain};
mod player;
mod policy;
pub use player::PlayerObservation;
mod worker;
pub use policy::SendspinConnectPermit;
pub use worker::{
    EventLease, Observation, ProducerRejected, RetirementState, SendspinHandle, SendspinObserver,
    SendspinWorker, ServiceKind,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverError {
    Admission,
    TlsUnsupported,
    Expired,
    Busy,
    Cancelled,
    PeerClosed,
    Connect,
    Upgrade,
    Protocol,
    Authentication,
    Budget,
    Deadline,
    Service,
    StorageUncertain,
    CleanupTimeout,
    Internal,
    Clock,
}
impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sendspin transport {self:?}")
    }
}
impl std::error::Error for DriverError {}

use earph_sendspin::{
    pairing::{AttemptId, Candidate, CommitOutcome, RecordUpdate},
    session::{AdmissionProposal, SessionLimits},
};
use std::{future::Future, pin::Pin};
/// Operations must own their inputs, remain nonblocking while polled, and release cancellable work on drop.
pub type Operation<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;
/// The caller owns native policy and persistence. No production store is selected here.
pub trait SendspinServices: Send + 'static {
    /// Current native playback policy only; never source ownership or output readiness.
    fn playback_authorized(&self) -> bool {
        false
    }
    fn admission(&mut self, proposal: AdmissionProposal) -> Operation<Result<bool, DriverError>>;
    fn candidate(&mut self, id: AttemptId) -> Operation<Result<Candidate, DriverError>>;
    /// Once called, this operation stays owned until its exact result returns, even after socket close.
    fn commit(
        &mut self,
        id: AttemptId,
        update: RecordUpdate,
    ) -> Operation<Result<(), CommitOutcome>>;
}
/// Local resource ceilings. Callers may only tighten these limits.
#[derive(Debug, Clone, Copy)]
pub struct DriverLimits {
    pub connect_ms: u64,
    pub upgrade_ms: u64,
    pub write_ms: u64,
    pub service_ms: u64,
    pub ping_ms: u64,
    pub pong_ms: u64,
    pub close_ms: u64,
    pub retire_ms: u64,
    pub commands: usize,
    pub events: usize,
    pub producer_bytes: usize,
    pub event_bytes: usize,
    pub total_payload_bytes: usize,
}
impl Default for DriverLimits {
    fn default() -> Self {
        Self {
            connect_ms: 3000,
            upgrade_ms: 3000,
            write_ms: 5000,
            service_ms: 5000,
            ping_ms: 30000,
            pong_ms: 10000,
            close_ms: 1000,
            retire_ms: 1000,
            commands: 16,
            events: 32,
            producer_bytes: 1048576,
            event_bytes: 1048576,
            total_payload_bytes: 8388608,
        }
    }
}
/// Required roleless session ceilings, independent of transport-owned timeouts.
pub fn roleless_session_limits() -> SessionLimits {
    SessionLimits {
        pending_input_bytes: 1048576,
        pending_output_bytes: 1048576,
        pending_messages: 64,
        messages: earph_sendspin::message::MessageLimits {
            body_bytes: 1048576,
            ..Default::default()
        },
        ..Default::default()
    }
}
impl DriverLimits {
    pub fn validate(&self, s: SessionLimits) -> Result<(), DriverError> {
        self.validate_with_metadata(s, 0)
    }
    /// Player cache storage shares the existing total allowance; core-only validation stays unchanged.
    fn validate_with_metadata(
        &self,
        s: SessionLimits,
        metadata_bytes: usize,
    ) -> Result<(), DriverError> {
        let d = Self::default();
        for (n, max) in [
            (self.connect_ms, d.connect_ms),
            (self.upgrade_ms, d.upgrade_ms),
            (self.write_ms, d.write_ms),
            (self.service_ms, d.service_ms),
            (self.ping_ms, d.ping_ms),
            (self.pong_ms, d.pong_ms),
            (self.close_ms, d.close_ms),
            (self.retire_ms, d.retire_ms),
            (s.exchange_ms, 30000),
            (s.messages.assembly_ms, 30000),
        ] {
            if n == 0 || n > max {
                return Err(DriverError::Budget);
            }
        }
        for (n, max) in [
            (self.commands, 16),
            (self.events, 32),
            (self.producer_bytes, 1048576),
            (self.event_bytes, 1048576),
            (self.total_payload_bytes, 8388608),
            (s.pending_input_bytes, 1048576),
            (s.pending_output_bytes, 1048576),
            (s.cleartext_message_bytes, 100000),
            (s.pending_messages, 64),
            (s.messages.body_bytes, 1048576),
            (s.messages.fragment_count, 4096),
            (s.messages.record_plaintext_bytes, 65519),
            (s.noise.record_bytes, 65535),
            (s.noise.plaintext_bytes, 65519),
            (s.noise.output_bytes, 65535),
            (s.noise.init_bytes, 4096),
            (s.noise.handshake_payload_bytes, 4096),
        ] {
            if n == 0 || n > max {
                return Err(DriverError::Budget);
            }
        }
        // Includes session and producer bodies, one aggregate parse-copy allowance for each,
        // reassembly, WebSocket buffers, and the catalog. Concurrent producer copies share their limit.
        let required = [
            s.pending_input_bytes,
            s.pending_input_bytes,
            // A transferred producer can retain its admitted Vec capacity inside the session.
            s.pending_output_bytes.max(self.producer_bytes),
            s.messages.body_bytes,
            self.producer_bytes,
            self.producer_bytes,
            self.event_bytes,
            2 * 100000,
            131072,
            4096,
            65536,
            8192,
        ]
        .into_iter()
        .try_fold(metadata_bytes, |sum, n| sum.checked_add(n))
        .ok_or(DriverError::Budget)?;
        if required > self.total_payload_bytes {
            return Err(DriverError::Budget);
        }
        Ok(())
    }
}
#[cfg(test)]
mod limits_tests {
    use super::*;
    #[test]
    fn ceilings_bound_all_session_and_transport_allowances() {
        let d = DriverLimits::default();
        assert!(d.validate(roleless_session_limits()).is_ok());
        assert!(d.validate(SessionLimits::default()).is_err());
        for bad in [
            DriverLimits { commands: 0, ..d },
            DriverLimits { events: 33, ..d },
            DriverLimits {
                service_ms: 5001,
                ..d
            },
            DriverLimits { write_ms: 0, ..d },
            DriverLimits {
                event_bytes: usize::MAX,
                ..d
            },
            DriverLimits {
                total_payload_bytes: 1,
                ..d
            },
        ] {
            assert!(bad.validate(roleless_session_limits()).is_err());
        }
        let mut session = roleless_session_limits();
        session.noise.record_bytes = 65536;
        assert!(d.validate(session).is_err());
    }
}

#[cfg(test)]
#[path = "../../../earph-sendspin/tests/support/session_fixture.rs"]
mod session_fixture;
