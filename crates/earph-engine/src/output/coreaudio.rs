// ABOUTME: Bridges the shared worker to explicitly selected Core Audio outputs.
// ABOUTME: Reports native formats and keeps callback delivery separate from audible position.
use super::*;
use earph_core::session::ObservedSampleRepresentation;
use earph_coreaudio::{OutputRequest, PlaybackRoute, StreamFormat};
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub(super) use native::NativeOutput;

fn request(config: &OutputConfiguration, format: PcmFormat) -> Result<OutputRequest, EngineError> {
    config.validate()?;
    let route = match config.route {
        OutputRoute::DirectHardware => PlaybackRoute::StrictInteger,
        OutputRoute::System => PlaybackRoute::Float32Compatibility,
    };
    if route == PlaybackRoute::Float32Compatibility
        && config.fidelity == earph_core::safety::FidelityPolicy::StrictPreservation
    {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "strict preservation rejects the float32 compatibility route",
        ));
    }
    Ok(OutputRequest {
        device_uid: config.endpoint.clone(),
        format,
        buffer_frames: config.period_frames,
        ring_frames: config.buffer_frames,
        route,
    })
}
fn observed(format: &StreamFormat) -> ObservedStreamFormat {
    let representation = match (
        format.format_id == u32::from_be_bytes(*b"lpcm"),
        format.format_flags & 5,
        format.bits_per_channel,
    ) {
        (true, 1, 32) => ObservedSampleRepresentation::Float32,
        (true, 1, 64) => ObservedSampleRepresentation::Float64,
        (true, 4, 16) => ObservedSampleRepresentation::Signed16,
        (true, 4, 24) => ObservedSampleRepresentation::Signed24,
        (true, 4, 32) => ObservedSampleRepresentation::Signed32,
        _ => ObservedSampleRepresentation::Unknown,
    };
    ObservedStreamFormat {
        sample_rate_hz: format.sample_rate_hz,
        channels: format.channels_per_frame,
        representation,
        bytes_per_frame: format.bytes_per_frame,
        interleaved: format.format_flags & 32 == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        audio::{ChannelLayout, PcmRepresentation},
        ids::{DeviceId, ProfileId},
        safety::{FidelityPolicy, VolumeMode},
    };
    fn config() -> OutputConfiguration {
        OutputConfiguration {
            backend: OutputBackend::CoreAudio,
            route: OutputRoute::System,
            endpoint: "explicit-test-uid".into(),
            device_id: DeviceId::new("test").unwrap(),
            profile_id: ProfileId::new("test").unwrap(),
            period_frames: 512,
            buffer_frames: 4096,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        }
    }
    fn format() -> PcmFormat {
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap()
    }
    #[test]
    fn explicit_uid_and_buffer_bounds_reach_the_native_request_without_a_default_route() {
        let native = request(&config(), format()).unwrap();
        assert_eq!(native.device_uid, "explicit-test-uid");
        assert_eq!(native.buffer_frames, 512);
        assert_eq!(native.ring_frames, 4096);
        assert_eq!(native.route, PlaybackRoute::Float32Compatibility);
        let mut direct = config();
        direct.route = OutputRoute::DirectHardware;
        assert_eq!(
            request(&direct, format()).unwrap().route,
            PlaybackRoute::StrictInteger
        );
    }
    #[test]
    fn strict_policy_rejects_a_known_float_conversion_before_native_open() {
        let mut selected = config();
        selected.fidelity = FidelityPolicy::StrictPreservation;
        assert_eq!(
            request(&selected, format()).unwrap_err().code,
            ErrorCode::FidelityPolicyConflict
        );
        selected.route = OutputRoute::DirectHardware;
        assert!(request(&selected, format()).is_ok());
        selected.endpoint.clear();
        assert_eq!(
            request(&selected, format()).unwrap_err().code,
            ErrorCode::AssetInvalid
        );
    }
    #[test]
    fn observed_formats_preserve_the_difference_between_float_clients_and_padded_integer_devices() {
        let mut native = StreamFormat {
            sample_rate_hz: 48000.0,
            format_id: u32::from_be_bytes(*b"lpcm"),
            format_flags: 9,
            bytes_per_packet: 8,
            frames_per_packet: 1,
            bytes_per_frame: 8,
            channels_per_frame: 2,
            bits_per_channel: 32,
        };
        let client = observed(&native);
        assert_eq!(client.representation, ObservedSampleRepresentation::Float32);
        assert_eq!(client.sample_rate_hz, 48000.0);
        assert_eq!(client.channels, 2);
        assert!(client.interleaved);
        native.format_flags = 20;
        native.bits_per_channel = 24;
        let physical = observed(&native);
        assert_eq!(
            physical.representation,
            ObservedSampleRepresentation::Signed24
        );
        assert_eq!(physical.bytes_per_frame, 8);
        native.format_flags |= 32;
        assert!(!observed(&native).interleaved);
        native.format_id = 0;
        assert_eq!(
            observed(&native).representation,
            ObservedSampleRepresentation::Unknown
        );
    }
}
