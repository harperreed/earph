// ABOUTME: Checks sample-level software volume, ramps, and unchanged unity output.
// ABOUTME: Verifies that block boundaries do not reset a prepared gain ramp.
use earph_core::{audio::*, gain::GainRamp, ids::Epochs};

fn format() -> PcmFormat {
    PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap()
}
fn block(position: u64, values: Vec<i32>) -> PcmBlock {
    PcmBlock::new(
        BlockHeader {
            epochs: Epochs::default(),
            sequence: position,
            frame_position: position,
            frame_count: values.len() as u64 / 2,
            format: format(),
            time_domain: TimeDomain::LocalFrames,
        },
        PcmSamples::Signed24(values),
    )
    .unwrap()
}

#[test]
fn unity_keeps_integer_extremes_exact_and_reports_peaks() {
    let mut gain = GainRamp::new(format()).unwrap();
    gain.set_target(Some(0.0), 0).unwrap();
    let mut audio = block(0, vec![-8388608, 8388607, 1, -1]);
    let before = audio.clone();
    let result = gain.process(&mut audio).unwrap();
    assert_eq!(audio, before);
    assert_eq!(result.input_peak, 1.0);
    assert_eq!(result.output_peak, 1.0);
    assert!(!result.modified);
}

#[test]
fn attenuation_is_explicit_and_muted_start_is_silent() {
    let mut gain = GainRamp::new(format()).unwrap();
    let mut audio = block(0, vec![10000, -10000]);
    gain.process(&mut audio).unwrap();
    assert_eq!(audio.integer_sample(0), Some(0));
    gain.set_target(Some(-40.0), 0).unwrap();
    let mut audio = block(0, vec![10000, -10000]);
    let result = gain.process(&mut audio).unwrap();
    assert_eq!(audio.integer_sample(0), Some(100));
    assert_eq!(audio.integer_sample(1), Some(-100));
    assert!(result.modified);
}

#[test]
fn a_ramp_uses_the_same_gain_for_channels_and_survives_blocks() {
    let mut joined = GainRamp::new(format()).unwrap();
    joined.set_target(Some(0.0), 4).unwrap();
    let mut split = joined.clone();
    let mut all = block(0, vec![1000; 12]);
    joined.process(&mut all).unwrap();
    let mut a = block(0, vec![1000; 6]);
    let mut b = block(3, vec![1000; 6]);
    split.process(&mut a).unwrap();
    split.process(&mut b).unwrap();
    for i in 0..12 {
        let value = if i < 6 {
            a.integer_sample(i)
        } else {
            b.integer_sample(i - 6)
        };
        assert_eq!(value, all.integer_sample(i));
    }
    for (frame, expected) in [250, 500, 750, 1000, 1000, 1000].into_iter().enumerate() {
        assert_eq!(all.integer_sample(frame * 2), Some(expected));
        assert_eq!(all.integer_sample(frame * 2 + 1), Some(expected));
    }
    split.set_target(None, 2).unwrap();
    let mut audio = block(6, vec![1000; 6]);
    split.process(&mut audio).unwrap();
    assert_eq!(audio.integer_sample(0), Some(500));
    assert_eq!(audio.integer_sample(2), Some(0));
    assert_eq!(audio.integer_sample(4), Some(0));
}

#[test]
fn invalid_gain_or_format_leaves_a_running_ramp_unchanged() {
    let mut gain = GainRamp::new(format()).unwrap();
    gain.set_target(Some(-40.0), 5).unwrap();
    let reference = gain.clone();
    for db in [1.0, -121.0, f64::NAN, f64::INFINITY] {
        assert!(gain.set_target(Some(db), 0).is_err());
    }
    assert!(gain.set_target(Some(0.0), 48001).is_err());
    let mut expected = reference;
    let mut a = block(0, vec![1000; 12]);
    let mut b = a.clone();
    gain.process(&mut a).unwrap();
    expected.process(&mut b).unwrap();
    assert_eq!(a, b);
    let other = PcmFormat::new(96000, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap();
    let mut wrong =
        PcmBlock::prepare(other, 1, Epochs::default(), MemoryBudget::default()).unwrap();
    assert!(gain.process(&mut wrong).is_err());
}
