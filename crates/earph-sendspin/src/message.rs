// ABOUTME: Frames bounded Sendspin records and authenticated fragment sequences.
// ABOUTME: Leaves cryptographic authentication and role payload contracts to their owners.
use serde_json::{Value, json};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageError {
    Malformed,
    Budget,
    Deadline,
    Closed,
}
impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sendspin message {self:?}")
    }
}
impl std::error::Error for MessageError {}

/// Local policies. Bytes exclude the original message ID; deadlines use monotonic milliseconds.
#[derive(Debug, Clone, Copy)]
pub struct MessageLimits {
    pub body_bytes: usize,
    pub fragment_count: usize,
    pub assembly_ms: u64,
    pub record_plaintext_bytes: usize,
}
impl Default for MessageLimits {
    fn default() -> Self {
        Self {
            body_bytes: 64 * 1024 * 1024,
            fragment_count: 4096,
            assembly_ms: 30_000,
            record_plaintext_bytes: 65519,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Client,
    Server,
    Both,
}
pub struct Envelope {
    pub kind: String,
    pub payload: Value,
}
impl fmt::Debug for Envelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Envelope([REDACTED])")
    }
}
impl Envelope {
    pub fn parse(body: &[u8]) -> Result<Self, MessageError> {
        let mut value: Value = serde_json::from_slice(body).map_err(|_| MessageError::Malformed)?;
        let object = value.as_object_mut().ok_or(MessageError::Malformed)?;
        let kind = object
            .remove("type")
            .and_then(|v| v.as_str().map(str::to_owned))
            .ok_or(MessageError::Malformed)?;
        let payload = object
            .remove("payload")
            .filter(Value::is_object)
            .ok_or(MessageError::Malformed)?;
        Ok(Self { kind, payload })
    }
    pub fn direction(&self) -> Option<Direction> {
        direction(&self.kind)
    }
}
/// Classify only pinned recognized messages. Prefix guesses would break future compatibility.
pub fn direction(kind: &str) -> Option<Direction> {
    Some(match kind {
        "client/init"
        | "client/hello"
        | "client/time"
        | "client/state"
        | "client/command"
        | "client/leave"
        | "client/goodbye"
        | "client-stream/start"
        | "client-stream/end"
        | "client/pair-init"
        | "client/pair-finalize"
        | "client/pair-pending"
        | "client/pair-auth"
        | "client/pair-retry"
        | "client/pair-confirm" => Direction::Client,
        "server/init"
        | "server/error"
        | "server/hello"
        | "server/activate"
        | "server/time"
        | "server/state"
        | "server/command"
        | "server/unpair"
        | "stream/start"
        | "stream/clear"
        | "stream/end"
        | "group/update"
        | "server/pair-finalize"
        | "server/pair-init"
        | "server/pair-auth"
        | "server/pair-confirm" => Direction::Server,
        "noise/handshake" | "pair/abort" => Direction::Both,
        _ => return None,
    })
}

pub(crate) fn encode(kind: &str, payload: Value, limit: usize) -> Result<Vec<u8>, MessageError> {
    bounded_json(&json!({"type":kind,"payload":payload}), limit)
}
pub(crate) fn bounded_json(value: &Value, limit: usize) -> Result<Vec<u8>, MessageError> {
    struct Sink {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl std::io::Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::ErrorKind::OutOfMemory.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = Sink {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut sink, value).map_err(|_| MessageError::Budget)?;
    Ok(sink.bytes)
}

/// One checked local receive-time fact. Deadlines derive milliseconds from this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReceiveStamp(i64);
impl ReceiveStamp {
    pub fn new(microseconds: i64) -> Result<Self, MessageError> {
        if !(0..=crate::timing::MAX_TIME_US).contains(&microseconds) {
            return Err(MessageError::Malformed);
        }
        Ok(Self(microseconds))
    }
    pub fn microseconds(self) -> i64 {
        self.0
    }
    pub fn milliseconds(self) -> u64 {
        (self.0 / 1000) as u64
    }
}

#[derive(PartialEq, Eq)]
pub struct Complete {
    pub id: u8,
    pub body: Vec<u8>,
    pub arrival: ReceiveStamp,
}
impl fmt::Debug for Complete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Complete")
            .field("id", &self.id)
            .field("bytes", &self.body.len())
            .field("arrival", &self.arrival)
            .finish()
    }
}
struct Assembly {
    id: u8,
    body: Vec<u8>,
    bytes: usize,
    count: usize,
    started: u64,
}
pub struct Reassembler {
    limits: MessageLimits,
    assembly: Option<Assembly>,
    closed: bool,
}
impl Reassembler {
    pub fn new(limits: MessageLimits) -> Self {
        Self {
            limits,
            assembly: None,
            closed: false,
        }
    }
    pub fn retained_bytes(&self) -> usize {
        self.assembly.as_ref().map_or(0, |a| a.body.len())
    }
    pub fn is_assembling(&self) -> bool {
        self.assembly.is_some()
    }
    pub fn close(&mut self) {
        self.closed = true;
        self.assembly = None;
    }
    pub fn check_deadline(&mut self, now: u64) -> Result<(), MessageError> {
        if self.closed {
            return Err(MessageError::Closed);
        }
        if self
            .assembly
            .as_ref()
            .is_some_and(|a| now < a.started || now - a.started >= self.limits.assembly_ms)
        {
            self.close();
            return Err(MessageError::Deadline);
        }
        Ok(())
    }
    /// Pass only already authenticated plaintext, in nonce order. Unknown IDs retain only counters.
    /// Completed unknown IDs return an empty body marker, allowing the session to enforce sequencing.
    pub fn push(
        &mut self,
        record: &[u8],
        arrival: ReceiveStamp,
    ) -> Result<Option<Complete>, MessageError> {
        let result = self.push_inner(record, arrival);
        if result.is_err() {
            self.close();
        }
        result
    }
    fn push_inner(
        &mut self,
        record: &[u8],
        arrival: ReceiveStamp,
    ) -> Result<Option<Complete>, MessageError> {
        self.check_deadline(arrival.milliseconds())?;
        if record.len() > self.limits.record_plaintext_bytes.min(65519) {
            return Err(MessageError::Budget);
        }
        let id = *record.first().ok_or(MessageError::Malformed)?;
        if id != 1 {
            if self.assembly.is_some() {
                return Err(MessageError::Malformed);
            }
            let body = &record[1..];
            if body.len() > self.limits.body_bytes {
                return Err(MessageError::Budget);
            }
            return Ok(Some(Complete {
                id,
                body: if implemented(id) {
                    body.to_vec()
                } else {
                    vec![]
                },
                arrival,
            }));
        }
        let flags = *record.get(1).ok_or(MessageError::Malformed)?;
        if flags & !3 != 0 {
            return Err(MessageError::Malformed);
        }
        let first = flags & 2 != 0;
        let last = flags & 1 != 0;
        let data = if first {
            if self.assembly.is_some() {
                return Err(MessageError::Malformed);
            }
            let id = *record.get(2).ok_or(MessageError::Malformed)?;
            if id == 1 {
                return Err(MessageError::Malformed);
            }
            self.assembly = Some(Assembly {
                id,
                body: vec![],
                bytes: 0,
                count: 0,
                started: arrival.milliseconds(),
            });
            &record[3..]
        } else {
            &record[2..]
        };
        let a = self.assembly.as_mut().ok_or(MessageError::Malformed)?;
        let bytes = a
            .bytes
            .checked_add(data.len())
            .ok_or(MessageError::Budget)?;
        let count = a.count.checked_add(1).ok_or(MessageError::Budget)?;
        if bytes > self.limits.body_bytes || count > self.limits.fragment_count {
            return Err(MessageError::Budget);
        }
        if implemented(a.id) {
            a.body.extend_from_slice(data);
        }
        a.bytes = bytes;
        a.count = count;
        if last {
            let a = self.assembly.take().expect("assembly exists");
            Ok(Some(Complete {
                id: a.id,
                body: a.body,
                arrival,
            }))
        } else {
            Ok(None)
        }
    }
}
fn implemented(id: u8) -> bool {
    matches!(id, 0 | 4 | 12)
}

pub struct Outgoing {
    id: u8,
    body: Vec<u8>,
    offset: usize,
    done: bool,
    fragmented: bool,
    limits: MessageLimits,
}
impl Outgoing {
    pub fn validate(id: u8, bytes: usize, limits: MessageLimits) -> Result<(), MessageError> {
        let record = limits.record_plaintext_bytes.min(65519);
        if id == 1 || record < 4 {
            return Err(MessageError::Malformed);
        }
        if bytes > limits.body_bytes {
            return Err(MessageError::Budget);
        }
        let count = if bytes < record {
            1
        } else {
            1 + (bytes - (record - 3)).div_ceil(record - 2)
        };
        if count > limits.fragment_count {
            return Err(MessageError::Budget);
        }
        Ok(())
    }
    pub fn new(id: u8, body: Vec<u8>, limits: MessageLimits) -> Result<Self, MessageError> {
        Self::validate(id, body.len(), limits)?;
        let fragmented = body.len() >= limits.record_plaintext_bytes.min(65519);
        Ok(Self {
            id,
            body,
            offset: 0,
            done: false,
            fragmented,
            limits,
        })
    }
    pub(crate) fn into_parts(self) -> (u8, Vec<u8>) {
        (self.id, self.body)
    }
    pub(crate) fn first_record_len(
        bytes: usize,
        limits: MessageLimits,
    ) -> Result<usize, MessageError> {
        Ok(bytes
            .checked_add(1)
            .ok_or(MessageError::Budget)?
            .min(limits.record_plaintext_bytes.min(65519)))
    }
    pub(crate) fn next_record_len(&self) -> Option<usize> {
        if self.done {
            return None;
        }
        if self.offset == 0 {
            return Self::first_record_len(self.body.len(), self.limits).ok();
        }
        let header = 2;
        Some(
            header
                + (self.limits.record_plaintext_bytes.min(65519) - header)
                    .min(self.body.len() - self.offset),
        )
    }
    pub fn body_bytes(&self) -> usize {
        self.body.len()
    }
    pub fn has_more(&self) -> bool {
        !self.done
    }
    pub fn started(&self) -> bool {
        self.offset != 0 || self.done
    }
    pub fn next_record(&mut self) -> Option<Vec<u8>> {
        if self.done {
            return None;
        }
        if !self.fragmented {
            let mut out = Vec::with_capacity(self.body.len() + 1);
            out.push(self.id);
            out.extend_from_slice(&self.body);
            self.done = true;
            return Some(out);
        }
        let first = self.offset == 0;
        let header = if first { 3 } else { 2 };
        let count = (self.limits.record_plaintext_bytes.min(65519) - header)
            .min(self.body.len() - self.offset);
        let last = count == self.body.len() - self.offset;
        let mut out = Vec::with_capacity(header + count);
        out.extend_from_slice(&[1, u8::from(first) * 2 + u8::from(last)]);
        if first {
            out.push(self.id);
        }
        out.extend_from_slice(&self.body[self.offset..self.offset + count]);
        self.offset += count;
        self.done = last;
        Some(out)
    }
}
