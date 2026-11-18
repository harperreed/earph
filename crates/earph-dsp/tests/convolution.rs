// ABOUTME: Checks partitioned convolution against independent direct time-domain sums.
// ABOUTME: Exercises exact stream lengths, channel isolation, and prepared ownership offline.
#[path = "../src/convolution.rs"]
mod convolution;

use convolution::{ConvolutionError, PreparedConvolution};

fn reference(input: &[f64], taps: &[Vec<f64>]) -> Vec<f64> {
    let channels = taps.len();
    let frames = input.len() / channels;
    if frames == 0 {
        return Vec::new();
    }
    let mut output = vec![0.0; (frames + 256 + taps[0].len() - 1) * channels];
    for frame in 0..frames {
        for channel in 0..channels {
            for (tap, value) in taps[channel].iter().enumerate() {
                output[(frame + tap + 256) * channels + channel] +=
                    input[frame * channels + channel] * value;
            }
        }
    }
    output
}

fn render(input: &[f64], taps: &[Vec<f64>], quantum: usize) -> Vec<f64> {
    let channels = taps.len();
    let mut kernel = PreparedConvolution::prepare(taps, quantum, 128 * 1024 * 1024).unwrap();
    let mut output = Vec::new();
    for chunk in input.chunks(quantum * channels) {
        let mut span = vec![99.0; chunk.len()];
        assert_eq!(
            kernel.process(chunk, &mut span).unwrap(),
            chunk.len() / channels
        );
        output.extend(span);
    }
    kernel.begin_drain().unwrap();
    loop {
        let mut span = vec![99.0; quantum * channels];
        let frames = kernel.drain(&mut span).unwrap();
        output.extend_from_slice(&span[..frames * channels]);
        if frames == 0 {
            break;
        }
    }
    output
}

fn compare(actual: &[f64], expected: &[f64]) {
    assert_eq!(actual.len(), expected.len());
    let mut error_squared = 0.0;
    let mut norm_squared = 0.0;
    let mut maximum: f64 = 0.0;
    for (actual, expected) in actual.iter().zip(expected) {
        let error = actual - expected;
        maximum = maximum.max(error.abs());
        error_squared += error * error;
        norm_squared += expected * expected;
    }
    assert!(maximum <= 1e-9, "maximum error {maximum}");
    if norm_squared != 0.0 {
        assert!(
            (error_squared / norm_squared).sqrt() <= 1e-10,
            "relative L2 error {}",
            (error_squared / norm_squared).sqrt()
        );
    }
}

#[test]
fn identity_preserves_authored_latency_and_short_final_span() {
    let input: Vec<_> = (0..777).map(|i| (i as f64 * 0.03).sin() * 0.5).collect();
    let taps = [vec![1.0]];
    let expected = reference(&input, &taps);
    for quantum in [1, 17, 255, 256, 257, 1024] {
        compare(&render(&input, &taps, quantum), &expected);
    }
}

#[test]
fn stereo_noise_crosses_filter_and_input_partition_boundaries() {
    let mut state = 71_u64;
    let mut random = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 11) as f64 / ((1_u64 << 53) as f64) - 0.5
    };
    let input: Vec<_> = (0..2051 * 2).map(|_| random()).collect();
    let taps: Vec<Vec<_>> = (0..2)
        .map(|_| (0..787).map(|_| random() * 0.02).collect())
        .collect();
    let expected = reference(&input, &taps);
    for quantum in [13, 256, 513] {
        compare(&render(&input, &taps, quantum), &expected);
    }
}

#[test]
fn authored_trailing_zeros_and_empty_source_have_exact_lengths() {
    let taps = [vec![2.0, -1.0, 0.0, 0.0]];
    compare(&render(&[1.0], &taps, 7), &reference(&[1.0], &taps));
    assert!(render(&[], &taps, 7).is_empty());
}

#[test]
fn admission_rejects_invalid_geometry_nonfinite_taps_and_small_budget() {
    for taps in [
        vec![],
        vec![vec![]],
        vec![vec![1.0]; 3],
        vec![vec![1.0], vec![1.0, 0.0]],
        vec![vec![f64::NAN]],
        vec![vec![0.0; 262145]],
    ] {
        assert!(PreparedConvolution::prepare(&taps, 256, 128 * 1024 * 1024).is_err());
    }
    assert!(PreparedConvolution::prepare(&[vec![1.0]], 0, 128 * 1024 * 1024).is_err());
    assert!(matches!(
        PreparedConvolution::prepare(&[vec![1.0]], 256, 0),
        Err(ConvolutionError::MemoryBudget)
    ));
}

#[test]
fn validation_errors_preserve_history_and_output() {
    let taps = [vec![0.5; 513], vec![-0.25; 513]];
    let mut kernel = PreparedConvolution::prepare(&taps, 300, 1 << 26).unwrap();
    let first = vec![0.01; 598];
    let mut output = vec![0.0; first.len()];
    kernel.process(&first, &mut output).unwrap();
    let mut untouched = [81.0; 4];
    assert_eq!(
        kernel.process(&[0.0, f64::INFINITY, 0.0, 0.0], &mut untouched),
        Err(ConvolutionError::NonFinite)
    );
    assert_eq!(untouched, [81.0; 4]);
    assert_eq!(
        kernel.process(&[0.0; 3], &mut untouched),
        Err(ConvolutionError::Span)
    );
    assert_eq!(
        kernel.process(&[f64::MAX; 4], &mut untouched),
        Err(ConvolutionError::InputPeak)
    );
    assert_eq!(
        kernel.drain(&mut untouched),
        Err(ConvolutionError::Lifecycle)
    );
    assert_eq!(untouched, [81.0; 4]);
    let last = [0.9, -0.8, 0.2, 0.3];
    kernel.process(&last, &mut untouched).unwrap();
    output.extend(untouched);
    kernel.begin_drain().unwrap();
    loop {
        let mut span = [81.0; 14];
        let count = kernel.drain(&mut span).unwrap();
        assert!(span[count * 2..].iter().all(|value| *value == 81.0));
        output.extend_from_slice(&span[..count * 2]);
        if count == 0 {
            break;
        }
    }
    let mut whole = first;
    whole.extend(last);
    compare(&output, &reference(&whole, &taps));
}

#[test]
fn reset_and_cancel_remove_history_without_changing_filter() {
    let taps = [vec![0.02; 701]];
    let input = vec![0.3; 277];
    let mut kernel = PreparedConvolution::prepare(&taps, 277, 1 << 25).unwrap();
    let mut output = vec![0.0; input.len()];
    kernel.process(&input, &mut output).unwrap();
    kernel.cancel();
    output.fill(8.0);
    assert_eq!(
        kernel.process(&input, &mut output),
        Err(ConvolutionError::Lifecycle)
    );
    assert_eq!(kernel.begin_drain(), Err(ConvolutionError::Lifecycle));
    assert_eq!(kernel.drain(&mut output), Err(ConvolutionError::Lifecycle));
    assert!(output.iter().all(|value| *value == 8.0));
    kernel.reset();
    kernel.process(&input, &mut output).unwrap();
    compare(&output, &reference(&input, &taps)[..input.len()]);
    kernel.begin_drain().unwrap();
    assert_eq!(kernel.begin_drain(), Err(ConvolutionError::Lifecycle));
    let mut scratch = [0.0; 31];
    while kernel.drain(&mut scratch).unwrap() != 0 {}
    kernel.reset();
    kernel.begin_drain().unwrap();
    assert_eq!(kernel.drain(&mut scratch).unwrap(), 0);
}

#[test]
fn partition_boundary_impulses_and_silence_keep_exact_support() {
    for frames in [0, 1, 255, 256, 257, 511, 512, 513] {
        for length in [1, 255, 256, 257, 512, 513] {
            let mut taps = vec![0.0; length];
            taps[0] = 0.75;
            taps[length - 1] += -0.125;
            let mut input = vec![0.0; frames];
            if frames > 0 {
                input[frames - 1] = -0.3;
            }
            compare(
                &render(&input, std::slice::from_ref(&taps), 129),
                &reference(&input, &[taps]),
            );
        }
    }
    let silence = render(&[0.0; 1000], &[vec![0.25; 129]], 127);
    assert!(silence.iter().all(|sample| *sample == 0.0));
}

#[test]
fn stereo_has_no_cross_channel_energy_or_hidden_normalization() {
    let mut input = vec![0.0; 1026];
    input[0] = 0.5;
    let mut left = vec![0.0; 513];
    left[0] = 4.0;
    left[512] = -2.0;
    let taps = [left, vec![9.0; 513]];
    let output = render(&input, &taps, 257);
    assert!((output[256 * 2] - 2.0).abs() < 1e-12);
    assert!((output[768 * 2] + 1.0).abs() < 1e-12);
    assert!(
        output
            .iter()
            .skip(1)
            .step_by(2)
            .all(|sample| *sample == 0.0)
    );
    compare(&output, &reference(&input, &taps));
}

#[test]
fn resource_admission_counts_heap_storage_and_rejects_before_allocating() {
    let taps = [vec![0.01; 4097], vec![-0.02; 4097]];
    let (kernel, measured) = tracked(|| PreparedConvolution::prepare(&taps, 513, 1 << 26).unwrap());
    let resource = kernel.resources();
    assert_eq!(measured.allocations, 3);
    assert_eq!(measured.frees, 0);
    assert_eq!(
        measured.peak_bytes as u64,
        resource.spectra_bytes + resource.history_bytes + resource.staging_bytes
    );
    assert!(
        resource.resident_bytes
            >= measured.peak_bytes as u64 + std::mem::size_of::<PreparedConvolution>() as u64
    );
    assert!(resource.preparation_peak_bytes >= resource.resident_bytes);
    assert_eq!(resource.partition_count, 17);
    assert!(resource.operations_per_block_per_channel > 46080);
    assert_eq!(kernel.channels(), 2);
    assert_eq!(kernel.taps_per_channel(), 4097);
    assert_eq!(kernel.latency_frames(), 256);
    assert_eq!(kernel.tail_frames(), 4352);
    assert!(convolution::ALGORITHM_VERSION.contains("radix2"));
    let (result, measured) =
        tracked(|| PreparedConvolution::prepare(&taps, 513, resource.preparation_peak_bytes - 1));
    assert!(matches!(result, Err(ConvolutionError::MemoryBudget)));
    assert_eq!(measured.allocations, 0);
    let (_, measured) = tracked(|| drop(kernel));
    assert_eq!(measured.frees, 3);
}

#[test]
fn maximum_authored_kernel_and_overflowed_quantum_have_bounded_admission() {
    let taps = [vec![0.0; 262144], vec![0.0; 262144]];
    let kernel = PreparedConvolution::prepare(&taps, 1, 40 * 1024 * 1024).unwrap();
    assert_eq!(kernel.tail_frames(), 262399);
    assert!(kernel.resources().preparation_peak_bytes < 40 * 1024 * 1024);
    assert!(matches!(
        PreparedConvolution::prepare(&[vec![1.0]], usize::MAX, u64::MAX),
        Err(ConvolutionError::MemoryBudget)
    ));
}

#[test]
fn numerical_peak_allowance_encloses_cancellation_and_subnormal_signals() {
    let taps: Vec<_> = (0..1025)
        .map(|i| if i % 2 == 0 { 0.02 } else { -0.02 })
        .collect();
    let kernel = PreparedConvolution::prepare(std::slice::from_ref(&taps), 513, 1 << 26).unwrap();
    let l1 = kernel.tap_l1_bound(0).unwrap();
    let bound = kernel.peak_bound(0, 0.5).unwrap();
    assert!(l1 >= 20.5);
    assert!(bound > 0.5 * l1);
    assert!(bound < 0.5 * l1 * 1.0001);
    assert_eq!(kernel.peak_bound(1, 1.0), Err(ConvolutionError::Parameters));
    for value in [-1.0, f64::NAN, f64::INFINITY, f64::MAX] {
        assert_eq!(
            kernel.peak_bound(0, value),
            Err(ConvolutionError::InputPeak)
        );
    }
    for input in [
        vec![0.5; 1027],
        (0..1027)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect(),
        vec![f64::from_bits(1); 1027],
    ] {
        let peak = input.iter().fold(0.0_f64, |peak, x| peak.max(x.abs()));
        let bound = kernel.peak_bound(0, peak).unwrap();
        let output = render(&input, std::slice::from_ref(&taps), 513);
        assert!(output.iter().all(|value| value.abs() <= bound));
        compare(&output, &reference(&input, std::slice::from_ref(&taps)));
    }
}

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct AllocationCounts {
    allocations: usize,
    frees: usize,
    live_bytes: isize,
    peak_bytes: usize,
}
thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<AllocationCounts> = const { Cell::new(AllocationCounts { allocations: 0, frees: 0, live_bytes: 0, peak_bytes: 0 }) };
}
struct TrackingAllocator;
// SAFETY: Each operation delegates to System with the caller's original pointer and layout.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record_allocation(false, layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        record_allocation(true, layout.size());
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record_allocation(true, layout.size());
        record_allocation(false, size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
fn record_allocation(free: bool, bytes: usize) {
    if TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            if free {
                counts.frees += 1;
                counts.live_bytes -= bytes as isize;
            } else {
                counts.allocations += 1;
                counts.live_bytes += bytes as isize;
            }
            counts.peak_bytes = counts.peak_bytes.max(counts.live_bytes.max(0) as usize);
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
fn processing_drain_reset_cancel_and_errors_have_no_heap_activity() {
    let taps = [vec![0.001; 787], vec![-0.002; 787]];
    let mut kernel = PreparedConvolution::prepare(&taps, 513, 1 << 26).unwrap();
    let mut output = [0.0; 1026];
    let (_, counts) = tracked(|| {
        kernel.process(&[0.25; 1026], &mut output).unwrap();
        assert_eq!(
            kernel.process(&[f64::NAN; 1026], &mut output),
            Err(ConvolutionError::NonFinite)
        );
        assert_eq!(
            kernel.process(&[f64::MAX; 1026], &mut output),
            Err(ConvolutionError::InputPeak)
        );
        assert_eq!(
            kernel.process(&[0.0; 1025], &mut output),
            Err(ConvolutionError::Span)
        );
        assert_eq!(
            kernel.process(&[0.0; 1026], &mut [0.0; 1025]),
            Err(ConvolutionError::Span)
        );
        assert_eq!(
            kernel.process(&[0.0; 1028], &mut [0.0; 1028]),
            Err(ConvolutionError::Span)
        );
        kernel.begin_drain().unwrap();
        assert_eq!(kernel.drain(&mut [0.0; 3]), Err(ConvolutionError::Span));
        assert_eq!(
            kernel.process(&[0.0; 1026], &mut output),
            Err(ConvolutionError::Lifecycle)
        );
        while kernel.drain(&mut output).unwrap() != 0 {}
        kernel.reset();
        kernel.process(&[0.0; 1026], &mut output).unwrap();
        kernel.cancel();
        assert_eq!(kernel.drain(&mut output), Err(ConvolutionError::Lifecycle));
        kernel.reset();
    });
    assert_eq!(counts, AllocationCounts::default());
}

#[test]
fn generated_long_stereo_correction_emits_numeric_receipt() {
    let frames = 2053;
    let input: Vec<_> = (0..frames)
        .flat_map(|frame| {
            let t = frame as f64;
            [0.3 * (0.0003 * t * t).sin(), 0.2 * (0.19 * t).cos()]
        })
        .collect();
    let taps: Vec<Vec<_>> = (0..2)
        .map(|channel| {
            (0..4097)
                .map(|index| {
                    if index == 0 {
                        0.7 + channel as f64 * 0.1
                    } else {
                        0.001 * (index as f64 * (0.031 + channel as f64 * 0.004)).cos()
                            / (1.0 + index as f64 * 0.01)
                    }
                })
                .collect()
        })
        .collect();
    let output = render(&input, &taps, 513);
    let expected = reference(&input, &taps);
    compare(&output, &expected);
    let maximum = output
        .iter()
        .zip(&expected)
        .fold(0.0_f64, |peak, (actual, expected)| {
            peak.max((actual - expected).abs())
        });
    let error_squared: f64 = output
        .iter()
        .zip(&expected)
        .map(|(actual, expected)| (actual - expected) * (actual - expected))
        .sum();
    let norm_squared: f64 = expected.iter().map(|value| value * value).sum();
    let (kernel, measured) = tracked(|| PreparedConvolution::prepare(&taps, 513, 1 << 26).unwrap());
    let resources = kernel.resources();
    let receipt = serde_json::json!({
        "algorithm": convolution::ALGORITHM_VERSION,
        "qualification": "offline numeric and owned-allocation evidence; no CPU or hardware qualification",
        "architecture": std::env::consts::ARCH,
        "os": std::env::consts::OS,
        "source_frames": frames,
        "channels": 2,
        "authored_taps_per_channel": 4097,
        "quantum_frames": 513,
        "latency_frames": kernel.latency_frames(),
        "tail_frames": kernel.tail_frames(),
        "rendered_frames": output.len() / 2,
        "max_absolute_error": maximum,
        "relative_l2_error": (error_squared / norm_squared).sqrt(),
        "tolerance_max_absolute": 1e-9,
        "tolerance_relative_l2": 1e-10,
        "heap_preparation_peak_bytes": measured.peak_bytes,
        "resident_bytes": resources.resident_bytes,
        "admitted_preparation_peak_bytes": resources.preparation_peak_bytes,
        "preparation_stack_allowance_bytes": resources.preparation_stack_bytes,
        "runtime_peak_bounds": [kernel.peak_bound(0, 0.3).unwrap(), kernel.peak_bound(1, 0.2).unwrap()],
        "model": "IEEE binary64, round-to-nearest, gradual underflow; enclosure is not a machine-checked formal proof"
    });
    std::fs::create_dir_all("../../target/convolution-audit").unwrap();
    std::fs::write(
        "../../target/convolution-audit/convolution-comparison.json",
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
}

#[test]
fn an_empty_drain_buffer_cannot_report_false_end_of_stream() {
    let mut kernel = PreparedConvolution::prepare(&[vec![1.0]], 1, 1 << 24).unwrap();
    kernel.process(&[0.5], &mut [0.0]).unwrap();
    kernel.begin_drain().unwrap();
    assert_eq!(kernel.drain(&mut []), Err(ConvolutionError::Span));
    let mut count = 0;
    while kernel.drain(&mut [0.0]).unwrap() > 0 {
        count += 1;
    }
    assert_eq!(count, 256);
    assert_eq!(kernel.drain(&mut []).unwrap(), 0);
}
