// ABOUTME: Maps caller-supplied Sendspin timestamps through a bounded burst filter.
// ABOUTME: Owns no clock, transport, output, credentials, or playback admission.
use crate::PlayerAudioFrame;
use std::fmt;

pub const MAX_TIME_US: i64 = (1_i64 << 53) - 1;
const CADENCE_US: i64 = 10_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    server_public: [u8; 32],
    connection_generation: u64,
    clock_domain_generation: u64,
}
impl Binding {
    pub fn new(
        server_public: [u8; 32],
        connection_generation: u64,
        clock_domain_generation: u64,
    ) -> Self {
        Self {
            server_public,
            connection_generation,
            clock_domain_generation,
        }
    }
}
impl fmt::Debug for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Binding").finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimingError {
    InvalidPolicy,
    NumericRange,
    LocalOrder,
    ServerOrder,
    ClockRegression,
    ClockNotAdvanced,
    BindingMismatch,
    UnknownProbe,
    OfferExpired,
    ReplyExpired,
    DeadlineOverflow,
    CounterOverflow,
    NumericFault,
    Uninitialized,
    Stale,
    Retired,
    DelayRange,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimePolicy {
    reply_timeout_us: i64,
    burst_period_us: i64,
    max_measurement_age_us: i64,
}
impl TimePolicy {
    pub fn new(
        reply_timeout_us: i64,
        burst_period_us: i64,
        max_measurement_age_us: i64,
    ) -> Result<Self, TimingError> {
        if reply_timeout_us <= 0
            || burst_period_us <= 0
            || max_measurement_age_us <= 0
            || reply_timeout_us > MAX_TIME_US
            || max_measurement_age_us > MAX_TIME_US
            || burst_period_us > CADENCE_US
            || i128::from(reply_timeout_us) * 8 >= i128::from(burst_period_us)
        {
            return Err(TimingError::InvalidPolicy);
        }
        Ok(Self {
            reply_timeout_us,
            burst_period_us,
            max_measurement_age_us,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeToken {
    binding: Binding,
    serial: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ClientTime {
    pub client_transmitted: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct ServerTime {
    pub client_transmitted: i64,
    pub server_received: i64,
    pub server_transmitted: i64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PrepareProbe(ProbeToken),
    AwaitReply,
    WaitForBurst,
    NoAction,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub emitted_requests: u64,
    pub completed_bursts: u64,
    pub incomplete_bursts: u64,
    pub accepted_measurements: u64,
    pub timeouts: u64,
    pub expired_offers: u64,
    pub cancelled_probes: u64,
    pub cadence_misses: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poll {
    pub action: Action,
    pub next_deadline_us: Option<i64>,
    pub diagnostics: Diagnostics,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Uninitialized,
    Mapped,
    Stale,
    NumericFault,
    Retired,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Covariance {
    pub offset_variance: f64,
    pub offset_drift: f64,
    pub drift_variance: f64,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub offset_us: f64,
    pub drift: f64,
    pub use_drift: bool,
    pub covariance: Covariance,
    pub uncertainty_us: f64,
    pub last_update_us: i64,
    pub count: u32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapshot {
    pub binding: Binding,
    pub status: Status,
    pub count: u32,
    pub estimate: Option<Estimate>,
    pub age_us: Option<i64>,
    pub mapping_revision: u64,
    pub diagnostics: Diagnostics,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MappedTime {
    pub time_us: i64,
    pub binding: Binding,
    pub mapping_revision: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Measurement {
    offset: i64,
    error: i64,
    at: i64,
}
impl Measurement {
    fn exchange(reply: ServerTime, t4: i64) -> Result<Self, TimingError> {
        local_us(reply.client_transmitted)?;
        local_us(t4)?;
        signed_us(i128::from(reply.server_received))?;
        signed_us(i128::from(reply.server_transmitted))?;
        let (t1, t2, t3, t4) = (
            i128::from(reply.client_transmitted),
            i128::from(reply.server_received),
            i128::from(reply.server_transmitted),
            i128::from(t4),
        );
        if t4 < t1 {
            return Err(TimingError::LocalOrder);
        }
        if t3 < t2 {
            return Err(TimingError::ServerOrder);
        }
        Ok(Self {
            offset: signed_us(((t2 - t1) + (t3 - t4)) / 2)?,
            error: signed_us(((t4 - t1) - (t3 - t2)) / 2)?,
            at: local_us(i64::try_from(t4).map_err(|_| TimingError::NumericRange)?)?,
        })
    }
}
#[derive(Clone, Copy, Debug)]
struct Filter {
    offset: f64,
    drift: f64,
    poo: f64,
    pod: f64,
    pdd: f64,
    last: i64,
    count: u32,
    use_drift: bool,
}
impl Filter {
    fn new() -> Self {
        Self {
            offset: 0.,
            drift: 0.,
            poo: f64::INFINITY,
            pod: 0.,
            pdd: 0.,
            last: 0,
            count: 0,
            use_drift: false,
        }
    }
    fn update(&mut self, sample: Measurement) -> Result<bool, TimingError> {
        // Steps 1–4 intentionally return before drift significance is evaluated.
        if sample.at <= self.last || sample.error <= 0 {
            return Ok(false);
        }
        local_us(sample.at)?;
        signed_us(i128::from(sample.offset))?;
        signed_us(i128::from(sample.error))?;
        if self.count > 0 {
            self.validate()?;
        }
        let mut next = *self;
        let dt = (sample.at - self.last) as f64;
        next.last = sample.at;
        let r = (0.5 * sample.error as f64).powi(2);
        if self.count == 0 {
            next.offset = sample.offset as f64;
            next.poo = r;
            next.drift = 0.;
            next.count = 1;
        } else if self.count == 1 {
            next.drift = (sample.offset as f64 - self.offset) / dt;
            next.pdd = (self.poo + r) / dt.powi(2);
            next.offset = sample.offset as f64;
            next.poo = r;
            next.count = 2;
        } else {
            // Step 5: predict, including drift diffusion only in P_dd_p.
            let offset_p = self.offset + self.drift * dt;
            let mut poo_p = self.poo + 2. * self.pod * dt + self.pdd * dt.powi(2);
            let mut pod_p = self.pod + self.pdd * dt;
            let mut pdd_p = self.pdd + 1e-11_f64.powi(2) * dt;
            // Step 6: the count saturates at 100 before adaptive forgetting applies.
            let residual = sample.offset as f64 - offset_p;
            if self.count < 100 {
                next.count += 1;
            } else if residual.abs() > 3. * sample.error as f64 {
                poo_p *= 4.;
                pod_p *= 4.;
                pdd_p *= 4.;
            }
            // Step 7: preserve the specified subtraction order without covariance repair.
            let denominator = poo_p + r;
            if !denominator.is_finite() || denominator <= 0. {
                return Err(TimingError::NumericFault);
            }
            let ko = poo_p / denominator;
            let kd = pod_p / denominator;
            next.offset = offset_p + ko * residual;
            next.drift = self.drift + kd * residual;
            next.poo = poo_p - ko * poo_p;
            next.pod = pod_p - kd * poo_p;
            next.pdd = pdd_p - kd * pod_p;
            // Step 8 uses a strict comparison and the corrected drift covariance.
            next.use_drift = next.drift.powi(2) > 4. * next.pdd;
        }
        next.validate()?;
        *self = next;
        Ok(true)
    }
    fn validate(&self) -> Result<(), TimingError> {
        let determinant = self.poo * self.pdd - self.pod.powi(2);
        let denominator = 1. + if self.use_drift { self.drift } else { 0. };
        if [
            self.offset,
            self.drift,
            self.poo,
            self.pod,
            self.pdd,
            determinant,
            denominator,
        ]
        .iter()
        .any(|v| !v.is_finite())
            || self.poo < 0.
            || self.pdd < 0.
            || determinant < 0.
            || denominator <= 0.
        {
            return Err(TimingError::NumericFault);
        }
        Ok(())
    }
    fn estimate(&self) -> Option<Estimate> {
        (self.count > 0).then_some(Estimate {
            offset_us: self.offset,
            drift: self.drift,
            use_drift: self.use_drift,
            covariance: Covariance {
                offset_variance: self.poo,
                offset_drift: self.pod,
                drift_variance: self.pdd,
            },
            uncertainty_us: self.poo.sqrt(),
            last_update_us: self.last,
            count: self.count,
        })
    }
}
#[derive(Clone, Copy, Debug)]
enum Probe {
    Idle,
    Offered {
        token: ProbeToken,
        deadline: i64,
    },
    Pending {
        token: ProbeToken,
        t1: i64,
        deadline: i64,
    },
}
#[derive(Clone, Copy, Debug)]
struct Burst {
    start: Option<i64>,
    hard_deadline: Option<i64>,
    emitted: u8,
    best: Option<Measurement>,
}
/// A sole mutable mapping owner. Create a fresh owner after a binding changes.
pub struct TimeSync {
    binding: Binding,
    policy: TimePolicy,
    filter: Filter,
    serial: u64,
    revision: u64,
    diagnostics: Diagnostics,
    probe: Probe,
    burst: Option<Burst>,
    last_now: Option<i64>,
    last_t1: Option<i64>,
    next_burst_due: Option<i64>,
    cadence_deadline: Option<i64>,
    cadence_reported: bool,
    suppressed: bool,
    terminal: Option<Status>,
}
impl fmt::Debug for TimeSync {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimeSync")
            .field("binding", &self.binding)
            .field("count", &self.filter.count)
            .field("mapping_revision", &self.revision)
            .field("diagnostics", &self.diagnostics)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}
impl TimeSync {
    pub fn new(binding: Binding, policy: TimePolicy) -> Self {
        Self {
            binding,
            policy,
            filter: Filter::new(),
            serial: 0,
            revision: 0,
            diagnostics: Diagnostics::default(),
            probe: Probe::Idle,
            burst: None,
            last_now: None,
            last_t1: None,
            next_burst_due: None,
            cadence_deadline: None,
            cadence_reported: false,
            suppressed: false,
            terminal: None,
        }
    }
    /// Deliver captured replies before polling a later deadline.
    pub fn poll(&mut self, now_us: i64) -> Result<Poll, TimingError> {
        self.observe(now_us)?;
        if self.terminal.is_some() {
            return Ok(self.effect(Action::NoAction, None));
        }
        self.check_cadence(now_us)?;
        if self.suppressed {
            let next = if self.cadence_reported {
                None
            } else {
                self.cadence_deadline
            };
            return Ok(self.effect(Action::NoAction, next));
        }
        match self.probe {
            Probe::Offered { deadline, .. } if now_us >= deadline => {
                self.bump(|d| &mut d.expired_offers)?;
                self.probe = Probe::Idle;
            }
            Probe::Pending { deadline, .. } if now_us >= deadline => {
                self.bump(|d| &mut d.timeouts)?;
                self.probe = Probe::Idle;
                self.finish_if_complete()?;
            }
            _ => {}
        }
        match self.probe {
            Probe::Offered { token, deadline } => {
                return Ok(self.effect(Action::PrepareProbe(token), self.work_deadline(deadline)));
            }
            Probe::Pending { deadline, .. } => {
                return Ok(self.effect(Action::AwaitReply, self.work_deadline(deadline)));
            }
            Probe::Idle => {}
        }
        if self.burst.is_none() {
            if let Some(due) = self.next_burst_due.filter(|due| now_us < *due) {
                return Ok(self.effect(Action::WaitForBurst, Some(due)));
            }
            self.burst = Some(Burst {
                start: None,
                hard_deadline: None,
                emitted: 0,
                best: None,
            });
        }
        let deadline = self.deadline(now_us, self.policy.reply_timeout_us)?;
        let Some(serial) = self.serial.checked_add(1) else {
            return self.fail(TimingError::CounterOverflow);
        };
        self.serial = serial;
        let token = ProbeToken {
            binding: self.binding,
            serial,
        };
        self.probe = Probe::Offered { token, deadline };
        Ok(self.effect(Action::PrepareProbe(token), self.work_deadline(deadline)))
    }
    /// Report the caller's actual send-boundary timestamp, rather than producer creation.
    pub fn probe_started(
        &mut self,
        token: ProbeToken,
        t1_us: i64,
    ) -> Result<ClientTime, TimingError> {
        self.live()?;
        self.check_binding(token)?;
        let Probe::Offered {
            token: current,
            deadline: offer_deadline,
        } = self.probe
        else {
            return Err(TimingError::UnknownProbe);
        };
        if token != current {
            return Err(TimingError::UnknownProbe);
        }
        local_us(t1_us)?;
        self.observe(t1_us)?;
        if self.last_t1 == Some(t1_us) {
            return Err(TimingError::ClockNotAdvanced);
        }
        self.check_cadence(t1_us)?;
        if !matches!(self.probe,Probe::Offered{token:current,..} if current==token) {
            return Err(TimingError::OfferExpired);
        }
        if t1_us >= offer_deadline {
            self.bump(|d| &mut d.expired_offers)?;
            self.probe = Probe::Idle;
            return Err(TimingError::OfferExpired);
        }
        let deadline = self.deadline(t1_us, self.policy.reply_timeout_us)?;
        let mut burst = self.burst.expect("an offered probe owns a burst");
        if burst.start.is_none() {
            let due = self.deadline(t1_us, self.policy.burst_period_us)?;
            let cadence = self.deadline(t1_us, CADENCE_US)?;
            burst.start = Some(t1_us);
            burst.hard_deadline = Some(cadence);
            self.next_burst_due = Some(due);
            self.cadence_deadline = Some(cadence);
            self.cadence_reported = false;
        }
        let Some(emitted) = burst.emitted.checked_add(1).filter(|n| *n <= 8) else {
            return self.fail(TimingError::CounterOverflow);
        };
        self.bump(|d| &mut d.emitted_requests)?;
        burst.emitted = emitted;
        self.burst = Some(burst);
        self.last_t1 = Some(t1_us);
        self.probe = Probe::Pending {
            token,
            t1: t1_us,
            deadline,
        };
        Ok(ClientTime {
            client_transmitted: t1_us,
        })
    }
    /// Unmatched tokens and echoes have no authority over current-domain time.
    pub fn reply(
        &mut self,
        token: ProbeToken,
        reply: ServerTime,
        captured_t4: i64,
    ) -> Result<(), TimingError> {
        self.live()?;
        self.check_binding(token)?;
        let Probe::Pending {
            token: current,
            t1,
            deadline,
        } = self.probe
        else {
            return Err(TimingError::UnknownProbe);
        };
        if token != current || reply.client_transmitted != t1 {
            return Err(TimingError::UnknownProbe);
        }
        local_us(captured_t4)?;
        if self.last_now.is_some_and(|last| captured_t4 < last) {
            return self.fail(TimingError::ClockRegression);
        }
        // A matched expired reply cannot keep the request alive through malformed fields.
        if captured_t4 >= deadline {
            self.observe(captured_t4)?;
            self.check_cadence(captured_t4)?;
            if !matches!(self.probe,Probe::Pending{token:current,..} if current==token) {
                return Err(TimingError::ReplyExpired);
            }
            self.bump(|d| &mut d.timeouts)?;
            self.probe = Probe::Idle;
            self.finish_if_complete()?;
            return Err(TimingError::ReplyExpired);
        }
        let sample = Measurement::exchange(reply, captured_t4)?;
        self.observe(captured_t4)?;
        self.check_cadence(captured_t4)?;
        if !matches!(self.probe,Probe::Pending{token:current,..} if current==token) {
            return Err(TimingError::ReplyExpired);
        }
        let burst = self.burst.as_mut().expect("a pending request owns a burst");
        if sample.error > 0 && burst.best.is_none_or(|best| sample.error < best.error) {
            burst.best = Some(sample);
        }
        self.probe = Probe::Idle;
        self.finish_if_complete()
    }
    /// An uncertain pending send abandons its burst. An offered cancellation consumes no emission.
    pub fn cancel_probe(&mut self, token: ProbeToken, now_us: i64) -> Result<(), TimingError> {
        self.live()?;
        self.check_binding(token)?;
        let pending = match self.probe {
            Probe::Offered { token: current, .. } if current == token => false,
            Probe::Pending { token: current, .. } if current == token => true,
            _ => return Err(TimingError::UnknownProbe),
        };
        self.observe(now_us)?;
        self.bump(|d| &mut d.cancelled_probes)?;
        if pending {
            self.abandon_burst()?;
        }
        self.probe = Probe::Idle;
        Ok(())
    }
    /// Re-handshake suppresses application sends while preserving this binding's filter.
    pub fn set_suppressed(&mut self, suppressed: bool, now_us: i64) -> Result<(), TimingError> {
        self.live()?;
        self.observe(now_us)?;
        self.check_cadence(now_us)?;
        if suppressed && !self.suppressed {
            self.abandon_burst()?;
            self.next_burst_due = None;
        }
        self.suppressed = suppressed;
        Ok(())
    }
    pub fn snapshot(&mut self, now_us: i64) -> Result<Snapshot, TimingError> {
        self.observe(now_us)?;
        let estimate = self.filter.estimate();
        let age = estimate.map(|e| now_us - e.last_update_us);
        let status = if let Some(status) = self.terminal {
            status
        } else if estimate.is_none() {
            Status::Uninitialized
        } else if age.is_some_and(|age| age > self.policy.max_measurement_age_us) {
            Status::Stale
        } else {
            Status::Mapped
        };
        Ok(Snapshot {
            binding: self.binding,
            status,
            count: self.filter.count,
            estimate,
            age_us: age,
            mapping_revision: self.revision,
            diagnostics: self.diagnostics,
        })
    }
    pub fn map_server_time(
        &mut self,
        server_us: i64,
        now_us: i64,
    ) -> Result<MappedTime, TimingError> {
        signed_us(i128::from(server_us))?;
        let estimate = self.mapping(now_us)?;
        let d = if estimate.use_drift {
            estimate.drift
        } else {
            0.
        };
        let time = round_us(
            (server_us as f64 - estimate.offset_us + d * estimate.last_update_us as f64) / (1. + d),
        )?;
        Ok(self.mapped(local_us(time)?))
    }
    pub fn map_client_time(
        &mut self,
        client_us: i64,
        now_us: i64,
    ) -> Result<MappedTime, TimingError> {
        local_us(client_us)?;
        let estimate = self.mapping(now_us)?;
        let d = if estimate.use_drift {
            estimate.drift
        } else {
            0.
        };
        let correction =
            round_us(estimate.offset_us + d * (client_us - estimate.last_update_us) as f64)?;
        Ok(self.mapped(signed_us(i128::from(client_us) + i128::from(correction))?))
    }
    /// Subtract output delay once, in local time, after server-to-client conversion.
    pub fn port_target(
        &mut self,
        frame: PlayerAudioFrame<'_>,
        output_delay_ms: u32,
        now_us: i64,
    ) -> Result<MappedTime, TimingError> {
        if output_delay_ms > 5000 {
            return Err(TimingError::DelayRange);
        }
        let mapped = self.map_server_time(frame.server_timestamp_us, now_us)?;
        let target = signed_us(i128::from(mapped.time_us) - i128::from(output_delay_ms) * 1000)?;
        Ok(MappedTime {
            time_us: local_us(target)?,
            ..mapped
        })
    }
    /// This arrival-delay diagnostic never changes playback targets or filter state.
    pub fn arrival_delay(
        &mut self,
        frame: PlayerAudioFrame<'_>,
        arrival_us: i64,
        now_us: i64,
    ) -> Result<Option<i64>, TimingError> {
        local_us(arrival_us)?;
        self.live()?;
        self.observe(now_us)?;
        if frame.send_ahead_us == 0 || frame.send_ahead_us == u32::MAX {
            return Ok(None);
        }
        let sent =
            signed_us(i128::from(frame.server_timestamp_us) - i128::from(frame.send_ahead_us))?;
        let mapped = self.map_server_time(sent, now_us)?;
        Ok(Some(signed_us(
            i128::from(arrival_us) - i128::from(mapped.time_us),
        )?))
    }
    pub fn retire(&mut self) {
        self.terminal = Some(Status::Retired);
        self.probe = Probe::Idle;
        self.burst = None;
    }
    fn live(&self) -> Result<(), TimingError> {
        match self.terminal {
            Some(Status::NumericFault) => Err(TimingError::NumericFault),
            Some(_) => Err(TimingError::Retired),
            None => Ok(()),
        }
    }
    fn check_binding(&self, token: ProbeToken) -> Result<(), TimingError> {
        if token.binding != self.binding {
            Err(TimingError::BindingMismatch)
        } else {
            Ok(())
        }
    }
    fn fail<T>(&mut self, error: TimingError) -> Result<T, TimingError> {
        self.retire();
        if error == TimingError::NumericFault {
            self.terminal = Some(Status::NumericFault);
        }
        Err(error)
    }
    fn observe(&mut self, now: i64) -> Result<(), TimingError> {
        local_us(now)?;
        if self.last_now.is_some_and(|last| now < last) {
            return self.fail(TimingError::ClockRegression);
        }
        self.last_now = Some(now);
        Ok(())
    }
    fn deadline(&mut self, now: i64, duration: i64) -> Result<i64, TimingError> {
        match now
            .checked_add(duration)
            .filter(|value| *value <= MAX_TIME_US)
        {
            Some(deadline) => Ok(deadline),
            None => self.fail(TimingError::DeadlineOverflow),
        }
    }
    fn bump(&mut self, field: fn(&mut Diagnostics) -> &mut u64) -> Result<(), TimingError> {
        let counter = field(&mut self.diagnostics);
        if let Some(next) = counter.checked_add(1) {
            *counter = next;
            Ok(())
        } else {
            self.fail(TimingError::CounterOverflow)
        }
    }
    fn abandon_burst(&mut self) -> Result<(), TimingError> {
        if self.burst.is_some_and(|burst| burst.emitted > 0) {
            self.bump(|d| &mut d.incomplete_bursts)?;
        }
        self.burst = None;
        self.probe = Probe::Idle;
        Ok(())
    }
    fn check_cadence(&mut self, now: i64) -> Result<(), TimingError> {
        let incomplete = self
            .burst
            .is_some_and(|burst| burst.hard_deadline.is_some_and(|deadline| now >= deadline));
        if incomplete {
            self.abandon_burst()?;
        }
        if !self.cadence_reported
            && self.cadence_deadline.is_some_and(|deadline| {
                now > deadline || (now == deadline && (incomplete || self.suppressed))
            })
        {
            self.bump(|d| &mut d.cadence_misses)?;
            self.cadence_reported = true;
        }
        Ok(())
    }
    fn finish_if_complete(&mut self) -> Result<(), TimingError> {
        let Some(burst) = self.burst.filter(|burst| burst.emitted == 8) else {
            return Ok(());
        };
        let mut filter = self.filter;
        let changed = if let Some(sample) = burst.best {
            match filter.update(sample) {
                Ok(changed) => changed,
                Err(error) => return self.fail(error),
            }
        } else {
            false
        };
        let revision = if changed {
            let Some(revision) = self.revision.checked_add(1) else {
                return self.fail(TimingError::CounterOverflow);
            };
            revision
        } else {
            self.revision
        };
        self.bump(|d| &mut d.completed_bursts)?;
        if changed {
            self.bump(|d| &mut d.accepted_measurements)?;
        }
        self.filter = filter;
        self.revision = revision;
        self.burst = None;
        Ok(())
    }
    fn effect(&self, action: Action, next_deadline_us: Option<i64>) -> Poll {
        Poll {
            action,
            next_deadline_us,
            diagnostics: self.diagnostics,
        }
    }
    fn work_deadline(&self, deadline: i64) -> Option<i64> {
        Some(
            self.burst
                .and_then(|burst| burst.hard_deadline)
                .map_or(deadline, |hard| hard.min(deadline)),
        )
    }
    fn mapping(&mut self, now: i64) -> Result<Estimate, TimingError> {
        let snapshot = self.snapshot(now)?;
        match snapshot.status {
            Status::Mapped => {
                if let Err(error) = self.filter.validate() {
                    return self.fail(error);
                }
                Ok(snapshot.estimate.expect("mapped state has an estimate"))
            }
            Status::Uninitialized => Err(TimingError::Uninitialized),
            Status::Stale => Err(TimingError::Stale),
            Status::NumericFault => Err(TimingError::NumericFault),
            Status::Retired => Err(TimingError::Retired),
        }
    }
    fn mapped(&self, time_us: i64) -> MappedTime {
        MappedTime {
            time_us,
            binding: self.binding,
            mapping_revision: self.revision,
        }
    }
}
fn signed_us(value: i128) -> Result<i64, TimingError> {
    if !(-i128::from(MAX_TIME_US)..=i128::from(MAX_TIME_US)).contains(&value) {
        return Err(TimingError::NumericRange);
    }
    i64::try_from(value).map_err(|_| TimingError::NumericRange)
}
fn local_us(value: i64) -> Result<i64, TimingError> {
    if !(0..=MAX_TIME_US).contains(&value) {
        Err(TimingError::NumericRange)
    } else {
        Ok(value)
    }
}
fn round_us(value: f64) -> Result<i64, TimingError> {
    if !value.is_finite() || value.abs() > MAX_TIME_US as f64 {
        return Err(TimingError::NumericRange);
    }
    let rounded = value.round();
    if rounded.abs() > MAX_TIME_US as f64 {
        return Err(TimingError::NumericRange);
    }
    // The explicit finite envelope check precedes this conversion; no saturating cast is possible.
    Ok(rounded as i64)
}

#[cfg(test)]
mod tests;
