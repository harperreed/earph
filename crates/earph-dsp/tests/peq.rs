// ABOUTME: Checks PEQ response, peak enclosures, finite tails, and prepared ownership.
// ABOUTME: Uses analytical transfer functions and generated bounded signals without hardware.
use earph_core::{audio::ChannelLayout, dsp_nodes::PeqFilter, graph::FiniteGraphScalar};
use earph_dsp::peq::{self, PeqDesign, PeqError};
#[path = "support/evidence.rs"]
mod evidence;

#[test]
fn explicit_peq_export_roundtrips_without_overwriting_evidence() {
    let (report, directory) = evidence::check_export(
        "measured_response_matches_independent_analog_oracle_and_writes_numeric_receipts",
        "peq",
        "numeric-test-results.json",
    );
    assert!(report["maximum_response_error_db"].as_f64().unwrap() <= 0.01);
    assert!(!report["results"].as_array().unwrap().is_empty());
    assert_eq!(report["source_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(report["test_sha256"].as_str().unwrap().len(), 64);
    let csv = std::fs::read_to_string(directory.join("peq-response.csv")).unwrap();
    let mut lines = csv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "filter,rate_hz,frequency_hz,oracle_db,measured_db,error_db,tail_frames,discarded_bound"
    );
    let rows: Vec<_> = lines.collect();
    assert_eq!(rows.len(), report["results"].as_array().unwrap().len());
    for row in rows {
        let values: Vec<f64> = row.split(',').map(|n| n.parse().unwrap()).collect();
        assert_eq!(values.len(), 8);
        assert!(values.iter().all(|n| n.is_finite()));
        assert!(values[5] <= 0.01);
    }
}

fn scalar(value: f64) -> FiniteGraphScalar {
    FiniteGraphScalar::new(value).unwrap()
}
fn peak(frequency: f64, q: f64, gain: f64) -> PeqFilter {
    PeqFilter::Peaking {
        frequency_hz: scalar(frequency),
        q: scalar(q),
        gain_db: scalar(gain),
    }
}
fn design(filter: PeqFilter, rate: u32, channels: ChannelLayout) -> PeqDesign {
    PeqDesign::prepare(&filter, rate, channels).unwrap()
}

fn q_shelf(high: bool, frequency: f64, q: f64, gain: f64) -> PeqFilter {
    serde_json::from_value(serde_json::json!({"kind":if high {"high_shelf_q"}else{"low_shelf_q"},"frequency_hz":frequency,"q":q,"gain_db":gain})).unwrap()
}

#[test]
fn q_shelves_match_independent_analog_response_including_resonant_controls() {
    for high in [false, true] {
        for q in [0.2, 0.7, 2.0, 10.0] {
            for gain in [-12.0, 12.0] {
                let center = if high { 8000.0 } else { 105.0 };
                let filter = q_shelf(high, center, q, gain);
                let design = design(filter, 48000, ChannelLayout::Mono);
                let [b0, b1, b2, a1, a2] = design.coefficients();
                for frequency in [20.0, 80.0, 300.0, 2000.0, 10000.0, 20000.0] {
                    let omega = std::f64::consts::TAU * frequency / 48000.0;
                    let measured = (b0 + b1 * omega.cos() + b2 * (2.0 * omega).cos())
                        .hypot(-b1 * omega.sin() - b2 * (2.0 * omega).sin())
                        / (1.0 + a1 * omega.cos() + a2 * (2.0 * omega).cos())
                            .hypot(-a1 * omega.sin() - a2 * (2.0 * omega).sin());
                    let a = 10.0_f64.powf(gain / 40.0);
                    let t = (std::f64::consts::PI * frequency / 48000.0).tan()
                        / (std::f64::consts::PI * center / 48000.0).tan();
                    let (nr, dr) = if high {
                        (a * (1.0 - a * t * t), a - t * t)
                    } else {
                        (a * (a - t * t), 1.0 - a * t * t)
                    };
                    let damping = a.sqrt() * t / q;
                    let expected = nr.hypot(a * damping) / dr.hypot(damping);
                    assert!(
                        (20.0 * (measured / expected).log10()).abs() < 1e-7,
                        "{filter:?} at {frequency}"
                    );
                }
            }
        }
    }
}

#[test]
fn settled_envelope_bounds_small_persistent_input_after_large_history() {
    for filter in [
        peak(40.0, 0.1, -24.0),
        q_shelf(false, 105.0, 2.0, 12.0),
        q_shelf(true, 8000.0, 1.2, -12.0),
    ] {
        let design = design(filter, 48000, ChannelLayout::Stereo);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        for frames in [0, 1, 257, 16384] {
            let mut kernel = design.instantiate(tail).unwrap();
            let mut output = [0.0; 128];
            for _ in 0..32 {
                kernel.process(&[1.0; 128], &mut output).unwrap();
            }
            let epsilon = 1e-8;
            let bound = design.settled_output_bound(1.0, epsilon, frames).unwrap();
            for _ in 0..frames {
                kernel
                    .process(&[epsilon, -epsilon], &mut output[..2])
                    .unwrap();
            }
            for _ in 0..1024 {
                kernel
                    .process(&[epsilon, -epsilon], &mut output[..2])
                    .unwrap();
                assert!(output[..2].iter().all(|x| x.abs() <= bound));
            }
        }
        assert!(
            design.settled_output_bound(1.0, 1e-8, 16384).unwrap()
                < design.settled_output_bound(1.0, 1e-8, 0).unwrap()
        );
        for (past, future) in [(f64::NAN, 0.0), (1.0, -1.0), (1.0, f64::INFINITY)] {
            assert!(design.settled_output_bound(past, future, 1).is_err());
        }
    }
}

#[test]
fn peaking_impulse_matches_literal_cookbook_fixture_and_reset() {
    let design = design(peak(1000.0, 1.0, 6.0), 48000, ChannelLayout::Mono);
    let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
    let mut kernel = design.instantiate(tail).unwrap();
    let input = [1.0, 0.0, 0.0, 0.0];
    let mut output = [0.0; 4];
    kernel.process(&input, &mut output).unwrap();
    // Independent direct-form recurrence evaluation of the W3C cookbook formula.
    let expected = [
        1.043_953_086_990_335,
        0.083_305_196_653_770_12,
        0.073_866_031_717_686_8,
        0.064_052_524_581_441_03,
    ];
    for (actual, expected) in output.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 2e-14, "{actual} != {expected}");
    }
    kernel.reset();
    kernel.process(&input, &mut output).unwrap();
    assert!((output[0] - expected[0]).abs() < 2e-14);
}

#[test]
fn boundary_filters_have_finite_peak_and_explicit_tail_admission() {
    for rate in [44100, 48000, 96000, 192000] {
        for frequency in [10.0, 1000.0, 0.45 * f64::from(rate)] {
            for gain in [-24.0, 0.0, 24.0] {
                let filters = [
                    peak(frequency, 0.1, gain),
                    peak(frequency, 20.0, gain),
                    PeqFilter::LowShelf {
                        frequency_hz: scalar(frequency),
                        gain_db: scalar(gain),
                    },
                    PeqFilter::HighShelf {
                        frequency_hz: scalar(frequency),
                        gain_db: scalar(gain),
                    },
                    q_shelf(false, frequency, 0.1, gain),
                    q_shelf(false, frequency, 20.0, gain),
                    q_shelf(true, frequency, 0.1, gain),
                    q_shelf(true, frequency, 20.0, gain),
                ];
                for filter in filters {
                    let design = design(filter, rate, ChannelLayout::Mono);
                    assert!(design.full_l1_bound().is_finite());
                    assert!(design.peak_bound(1.0).unwrap().is_finite());
                    match design.tail(1.0, 1.0, 1e-12) {
                        Ok(tail) => {
                            assert!(tail.frames() <= 1_048_576);
                            assert!(tail.discarded_output_bound() <= 1e-12);
                        }
                        Err(PeqError::TailBudget) => {}
                        other => panic!("unexpected tail result: {other:?}"),
                    }
                }
            }
        }
    }
}

#[test]
fn finite_finish_emits_exact_horizon_and_cancel_clears_stereo_state() {
    let design = design(peak(800.0, 2.0, 12.0), 48000, ChannelLayout::Stereo);
    let tail = design.tail(1.0, 3.0, 1e-12).unwrap();
    let mut kernel = design.instantiate(tail).unwrap();
    let mut output = [0.0; 18];
    kernel.process(&[1.0, 0.0], &mut output[..2]).unwrap();
    kernel.begin_drain().unwrap();
    let mut frames = 0;
    loop {
        let written = kernel.drain(&mut output).unwrap();
        assert!(
            output[..written * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .all(|frame| frame[1] == 0.0)
        );
        frames += written;
        if written == 0 {
            break;
        }
    }
    assert_eq!(frames, tail.frames());
    assert_eq!(
        kernel.process(&[0.0, 0.0], &mut output[..2]),
        Err(PeqError::Lifecycle)
    );
    kernel.reset();
    kernel.process(&[0.0, 1.0], &mut output[..2]).unwrap();
    assert_eq!(output[0], 0.0);
    kernel.cancel();
    assert_eq!(kernel.drain(&mut output), Err(PeqError::Lifecycle));
    kernel.reset();
    kernel.process(&[0.0; 18], &mut output).unwrap();
    assert_eq!(output, [0.0; 18]);
}

#[test]
fn invalid_spans_nonfinite_and_peak_violations_fail_closed() {
    let design = design(peak(1000.0, 1.0, 6.0), 48000, ChannelLayout::Stereo);
    let tail = design.tail(0.5, 1.0, 1e-12).unwrap();
    let mut kernel = design.instantiate(tail).unwrap();
    let mut output = [9.0; 4];
    assert_eq!(kernel.process(&[0.0; 3], &mut output), Err(PeqError::Span));
    assert_eq!(output, [0.0; 4]);
    kernel.reset();
    assert_eq!(
        kernel.process(&[0.0, f64::NAN, 0.0, 0.0], &mut output),
        Err(PeqError::NonFinite)
    );
    assert_eq!(output, [0.0; 4]);
    kernel.reset();
    assert_eq!(
        kernel.process(&[0.6, 0.0, 0.0, 0.0], &mut output),
        Err(PeqError::InputPeak)
    );
    assert_eq!(output, [0.0; 4]);
    assert_eq!(design.tail(1.0, 1.0, 0.0), Err(PeqError::Parameters));
    assert_eq!(design.peak_bound(f64::INFINITY), Err(PeqError::Parameters));
}

fn analog_magnitude(filter: &PeqFilter, rate: u32, frequency: f64) -> f64 {
    let (center, gain) = match filter {
        PeqFilter::Peaking {
            frequency_hz,
            gain_db,
            ..
        }
        | PeqFilter::LowShelf {
            frequency_hz,
            gain_db,
        }
        | PeqFilter::HighShelf {
            frequency_hz,
            gain_db,
        }
        | PeqFilter::LowShelfQ {
            frequency_hz,
            gain_db,
            ..
        }
        | PeqFilter::HighShelfQ {
            frequency_hz,
            gain_db,
            ..
        } => (frequency_hz.get(), gain_db.get()),
    };
    let a = 10_f64.powf(gain / 40.0);
    let t = (std::f64::consts::PI * frequency / f64::from(rate)).tan()
        / (std::f64::consts::PI * center / f64::from(rate)).tan();
    let (nr, ni, dr, di) = match filter {
        PeqFilter::Peaking { q, .. } => {
            (1.0 - t * t, a * t / q.get(), 1.0 - t * t, t / (a * q.get()))
        }
        PeqFilter::LowShelf { .. } => (
            a * (a - t * t),
            a * (2.0 * a).sqrt() * t,
            1.0 - a * t * t,
            (2.0 * a).sqrt() * t,
        ),
        PeqFilter::HighShelf { .. } => (
            a * (1.0 - a * t * t),
            a * (2.0 * a).sqrt() * t,
            a - t * t,
            (2.0 * a).sqrt() * t,
        ),
        PeqFilter::LowShelfQ { q, .. } => (
            a * (a - t * t),
            a * a.sqrt() * t / q.get(),
            1.0 - a * t * t,
            a.sqrt() * t / q.get(),
        ),
        PeqFilter::HighShelfQ { q, .. } => (
            a * (1.0 - a * t * t),
            a * a.sqrt() * t / q.get(),
            a - t * t,
            a.sqrt() * t / q.get(),
        ),
    };
    nr.hypot(ni) / dr.hypot(di)
}
fn reference(input: &[f64], coefficients: [f64; 5]) -> Vec<f64> {
    let [b0, b1, b2, a1, a2] = coefficients;
    let mut x1 = 0.0;
    let mut x2 = 0.0;
    let mut y1 = 0.0;
    let mut y2 = 0.0;
    input
        .iter()
        .map(|&x| {
            let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = x;
            y2 = y1;
            y1 = y;
            y
        })
        .collect()
}

#[test]
fn measured_response_matches_independent_analog_oracle_and_writes_numeric_receipts() {
    use sha2::{Digest, Sha256};
    let rate = 48000;
    let mut csv = String::from(
        "filter,rate_hz,frequency_hz,oracle_db,measured_db,error_db,tail_frames,discarded_bound\n",
    );
    let mut rows = Vec::new();
    let fixtures = [
        peak(40.0, 0.1, -24.0),
        peak(40.0, 20.0, 24.0),
        peak(1000.0, 1.0, 6.0),
        peak(21600.0, 20.0, -24.0),
        PeqFilter::LowShelf {
            frequency_hz: scalar(40.0),
            gain_db: scalar(24.0),
        },
        PeqFilter::HighShelf {
            frequency_hz: scalar(21600.0),
            gain_db: scalar(-24.0),
        },
        q_shelf(false, 105.0, 0.7, 6.0),
        q_shelf(true, 8000.0, 1.2, -3.0),
    ];
    let mut maximum_error = 0_f64;
    for (index, filter) in fixtures.into_iter().enumerate() {
        let design = design(filter, rate, ChannelLayout::Mono);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        let mut kernel = design.instantiate(tail).unwrap();
        let mut impulse = vec![0.0; tail.frames() + 1];
        let mut input = impulse.clone();
        input[0] = 1.0;
        kernel.process(&input, &mut impulse).unwrap();
        for frequency in [0.0, 10.0, 40.0, 100.0, 1000.0, 10000.0, 21600.0, 23999.0] {
            let w = 2.0 * std::f64::consts::PI * frequency / f64::from(rate);
            let mut re = 0.0;
            let mut im = 0.0;
            for (n, &h) in impulse.iter().enumerate() {
                let angle = w * n as f64;
                re += h * angle.cos();
                im -= h * angle.sin();
            }
            let measured_db = 20.0 * re.hypot(im).log10();
            let oracle_db = 20.0 * analog_magnitude(&filter, rate, frequency).log10();
            let error = (measured_db - oracle_db).abs();
            maximum_error = maximum_error.max(error);
            assert!(
                error <= 0.01,
                "filter={filter:?}, frequency={frequency}, error={error}"
            );
            csv.push_str(&format!("{index},{rate},{frequency:.17},{oracle_db:.17},{measured_db:.17},{error:.17},{},{:.17e}\n",tail.frames(),tail.discarded_output_bound()));
            rows.push(serde_json::json!({"filter":filter,"rate_hz":rate,"frequency_hz":frequency,"coefficients":design.coefficients(),"oracle_db":oracle_db,"measured_db":measured_db,"error_db":error,"tail_frames":tail.frames(),"discarded_bound":tail.discarded_output_bound(),"full_l1_bound":design.full_l1_bound(),"runtime_peak_bound":design.peak_bound(1.0).unwrap()}));
        }
    }
    if let Some(root) = evidence::directory("peq") {
        evidence::write_file(&root, "peq-response.csv", csv.as_bytes());
        let source_hash = Sha256::digest(include_bytes!("../src/peq.rs"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let test_hash = Sha256::digest(include_bytes!("peq.rs"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let report = serde_json::json!({"algorithm":peq::ALGORITHM_VERSION,"source":"generated unit impulse; no device I/O","coefficient_reference":"https://www.w3.org/TR/audio-eq-cookbook/","oracle":"independent bilinear-transform analog complex transfer functions; fixed S=1 and explicit Q shelves","source_sha256":source_hash,"test_sha256":test_hash,"maximum_response_error_db":maximum_error,"cpu_qualification":"unknown; no reference-host timing","results":rows});
        evidence::write_file(
            &root,
            "numeric-test-results.json",
            &serde_json::to_vec_pretty(&report).unwrap(),
        );
    }
}

#[test]
fn adversarial_sign_input_challenges_peak_bound_beyond_frequency_gain() {
    for filter in [
        peak(1000.0, 1.0, 24.0),
        q_shelf(false, 105.0, 3.0, 12.0),
        q_shelf(true, 8000.0, 2.0, 12.0),
    ] {
        let design = design(filter, 48000, ChannelLayout::Mono);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        let mut impulse = vec![0.0; 8192];
        impulse[0] = 1.0;
        let impulse = reference(&impulse, design.coefficients());
        let input: Vec<_> = impulse
            .iter()
            .rev()
            .map(|h| if *h >= 0.0 { 1.0 } else { -1.0 })
            .collect();
        let mut output = vec![0.0; input.len()];
        let mut kernel = design.instantiate(tail).unwrap();
        kernel.process(&input, &mut output).unwrap();
        let achieved = output.last().unwrap().abs();
        assert!(
            achieved
                > 10_f64.powf(
                    if matches!(filter, PeqFilter::Peaking { .. }) {
                        24.0
                    } else {
                        12.0
                    } / 20.0
                )
        );
        assert!(achieved <= design.full_l1_bound());
        assert!(
            output
                .iter()
                .all(|x| x.abs() <= design.peak_bound(1.0).unwrap())
        );
    }
}

#[test]
fn terminal_residual_encloses_longer_independent_zero_input_reference() {
    for filter in [
        peak(40.0, 20.0, 24.0),
        peak(21600.0, 0.1, -24.0),
        PeqFilter::LowShelf {
            frequency_hz: scalar(30.0),
            gain_db: scalar(24.0),
        },
        q_shelf(false, 105.0, 3.0, 12.0),
        q_shelf(true, 8000.0, 1.2, -12.0),
    ] {
        let design = design(filter, 48000, ChannelLayout::Mono);
        let tail = design.tail(1.0, 8.0, 1e-12).unwrap();
        let mut input = vec![0.0; 4096 + tail.frames() + 16384];
        let mut rng = 0x812346781234_u64;
        for x in &mut input[..4096] {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            *x = (rng as i64 as f64) / (i64::MAX as f64);
        }
        let output = reference(&input, design.coefficients());
        let remainder = output[4096 + tail.frames()..]
            .iter()
            .fold(0_f64, |m, x| m.max(8.0 * x.abs()));
        assert!(
            remainder <= tail.discarded_output_bound(),
            "{remainder} > {}",
            tail.discarded_output_bound()
        );
    }
}

#[test]
fn bounded_noise_alternating_dc_and_long_silence_stay_finite() {
    for filter in [
        peak(10.0, 20.0, -24.0),
        peak(21600.0, 0.1, -24.0),
        PeqFilter::HighShelf {
            frequency_hz: scalar(10.0),
            gain_db: scalar(24.0),
        },
    ] {
        let design = design(filter, 48000, ChannelLayout::Stereo);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        let mut kernel = design.instantiate(tail).unwrap();
        let mut input = [0.0; 512];
        let mut output = [0.0; 512];
        let mut rng = 18_u64;
        for block in 0..1024 {
            for (i, x) in input.iter_mut().enumerate() {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                *x = match block {
                    0..256 => 1.0,
                    256..512 => {
                        if (i / 2) % 2 == 0 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    512..768 => rng as i64 as f64 / i64::MAX as f64,
                    _ => 0.0,
                };
            }
            kernel.process(&input, &mut output).unwrap();
            assert!(
                output
                    .iter()
                    .all(|x| x.is_finite() && x.abs() <= design.peak_bound(1.0).unwrap())
            );
        }
    }
}

#[test]
fn a_tail_proof_cannot_be_reused_for_another_filter_or_layout() {
    let a = design(peak(1000.0, 1.0, 6.0), 48000, ChannelLayout::Mono);
    let b = design(peak(1000.0, 1.0, -6.0), 48000, ChannelLayout::Mono);
    let stereo = design(peak(1000.0, 1.0, 6.0), 48000, ChannelLayout::Stereo);
    let tail = a.tail(1.0, 1.0, 1e-12).unwrap();
    assert!(matches!(b.instantiate(tail), Err(PeqError::Parameters)));
    assert!(matches!(
        stereo.instantiate(tail),
        Err(PeqError::Parameters)
    ));
}
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

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
// SAFETY: Each operation delegates to System with the caller's original pointer and layout.
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
fn prepared_process_drain_reset_cancel_and_failure_have_no_heap_activity() {
    let (allocation, counts) = tracked(|| std::hint::black_box(vec![7_u8; 128]));
    assert!(counts.allocations > 0);
    let (_, counts) = tracked(|| drop(allocation));
    assert!(counts.frees > 0);
    for filter in [
        peak(1000.0, 1.0, 6.0),
        q_shelf(false, 105.0, 0.7, 6.0),
        q_shelf(true, 8000.0, 1.2, -3.0),
    ] {
        let (design, counts) = tracked(|| design(filter, 48000, ChannelLayout::Stereo));
        assert_eq!(counts, AllocationCounts::default());
        let resources = design.resources();
        assert!(resources.resident_bytes >= size_of::<peq::PreparedPeq>());
        assert!(resources.preparation_scratch_bytes < 65536);
        assert_eq!(resources.state_scalars, 4);
        assert_eq!(resources.filter_operations_per_frame, 18);
        assert_eq!(resources.proof_matrix_products, 2);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        let mut kernel = design.instantiate(tail).unwrap();
        let mut output = [0.0; 128];
        let (_, counts) = tracked(|| {
            kernel.process(&[0.25; 128], &mut output).unwrap();
            kernel.begin_drain().unwrap();
            while kernel.drain(&mut output).unwrap() > 0 {}
            kernel.reset();
            kernel.process(&[0.5; 128], &mut output).unwrap();
            kernel.cancel();
            assert_eq!(
                kernel.process(&[0.0; 128], &mut output),
                Err(PeqError::Lifecycle)
            );
            kernel.reset();
            assert_eq!(
                kernel.process(&[f64::NAN; 128], &mut output),
                Err(PeqError::NonFinite)
            );
            kernel.reset();
            kernel.begin_drain().unwrap();
            assert_eq!(kernel.drain(&mut []), Err(PeqError::Span));
        });
        assert_eq!(counts, AllocationCounts::default());
    }
}

#[test]
fn slow_poles_explicitly_reject_the_fixed_terminal_budget() {
    let design = design(peak(10.0, 20.0, 24.0), 48000, ChannelLayout::Mono);
    assert_eq!(design.tail(1.0, 1.0, 1e-12), Err(PeqError::TailBudget));
}

#[test]
fn generated_sweep_matches_independent_direct_form_reference() {
    let design = design(peak(80.0, 8.0, 18.0), 48000, ChannelLayout::Mono);
    let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
    let mut phase = 0.0;
    let input: Vec<_> = (0..96000)
        .map(|n| {
            let frequency = 10.0 * 2200_f64.powf(n as f64 / 95999.0);
            phase += 2.0 * std::f64::consts::PI * frequency / 48000.0;
            phase.sin()
        })
        .collect();
    let expected = reference(&input, design.coefficients());
    let mut output = vec![0.0; input.len()];
    let mut kernel = design.instantiate(tail).unwrap();
    kernel.process(&input, &mut output).unwrap();
    let error = output
        .iter()
        .zip(expected)
        .map(|(a, b)| {
            let error = (a - b).abs();
            assert!(error.is_finite(), "non-finite sweep error");
            error
        })
        .fold(0_f64, f64::max);
    assert!(error < 1e-9, "{error}");
}

#[test]
fn overdamped_cut_retains_a_practical_time_domain_peak_bound() {
    let design = design(peak(40.0, 0.1, -24.0), 48000, ChannelLayout::Mono);
    assert!(design.full_l1_bound() < 4.0, "{}", design.full_l1_bound());
    assert!(design.peak_bound(1.0).unwrap() < 4.0);
}

#[test]
fn real_pole_residue_enclosures_cover_adversarial_bounded_inputs() {
    for filter in [
        peak(40.0, 0.1, -24.0),
        peak(21600.0, 0.1, -24.0),
        peak(500.0, 0.25, 18.0),
    ] {
        let design = design(filter, 48000, ChannelLayout::Mono);
        let tail = design.tail(1.0, 1.0, 1e-12).unwrap();
        let mut input = vec![0.0; 65536];
        input[0] = 1.0;
        let impulse = reference(&input, design.coefficients());
        assert!(impulse.iter().map(|x| x.abs()).sum::<f64>() <= design.full_l1_bound());
        for (x, h) in input.iter_mut().zip(impulse.iter().rev()) {
            *x = if *h >= 0.0 { 1.0 } else { -1.0 };
        }
        let mut output = vec![0.0; input.len()];
        let mut kernel = design.instantiate(tail).unwrap();
        kernel.process(&input, &mut output).unwrap();
        assert!(
            output
                .iter()
                .all(|x| x.abs() <= design.peak_bound(1.0).unwrap())
        );
    }
}
