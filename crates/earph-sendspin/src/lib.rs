// ABOUTME: Decodes pinned Sendspin player messages after authentication and reassembly.
// ABOUTME: Grants no connection, playback, pairing, clock, or native output authority.
use std::fmt;

pub mod noise;

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
    let header: &[u8; 13] = plaintext
        .get(..13)
        .ok_or(ProtocolError::TruncatedHeader)?
        .try_into()
        .map_err(|_| ProtocolError::TruncatedHeader)?;
    if header[0] != 4 {
        return Err(ProtocolError::WrongMessageType);
    }
    Ok(PlayerAudioFrame {
        server_timestamp_us: i64::from_be_bytes(
            header[1..9]
                .try_into()
                .map_err(|_| ProtocolError::TruncatedHeader)?,
        ),
        send_ahead_us: u32::from_be_bytes(
            header[9..13]
                .try_into()
                .map_err(|_| ProtocolError::TruncatedHeader)?,
        ),
        payload: &plaintext[13..],
    })
}
