// ABOUTME: Exercises real Core Audio discovery and never-started IOProc ownership.
// ABOUTME: Checks current settings before and after silent registration and cleanup.
#![cfg(target_os = "macos")]
use earph_core::ErrorCode;
use earph_coreaudio::{SilentRegistration, devices, inspect};

#[test]
fn native_discovery_and_explicit_inspection_keep_stream_boundaries() {
    let inventory = devices().unwrap();
    assert!(inventory.failures.is_empty(), "{:?}", inventory.failures);
    assert!(
        !inventory.devices.is_empty(),
        "native integration requires an available output device"
    );
    for device in &inventory.devices {
        assert!(!device.uid.is_empty());
        assert!(!device.output_streams.is_empty());
        assert!(device.nominal_rate_hz.is_finite());
        let fresh = inspect(&device.uid).unwrap();
        assert_eq!(fresh.uid, device.uid);
        assert_eq!(fresh.object_id, device.object_id);
        for stream in &fresh.output_streams {
            assert!(stream.physical.sample_rate_hz.is_finite());
            assert!(stream.virtual_format.sample_rate_hz.is_finite());
            assert!(stream.physical.bytes_per_frame > 0);
        }
    }
}

#[test]
fn unavailable_uids_do_not_select_another_output() {
    let error = inspect("earph-nonexistent-device-2389").unwrap_err();
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    assert_eq!(inspect("").unwrap_err().code, ErrorCode::AssetInvalid);
}

#[test]
fn registration_close_and_reopen_never_start_callbacks_or_change_settings() {
    let inventory = devices().unwrap();
    let device = inventory
        .devices
        .iter()
        .find(|device| device.alive)
        .expect("native integration requires a live output");
    let before = inspect(&device.uid).unwrap();
    for _ in 0..3 {
        let mut registration = SilentRegistration::new(&device.uid).unwrap();
        assert_eq!(
            registration.report().evidence_level,
            "registered_not_started"
        );
        assert_eq!(registration.report().signals_submitted, 0);
        registration.close().unwrap();
        registration.close().unwrap();
        assert_eq!(registration.callbacks_observed(), 0);
    }
    let after = inspect(&device.uid).unwrap();
    assert_eq!(before.nominal_rate_hz, after.nominal_rate_hz);
    assert_eq!(before.buffer_frames, after.buffer_frames);
    assert_eq!(before.hog_owner_pid, after.hog_owner_pid);
    assert_eq!(
        serde_json::to_value(&before.output_streams).unwrap(),
        serde_json::to_value(&after.output_streams).unwrap()
    );
}

#[test]
fn dropping_registration_releases_only_its_own_callback() {
    let inventory = devices().unwrap();
    let device = inventory
        .devices
        .iter()
        .find(|device| device.alive)
        .unwrap();
    let first = SilentRegistration::new(&device.uid).unwrap();
    let mut second = SilentRegistration::new(&device.uid).unwrap();
    drop(first);
    second.close().unwrap();
    assert_eq!(second.callbacks_observed(), 0);
    let mut reopened = SilentRegistration::new(&device.uid).unwrap();
    reopened.close().unwrap();
}
