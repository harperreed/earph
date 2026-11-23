// ABOUTME: Decodes pinned Sendspin player messages after authentication and reassembly.
// ABOUTME: Grants no connection, playback, pairing, clock, or native output authority.
use std::fmt;

pub mod credentials;
pub mod message;
pub mod noise;
pub mod pairing;
pub mod pairing_token;
pub mod player;
pub mod session;
pub mod timing;

pub const SPEC_REVISION: &str = "9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    FrameBudgetExceeded,
    TruncatedHeader,
    WrongMessageType,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::FrameBudgetExceeded => "player audio message exceeds its byte budget",
            Self::TruncatedHeader => "player audio header is shorter than thirteen bytes",
            Self::WrongMessageType => "message is not player audio",
        })
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PlayerAudioFrame<'a> {
    /// Server clock microseconds, without a local-clock or DAC-presentation claim.
    pub server_timestamp_us: i64,
    /// Arrival-lead observation. Zero and u32::MAX are saturated, not playback deadlines.
    pub send_ahead_us: u32,
    pub payload: &'a [u8],
}

impl fmt::Debug for PlayerAudioFrame<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayerAudioFrame")
            .field("server_timestamp_us", &self.server_timestamp_us)
            .field("send_ahead_us", &self.send_ahead_us)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Input is one authenticated, reassembled ID-4 plaintext message, not a raw WebSocket frame.
/// The caller enforces the protocol revision; legacy layouts cannot be detected from arbitrary payloads.
pub fn decode_player_audio(
    plaintext: &[u8],
    max_frame_bytes: usize,
) -> Result<PlayerAudioFrame<'_>, ProtocolError> {
    if plaintext.len() > max_frame_bytes {
        return Err(ProtocolError::FrameBudgetExceeded);
    }
    if plaintext.len() < 13 {
        return Err(ProtocolError::TruncatedHeader);
    }
    decode_player_audio_body(plaintext[0], &plaintext[1..], max_frame_bytes)
}

/// Decode the ID and borrowed body returned by authenticated message reassembly.
pub fn decode_player_audio_body(
    id: u8,
    body: &[u8],
    max_frame_bytes: usize,
) -> Result<PlayerAudioFrame<'_>, ProtocolError> {
    if body
        .len()
        .checked_add(1)
        .is_none_or(|bytes| bytes > max_frame_bytes)
    {
        return Err(ProtocolError::FrameBudgetExceeded);
    }
    let header = body.get(..12).ok_or(ProtocolError::TruncatedHeader)?;
    if id != 4 {
        return Err(ProtocolError::WrongMessageType);
    }
    Ok(PlayerAudioFrame {
        server_timestamp_us: i64::from_be_bytes(
            header[..8]
                .try_into()
                .map_err(|_| ProtocolError::TruncatedHeader)?,
        ),
        send_ahead_us: u32::from_be_bytes(
            header[8..12]
                .try_into()
                .map_err(|_| ProtocolError::TruncatedHeader)?,
        ),
        payload: &body[12..],
    })
}
