// ABOUTME: Encodes the pinned Sendspin version-zero pairing credential token.
// ABOUTME: Treats encoding as explicit secret export and redacts diagnostics.
use std::fmt;

#[derive(Clone, PartialEq, Eq)]
pub struct PairingToken {
    pub client_public: [u8; 32],
    pub pairing_psk: [u8; 32],
}
impl fmt::Debug for PairingToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingToken([REDACTED])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    UnsupportedVersion,
}
impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pairing token {self:?}")
    }
}
impl std::error::Error for TokenError {}

pub fn encode_pairing_token(token: &PairingToken) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ934567";
    let mut result = String::with_capacity(107);
    result.push_str("SP:0");
    let mut buffer = 0u16;
    let mut bits = 0;
    for byte in token.client_public.iter().chain(&token.pairing_psk) {
        buffer = (buffer << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            result.push(ALPHABET[usize::from((buffer >> bits) & 31)] as char);
        }
    }
    if bits > 0 {
        result.push(ALPHABET[usize::from((buffer << (5 - bits)) & 31)] as char);
    }
    result
}

pub fn decode_pairing_token(text: &str) -> Result<PairingToken, TokenError> {
    if !text.is_ascii() {
        return Err(TokenError::Malformed);
    }
    let text = text.trim().to_ascii_uppercase();
    let text = text.strip_prefix("SP:").unwrap_or(&text);
    let (version, body) = text.split_at_checked(1).ok_or(TokenError::Malformed)?;
    if version != "0" {
        return Err(TokenError::UnsupportedVersion);
    }
    let symbols = body.trim_end_matches('=');
    let padding = (8 - symbols.len() % 8) % 8;
    if ![0, 2, 4, 5, 7].contains(&(symbols.len() % 8))
        || (symbols.len() != body.len() && body.len() - symbols.len() != padding)
    {
        return Err(TokenError::Malformed);
    }
    let mut payload = [0; 64];
    let mut count = 0;
    let mut buffer = 0u16;
    let mut bits = 0;
    for symbol in symbols.bytes() {
        let value = match symbol {
            b'A'..=b'Z' => symbol - b'A',
            b'2' | b'9' => 26,
            b'3'..=b'7' => symbol - b'3' + 27,
            _ => return Err(TokenError::Malformed),
        };
        buffer = (buffer << 5) | u16::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            if let Some(byte) = payload.get_mut(count) {
                *byte = (buffer >> bits) as u8;
            }
            count += 1;
        }
    }
    if count < 64 {
        return Err(TokenError::Malformed);
    }
    Ok(PairingToken {
        client_public: payload[..32]
            .try_into()
            .map_err(|_| TokenError::Malformed)?,
        pairing_psk: payload[32..]
            .try_into()
            .map_err(|_| TokenError::Malformed)?,
    })
}
