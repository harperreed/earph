// ABOUTME: Frames secret requests and responses within a fixed byte budget.
// ABOUTME: Never includes secret material in diagnostics or debug output.
use crate::{FRAME_BYTES, NativeConfig, StoreError};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::io::{Read, Write};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub version: u8,
    pub operation: u64,
    pub config: NativeConfig,
    pub action: Action,
}
#[derive(Serialize, Deserialize)]
pub(crate) enum Action {
    Read,
    Commit(Vec<u8>),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub version: u8,
    pub operation: u64,
    pub result: Result<Vec<u8>, StoreError>,
}
pub(crate) fn read<T: DeserializeOwned>(input: &mut impl Read) -> Result<T, StoreError> {
    let mut head = [0; 4];
    input
        .read_exact(&mut head)
        .map_err(|_| StoreError::Unavailable)?;
    let len = u32::from_be_bytes(head) as usize;
    if len == 0 || len > FRAME_BYTES {
        return Err(StoreError::Unavailable);
    }
    let mut bytes = vec![0; len];
    input
        .read_exact(&mut bytes)
        .map_err(|_| StoreError::Unavailable)?;
    let mut tail = [0];
    if input.read(&mut tail).map_err(|_| StoreError::Unavailable)? != 0 {
        return Err(StoreError::Unavailable);
    }
    serde_json::from_slice(&bytes).map_err(|_| StoreError::Unavailable)
}
pub(crate) fn frame<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| StoreError::Unavailable)?;
    if bytes.len() > FRAME_BYTES {
        return Err(StoreError::Unavailable);
    }
    let mut result = (bytes.len() as u32).to_be_bytes().to_vec();
    result.extend_from_slice(&bytes);
    Ok(result)
}
pub(crate) fn write<T: Serialize>(output: &mut impl Write, value: &T) -> Result<(), StoreError> {
    output
        .write_all(&frame(value)?)
        .map_err(|_| StoreError::Unavailable)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_incomplete_frames_and_trailing_data() {
        let mut bytes = frame(&Response {
            version: 1,
            operation: 7,
            result: Err(StoreError::Missing),
        })
        .unwrap();
        assert!(read::<Response>(&mut &bytes[..bytes.len() - 1]).is_err());
        bytes.push(0);
        assert!(read::<Response>(&mut bytes.as_slice()).is_err());
    }
    #[test]
    fn rejects_an_excessive_declared_frame_before_body_read() {
        let bytes = ((FRAME_BYTES + 1) as u32).to_be_bytes();
        assert!(read::<Request>(&mut bytes.as_slice()).is_err());
    }
}
