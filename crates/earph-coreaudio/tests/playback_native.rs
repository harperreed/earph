// ABOUTME: Checks real playback registration and permission denial without starting devices.
// ABOUTME: Verifies native cleanup preserves observed device settings and callback ownership.
#![cfg(target_os = "macos")]
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation},
    ids::Epochs,
};
use earph_coreaudio::*;
fn selected() -> DeviceSnapshot {
    devices()
        .unwrap()
        .devices
        .into_iter()
        .find(|device| {
            device.alive
                && device.output_streams.len() == 1
                && device.output_streams[0].virtual_format.format_flags & !64 == 9
                && device.output_streams[0].virtual_format.bits_per_channel == 32
                && [1, 2].contains(&device.output_streams[0].virtual_format.channels_per_frame)
        })
        .expect("native silent integration requires a live interleaved float32 mono/stereo output")
}
fn request(device: &DeviceSnapshot) -> OutputRequest {
    let raw = &device.output_streams[0].virtual_format;
    assert_eq!(raw.sample_rate_hz.fract(), 0.0);
    OutputRequest {
        device_uid: device.uid.clone(),
        format: PcmFormat::new(
            raw.sample_rate_hz as u32,
            if raw.channels_per_frame == 1 {
                ChannelLayout::Mono
            } else {
                ChannelLayout::Stereo
            },
            PcmRepresentation::Signed24,
        )
        .unwrap(),
        buffer_frames: device.buffer_frames,
        ring_frames: device.buffer_frames * 4,
        route: PlaybackRoute::Float32Compatibility,
    }
}
#[test]
fn denied_start_queue_cancel_close_and_reopen_never_emit_or_change_settings() {
    let device = selected();
    let before = inspect(&device.uid).unwrap();
    for _ in 0..3 {
        let input = request(&device);
        let mut output = CoreAudioOutput::open(input.clone(), Epochs::default()).unwrap();
        assert_eq!(
            output.start(SignalPermission::Denied).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_eq!(
            output.resume(SignalPermission::Denied).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_eq!(output.state(), OutputState::Prepared);
        let mut block = PcmBlock::prepare(
            input.format,
            input.buffer_frames as usize,
            Epochs::default(),
            MemoryBudget::default(),
        )
        .unwrap();
        block.set_integer_sample(0, 1).unwrap();
        assert_eq!(
            output
                .write(&block, 0, input.buffer_frames as usize)
                .unwrap(),
            WriteProgress::Accepted {
                frames: input.buffer_frames as usize
            }
        );
        let progress = output.progress();
        assert_eq!(progress.submitted_frames, u64::from(input.buffer_frames));
        assert_eq!(progress.callback_delivered_frames, 0);
        assert_eq!(progress.callbacks_observed, 0);
        assert_eq!(progress.audible_position_frames, None);
        assert_eq!(
            output.report().payload_preservation,
            "modified_integer_to_float32"
        );
        output.stop().unwrap();
        assert_eq!(output.progress().queued_frames, 0);
        assert!(output.write(&block, 0, 1).is_err());
        output.close().unwrap();
        output.close().unwrap();
        assert_eq!(output.progress().callbacks_observed, 0);
    }
    let after = inspect(&device.uid).unwrap();
    assert_eq!(before.nominal_rate_hz, after.nominal_rate_hz);
    assert_eq!(before.buffer_frames, after.buffer_frames);
    assert_eq!(before.hog_owner_pid, after.hog_owner_pid);
    assert_eq!(
        serde_json::to_value(before.output_streams).unwrap(),
        serde_json::to_value(after.output_streams).unwrap()
    );
}
#[test]
fn dropping_one_playback_registration_preserves_another_and_strict_refuses_float() {
    let device = selected();
    let input = request(&device);
    let first = CoreAudioOutput::open(input.clone(), Epochs::default()).unwrap();
    let mut second = CoreAudioOutput::open(input.clone(), Epochs::default()).unwrap();
    drop(first);
    second.close().unwrap();
    let mut strict = input;
    strict.route = PlaybackRoute::StrictInteger;
    let error = CoreAudioOutput::open(strict, Epochs::default())
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::FidelityPolicyConflict);
}
