// ABOUTME: Tests versioned requests before any native device access.
// ABOUTME: Rejects implicit routes and buffers outside the admitted budget.
use earph_alsa::{OpenRequest, Route};
use earph_core::{ErrorCode, audio::*};

fn request() -> OpenRequest {
    OpenRequest {
        schema_version: 1,
        device: "hw:CARD=PCH,DEV=0".into(),
        route: Route::DirectHardware,
        format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap(),
        period_frames: 64,
        buffer_frames: 256,
    }
}

#[test]
fn explicit_integer_requests_are_accepted() {
    for representation in [
        PcmRepresentation::Signed16,
        PcmRepresentation::Signed24,
        PcmRepresentation::Signed32,
    ] {
        let mut value = request();
        value.format = PcmFormat::new(48000, ChannelLayout::Stereo, representation).unwrap();
        value.validate().unwrap();
        let encoded = serde_json::to_vec(&value).unwrap();
        let decoded: OpenRequest = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, value);
    }
}

#[test]
fn version_and_implicit_or_injected_endpoints_are_rejected() {
    let mut value = request();
    value.schema_version = 2;
    assert_eq!(
        value.validate().unwrap_err().code,
        ErrorCode::CapabilityUnsupported
    );
    for device in [
        "",
        " ",
        "hw:0,0\0hidden",
        "hw:0,0\n",
        "hw:0,0\t",
        &"x".repeat(257),
    ] {
        let mut value = request();
        value.device = device.into();
        assert!(value.validate().is_err(), "{device:?}");
    }
}

#[test]
fn compatibility_routes_never_imply_direct_output() {
    for device in ["default", "plughw:0,0", "pipewire", "null", "hw:"] {
        let mut value = request();
        value.device = device.into();
        assert_eq!(
            value.validate().unwrap_err().code,
            ErrorCode::FidelityPolicyConflict
        );
    }
    let mut value = request();
    value.route = Route::System;
    value.device = "pipewire".into();
    value.validate().unwrap();
}

#[test]
fn unsupported_domain_and_unbounded_buffers_fail_before_open() {
    let mut value = request();
    value.format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    assert_eq!(
        value.validate().unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    for (period, buffer) in [
        (0, 256),
        (64, 0),
        (64, 64),
        (8193, 32768),
        (64, 65537),
        (u32::MAX, u32::MAX),
    ] {
        let mut value = request();
        value.period_frames = period;
        value.buffer_frames = buffer;
        assert_eq!(
            value.validate().unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
}

#[test]
fn unknown_fields_and_missing_route_fail_deserialization() {
    let mut document = serde_json::to_value(request()).unwrap();
    document["fallback"] = true.into();
    assert!(serde_json::from_value::<OpenRequest>(document).is_err());
    let mut document = serde_json::to_value(request()).unwrap();
    document.as_object_mut().unwrap().remove("route");
    assert!(serde_json::from_value::<OpenRequest>(document).is_err());
}
