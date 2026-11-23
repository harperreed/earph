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
            output.audio_storage_bytes().unwrap(),
            u64::from(input.ring_frames) * input.format.channels().count() as u64 * 8
        );
        assert_eq!(
            output.start(SignalPermission::Denied).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_eq!(
            output.resume(SignalPermission::Denied).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_eq!(output.state(), OutputState::Prepared);
        let mut timing_spans = vec![NativeEnqueueSpan::default(); input.ring_frames as usize];
        let timing = output.timing_observation(&mut timing_spans).unwrap();
        assert_eq!(timing.state, OutputState::Prepared);
        assert_eq!(timing.availability, TimingAvailability::Prepared);
        assert_eq!(timing.accepted_frames, 0);
        assert_eq!(timing.queue_sample, None);
        assert_eq!(timing.native_accuracy_ns, None);
        assert_eq!(timing.initialized_spans, 0);
        assert_eq!(output.scheduled_output_complete().unwrap(), None);
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
        let timing = output.timing_observation(&mut timing_spans).unwrap();
        assert_eq!(timing.accepted_frames, u64::from(input.buffer_frames));
        assert_eq!(timing.availability, TimingAvailability::Prepared);
        assert_eq!(timing.queue_sample, None);
        assert!(timing.initialized_spans <= timing_spans.len());
        assert_eq!(progress.submitted_frames, u64::from(input.buffer_frames));
        assert_eq!(progress.callback_delivered_frames, 0);
        assert!(progress.queue_acquired_frames.is_some());
        assert_eq!(progress.audible_position_frames, None);
        assert_eq!(
            output.report().payload_preservation,
            "modified_integer_to_float32"
        );
        output.stop().unwrap();
        assert_eq!(output.scheduled_output_complete().unwrap(), None);
        assert_eq!(output.progress().queued_frames, 0);
        assert!(output.write(&block, 0, 1).is_err());
        output.close().unwrap();
        assert_eq!(output.scheduled_output_complete().unwrap(), None);
        output.close().unwrap();
        assert_eq!(output.progress().callback_delivered_frames, 0);
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

#[test]
#[ignore = "requires Root's explicit Mac preparation gate; never starts output"]
fn source_44100_prepares_selected_48000_device_with_application_64_256_geometry() {
    let selection: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            std::env::var_os("EARPH_ENGINE_PLAYBACK_REQUEST")
                .expect("explicit private selected-device request required"),
        )
        .unwrap(),
    )
    .unwrap();
    let device = inspect(
        selection["device_uid"]
            .as_str()
            .expect("exact UID required"),
    )
    .unwrap();
    assert_eq!(device.nominal_rate_hz, 48000.0);
    assert_eq!(device.buffer_frames, 512);
    let before = inspect(&device.uid).unwrap();
    for _ in 0..2 {
        let mut input = request(&device);
        input.format =
            PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
        input.buffer_frames = 64;
        input.ring_frames = 256;
        let mut output = CoreAudioOutput::open(input.clone(), Epochs::default()).unwrap();
        let client = output.report().queue_client_format.as_ref().unwrap();
        assert_eq!(client.sample_rate_hz, 44100.0);
        assert_eq!(client.format_flags, 9);
        assert_eq!(client.bytes_per_frame, 8);
        assert_eq!(output.audio_storage_bytes().unwrap(), 4096);
        let mut block = PcmBlock::prepare(
            input.format,
            600,
            Epochs::default(),
            MemoryBudget::default(),
        )
        .unwrap();
        block.set_span(Epochs::default(), 0, 0, 600).unwrap();
        let mut accepted = 0;
        while let WriteProgress::Accepted { frames } = output
            .write(&block, accepted, 600 - accepted)
            .unwrap_or_else(|failure| {
                eprintln!(
                    "{}",
                    serde_json::json!({"prepare_fault":{
                        "error":failure, "progress":output.progress()
                    }})
                );
                panic!("native preparation failed after its diagnostic record");
            })
        {
            accepted += frames;
        }
        assert_eq!(accepted, 512);
        let progress = output.progress();
        let schedule = progress
            .queue_schedule
            .expect("real enqueue returns its actual schedule");
        let start = f64::from_bits(
            schedule
                .sample_time_bits
                .expect("returned schedule timestamp"),
        );
        assert!(start.is_finite() && start >= 0.0);
        assert_ne!(schedule.flags & 1, 0);
        assert_eq!(schedule.payload_start, 192);
        assert_eq!(schedule.frames, 64);
        assert_eq!(schedule.epochs, Epochs::default());
        assert_eq!(progress.queue_enqueued_frames, Some(256));
        assert_eq!(progress.queue_retired_frames, Some(0));
        assert_eq!(
            output.start(SignalPermission::Denied).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_eq!(output.progress().submitted_frames, 512);
        assert_eq!(output.progress().callback_delivered_frames, 0);
        assert_eq!(output.restart_cursor_frames().unwrap(), Some(0));
        output.close().unwrap();
        assert_eq!(output.audio_storage_bytes().unwrap(), 2048);
    }
    let after = inspect(&device.uid).unwrap();
    assert_eq!(
        serde_json::to_value(before).unwrap(),
        serde_json::to_value(after).unwrap()
    );
}
