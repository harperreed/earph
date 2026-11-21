// ABOUTME: Owns live roleless Sendspin clock probes in the existing Network pump.
// ABOUTME: Binds captured timestamps to authenticated sessions without granting output authority.
#[cfg(test)]
#[path = "../../tests/support/sendspin_clock.rs"]
mod tests;

use super::DriverError;
use earph_sendspin::{
    message::{Complete, Envelope, ReceiveStamp},
    session::ClockContext,
    timing::{
        Action, Binding, MAX_TIME_US, ProbeToken, ServerTime, Snapshot, TimePolicy, TimeSync,
        TimingError,
    },
};
use std::{fmt, sync::Arc, time::Instant};

struct Origin {
    instant: Instant,
    generation: u64,
}
/// Clones sample the same private monotonic origin and explicit domain generation.
#[derive(Clone)]
pub struct MonotonicDomain(Arc<Origin>);
impl MonotonicDomain {
    pub fn new(generation: u64) -> Result<Self, DriverError> {
        if generation == 0 {
            return Err(DriverError::Clock);
        }
        Ok(Self(Arc::new(Origin {
            instant: Instant::now(),
            generation,
        })))
    }
    pub fn generation(&self) -> u64 {
        self.0.generation
    }
    pub fn sample(&self) -> Result<ReceiveStamp, DriverError> {
        let micros =
            i64::try_from(self.0.instant.elapsed().as_micros()).map_err(|_| DriverError::Clock)?;
        ReceiveStamp::new(micros).map_err(|_| DriverError::Clock)
    }
    pub(super) fn milliseconds(&self) -> u64 {
        u64::try_from(self.0.instant.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}
impl fmt::Debug for MonotonicDomain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MonotonicDomain")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}
/// The caller supplies clock ownership separately from transport admission.
#[derive(Clone, Debug)]
pub struct ClockConfiguration {
    pub domain: MonotonicDomain,
    pub policy: TimePolicy,
}
/// Copied mapping estimates grant no clock-ready or output-ready authority.
#[derive(Clone, Copy)]
pub struct ClockObservation {
    pub domain_generation: u64,
    pub snapshot: Snapshot,
}
impl fmt::Debug for ClockObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockObservation")
            .field("domain_generation", &self.domain_generation)
            .field("status", &self.snapshot.status)
            .field("count", &self.snapshot.count)
            .field("mapping_revision", &self.snapshot.mapping_revision)
            .field("age_us", &self.snapshot.age_us)
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy)]
enum Probe {
    Offered(ProbeToken),
    Started { token: ProbeToken, t1: i64 },
}
pub(super) struct ClockDriver {
    pub domain: MonotonicDomain,
    policy: TimePolicy,
    sync: Option<(Binding, TimeSync)>,
    probe: Option<Probe>,
    suppressed: bool,
    last_t1: Option<i64>,
}
impl ClockDriver {
    pub fn new(config: ClockConfiguration) -> Self {
        Self {
            domain: config.domain,
            policy: config.policy,
            sync: None,
            probe: None,
            suppressed: true,
            last_t1: None,
        }
    }
    fn binding(&self, context: ClockContext) -> Binding {
        Binding::new(
            context.server_public(),
            context.connection_generation(),
            self.domain.generation(),
        )
    }
    /// Process delivered replies before advancing timeout and age policy.
    pub fn advance(
        &mut self,
        context: Option<ClockContext>,
        now: ReceiveStamp,
    ) -> Result<Option<ClockObservation>, DriverError> {
        let Some(context) = context else {
            return Ok(None);
        };
        let binding = self.binding(context);
        if self
            .sync
            .as_ref()
            .is_none_or(|(current, _)| *current != binding)
        {
            self.sync = Some((binding, TimeSync::new(binding, self.policy)));
            self.probe = None;
            self.last_t1 = None;
            self.suppressed = false;
        }
        let suppressed = context.send_suppressed() || context.delivery_blocked();
        let sync = &mut self.sync.as_mut().expect("authenticated clock exists").1;
        if suppressed != self.suppressed {
            // Abandon held work before any later timeout poll or snapshot observes this interval.
            sync.set_suppressed(suppressed, now.microseconds())
                .map_err(clock_error)?;
            self.probe = None;
            self.suppressed = suppressed;
        }
        let action = sync.poll(now.microseconds()).map_err(clock_error)?.action;
        self.probe = match action {
            Action::PrepareProbe(token) => Some(Probe::Offered(token)),
            Action::AwaitReply => self.probe,
            _ => None,
        };
        self.observation(now)
    }
    pub fn has_offer(&self) -> bool {
        matches!(self.probe, Some(Probe::Offered(_))) && !self.suppressed
    }
    /// Recheck an expiring offer after readiness, without counting an unsent probe.
    pub fn offered_at(&mut self, now: ReceiveStamp) -> Result<Option<ProbeToken>, DriverError> {
        if self.suppressed {
            return Ok(None);
        }
        let Some((_, sync)) = &mut self.sync else {
            return Ok(None);
        };
        let action = sync.poll(now.microseconds()).map_err(clock_error)?.action;
        match action {
            Action::PrepareProbe(token) => {
                self.probe = Some(Probe::Offered(token));
                Ok(self
                    .last_t1
                    .filter(|last| now.microseconds() <= *last)
                    .map_or(Some(token), |_| None))
            }
            Action::AwaitReply => Ok(None),
            _ => {
                self.probe = None;
                Ok(None)
            }
        }
    }
    /// Call only after the original socket accepts start_send for this ciphertext.
    pub fn started(&mut self, token: ProbeToken, t1: ReceiveStamp) -> Result<(), DriverError> {
        if !matches!(self.probe,Some(Probe::Offered(current)) if current==token) {
            return Err(DriverError::Clock);
        }
        self.sync
            .as_mut()
            .ok_or(DriverError::Clock)?
            .1
            .probe_started(token, t1.microseconds())
            .map_err(clock_error)?;
        self.probe = Some(Probe::Started {
            token,
            t1: t1.microseconds(),
        });
        self.last_t1 = Some(t1.microseconds());
        Ok(())
    }
    /// Consume only authenticated server/time events produced by the original session.
    pub fn consume(
        &mut self,
        context: Option<ClockContext>,
        message: &Complete,
    ) -> Result<bool, DriverError> {
        if message.id != 0 {
            return Ok(false);
        }
        let envelope = Envelope::parse(&message.body).map_err(|_| DriverError::Protocol)?;
        if envelope.kind != "server/time" {
            return Ok(false);
        }
        let Some(context) = context else {
            return Ok(true);
        };
        if self.suppressed
            || self
                .sync
                .as_ref()
                .is_none_or(|(binding, _)| *binding != self.binding(context))
        {
            return Ok(true);
        }
        let Some(Probe::Started { token, t1 }) = self.probe else {
            return Ok(true);
        };
        let Ok(reply) = serde_json::from_value::<ServerTime>(envelope.payload) else {
            return Ok(true);
        };
        if reply.client_transmitted != t1
            || !(0..=MAX_TIME_US).contains(&reply.client_transmitted)
            || !(-MAX_TIME_US..=MAX_TIME_US).contains(&reply.server_received)
            || !(-MAX_TIME_US..=MAX_TIME_US).contains(&reply.server_transmitted)
        {
            return Ok(true);
        }
        match self.sync.as_mut().expect("bound clock exists").1.reply(
            token,
            reply,
            message.arrival.microseconds(),
        ) {
            Ok(()) | Err(TimingError::ReplyExpired) => self.probe = None,
            Err(
                TimingError::NumericRange
                | TimingError::LocalOrder
                | TimingError::ServerOrder
                | TimingError::UnknownProbe,
            ) => {}
            Err(error) => return Err(clock_error(error)),
        }
        Ok(true)
    }
    fn observation(&mut self, now: ReceiveStamp) -> Result<Option<ClockObservation>, DriverError> {
        self.sync
            .as_mut()
            .map(|(_, sync)| {
                sync.snapshot(now.microseconds())
                    .map(|snapshot| ClockObservation {
                        domain_generation: self.domain.generation(),
                        snapshot,
                    })
                    .map_err(clock_error)
            })
            .transpose()
    }
    pub fn retire(&mut self, now: ReceiveStamp) -> Result<Option<ClockObservation>, DriverError> {
        if let Some((_, sync)) = &mut self.sync {
            sync.retire();
        }
        self.probe = None;
        self.suppressed = true;
        self.observation(now)
    }
}
fn clock_error(_: TimingError) -> DriverError {
    DriverError::Clock
}
