// ABOUTME: Creates shareable Core Audio records without device names or persistent UIDs.
// ABOUTME: Preserves raw observations and hashes the unchanged private source record.
use crate::CoreAudioError;
use earph_core::ErrorCode;
use serde_json::Value;

fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                match key.as_str() {
                    "uid" | "device_uid" => *value = "redacted-device".into(),
                    "name" => *value = "redacted-device-name".into(),
                    _ => redact(value),
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}
pub fn redact_record(bytes: &[u8]) -> Result<Value, CoreAudioError> {
    let invalid = || {
        CoreAudioError::new(
            ErrorCode::AssetInvalid,
            "input is not a version 1 Core Audio diagnostic record",
        )
    };
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(invalid());
    }
    let mut value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if value["schema_version"] != 1
        || !matches!(
            value["command"].as_str(),
            Some("devices" | "inspect" | "precision-check" | "registration-check")
        )
    {
        return Err(invalid());
    }
    redact(&mut value);
    value["identifiers_redacted"] = true.into();
    value["source_record_sha256"] = earph_harness::sha256(bytes).into();
    Ok(value)
}
