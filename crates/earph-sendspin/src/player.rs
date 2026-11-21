// ABOUTME: Defines the fixed PCM player wire contract after authenticated reassembly.
// ABOUTME: Leaves connection admission, clock ownership, and output effects to their owners.
use crate::{
    PlayerAudioFrame, ProtocolError, decode_player_audio_body,
    message::{Complete, Envelope, ReceiveStamp},
    timing::MAX_TIME_US,
};
use serde_json::{Value, json};
use std::fmt;

pub mod timeline;
pub use timeline::*;

pub const MAX_CHUNK_FRAMES: usize = 7200;
pub const MAX_CHUNK_WIRE_BYTES: usize = 28813;
pub const MAX_OUTSTANDING_WIRE_BYTES: usize = 1048576;
pub const MAX_DECODED_FRAMES: usize = 262144;
pub const MAX_CHUNKS: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerError {
    Malformed,
    NumericRange,
    Header(ProtocolError),
    UnsupportedFormat,
    IncompleteFrame,
    ChunkDuration,
    LocalBudget,
    BindingMismatch,
    InactiveStream,
    Overlap,
    CounterOverflow,
    UnknownChunk,
    ArrivalOrder,
    OutputDelayChanged,
    ServerTimeOrder,
}
impl fmt::Display for PlayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sendspin player {self:?}")
    }
}
impl std::error::Error for PlayerError {}

/// The sole supported format. Chunks retain this value independently of later starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamFormat(());
impl StreamFormat {
    pub const PCM: Self = Self(());
    pub fn to_value(self) -> Value {
        json!({"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16})
    }
    fn parse(value: &Value) -> Result<Self, PlayerError> {
        let object = value.as_object().ok_or(PlayerError::Malformed)?;
        for field in ["codec", "sample_rate", "channels", "bit_depth"] {
            if !object.contains_key(field) {
                return Err(PlayerError::Malformed);
            }
        }
        if object.get("codec").and_then(Value::as_str) != Some("pcm")
            || object.get("sample_rate").and_then(Value::as_u64) != Some(48000)
            || object.get("channels").and_then(Value::as_u64) != Some(2)
            || object.get("bit_depth").and_then(Value::as_u64) != Some(16)
            || object.contains_key("codec_header")
        {
            return Err(PlayerError::UnsupportedFormat);
        }
        Ok(Self::PCM)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerSupport {
    buffer_capacity: usize,
}
impl PlayerSupport {
    pub fn new(buffer_capacity: usize) -> Result<Self, PlayerError> {
        if buffer_capacity == 0 || buffer_capacity > MAX_OUTSTANDING_WIRE_BYTES {
            return Err(PlayerError::LocalBudget);
        }
        Ok(Self { buffer_capacity })
    }
    pub fn buffer_capacity(self) -> usize {
        self.buffer_capacity
    }
    pub fn to_value(self) -> Value {
        json!({"supported_formats":[StreamFormat::PCM.to_value()],"buffer_capacity":self.buffer_capacity})
    }
}
/// Numeric player state only. Engine must independently establish availability and authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayerState {
    output_delay_ms: u32,
    required_lead_time_ms: u64,
    min_buffer_ms: u32,
}
impl PlayerState {
    pub fn new(
        output_delay_ms: u32,
        required_lead_time_ms: u64,
        min_buffer_ms: u32,
    ) -> Result<Self, PlayerError> {
        if output_delay_ms > 5000
            || min_buffer_ms > 5000
            || required_lead_time_ms > MAX_TIME_US as u64
        {
            return Err(PlayerError::NumericRange);
        }
        Ok(Self {
            output_delay_ms,
            required_lead_time_ms,
            min_buffer_ms,
        })
    }
    pub fn to_value(self) -> Value {
        json!({"output_delay_ms":self.output_delay_ms,"required_lead_time_ms":self.required_lead_time_ms,"min_buffer_ms":self.min_buffer_ms,"supported_commands":[]})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerEvent<'a> {
    Start {
        server_transmitted_us: i64,
        format: StreamFormat,
        arrival: ReceiveStamp,
    },
    Audio {
        frame: PlayerAudioFrame<'a>,
        arrival: ReceiveStamp,
    },
    Clear {
        server_transmitted_us: i64,
        from_timestamp_us: Option<i64>,
        arrival: ReceiveStamp,
    },
    End {
        arrival: ReceiveStamp,
    },
}
impl<'a> PlayerEvent<'a> {
    /// `participating` means both active player role and playback activity, never availability.
    pub fn decode(
        complete: &'a Complete,
        participating: bool,
    ) -> Result<Option<Self>, PlayerError> {
        let arrival = complete.arrival;
        if complete.id == 4 {
            if !participating {
                return Ok(None);
            }
            let frame = decode_player_audio_body(4, &complete.body, MAX_CHUNK_WIRE_BYTES)
                .map_err(PlayerError::Header)?;
            validate_audio(frame)?;
            return Ok(Some(Self::Audio { frame, arrival }));
        }
        if complete.id != 0 {
            return Ok(None);
        }
        let envelope = Envelope::parse(&complete.body).map_err(|_| PlayerError::Malformed)?;
        let payload = &envelope.payload;
        Ok(match envelope.kind.as_str() {
            "stream/start" => {
                let server_transmitted_us = timestamp(
                    payload
                        .get("server_transmitted")
                        .ok_or(PlayerError::Malformed)?,
                )?;
                if !participating {
                    return Ok(None);
                }
                payload
                    .get("player")
                    .map(|value| {
                        StreamFormat::parse(value).map(|format| Self::Start {
                            server_transmitted_us,
                            format,
                            arrival,
                        })
                    })
                    .transpose()?
            }
            "stream/clear" => {
                let server_transmitted_us = timestamp(
                    payload
                        .get("server_transmitted")
                        .ok_or(PlayerError::Malformed)?,
                )?;
                let from_timestamp_us = payload.get("from_timestamp").map(timestamp).transpose()?;
                let targeted = targets_player(payload)?;
                (participating && targeted).then_some(Self::Clear {
                    server_transmitted_us,
                    from_timestamp_us,
                    arrival,
                })
            }
            "stream/end" => {
                (targets_player(payload)? && participating).then_some(Self::End { arrival })
            }
            // No player commands were offered, so their payloads remain unvalidated.
            _ => None,
        })
    }
    pub fn arrival(self) -> ReceiveStamp {
        match self {
            Self::Start { arrival, .. }
            | Self::Audio { arrival, .. }
            | Self::Clear { arrival, .. }
            | Self::End { arrival } => arrival,
        }
    }
}
fn targets_player(payload: &Value) -> Result<bool, PlayerError> {
    let Some(roles) = payload.get("roles") else {
        return Ok(true);
    };
    let roles = roles
        .as_array()
        .filter(|roles| !roles.is_empty())
        .ok_or(PlayerError::Malformed)?;
    let mut targeted = false;
    for role in roles {
        targeted |= role.as_str().ok_or(PlayerError::Malformed)? == "player";
    }
    Ok(targeted)
}
fn timestamp(value: &Value) -> Result<i64, PlayerError> {
    let value = value.as_i64().ok_or(PlayerError::Malformed)?;
    check_timestamp(value)?;
    Ok(value)
}
fn check_timestamp(value: i64) -> Result<(), PlayerError> {
    if !(-MAX_TIME_US..=MAX_TIME_US).contains(&value) {
        return Err(PlayerError::NumericRange);
    }
    Ok(())
}
fn validate_audio(frame: PlayerAudioFrame<'_>) -> Result<usize, PlayerError> {
    check_timestamp(frame.server_timestamp_us)?;
    if !frame.payload.len().is_multiple_of(4) {
        return Err(PlayerError::IncompleteFrame);
    }
    let frames = frame.payload.len() / 4;
    if frames > MAX_CHUNK_FRAMES {
        return Err(PlayerError::ChunkDuration);
    }
    Ok(frames)
}
