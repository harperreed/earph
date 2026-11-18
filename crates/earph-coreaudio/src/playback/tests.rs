// ABOUTME: Verifies exact output negotiation and bounded producer-consumer behavior.
// ABOUTME: Uses owned PCM without starting devices or changing native settings.
use super::*;
use crate::StreamFormat;
use earph_core::audio::{ChannelLayout, MemoryBudget, PcmRepresentation};
#[cfg(not(target_os = "macos"))]
#[allow(unsafe_code)]
#[path = "../../../earph-engine/tests/support/allocations.rs"]
mod allocations;

#[test]
#[cfg(not(target_os = "macos"))]
fn audio_storage_reports_the_actual_ring_once_without_heap_work() {
    for (route, expected) in [
        (PlaybackRoute::StrictInteger, 24),
        (PlaybackRoute::Float32Compatibility, 32),
    ] {
        let mut output = CoreAudioOutput {
            producer: producer(route),
            report: OutputReport {
                requested: request(route),
                observed: device(),
                evidence_level: "owned_ring_without_sdk",
                payload_preservation: "unverified",
                downstream_transformations: "unknown",
            },
            state: OutputState::Prepared,
        };
        let callback_owner = output.producer.ring.clone();
        let (bytes, counts) = allocations::tracked(|| output.audio_storage_bytes().unwrap());
        assert_eq!(bytes, expected);
        assert_eq!(counts, allocations::AllocationCounts::default());
        assert_eq!(
            bytes as usize,
            callback_owner.bytes.len() * size_of::<AtomicU8>()
        );
        output.close().unwrap();
        assert_eq!(output.audio_storage_bytes().unwrap(), expected);
    }
}

fn request(route: PlaybackRoute) -> OutputRequest {
    OutputRequest {
        device_uid: "owned-test".into(),
        format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap(),
        buffer_frames: 2,
        ring_frames: 4,
        route,
    }
}
fn device() -> DeviceSnapshot {
    let stream = StreamFormat {
        sample_rate_hz: 48000.0,
        format_id: u32::from_be_bytes(*b"lpcm"),
        format_flags: 12,
        bytes_per_packet: 6,
        frames_per_packet: 1,
        bytes_per_frame: 6,
        channels_per_frame: 2,
        bits_per_channel: 24,
    };
    DeviceSnapshot {
        object_id: 1,
        uid: "owned-test".into(),
        name: "Owned fixture".into(),
        alive: true,
        nominal_rate_hz: 48000.0,
        nominal_rate_settable: false,
        buffer_frames: 2,
        buffer_frame_range: crate::ValueRange {
            minimum: 1.0,
            maximum: 8192.0,
        },
        hog_owner_pid: None,
        hog_mode_settable: None,
        output_streams: vec![crate::StreamSnapshot {
            object_id: 2,
            physical: stream.clone(),
            virtual_format: stream,
            physical_settable: false,
            virtual_settable: false,
            available_physical: vec![],
            available_virtual: vec![],
        }],
    }
}
pub(super) fn producer(route: PlaybackRoute) -> Producer {
    Producer::new(request(route), Epochs::default()).unwrap()
}
pub(super) fn block(position: u64, values: &[i64]) -> PcmBlock {
    let mut block = PcmBlock::prepare(
        request(PlaybackRoute::StrictInteger).format,
        values.len() / 2,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    for (i, value) in values.iter().enumerate() {
        block.set_integer_sample(i, *value).unwrap();
    }
    block
        .set_span(Epochs::default(), 0, position, (values.len() / 2) as u64)
        .unwrap();
    block
}
#[test]
fn strict_requires_both_observed_asbds_and_compatibility_reports_conversion() {
    let strict = request(PlaybackRoute::StrictInteger);
    let mut observed = device();
    assert_eq!(negotiate(&strict, &observed).unwrap(), 6);
    observed.output_streams[0].physical.bytes_per_frame = 8;
    assert_eq!(
        negotiate(&strict, &observed).unwrap_err().code,
        ErrorCode::FidelityPolicyConflict
    );
    let compat = request(PlaybackRoute::Float32Compatibility);
    assert!(negotiate(&compat, &observed).is_err());
    let virtual_format = &mut observed.output_streams[0].virtual_format;
    virtual_format.format_flags = 9;
    virtual_format.bits_per_channel = 32;
    virtual_format.bytes_per_frame = 8;
    virtual_format.bytes_per_packet = 8;
    assert_eq!(negotiate(&compat, &observed).unwrap(), 8);
    virtual_format_for(&mut observed).sample_rate_hz = 44100.0;
    assert!(negotiate(&compat, &observed).is_err());
}
fn virtual_format_for(device: &mut DeviceSnapshot) -> &mut StreamFormat {
    &mut device.output_streams[0].virtual_format
}

#[test]
fn bounded_ring_preserves_partial_writes_and_wrap_order() {
    let mut output = producer(PlaybackRoute::StrictInteger);
    let input = block(0, &[1, -1, 2, -2, 3, -3, 4, -4, 5, -5]);
    assert_eq!(
        output.write(&input, 0, 5).unwrap(),
        WriteProgress::Accepted { frames: 4 }
    );
    assert_eq!(
        output.write(&input, 4, 1).unwrap(),
        WriteProgress::WouldBlock
    );
    let mut bytes = [0; 12];
    output.ring.enabled.store(true, Ordering::Release);
    output.ring.render(&mut bytes);
    assert_eq!(bytes, [1, 0, 0, 255, 255, 255, 2, 0, 0, 254, 255, 255]);
    assert_eq!(
        output.write(&input, 4, 1).unwrap(),
        WriteProgress::Accepted { frames: 1 }
    );
    output.finish().unwrap();
    let mut tail = [0; 24];
    output.ring.render(&mut tail);
    assert_eq!(
        &tail[..18],
        &[
            3, 0, 0, 253, 255, 255, 4, 0, 0, 252, 255, 255, 5, 0, 0, 251, 255, 255
        ]
    );
    assert_eq!(&tail[18..], &[0; 6]);
    assert_eq!(output.ring.consumed.load(Ordering::Acquire), 5);
    assert_eq!(output.ring.fault.load(Ordering::Acquire), 0);
}
#[test]
fn underrun_is_silent_terminal_and_never_replays_buffered_audio() {
    let mut output = producer(PlaybackRoute::StrictInteger);
    output.write(&block(0, &[1, 2]), 0, 1).unwrap();
    output.ring.enabled.store(true, Ordering::Release);
    let mut bytes = [255; 12];
    output.ring.render(&mut bytes);
    assert_eq!(bytes, [0; 12]);
    assert_eq!(
        output.write(&block(1, &[3, 4]), 0, 1).unwrap_err().code,
        ErrorCode::OutputFault
    );
    output.ring.render(&mut bytes);
    assert_eq!(bytes, [0; 12]);
    assert_eq!(output.ring.consumed.load(Ordering::Acquire), 0);
}
#[test]
fn producer_rejects_stale_epochs_format_gaps_and_invalid_spans() {
    let mut output = producer(PlaybackRoute::StrictInteger);
    let mut stale = block(0, &[1, 2]);
    stale
        .set_span(
            Epochs {
                session: 1,
                stream: 0,
            },
            0,
            0,
            1,
        )
        .unwrap();
    assert_eq!(
        output.write(&stale, 0, 1).unwrap_err().code,
        ErrorCode::StaleSession
    );
    let input = block(0, &[1, 2]);
    assert!(output.write(&input, usize::MAX, 1).is_err());
    output.write(&input, 0, 1).unwrap();
    assert!(output.write(&input, 0, 1).is_err());
    output.cancel();
    assert_eq!(output.ring.queued_frames(), 0);
    assert!(output.write(&block(1, &[1, 2]), 0, 1).is_err());
}
#[test]
fn float_conversion_is_explicit_and_handles_signed_limits() {
    let mut output = producer(PlaybackRoute::Float32Compatibility);
    output.write(&block(0, &[-8388608, 4194304]), 0, 1).unwrap();
    output.ring.enabled.store(true, Ordering::Release);
    let mut bytes = [0; 8];
    output.ring.render(&mut bytes);
    assert_eq!(f32::from_le_bytes(bytes[..4].try_into().unwrap()), -1.0);
    assert_eq!(f32::from_le_bytes(bytes[4..].try_into().unwrap()), 0.5);
}

#[test]
fn permission_denial_does_not_allow_start_or_resume() {
    assert_eq!(
        permission(SignalPermission::Denied).unwrap_err().code,
        ErrorCode::SafetyPolicyBlocked
    );
    permission(SignalPermission::Granted).unwrap();
}
#[test]
fn eof_padding_and_cancellation_are_distinct_from_underrun() {
    let mut output = producer(PlaybackRoute::StrictInteger);
    output.write(&block(0, &[7, 8]), 0, 1).unwrap();
    output.finish().unwrap();
    assert!(output.write(&block(1, &[9, 10]), 0, 1).is_err());
    output.ring.enabled.store(true, Ordering::Release);
    let mut bytes = [255; 12];
    output.ring.render(&mut bytes);
    assert_eq!(bytes, [7, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0, 0]);
    output.cancel();
    bytes.fill(255);
    output.ring.render(&mut bytes);
    assert_eq!(bytes, [0; 12]);
}
#[test]
fn concurrent_ring_publication_keeps_complete_frames_in_order() {
    let mut output = producer(PlaybackRoute::StrictInteger);
    let ring = output.ring.clone();
    // A full ring supplies startup lead. The consumer waits on observed availability in this scheduler test.
    let consumer = std::thread::spawn(move || {
        ring.enabled.store(true, Ordering::Release);
        let mut bytes = [0; 6];
        for expected in 0..10000_i64 {
            while ring.queued_frames() == 0 {
                std::thread::yield_now();
            }
            ring.render(&mut bytes);
            assert_eq!(&bytes[..3], &expected.to_le_bytes()[..3]);
            assert_eq!(&bytes[3..], &(-expected).to_le_bytes()[..3]);
        }
    });
    for n in 0..10000_i64 {
        let input = block(n as u64, &[n, -n]);
        while output.write(&input, 0, 1).unwrap() == WriteProgress::WouldBlock {
            std::thread::yield_now();
        }
    }
    consumer.join().unwrap();
    assert_eq!(output.ring.consumed.load(Ordering::Acquire), 10000);
}

#[test]
fn compatibility_rejects_physical_rate_and_channel_conversion() {
    let compat = request(PlaybackRoute::Float32Compatibility);
    let mut observed = device();
    let virtual_format = virtual_format_for(&mut observed);
    virtual_format.format_flags = 9;
    virtual_format.bits_per_channel = 32;
    virtual_format.bytes_per_frame = 8;
    virtual_format.bytes_per_packet = 8;
    observed.output_streams[0].physical.sample_rate_hz = 44100.0;
    assert_eq!(
        negotiate(&compat, &observed).unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
    observed.output_streams[0].physical.sample_rate_hz = 48000.0;
    observed.output_streams[0].physical.channels_per_frame = 1;
    assert!(negotiate(&compat, &observed).is_err());
}

#[test]
fn ring_bounds_and_noninteger_formats_fail_before_storage_preparation() {
    for frames in [0, 1, 65537, u32::MAX] {
        let mut input = request(PlaybackRoute::StrictInteger);
        input.ring_frames = frames;
        assert_eq!(
            input.validate().unwrap_err().code,
            ErrorCode::ResourceBudgetExceeded
        );
    }
    let mut input = request(PlaybackRoute::Float32Compatibility);
    input.format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    assert_eq!(
        input.validate().unwrap_err().code,
        ErrorCode::FormatUnsupported
    );
}

#[test]
fn strict_byte_delivery_covers_all_integer_widths_and_mono() {
    for (representation, values, expected) in [
        (
            PcmRepresentation::Signed16,
            [-32768, 32767],
            vec![0, 128, 255, 127],
        ),
        (
            PcmRepresentation::Signed24,
            [-8388608, 8388607],
            vec![0, 0, 128, 255, 255, 127],
        ),
        (
            PcmRepresentation::Signed32,
            [-2147483648, 2147483647],
            vec![0, 0, 0, 128, 255, 255, 255, 127],
        ),
    ] {
        let mut input = request(PlaybackRoute::StrictInteger);
        input.format = PcmFormat::new(48000, ChannelLayout::Mono, representation).unwrap();
        let mut output = Producer::new(input.clone(), Epochs::default()).unwrap();
        let mut block =
            PcmBlock::prepare(input.format, 2, Epochs::default(), MemoryBudget::default()).unwrap();
        for (i, value) in values.iter().enumerate() {
            block.set_integer_sample(i, *value).unwrap();
        }
        output.write(&block, 0, 2).unwrap();
        output.ring.enabled.store(true, Ordering::Release);
        let mut destination = vec![0; expected.len()];
        output.ring.render(&mut destination);
        assert_eq!(destination, expected);
    }
}

#[test]
fn retired_fault_signals_are_never_reused_and_have_a_fixed_budget() {
    static SIGNALS: FaultSignals<2> = FaultSignals::new();
    let retired = SIGNALS.reserve().unwrap();
    let current = SIGNALS.reserve().unwrap();
    retired.store(1, Ordering::Release);
    assert_eq!(retired.load(Ordering::Acquire), 1);
    assert_eq!(current.load(Ordering::Acquire), 0);
    assert_eq!(
        SIGNALS.reserve().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn prior_sdk_timestamps_are_unknown_after_fault_pause_or_shutdown() {
    let observed = CallbackTiming {
        generation: 7,
        output_host_time_ticks: Some(123),
        output_sample_time_frames: Some(456.0),
        measurement_boundary: "sdk_output_timestamp_not_dac_presentation",
    };
    for state in [
        OutputState::Prepared,
        OutputState::Paused,
        OutputState::Stopped,
        OutputState::Failed,
        OutputState::Closed,
    ] {
        let hidden = observed.for_state(state);
        assert_eq!(hidden.generation, 7);
        assert_eq!(hidden.output_host_time_ticks, None);
        assert_eq!(hidden.output_sample_time_frames, None);
    }
    assert_eq!(
        observed
            .for_state(OutputState::Running)
            .output_host_time_ticks,
        Some(123)
    );
}
