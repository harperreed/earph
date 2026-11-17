// ABOUTME: Checks bounded requests and exact physical and virtual PCM assessment.
// ABOUTME: Keeps format compatibility separate from acquired or measured capabilities.
use earph_core::{ErrorCode, audio::*};
use earph_coreaudio::*;

fn request() -> PrecisionRequest {
    PrecisionRequest {
        schema_version: 1,
        device_uid: "explicit-test-device".into(),
        format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap(),
        buffer_frames: 256,
    }
}
fn format() -> StreamFormat {
    StreamFormat {
        sample_rate_hz: 48000.0,
        format_id: u32::from_be_bytes(*b"lpcm"),
        format_flags: 12,
        bytes_per_packet: 6,
        frames_per_packet: 1,
        bytes_per_frame: 6,
        channels_per_frame: 2,
        bits_per_channel: 24,
    }
}
fn device() -> DeviceSnapshot {
    DeviceSnapshot {
        object_id: 1,
        uid: "explicit-test-device".into(),
        name: "Test format observation".into(),
        alive: true,
        nominal_rate_hz: 48000.0,
        nominal_rate_settable: true,
        buffer_frames: 256,
        buffer_frame_range: ValueRange {
            minimum: 32.0,
            maximum: 2048.0,
        },
        hog_owner_pid: None,
        hog_mode_settable: None,
        output_streams: vec![StreamSnapshot {
            object_id: 2,
            physical: format(),
            virtual_format: format(),
            physical_settable: false,
            virtual_settable: false,
            available_physical: vec![],
            available_virtual: vec![],
        }],
    }
}

#[test]
fn explicit_versioned_integer_requests_are_required() {
    request().validate().unwrap();
    for uid in ["", " ", "id\0hidden", "id\n", &"x".repeat(1025)] {
        let mut input = request();
        input.device_uid = uid.into();
        assert_eq!(input.validate().unwrap_err().code, ErrorCode::AssetInvalid);
    }
    let mut input = request();
    input.schema_version = 2;
    assert_eq!(
        input.validate().unwrap_err().code,
        ErrorCode::CapabilityUnsupported
    );
    for frames in [0, 8193, u32::MAX] {
        let mut input = request();
        input.buffer_frames = frames;
        assert_eq!(
            input.validate().unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
    let mut input = request();
    input.format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    assert_eq!(
        input.validate().unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
}

#[test]
fn a_current_match_does_not_claim_ownership_or_preservation() {
    let result = assess(&request(), &device()).unwrap();
    assert!(result.matches_current_format);
    assert_eq!(result.evidence_level, "observed_current_settings");
    assert_eq!(result.ownership, "unknown");
    assert_eq!(result.payload_preservation, "unverified");
    assert!(!result.opened);
}

#[test]
fn all_integer_widths_match_their_exact_native_byte_layout() {
    for (representation, bits, stereo_bytes) in [
        (PcmRepresentation::Signed16, 16, 4),
        (PcmRepresentation::Signed24, 24, 6),
        (PcmRepresentation::Signed32, 32, 8),
    ] {
        for flags in [12, 76] {
            let mut input = request();
            input.format = PcmFormat::new(48000, ChannelLayout::Stereo, representation).unwrap();
            let mut snapshot = device();
            let mut observed = format();
            observed.bits_per_channel = bits;
            observed.bytes_per_frame = stereo_bytes;
            observed.bytes_per_packet = observed.bytes_per_frame;
            observed.format_flags = flags;
            snapshot.output_streams[0].physical = observed.clone();
            snapshot.output_streams[0].virtual_format = observed;
            assert!(assess(&input, &snapshot).unwrap().matches_current_format);
        }
    }
}

#[test]
fn physical_and_virtual_formats_are_checked_independently() {
    for physical in [true, false] {
        for (name, change) in [
            ("float", 0),
            ("rate", 1),
            ("channels", 2),
            ("depth", 3),
            ("unpacked", 4),
            ("endian", 5),
            ("planar", 6),
            ("alignment", 7),
            ("frame bytes", 8),
            ("packet frames", 9),
            ("packet bytes", 10),
            ("codec", 11),
        ] {
            let mut snapshot = device();
            let stream = &mut snapshot.output_streams[0];
            let format = if physical {
                &mut stream.physical
            } else {
                &mut stream.virtual_format
            };
            match change {
                0 => format.format_flags |= 1,
                1 => format.sample_rate_hz = 47999.5,
                2 => format.channels_per_frame = 1,
                3 => format.bits_per_channel = 32,
                4 => format.format_flags &= !8,
                5 => format.format_flags |= 2,
                6 => format.format_flags |= 32,
                7 => format.format_flags |= 16,
                8 => format.bytes_per_frame = 8,
                9 => format.frames_per_packet = 2,
                10 => format.bytes_per_packet = 8,
                11 => format.format_id = 0,
                _ => unreachable!(),
            }
            let result = assess(&request(), &snapshot).unwrap();
            assert!(!result.matches_current_format, "accepted {name}");
            assert!(
                result
                    .conflicts
                    .iter()
                    .any(|conflict| conflict.contains(if physical {
                        "physical"
                    } else {
                        "virtual"
                    }))
            );
        }
    }
}

#[test]
fn stale_identity_dead_devices_and_unknown_streams_are_never_matches() {
    let mut snapshot = device();
    snapshot.uid = "other-device".into();
    assert_eq!(
        assess(&request(), &snapshot).unwrap_err().code,
        ErrorCode::DeviceUnavailable
    );
    let mut snapshot = device();
    snapshot.alive = false;
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
    let mut snapshot = device();
    snapshot.output_streams.clear();
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
    let mut snapshot = device();
    snapshot
        .output_streams
        .push(snapshot.output_streams[0].clone());
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
    let mut snapshot = device();
    snapshot.buffer_frames = 512;
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
    let mut snapshot = device();
    snapshot.nominal_rate_hz = 44100.0;
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
}

#[test]
fn hog_availability_is_separate_from_format_match() {
    let mut snapshot = device();
    snapshot.hog_owner_pid = Some(-1);
    snapshot.hog_mode_settable = Some(true);
    assert_eq!(
        assess(&request(), &snapshot).unwrap().ownership,
        "available_not_acquired"
    );
    snapshot.hog_owner_pid = Some(42);
    let result = assess(&request(), &snapshot).unwrap();
    assert_eq!(result.ownership, "owned_by_process");
    assert!(result.matches_current_format);
    assert_eq!(result.payload_preservation, "unverified");
}

#[test]
fn unknown_request_fields_are_rejected() {
    let mut value = serde_json::to_value(request()).unwrap();
    value["fallback"] = true.into();
    assert!(serde_json::from_value::<PrecisionRequest>(value).is_err());
}

#[test]
fn raw_sdk_flags_and_format_id_are_the_format_authority() {
    let mut snapshot = device();
    snapshot.output_streams[0].physical.format_flags = 9;
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
    let mut snapshot = device();
    snapshot.output_streams[0].physical.format_id = u32::from_be_bytes(*b"alac");
    assert!(
        !assess(&request(), &snapshot)
            .unwrap()
            .matches_current_format
    );
}
