// ABOUTME: Exercises bounded streaming SRC through the pinned real library.
// ABOUTME: Checks joins, priming, padding, faults, drift, and requested allocations offline.
use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
use earph_dsp::resample::{self, PreparedSrc, SrcError};

fn prepare(chunk: usize, preset: SrcPreset) -> PreparedSrc {
    PreparedSrc::prepare(48000, 44100, ChannelLayout::Stereo, chunk, preset, 16 << 20).unwrap()
}
fn render(kernel: &mut PreparedSrc, input: &[f64], packet: usize, output_size: usize) -> Vec<f64> {
    let mut output = vec![0.0; output_size * 2];
    let mut rendered = Vec::new();
    let mut offset = 0;
    for _ in 0..100000 {
        let end = (offset + packet * 2).min(input.len());
        let step = kernel.process(&input[offset..end], &mut output).unwrap();
        offset += step.consumed_frames * 2;
        rendered.extend_from_slice(&output[..step.produced_frames * 2]);
        if offset == input.len() {
            break;
        }
    }
    assert_eq!(offset, input.len());
    kernel.begin_finish().unwrap();
    for _ in 0..100000 {
        let step = kernel.finish(&mut output).unwrap();
        rendered.extend_from_slice(&output[..step.produced_frames * 2]);
        if step.terminal {
            return rendered;
        }
    }
    panic!("bounded stream did not finish");
}
#[test]
fn empty_stream_has_no_guard_no_calls_and_no_output() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    assert!(render(&mut kernel, &[], 7, 3).is_empty());
    let counts = kernel.stats();
    assert_eq!(counts.real_input_frames, 0);
    assert_eq!(counts.library_calls, 0);
    assert_eq!(counts.guard_frames, 0);
}
#[test]
fn packet_joins_keep_one_guard_and_actual_counts() {
    let input: Vec<_> = (0..4097 * 2)
        .map(|i| ((i * 17 % 127) as f64 - 63.0) / 64.0)
        .collect();
    for preset in [SrcPreset::Responsive, SrcPreset::Quality] {
        let mut a = prepare(64, preset);
        let mut b = prepare(64, preset);
        let whole = render(&mut a, &input, 4097, 2048);
        let joined = render(&mut b, &input, 13, 3);
        assert_eq!(whole, joined);
        let counts = b.stats();
        assert_eq!(counts.real_input_frames, 4097);
        assert_eq!(counts.guard_frames, 1);
        assert_eq!(
            counts.library_calls,
            4098_usize.div_ceil(64) as u64 + b.metadata().drain_zero_calls as u64
        );
        assert_eq!(
            counts.nominal_output_frames,
            (4097_u64 * 44100).div_ceil(48000)
        );
        assert_eq!(counts.generated_frames, (joined.len() / 2) as u64);
        assert_eq!(counts.generated_frames, counts.emitted_frames);
        assert_eq!(counts.library_input_frames, counts.library_calls * 64);
        assert_eq!(
            counts.explicit_padding_input_frames,
            counts.library_input_frames - 4097
        );
    }
}
#[test]
fn priming_zero_output_is_progress_and_never_eof() {
    let mut kernel = prepare(1, SrcPreset::Quality);
    let mut output = [0.0; 2];
    let first = kernel.process(&[1.0, -1.0], &mut output).unwrap();
    assert_eq!(first.produced_frames, 0);
    assert!(!first.terminal);
    let signal = render(&mut kernel, &[0.5, -0.5], 1, 1);
    assert!(signal.iter().any(|sample| sample.abs() > 0.1));
    assert_eq!(kernel.stats().real_input_frames, 2);
}
#[test]
fn admission_rejects_unqualified_geometry_and_full_preparation_peak() {
    assert!(matches!(
        PreparedSrc::prepare(
            192000,
            48000,
            ChannelLayout::Mono,
            64,
            SrcPreset::Quality,
            16 << 20
        ),
        Err(SrcError::RatePair)
    ));
    assert!(matches!(
        PreparedSrc::prepare(
            48000,
            44100,
            ChannelLayout::Mono,
            0,
            SrcPreset::Quality,
            16 << 20
        ),
        Err(SrcError::Geometry)
    ));
    assert!(matches!(
        PreparedSrc::prepare(
            48000,
            44100,
            ChannelLayout::Stereo,
            1024,
            SrcPreset::Quality,
            4 << 20
        ),
        Err(SrcError::Budget)
    ));
}
#[test]
fn faults_are_terminal_and_never_publish_a_partial_output_span() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    let mut output = [7.0; 8];
    assert_eq!(
        kernel.process(&[1.0, 0.0, f64::NAN, 0.0], &mut output),
        Err(SrcError::NonFinite)
    );
    assert_eq!(output, [0.0; 8]);
    assert_eq!(
        kernel.process(&[0.0; 2], &mut output),
        Err(SrcError::Lifecycle)
    );
    assert_eq!(kernel.stats().real_input_frames, 0);
    kernel.reset();
    assert!(kernel.process(&[0.0; 2], &mut output).is_ok());
    kernel.cancel();
    assert_eq!(kernel.finish(&mut output), Err(SrcError::Lifecycle));
}
#[test]
fn offline_ratio_is_bounded_and_reset_restores_construction() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    assert_eq!(
        kernel.set_offline_ratio_ppm(1001.0, false),
        Err(SrcError::Ratio)
    );
    for (ppm, ramp) in [(1000.0, false), (-1000.0, true), (0.0, true)] {
        kernel.set_offline_ratio_ppm(ppm, ramp).unwrap();
        kernel.process(&[0.125; 128], &mut [0.0; 256]).unwrap();
    }
    assert_eq!(kernel.stats().ratio_commands, 3);
    kernel.reset();
    assert_eq!(kernel.stats().ratio_commands, 0);
    let a = render(&mut kernel, &[1.0; 258], 7, 31);
    let b = render(&mut prepare(64, SrcPreset::Responsive), &[1.0; 258], 7, 31);
    assert_eq!(a, b);
}

#[test]
fn every_admitted_rate_pair_preserves_last_impulses_and_channel_separation() {
    for (input_rate, output_rate) in [
        (44100, 44100),
        (48000, 48000),
        (88200, 88200),
        (96000, 96000),
        (176400, 176400),
        (192000, 192000),
        (44100, 48000),
        (48000, 44100),
        (88200, 96000),
        (96000, 88200),
        (176400, 192000),
        (192000, 176400),
        (44100, 88200),
        (88200, 44100),
        (88200, 176400),
        (176400, 88200),
        (48000, 96000),
        (96000, 48000),
        (96000, 192000),
        (192000, 96000),
    ] {
        // Scaled rate families share identical ratio kernels and effective cutoff lengths.
        // Test every physical pair, then every chunk at each distinct kernel ratio.
        let chunks: &[usize] = if [
            (48000, 44100),
            (48000, 48000),
            (44100, 48000),
            (96000, 48000),
            (48000, 96000),
        ]
        .contains(&(input_rate, output_rate))
        {
            &[1, 2, 64, 128, 256, 512, 1024]
        } else {
            &[128]
        };
        for preset in [SrcPreset::Responsive, SrcPreset::Quality] {
            for &chunk in chunks {
                let mut kernel = PreparedSrc::prepare(
                    input_rate,
                    output_rate,
                    ChannelLayout::Stereo,
                    chunk,
                    preset,
                    16 << 20,
                )
                .unwrap();
                for length in [1, 2, 63, 64, 65, 255, 256, 257, 1023, 1024, 1025] {
                    kernel.reset();
                    let mut signal = vec![0.0; length * 2];
                    signal[(length - 1) * 2] = 1.0;
                    let output = render(&mut kernel, &signal, 17, 37);
                    assert!(
                        output.iter().step_by(2).any(|sample| sample.abs() > 0.1),
                        "lost last impulse: {input_rate}/{output_rate}, {preset:?}, C{chunk}, N{length}"
                    );
                    assert!(
                        output
                            .iter()
                            .skip(1)
                            .step_by(2)
                            .all(|sample| *sample == 0.0)
                    );
                    assert!(output.iter().all(|sample| sample.abs()
                        <= kernel.metadata().peak_gain + kernel.metadata().underflow_allowance));
                    assert_eq!(kernel.stats().real_input_frames, length as u64);
                }
            }
        }
    }
}

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
#[derive(Clone, Copy, Default, Debug)]
struct AllocationCounts {
    allocs: usize,
    frees: usize,
    live: usize,
    peak: usize,
}
thread_local! {
    static METER: Cell<Option<AllocationCounts>>=const { Cell::new(None) };
}
struct AllocationMeter;
#[global_allocator]
static ALLOCATOR: AllocationMeter = AllocationMeter;
unsafe impl GlobalAlloc for AllocationMeter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let _ = METER.try_with(|meter| {
                if let Some(mut c) = meter.get() {
                    c.allocs += 1;
                    c.live += layout.size();
                    c.peak = c.peak.max(c.live);
                    meter.set(Some(c));
                }
            });
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            let _ = METER.try_with(|meter| {
                if let Some(mut c) = meter.get() {
                    c.allocs += 1;
                    c.live += layout.size();
                    c.peak = c.peak.max(c.live);
                    meter.set(Some(c));
                }
            });
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = METER.try_with(|meter| {
            if let Some(mut c) = meter.get() {
                c.frees += 1;
                c.live = c.live.saturating_sub(layout.size());
                meter.set(Some(c));
            }
        });
        unsafe { System.dealloc(ptr, layout) };
    }
}
fn measured<T>(f: impl FnOnce() -> T) -> (T, AllocationCounts) {
    METER.with(|meter| meter.set(Some(AllocationCounts::default())));
    let value = f();
    let counts = METER.with(|meter| meter.replace(None).unwrap());
    (value, counts)
}
#[test]
fn requested_preparation_bytes_fit_admitted_bound_and_worker_paths_do_not_allocate_or_free() {
    for preset in [SrcPreset::Responsive, SrcPreset::Quality] {
        let (mut kernel, preparation) = measured(|| prepare(1024, preset));
        assert!(
            preparation.peak as u64 <= kernel.metadata().resources.preparation_peak_bytes,
            "{preparation:?}"
        );
        assert!(
            preparation.live as u64 <= kernel.metadata().resources.resident_bytes,
            "{preparation:?}"
        );
        let mut output = [0.0; 128];
        let (_, runtime) = measured(|| {
            for i in 0..90 {
                kernel
                    .set_offline_ratio_ppm(if i % 2 == 0 { -1000.0 } else { 1000.0 }, i % 3 == 0)
                    .unwrap();
                kernel.process(&[0.125; 128], &mut output).unwrap();
            }
            kernel.begin_finish().unwrap();
            for _ in 0..10000 {
                if kernel.finish(&mut output).unwrap().terminal {
                    break;
                }
            }
            kernel.reset();
            kernel.cancel();
            kernel.process(&[f64::NAN; 2], &mut output).unwrap_err();
        });
        assert_eq!((runtime.allocs, runtime.frees), (0, 0), "{runtime:?}");
    }
}

fn fitted_tone(samples: &[f64], cycles_per_frame: f64) -> (f64, f64, f64) {
    let (mut cc, mut ss, mut cs, mut yc, mut ys) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for (i, &sample) in samples.iter().enumerate() {
        let (s, c) = (std::f64::consts::TAU * cycles_per_frame * i as f64).sin_cos();
        cc += c * c;
        ss += s * s;
        cs += c * s;
        yc += sample * c;
        ys += sample * s;
    }
    let determinant = cc * ss - cs * cs;
    assert!(determinant > 1.0);
    let c = (yc * ss - ys * cs) / determinant;
    let s = (ys * cc - yc * cs) / determinant;
    (c.hypot(s), c, s)
}
fn mono_render(kernel: &mut PreparedSrc, input: &[f64]) -> Vec<f64> {
    let mut output = vec![0.0; 2048];
    let mut rendered = Vec::new();
    let mut consumed = 0;
    for _ in 0..100000 {
        let previous = kernel.stats().library_calls;
        let step = kernel.process(&input[consumed..], &mut output).unwrap();
        assert!(kernel.stats().library_calls - previous <= 1);
        consumed += step.consumed_frames;
        rendered.extend_from_slice(&output[..step.produced_frames]);
        if consumed == input.len() {
            break;
        }
    }
    assert_eq!(consumed, input.len());
    kernel.begin_finish().unwrap();
    for _ in 0..100000 {
        let previous = kernel.stats().library_calls;
        let step = kernel.finish(&mut output).unwrap();
        assert!(kernel.stats().library_calls - previous <= 1);
        rendered.extend_from_slice(&output[..step.produced_frames]);
        if step.terminal {
            return rendered;
        }
    }
    panic!("mono stream did not finish");
}
fn tone_output(kernel: &mut PreparedSrc, fraction: f64, ppm: f64) -> Vec<f64> {
    kernel.reset();
    kernel.set_offline_ratio_ppm(ppm, false).unwrap();
    let input: Vec<_> = (0..16384)
        .map(|i| (std::f64::consts::TAU * fraction * i as f64).sin())
        .collect();
    let output = mono_render(kernel, &input);
    let ratio = kernel.metadata().output_rate as f64 / kernel.metadata().input_rate as f64
        * (1.0 + ppm / 1e6);
    // Retain a common steady interval, far from both finite signal boundaries.
    let begin = (4096.0 * ratio).ceil() as usize;
    let end = (12288.0 * ratio).floor() as usize;
    output[begin..end].to_vec()
}
fn numeric_counts(kernel: &PreparedSrc) -> serde_json::Value {
    let counts = kernel.stats();
    serde_json::json!({"real_input_frames":counts.real_input_frames,"nominal_output_frames":counts.nominal_output_frames,
        "library_calls":counts.library_calls,"library_input_frames":counts.library_input_frames,
        "generated_frames":counts.generated_frames,"emitted_frames":counts.emitted_frames,
        "guard_frames":counts.guard_frames,"explicit_padding_input_frames":counts.explicit_padding_input_frames,
        "ratio_commands":counts.ratio_commands,"ratio_history_hash":counts.ratio_history_hash})
}
fn drift_trace(kernel: &mut PreparedSrc) -> serde_json::Value {
    use sha2::{Digest, Sha256};
    kernel.reset();
    let h = kernel.metadata().channels.count();
    let input = vec![0.125; 128 * h];
    let mut output = vec![0.0; kernel.metadata().max_output_frames * h];
    let mut trace = Vec::new();
    let mut hash = Sha256::new();
    for index in 0..256 {
        let command = match index {
            0 => Some((0.0, false)),
            32 => Some((-1000.0, false)),
            64 => Some((1000.0, false)),
            96 => Some((-1000.0, true)),
            128 => Some((1000.0, true)),
            192 => Some((0.0, true)),
            _ => None,
        };
        if let Some((ppm, ramp)) = command {
            kernel.set_offline_ratio_ppm(ppm, ramp).unwrap();
        }
        let before = kernel.stats();
        let step = kernel.process(&input, &mut output).unwrap();
        assert!(kernel.stats().library_calls - before.library_calls <= 1);
        for sample in &output[..step.produced_frames * h] {
            hash.update(sample.to_bits().to_le_bytes());
        }
        if let Some((ppm, ramp)) = command {
            let ratio = kernel.ratio_state();
            trace.push(serde_json::json!({"requested_ppm":ppm,"ramp":ramp,"source_before":before.real_input_frames,
                "library_calls_before":before.library_calls,"generated_before":before.generated_frames,
                "accepted_this_step":step.consumed_frames,"produced_this_step":step.produced_frames,
                "actual_ratio_after":ratio.current_ratio,"target_ratio":ratio.target_ratio}));
        }
    }
    kernel.begin_finish().unwrap();
    loop {
        let step = kernel.finish(&mut output).unwrap();
        for sample in &output[..step.produced_frames * h] {
            hash.update(sample.to_bits().to_le_bytes());
        }
        if step.terminal {
            break;
        }
    }
    let hash = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    serde_json::json!({"commands":trace,"final_counts":numeric_counts(kernel),"output_sha256":hash})
}
#[test]
fn quality_passband_and_stopband_hold_in_real_streaming_conversion() {
    for (input_rate, output_rate) in [
        (48000, 44100),
        (44100, 48000),
        (48000, 96000),
        (96000, 48000),
    ] {
        let mut kernel = PreparedSrc::prepare(
            input_rate,
            output_rate,
            ChannelLayout::Mono,
            128,
            SrcPreset::Quality,
            16 << 20,
        )
        .unwrap();
        let ratio = output_rate as f64 / input_rate as f64;
        let pass = 0.45 * ratio.min(1.0);
        let output = tone_output(&mut kernel, pass, 0.0);
        let gain = fitted_tone(&output, pass / ratio).0;
        assert!(
            (20.0 * gain.log10()).abs() <= 0.01,
            "{input_rate}->{output_rate}: {gain}"
        );
        if ratio < 1.0 {
            let frequency = 0.5 * ratio * 1.0001;
            let output = tone_output(&mut kernel, frequency, 0.0);
            let alias = 1.0 - frequency / ratio;
            assert!(fitted_tone(&output, alias).0 <= 1e-5);
        }
    }
}

#[test]
#[ignore = "writes reproducible offline numeric receipts under ignored target"]
fn numeric_qualification_receipts() {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let hex = |bytes: &[u8]| {
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let directory = root.join("target/src-numeric");
    std::fs::create_dir_all(&directory).unwrap();
    let mut csv = String::from(
        "preset,input_rate,output_rate,ppm,kind,input_fraction,output_fraction,amplitude,db\n",
    );
    let mut cases = Vec::new();
    let mut drift_cases = Vec::new();
    let mut data_hash = Sha256::new();
    for preset in [SrcPreset::Responsive, SrcPreset::Quality] {
        for (input_rate, output_rate) in [
            (48000, 44100),
            (48000, 48000),
            (44100, 48000),
            (96000, 48000),
            (48000, 96000),
        ] {
            let (mut kernel, alloc) = measured(|| {
                PreparedSrc::prepare(
                    input_rate,
                    output_rate,
                    ChannelLayout::Mono,
                    128,
                    preset,
                    16 << 20,
                )
                .unwrap()
            });
            let metadata = kernel.metadata();
            let impulse = mono_render(&mut kernel, &[1.0]);
            let impulse_peak = impulse
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .unwrap()
                .0;
            let impulse_first = impulse.iter().position(|sample| *sample != 0.0);
            let impulse_last = impulse.iter().rposition(|sample| *sample != 0.0);
            kernel.reset();
            let sweep: Vec<_> = (0..8192)
                .map(|i| {
                    let t = i as f64 / 8192.0;
                    (std::f64::consts::TAU * (0.0001 * i as f64 + 0.2498 * t * i as f64)).sin()
                })
                .collect();
            let sweep_output = mono_render(&mut kernel, &sweep);
            let sweep_peak = sweep_output
                .iter()
                .map(|sample| sample.abs())
                .fold(0.0_f64, f64::max);
            assert!(sweep_peak <= metadata.peak_gain + metadata.underflow_allowance);
            let drift = drift_trace(&mut kernel);
            assert_eq!(drift, drift_trace(&mut kernel));
            drift_cases.push(serde_json::json!({"preset":format!("{preset:?}"),"input_rate":input_rate,"output_rate":output_rate,"trace":drift}));
            for ppm in [-1000.0, 0.0, 1000.0] {
                let ratio = output_rate as f64 / input_rate as f64 * (1.0 + ppm / 1e6);
                let mut maximum_pass_error = 0.0_f64;
                let mut maximum_stop = 0.0_f64;
                let mut transition_boundary_db = None;
                let stopband_edge =
                    if preset == SrcPreset::Responsive && input_rate == 2 * output_rate {
                        1.05
                    } else {
                        1.0
                    };
                for j in 1..=24 {
                    let frequency = 0.45 * ratio.min(1.0) * j as f64 / 24.0;
                    let output = tone_output(&mut kernel, frequency, ppm);
                    let amplitude = fitted_tone(&output, frequency / ratio).0;
                    let db = 20.0 * amplitude.log10();
                    maximum_pass_error = maximum_pass_error.max(db.abs());
                    writeln!(csv,"{preset:?},{input_rate},{output_rate},{ppm},pass,{frequency:.17e},{:.17e},{amplitude:.17e},{db:.12}",frequency/ratio).unwrap();
                    for value in output {
                        data_hash.update(value.to_bits().to_le_bytes());
                    }
                }
                if ratio < 1.0 {
                    if preset == SrcPreset::Responsive && input_rate == 2 * output_rate {
                        let boundary = 0.5 * ratio * 1.00001;
                        let boundary_output = tone_output(&mut kernel, boundary, ppm);
                        transition_boundary_db = Some(
                            20.0 * fitted_tone(&boundary_output, 1.0 - boundary / ratio)
                                .0
                                .log10(),
                        );
                    }
                    for j in 0..24 {
                        let start = 0.5 * ratio * stopband_edge;
                        let frequency = start + (0.5 - start) * (j as f64 + 0.001) / 24.001;
                        let output = tone_output(&mut kernel, frequency, ppm);
                        let cycles = (frequency / ratio).fract();
                        let alias = cycles.min(1.0 - cycles);
                        let amplitude = fitted_tone(&output, alias).0;
                        let db = 20.0 * amplitude.log10();
                        maximum_stop = maximum_stop.max(amplitude);
                        writeln!(csv,"{preset:?},{input_rate},{output_rate},{ppm},alias,{frequency:.17e},{alias:.17e},{amplitude:.17e},{db:.12}").unwrap();
                    }
                } else if ratio > 1.0 {
                    // Images fold around input-rate multiples. Fit and remove the fundamental
                    // before measuring its image, avoiding finite-record spectral leakage.
                    for j in 0..24 {
                        let frequency = (1.0 - ratio * 0.5).max(0.0001)
                            + (0.4999 - (1.0 - ratio * 0.5).max(0.0001)) * (j as f64 + 0.1) / 24.1;
                        let mut output = tone_output(&mut kernel, frequency, ppm);
                        let (_, c, s) = fitted_tone(&output, frequency / ratio);
                        for (i, value) in output.iter_mut().enumerate() {
                            let (sn, co) =
                                (std::f64::consts::TAU * frequency / ratio * i as f64).sin_cos();
                            *value -= c * co + s * sn;
                        }
                        let image = (1.0 - frequency) / ratio;
                        let amplitude = fitted_tone(&output, image).0;
                        let db = 20.0 * amplitude.log10();
                        maximum_stop = maximum_stop.max(amplitude);
                        writeln!(csv,"{preset:?},{input_rate},{output_rate},{ppm},image,{frequency:.17e},{image:.17e},{amplitude:.17e},{db:.12}").unwrap();
                    }
                }
                let pass_limit = if preset == SrcPreset::Responsive && input_rate == 2 * output_rate
                {
                    0.25
                } else {
                    0.01
                };
                std::fs::write(directory.join("response-data.partial.csv"), &csv).unwrap();
                assert!(
                    maximum_pass_error <= pass_limit,
                    "{preset:?} {input_rate}/{output_rate} {ppm}: {maximum_pass_error}"
                );
                assert!(
                    maximum_stop <= 1e-5,
                    "{preset:?} {input_rate}/{output_rate} {ppm}: {maximum_stop}"
                );
                cases.push(serde_json::json!({"preset":format!("{preset:?}"),"input_rate":input_rate,"output_rate":output_rate,"ppm":ppm,
                    "filter_length":metadata.filter_length,"phases":metadata.phases,"cutoff":metadata.cutoff,
                    "pre_scaling_cutoff":metadata.pre_scaling_cutoff,"upstream_max_relative_ratio":metadata.library_max_relative_ratio,
                    "requested_relative_ratio":1.0+ppm/1e6,"applied_ratio":kernel.ratio_state().current_ratio,
                    "impulse_peak_output_frame":impulse_peak,"impulse_first_nonzero_observed":impulse_first,"impulse_last_nonzero_observed":impulse_last,
                    "sweep_peak":sweep_peak,"transition_boundary_rejection_db":transition_boundary_db,
                    "declared_maximum_passband_error_db":pass_limit,"declared_stopband_start_lower_nyquist_fraction":stopband_edge,
                    "chunk_frames":metadata.chunk_frames,"library_delay_truncated":metadata.truncated_output_delay,"guard_time_seconds":{"numerator":metadata.guard_time_numerator,"denominator":input_rate},
                    "drain_zero_calls":metadata.drain_zero_calls,"maximum_passband_error_db":maximum_pass_error,"worst_stopband_db":if maximum_stop>0.0{Some(20.0*maximum_stop.log10())}else{None},
                    "peak_gain":metadata.peak_gain,"underflow_allowance":metadata.underflow_allowance,
                    "measured_requested_resident_bytes":alloc.live,"measured_requested_peak_bytes":alloc.peak,
                    "admitted_resident_bytes":metadata.resources.resident_bytes,"admitted_preparation_peak_bytes":metadata.resources.preparation_peak_bytes,
                    "actual_last_tone_counts":numeric_counts(&kernel)}));
            }
        }
    }
    let hash_file = |path: &str| hex(&Sha256::digest(std::fs::read(root.join(path)).unwrap()));
    let receipt = serde_json::json!({"algorithm":resample::ALGORITHM_VERSION,"upstream_commit":resample::UPSTREAM_COMMIT,
        "rubato_crate_sha256":"6b4da228e06632d025557f798114057cf27a4c6a71d40024c84d6db82fa5ef4a",
        "source_sha256":hash_file("crates/earph-dsp/src/resample.rs"),"tests_sha256":hash_file("crates/earph-dsp/tests/resample.rs"),"lock_sha256":hash_file("Cargo.lock"),
        "output_sha256":hex(&data_hash.finalize()),"response_csv_sha256":hex(&Sha256::digest(csv.as_bytes())),
        "platform":{"arch":std::env::consts::ARCH,"os":std::env::consts::OS},"arithmetic":"f64, IEEE nearest, gradual underflow; no fast-math","signal":"16384-frame owned unit sines, steady interior [4096,12288) source frames",
        "admission":{"rates":[44100,48000,88200,96000,176400,192000],"nominal_ratios":["1/2","147/160","1","160/147","2"],"chunks":[1,2,64,128,256,512,1024],"channels":[1,2],
            "unsupported_cases":[{"input_rate":192000,"output_rate":48000,"reason":"unqualified ratio1/4 rejects explicitly"},{"chunk_frames":0,"reason":"unsupported geometry rejects before construction"},{"preset":"Quality","budget_bytes":4194304,"reason":"full requested preparation peak exceeds budget"}]},
        "quality_cutoff_rule":"For nominal ratios<=1, derive cutoff from floor(1024*ratio), multiply by0.999 in f32, then let upstream scale bynominalratio; upsampling stays automatic",
        "every_phase_peak_rule":"Upward-rounded maxstoredrowL1 times1.25 plus gamma(16L+128) times19/3maxrowL1, coefficient-underflow rounding slack and explicit underflow floor",
        "passband":"24 sampled tones through 0.9 times actual lower Nyquist","stopband":"24 sampled tones; Responsive half-rate starts at1.05lowerNyquist, others atlowerNyquist; fundamental removed before image fitting",
        "limitations":["Finite sampled envelope, not a continuous-frequency proof","CPU timing belongs to the parent receipt","No hardware or physical quality claims","RSS and allocator-private overhead are not requested bytes","Unity nominal + zero ppm has no representable alias/image region; null stopband","Only mono/stereo and declared chunks/rates are admitted"],
        "rejected_candidates":[{"filter_length":128,"reason":"approximately 7.67 dB loss through 0.9 lower Nyquist in initial public-table probe"},
            {"filter_length":256,"ratio":0.5,"rejected_target_db":0.01,"actual_maximum_error_db":0.22569342989413876,"reason":"Responsive retains its declared0.25dB passband and1.05lowerNyquist stopband"},
            {"filter_length":1024,"reason":"Automatic cutoff missed100dB near lowerNyquist for downsampling and minimum drift; use ratio-aware explicit cutoff"}],"drift_cases":drift_cases,"cases":cases});
    std::fs::write(directory.join("response-data.csv"), csv).unwrap();
    std::fs::write(
        directory.join("resampler-benchmarks.json"),
        serde_json::to_vec_pretty(&receipt).unwrap(),
    )
    .unwrap();
}

#[test]
fn output_backpressure_and_finish_bound_track_private_pending_frames() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    let mut accepted = 0;
    for _ in 0..4 {
        let before = kernel.stats().library_calls;
        let step = kernel.process(&[0.25; 128], &mut []).unwrap();
        assert!(kernel.stats().library_calls - before <= 1);
        accepted += step.consumed_frames;
        if step.need_output {
            break;
        }
    }
    let queued = kernel.status().queued_output_frames;
    assert!(queued > 0);
    let counts = kernel.stats();
    let blocked = kernel.process(&[1.0; 128], &mut []).unwrap();
    assert_eq!(blocked.consumed_frames, 0);
    assert!(blocked.need_output);
    assert_eq!(kernel.stats(), counts);
    kernel.begin_finish().unwrap();
    let bound = kernel.status().remaining_finish_output_bound;
    let mut output = [0.0; 14];
    let mut produced = 0;
    for _ in 0..10000 {
        let before = kernel.stats().library_calls;
        let step = kernel.finish(&mut output).unwrap();
        assert!(kernel.stats().library_calls - before <= 1);
        produced += step.produced_frames as u64;
        if step.terminal {
            break;
        }
    }
    assert!(kernel.status().terminal);
    assert!(produced <= bound);
    assert_eq!(kernel.stats().real_input_frames, accepted as u64);
}
#[test]
fn finite_extreme_input_rejects_internal_overflow_before_mutation() {
    let mut kernel = prepare(64, SrcPreset::Quality);
    let mut output = [3.0; 8];
    assert_eq!(
        kernel.process(&[f64::MAX, 0.0], &mut output),
        Err(SrcError::InputPeak)
    );
    assert_eq!(kernel.stats().real_input_frames, 0);
    assert_eq!(output, [0.0; 8]);
    assert!(kernel.status().terminal);
}

#[test]
fn failure_discards_queued_output_and_reports_no_remaining_work() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    for _ in 0..4 {
        if kernel.process(&[0.25; 128], &mut []).unwrap().need_output {
            break;
        }
    }
    assert!(kernel.status().queued_output_frames > 0);
    assert_eq!(kernel.process(&[], &mut [1.0; 3]), Err(SrcError::Geometry));
    assert_eq!(kernel.status().queued_output_frames, 0);
    assert_eq!(kernel.status().remaining_finish_output_bound, 0);
    assert!(kernel.status().terminal);
}
#[test]
fn offline_ratio_reports_requested_target_and_actual_historical_phase_rate() {
    let mut kernel = prepare(64, SrcPreset::Responsive);
    let nominal = 44100.0 / 48000.0;
    kernel.set_offline_ratio_ppm(-1000.0, true).unwrap();
    let before = kernel.ratio_state();
    assert_eq!(before.current_ratio, nominal);
    assert_eq!(before.target_ratio, nominal * 0.999);
    assert_eq!(before.requested_ppm, -1000.0);
    kernel.process(&[0.0; 128], &mut [0.0; 256]).unwrap();
    assert_eq!(kernel.ratio_state().current_ratio, nominal * 0.999);
    kernel.reset();
    assert_eq!(kernel.ratio_state().current_ratio, nominal);
    assert_eq!(kernel.ratio_state().target_ratio, nominal);
    assert_eq!(kernel.ratio_state().requested_ppm, 0.0);
}

#[test]
fn stereo_combined_path_matches_direct_mono_on_sweeps_and_drift() {
    for preset in [SrcPreset::Responsive, SrcPreset::Quality] {
        for (input_rate, output_rate) in [
            (48000, 44100),
            (48000, 48000),
            (44100, 48000),
            (96000, 48000),
            (48000, 96000),
        ] {
            let mut mono = PreparedSrc::prepare(
                input_rate,
                output_rate,
                ChannelLayout::Mono,
                128,
                preset,
                16 << 20,
            )
            .unwrap();
            let mut stereo = PreparedSrc::prepare(
                input_rate,
                output_rate,
                ChannelLayout::Stereo,
                128,
                preset,
                16 << 20,
            )
            .unwrap();
            for ppm in [-1000.0, 1000.0] {
                mono.reset();
                stereo.reset();
                mono.set_offline_ratio_ppm(ppm, false).unwrap();
                stereo.set_offline_ratio_ppm(ppm, false).unwrap();
                let signal: Vec<_> = (0..4096)
                    .map(|i| (0.0003 * std::f64::consts::PI * (i * i) as f64).sin())
                    .collect();
                let stereo_signal: Vec<_> = signal
                    .iter()
                    .flat_map(|sample| [*sample, -*sample])
                    .collect();
                let direct = mono_render(&mut mono, &signal);
                let combined = render(&mut stereo, &stereo_signal, 73, 257);
                assert_eq!(direct.len() * 2, combined.len());
                for (a, b) in direct.iter().zip(combined.as_chunks::<2>().0) {
                    assert!(
                        (a - b[0]).abs() < 1e-12,
                        "{preset:?} {input_rate}/{output_rate} {ppm}"
                    );
                    assert_eq!(b[0], -b[1]);
                }
            }
        }
    }
}
