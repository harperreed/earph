// ABOUTME: Checks normalized f64 attenuation against literal frame gains and native PCM.
// ABOUTME: Verifies invalid input is atomic and prepared gain paths allocate nothing.
use earph_core::{ErrorCode, audio::*, gain::GainRamp, ids::Epochs};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

fn format(channels: ChannelLayout, representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(48000, channels, representation).unwrap()
}

fn ramp() -> GainRamp {
    GainRamp::new(format(ChannelLayout::Stereo, PcmRepresentation::Signed24)).unwrap()
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AllocationCounts {
    allocations: usize,
    frees: usize,
}
thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<AllocationCounts> = const { Cell::new(AllocationCounts { allocations: 0, frees: 0 }) };
}
struct TrackingAllocator;
// SAFETY: Every call delegates to System with the original pointer and layout.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(false);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count_allocation(true);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(false);
        count_allocation(true);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
fn count_allocation(free: bool) {
    if TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            if free {
                counts.frees += 1;
            } else {
                counts.allocations += 1;
            }
            cell.set(counts);
        });
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;
struct TrackingGuard;
impl Drop for TrackingGuard {
    fn drop(&mut self) {
        TRACKING.with(|active| active.set(false));
    }
}
fn tracked<T>(operation: impl FnOnce() -> T) -> (T, AllocationCounts) {
    COUNTS.with(|counts| counts.set(AllocationCounts::default()));
    TRACKING.with(|active| active.set(true));
    let guard = TrackingGuard;
    let result = operation();
    drop(guard);
    (result, COUNTS.with(Cell::get))
}

#[test]
fn prepared_float_and_native_paths_allocate_and_free_nothing() {
    let (calibration, counts) = tracked(|| std::hint::black_box(vec![7_u8; 128]));
    assert!(counts.allocations > 0);
    let (_, counts) = tracked(|| drop(calibration));
    assert!(counts.frees > 0);

    let mut gain = ramp();
    gain.set_target(Some(0.0), 4).unwrap();
    let mut native = PcmBlock::prepare(
        format(ChannelLayout::Stereo, PcmRepresentation::Signed24),
        2,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    let mut wrong = PcmBlock::prepare(
        format(ChannelLayout::Mono, PcmRepresentation::Signed16),
        1,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    let mut samples = [2.0, -2.0, 1.0, -1.0];
    let (results, counts) = tracked(|| {
        let invalid_span = gain.process_f64(&mut samples, 1);
        let valid_float = gain.process_f64(&mut samples, 2);
        let valid_native = gain.process(&mut native);
        let unity_float = gain.process_f64(&mut samples, 2);
        let invalid_float = gain.process_f64(&mut [1.0, f64::NAN], 1);
        let invalid_native = gain.process(&mut wrong);
        gain.set_target(None, 0).unwrap();
        let muted_float = gain.process_f64(&mut samples, 2);
        let muted_native = gain.process(&mut native);
        (
            invalid_span,
            valid_float,
            valid_native,
            unity_float,
            invalid_float,
            invalid_native,
            muted_float,
            muted_native,
        )
    });
    assert_eq!(counts, AllocationCounts::default());
    assert!(results.0.is_err());
    assert!(results.1.unwrap().modified);
    assert!(results.2.unwrap().modified);
    assert!(!results.3.unwrap().modified);
    assert!(results.4.is_err());
    assert!(results.5.is_err());
    assert!(results.6.unwrap().modified);
    assert!(results.7.unwrap().modified);
}

#[test]
fn stereo_ramp_uses_literal_frame_gains_across_short_blocks() {
    let mut gain = ramp();
    gain.set_target(Some(0.0), 4).unwrap();
    let mut first = [1.0, -2.0, 1.0, -2.0, 1.0, -2.0];
    let report = gain.process_f64(&mut first, 3).unwrap();
    assert_eq!(first, [0.25, -0.5, 0.5, -1.0, 0.75, -1.5]);
    assert_eq!(report.input_peak, 2.0);
    assert_eq!(report.output_peak, 1.5);
    assert!(report.modified);
    let mut last = [1.0, -2.0, 1.0, -2.0];
    let report = gain.process_f64(&mut last, 2).unwrap();
    assert_eq!(last, [1.0, -2.0, 1.0, -2.0]);
    assert!(!report.modified);
}

#[test]
fn mute_unmute_and_retarget_continue_from_the_current_frame_gain() {
    let mut gain = ramp();
    let mut muted = [2.0, -2.0];
    let report = gain.process_f64(&mut muted, 1).unwrap();
    assert_eq!(muted, [0.0, 0.0]);
    assert_eq!(report.output_peak, 0.0);
    assert!(report.modified);
    gain.set_target(Some(0.0), 4).unwrap();
    let mut rising = [1.0; 4];
    gain.process_f64(&mut rising, 2).unwrap();
    assert_eq!(rising, [0.25, 0.25, 0.5, 0.5]);
    gain.set_target(None, 2).unwrap();
    let mut falling = [1.0; 6];
    gain.process_f64(&mut falling, 3).unwrap();
    assert_eq!(falling, [0.25, 0.25, 0.0, 0.0, 0.0, 0.0]);
    gain.set_target(Some(-40.0), 0).unwrap();
    let mut quiet = [2.0, -3.0];
    gain.process_f64(&mut quiet, 1).unwrap();
    for (actual, expected) in quiet.into_iter().zip([0.02_f64, -0.03]) {
        assert!((actual - expected).abs() <= 8.0 * f64::EPSILON * expected.abs());
    }
}

#[test]
fn unity_preserves_finite_bits_signed_zeros_and_headroom() {
    let mut gain = ramp();
    gain.set_target(Some(0.0), 0).unwrap();
    let mut samples = [0.0, -0.0, f64::MAX, -f64::MAX, f64::from_bits(1), -1.25];
    let before = samples.map(f64::to_bits);
    let report = gain.process_f64(&mut samples, 3).unwrap();
    assert_eq!(samples.map(f64::to_bits), before);
    assert_eq!(report.input_peak, f64::MAX);
    assert_eq!(report.output_peak, f64::MAX);
    assert!(!report.modified);
}

#[test]
fn attenuation_preserves_headroom_and_fractional_samples_without_quantization() {
    let mut gain = ramp();
    gain.set_target(Some(0.0), 2).unwrap();
    let mut samples = [4.0, -3.0, 0.000_000_001, -0.000_000_001];
    let report = gain.process_f64(&mut samples, 2).unwrap();
    assert_eq!(samples, [2.0, -1.5, 0.000_000_001, -0.000_000_001]);
    assert_eq!(report.input_peak, 4.0);
    assert_eq!(report.output_peak, 2.0);
}

#[test]
fn invalid_spans_and_nonfinite_samples_preserve_the_entire_buffer_and_ramp() {
    let mut gain = ramp();
    gain.set_target(Some(0.0), 4).unwrap();
    let mut prefix = [1.0; 2];
    gain.process_f64(&mut prefix, 1).unwrap();
    for (mut samples, frames) in [
        (vec![], 0),
        (vec![1.0, 2.0], 0),
        (vec![1.0], 1),
        (vec![1.0, 2.0, 3.0], 1),
        (vec![1.0, 2.0], 2),
        (vec![1.0, 2.0], usize::MAX),
        (vec![1.0, 2.0, 3.0, f64::NAN], 2),
        (vec![1.0, 2.0, 3.0, f64::INFINITY], 2),
        (vec![1.0, 2.0, 3.0, f64::NEG_INFINITY], 2),
    ] {
        let before: Vec<_> = samples.iter().map(|sample| sample.to_bits()).collect();
        let mut trial = gain.clone();
        let mut reference = gain.clone();
        assert_eq!(
            trial.process_f64(&mut samples, frames).unwrap_err().code,
            ErrorCode::AssetInvalid
        );
        assert_eq!(
            samples
                .iter()
                .map(|sample| sample.to_bits())
                .collect::<Vec<_>>(),
            before
        );
        let mut actual = [1.0; 2];
        let mut expected = actual;
        trial.process_f64(&mut actual, 1).unwrap();
        reference.process_f64(&mut expected, 1).unwrap();
        assert_eq!(actual, expected);
    }
}

#[test]
fn mono_geometry_and_prepared_rate_bound_apply_to_f64_processing() {
    let native = PcmFormat::new(4, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap();
    let mut gain = GainRamp::new(native).unwrap();
    gain.set_target(Some(0.0), 4).unwrap();
    for db in [1.0, -121.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            gain.set_target(Some(db), 1).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
    }
    assert_eq!(
        gain.set_target(None, 5).unwrap_err().code,
        ErrorCode::SafetyPolicyBlocked
    );
    let mut samples = [1.0; 4];
    gain.process_f64(&mut samples, 4).unwrap();
    assert_eq!(samples, [0.25, 0.5, 0.75, 1.0]);
    gain.set_target(Some(-120.0), 0).unwrap();
    let mut minimum = [1.0];
    gain.process_f64(&mut minimum, 1).unwrap();
    assert!((minimum[0] - 0.000_001).abs() <= 8.0 * f64::EPSILON * 0.000_001);
}

#[test]
fn float_and_native_processing_share_one_envelope_when_interleaved() {
    for representation in [
        PcmRepresentation::Signed16,
        PcmRepresentation::Signed24,
        PcmRepresentation::Signed32,
    ] {
        let native = format(ChannelLayout::Stereo, representation);
        let mut gain = GainRamp::new(native).unwrap();
        let mut reference = gain.clone();
        for (target, frames, count) in [(Some(0.0), 4, 1), (None, 2, 1), (Some(0.0), 3, 4)] {
            gain.set_target(target, frames).unwrap();
            reference.set_target(target, frames).unwrap();
            let mut input =
                PcmBlock::prepare(native, count, Epochs::default(), MemoryBudget::default())
                    .unwrap();
            for index in 0..count * 2 {
                input
                    .set_integer_sample(index, if index % 2 == 0 { 24000 } else { -24000 })
                    .unwrap();
            }
            let mut samples: Vec<f64> = (0..count * 2)
                .map(|index| if index % 2 == 0 { 0.5 } else { -0.5 })
                .collect();
            gain.process_f64(&mut samples, count).unwrap();
            reference.process(&mut input).unwrap();
            for (index, sample) in samples.into_iter().enumerate() {
                assert_eq!(
                    (sample * 48000.0).round() as i64,
                    input.integer_sample(index).unwrap()
                );
            }
            let mut actual =
                PcmBlock::prepare(native, 1, Epochs::default(), MemoryBudget::default()).unwrap();
            actual.set_integer_sample(0, 24000).unwrap();
            actual.set_integer_sample(1, -24000).unwrap();
            let mut expected = actual.clone();
            gain.process(&mut actual).unwrap();
            reference.process(&mut expected).unwrap();
            assert_eq!(actual, expected);
        }
    }
}
