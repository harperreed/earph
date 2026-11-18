// ABOUTME: Checks the owned low-level signal used only by explicit hardware acceptance.
// ABOUTME: Verifies fixed attenuation, ramps, silence boundaries, and bounded storage.
#[path = "support/hardware_signal.rs"]
mod hardware_signal;
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    ids::Epochs,
};

#[test]
fn generated_signal_is_bounded_ramped_and_silent_across_pause() {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    let fixture = hardware_signal::generate(format, 256, Epochs::default()).unwrap();
    let header = fixture.block.header();
    assert_eq!(header.frame_count, 36096);
    assert_eq!(fixture.pause_at_frames, 14438);
    let mut peak = 0_i64;
    let mut previous = 0_i64;
    for frame in 0..header.frame_count as usize {
        let sample = fixture.block.integer_sample(frame * 2).unwrap();
        assert_eq!(fixture.block.integer_sample(frame * 2 + 1), Some(sample));
        assert!(sample.abs() <= 32768);
        assert!(
            (sample - previous).abs() < 2100,
            "signal lacks a smooth ramp"
        );
        if frame as u64 >= fixture.pause_at_frames && (frame as u64) < fixture.quiet_until_frames {
            assert_eq!(sample, 0);
        }
        peak = peak.max(sample.abs());
        previous = sample;
    }
    assert!(peak > 32000);
    assert_eq!(fixture.block.integer_sample(0), Some(0));
    assert_eq!(previous, 0);
}

#[test]
fn signal_rejects_noninteger_lanes_and_unbounded_or_long_device_periods() {
    let integer = PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Signed24).unwrap();
    for period in [0, 8193, u32::MAX] {
        assert!(hardware_signal::generate(integer, period, Epochs::default()).is_err());
    }
    let float = PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
    assert!(hardware_signal::generate(float, 256, Epochs::default()).is_err());
    let slow = PcmFormat::new(8000, ChannelLayout::Mono, PcmRepresentation::Signed24).unwrap();
    assert!(hardware_signal::generate(slow, 8192, Epochs::default()).is_err());
}
