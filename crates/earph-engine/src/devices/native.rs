// ABOUTME: Maps native read-only discovery into bounded shared device observations.
// ABOUTME: Preserves advertised evidence, partial failures, and unsupported-property uncertainty.
use super::*;

pub(super) fn query(query: &DeviceQuery) -> Result<DeviceObservation, DeviceQueryError> {
    #[cfg(target_os = "linux")]
    {
        if query.backend != OutputBackend::Alsa {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "device discovery backend is unavailable",
            )
            .into());
        }
        let hints = earph_alsa::devices().map_err(|error| DeviceQueryError {
            code: error.code,
            message: error.message.into(),
            native_status: error.native_errno,
            native_object_id: None,
        })?;
        let devices = hints
            .into_iter()
            .map(|hint| {
                DeviceSummary::advertised(
                    OutputBackend::Alsa,
                    hint.device,
                    None,
                    hint.description,
                    None,
                )
            })
            .collect::<Vec<_>>();
        Ok(match &query.endpoint {
            Some(endpoint) => DeviceObservation::Inspection {
                endpoint: endpoint.clone(),
                advertisement: devices
                    .into_iter()
                    .find(|device| device.endpoint == *endpoint),
                current_properties: None,
                complete: true,
                failures: vec![],
            },
            None => DeviceObservation::List {
                devices,
                complete: true,
                failures: vec![],
            },
        })
    }
    #[cfg(target_os = "macos")]
    {
        if query.backend != OutputBackend::CoreAudio {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "device discovery backend is unavailable",
            )
            .into());
        }
        let inventory = earph_coreaudio::devices().map_err(|error| DeviceQueryError {
            code: error.code,
            message: error.message.into(),
            native_status: error.os_status,
            native_object_id: None,
        })?;
        core_audio(query, inventory)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = query;
        Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "native device discovery is unavailable",
        )
        .into())
    }
}

#[cfg(any(target_os = "macos", test))]
fn core_audio(
    query: &DeviceQuery,
    inventory: earph_coreaudio::DeviceInventory,
) -> Result<DeviceObservation, DeviceQueryError> {
    let failures: Vec<_> = inventory
        .failures
        .into_iter()
        .map(|failure| DeviceQueryError {
            code: failure.error.code,
            message: failure.error.message.into(),
            native_status: failure.error.os_status,
            native_object_id: Some(failure.object_id),
        })
        .collect();
    let complete = failures.is_empty();
    if let Some(endpoint) = &query.endpoint {
        if let Some(device) = inventory
            .devices
            .into_iter()
            .find(|device| device.uid == *endpoint)
        {
            let advertisement = DeviceSummary::advertised(
                query.backend,
                device.uid.clone(),
                Some(device.name.clone()),
                None,
                Some(device.alive),
            );
            let current_properties = serde_json::from_value(
                serde_json::to_value(device).map_err(|_| DeviceQueryError::from(INVALID))?,
            )
            .map_err(|_| DeviceQueryError::from(INVALID))?;
            return Ok(DeviceObservation::Inspection {
                endpoint: endpoint.clone(),
                advertisement: Some(advertisement),
                current_properties: Some(current_properties),
                complete,
                failures,
            });
        }
        if complete {
            return Err(EngineError::new(
                ErrorCode::DeviceUnavailable,
                "selected UID is absent from the complete property inventory",
            )
            .into());
        }
        Ok(DeviceObservation::Inspection {
            endpoint: endpoint.clone(),
            advertisement: None,
            current_properties: None,
            complete,
            failures,
        })
    } else {
        let devices = inventory
            .devices
            .into_iter()
            .map(|device| {
                DeviceSummary::advertised(
                    query.backend,
                    device.uid,
                    Some(device.name),
                    None,
                    Some(device.alive),
                )
            })
            .collect();
        Ok(DeviceObservation::List {
            devices,
            complete,
            failures,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Synthetic property records exercise mapping only, without native calls or opened-output claims.
    fn device(uid: &str) -> earph_coreaudio::DeviceSnapshot {
        serde_json::from_value(serde_json::json!({"object_id":12,"uid":uid,"name":"Same label","alive":true,"nominal_rate_hz":48000.0,"nominal_rate_settable":false,"buffer_frames":128,"buffer_frame_range":{"minimum":128.0,"maximum":128.0},"hog_owner_pid":null,"hog_mode_settable":null,"output_streams":[]})).unwrap()
    }
    fn inventory(failed: bool) -> earph_coreaudio::DeviceInventory {
        earph_coreaudio::DeviceInventory {
            devices: vec![device("uid-a"), device("uid-b")],
            failures: if failed {
                vec![earph_coreaudio::DeviceFailure {
                    object_id: 99,
                    error: earph_coreaudio::CoreAudioError {
                        code: ErrorCode::DeviceUnavailable,
                        message: "controlled property failure",
                        os_status: Some(-50),
                    },
                }]
            } else {
                vec![]
            },
        }
    }
    #[test]
    fn property_mapping_keeps_partial_failures_and_unknown_target_presence() {
        let mut query = DeviceQuery {
            backend: OutputBackend::CoreAudio,
            endpoint: None,
        };
        let list = core_audio(&query, inventory(true)).unwrap();
        let DeviceObservation::List {
            devices,
            complete,
            failures,
        } = list
        else {
            panic!("list required")
        };
        assert!(!complete);
        assert_eq!(failures[0].native_object_id, Some(99));
        assert_eq!(failures[0].native_status, Some(-50));
        assert_eq!(devices[0].display_name, devices[1].display_name);
        assert_ne!(devices[0].selection, devices[1].selection);
        query.endpoint = Some("missing-uid".into());
        let unknown = core_audio(&query, inventory(true)).unwrap();
        assert!(matches!(
            unknown,
            DeviceObservation::Inspection {
                advertisement: None,
                current_properties: None,
                complete: false,
                ..
            }
        ));
        assert_eq!(
            core_audio(&query, inventory(false)).unwrap_err().code,
            ErrorCode::DeviceUnavailable
        );
        query.endpoint = Some("uid-a".into());
        let inspected = core_audio(&query, inventory(true)).unwrap();
        let DeviceObservation::Inspection {
            current_properties: Some(properties),
            complete: false,
            ..
        } = inspected
        else {
            panic!("properties required")
        };
        assert_eq!(
            serde_json::to_value(properties).unwrap(),
            serde_json::to_value(device("uid-a")).unwrap()
        );
    }
    #[test]
    fn unselectable_long_uid_is_never_truncated_or_given_an_alias() {
        let uid = "u".repeat(257);
        let query = DeviceQuery {
            backend: OutputBackend::CoreAudio,
            endpoint: None,
        };
        let list = core_audio(
            &query,
            earph_coreaudio::DeviceInventory {
                devices: vec![device(&uid)],
                failures: vec![],
            },
        )
        .unwrap();
        let DeviceObservation::List { devices, .. } = list else {
            panic!("list required")
        };
        assert_eq!(devices[0].endpoint, uid);
        assert_eq!(devices[0].selection, None);
        assert_eq!(devices[0].selection_error, Some(ErrorCode::AssetInvalid));
    }
}
