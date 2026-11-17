// ABOUTME: Exercises deterministic PCM production and offline consumption.
// ABOUTME: Uses independent sample vectors instead of renderer-derived expectations.
use earph_core::{audio::*, ids::Epochs};
use earph_harness::fixture::*;
use earph_harness::{render::*, virtual_output::*};
mod support;

#[test]
fn fixture_loading_admits_raw_and_typed_buffers_together() {
    let temporary = support::TestDir::new();
    let path = temporary.0.join("fixtures");
    write_bundle(&path).unwrap();
    let fixture = path.join("signed32-stereo-48000-boundary.json");
    assert!(load_fixture_with_budget(&fixture, MemoryBudget::new(16000).unwrap()).is_err());
    let (_, block) = load_fixture_with_budget(&fixture, MemoryBudget::new(17000).unwrap()).unwrap();
    assert_eq!(block.integer_sample(0), Some(-2147483648));
    let bytes = earph_harness::read_bounded(&path.join("signed32-stereo-48000-boundary.pcm"), 8248)
        .unwrap();
    assert_eq!(bytes.len(), 8248);
    assert_eq!(bytes.capacity(), 8248);
}

#[test]
fn output_generation_cannot_move_backwards() {
    let fixture = spec(FixturePattern::Boundary, 3);
    let current = Epochs {
        session: 2,
        stream: 1,
    };
    let mut output = VirtualOutput::prepare(fixture.format, 3, current).unwrap();
    for old in [
        Epochs {
            session: 2,
            stream: 0,
        },
        Epochs {
            session: 1,
            stream: 99,
        },
        current,
    ] {
        assert_eq!(
            output.invalidate(old, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
    }
    let mut block = PcmBlock::prepare(fixture.format, 3, current, MemoryBudget::default()).unwrap();
    FixtureSource::new(fixture)
        .unwrap()
        .read_into(&mut block)
        .unwrap();
    assert_eq!(output.write(&block, 0).unwrap(), 3);
}

fn request(fixture: FixtureSpec, block_frames: usize) -> RenderRequest {
    RenderRequest {
        fixture,
        block_frames,
        budget: MemoryBudget::default(),
        epochs: Epochs {
            session: 1,
            stream: 1,
        },
        faults: FaultSchedule::default(),
    }
}

#[test]
fn virtual_render_preserves_every_integer_frame_across_block_sizes() {
    for fixture in qualification_specs() {
        let expected = collect(fixture.clone(), 1031);
        for size in [1, 64, 256, 1024] {
            let report = render(request(fixture.clone(), size)).unwrap();
            assert_eq!(
                report
                    .capture
                    .iter()
                    .copied()
                    .map(i64::from)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(report.output_frames, 1031);
            assert_eq!(report.requested_format, report.observed_format.unwrap());
        }
    }
}

#[test]
fn faults_at_precise_frames_are_repeatable_and_keep_partial_capture() {
    for at_frame in [0, 64, 1030] {
        for fault in [
            FaultKind::ShortWrite { max_frames: 3 },
            FaultKind::ZeroProgress,
            FaultKind::Underrun,
            FaultKind::BackendFailure,
            FaultKind::MalformedChunk,
            FaultKind::StaleEpoch,
        ] {
            let mut input = request(spec(FixturePattern::Boundary, 1031), 256);
            input.faults.faults.push(ScheduledFault { at_frame, fault });
            let a = render(input.clone());
            let b = render(input);
            match (a, b) {
                (Ok(a), Ok(b)) => {
                    assert!(matches!(fault, FaultKind::ShortWrite { .. }));
                    assert_eq!(a.capture.len(), 1031);
                    assert_eq!(a.capture, b.capture);
                    assert_eq!(a.events, b.events);
                    assert_eq!(
                        a.capture.iter().copied().map(i64::from).collect::<Vec<_>>(),
                        collect(spec(FixturePattern::Boundary, 1031), 64)
                    );
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a.report.output_frames, at_frame);
                    assert_eq!(a.report.capture.len(), at_frame as usize);
                    assert_eq!(a.error, b.error);
                    assert_eq!(a.report.events, b.report.events);
                    assert_eq!(a.report.payload_sha256, b.report.payload_sha256);
                    assert_eq!(
                        a.report
                            .capture
                            .iter()
                            .copied()
                            .map(i64::from)
                            .collect::<Vec<_>>(),
                        collect(spec(FixturePattern::Boundary, 1031), 64)[..at_frame as usize]
                    );
                }
                _ => panic!("non-deterministic render"),
            }
        }
    }
}

#[test]
fn preparation_rejects_invalid_schedules_and_resource_overflow() {
    let mut input = request(spec(FixturePattern::Silence, 10), 0);
    assert!(render(input.clone()).is_err());
    input.block_frames = 64;
    input.budget = MemoryBudget::new(16).unwrap();
    assert!(render(input.clone()).is_err());
    input.budget = MemoryBudget::default();
    input.faults.faults.push(ScheduledFault {
        at_frame: 10,
        fault: FaultKind::Underrun,
    });
    assert!(render(input.clone()).is_err());
    input.faults.faults[0] = ScheduledFault {
        at_frame: 0,
        fault: FaultKind::ShortWrite { max_frames: 0 },
    };
    assert!(render(input.clone()).is_err());
    input.faults.faults = vec![
        ScheduledFault {
            at_frame: 2,
            fault: FaultKind::Underrun,
        },
        ScheduledFault {
            at_frame: 1,
            fault: FaultKind::BackendFailure,
        },
    ];
    assert!(render(input).is_err());
}

#[test]
fn stale_blocks_after_seek_or_takeover_never_reach_virtual_capture() {
    let fixture = spec(FixturePattern::Boundary, 3);
    let mut block = PcmBlock::prepare(
        fixture.format,
        3,
        Epochs {
            session: 1,
            stream: 0,
        },
        MemoryBudget::default(),
    )
    .unwrap();
    FixtureSource::new(fixture.clone())
        .unwrap()
        .read_into(&mut block)
        .unwrap();
    let mut output = VirtualOutput::prepare(fixture.format, 9, block.header().epochs).unwrap();
    assert_eq!(output.write(&block, 0).unwrap(), 3);
    for epochs in [
        Epochs {
            session: 1,
            stream: 1,
        },
        Epochs {
            session: 2,
            stream: 0,
        },
    ] {
        output.invalidate(epochs, 0).unwrap();
        assert_eq!(
            output.write(&block, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        block.set_span(epochs, 0, 0, 3).unwrap();
        assert_eq!(output.write(&block, 0).unwrap(), 3);
    }
    assert_eq!(
        output.captured_samples(),
        &[-32768, -32767, -1, -32768, -32767, -1, -32768, -32767, -1]
    );
}

#[test]
fn virtual_clock_has_no_partition_rounding_drift() {
    let mut clock = VirtualClock::default();
    for _ in 0..44100 {
        clock.advance_frames(1, 44100).unwrap();
    }
    assert_eq!(clock.elapsed_ns(), 1000000000);
    assert!(clock.advance_frames(1, 48000).is_err());
    assert_eq!(clock.elapsed_ns(), 1000000000);
}

fn spec(pattern: FixturePattern, frames: u64) -> FixtureSpec {
    FixtureSpec {
        seed: 0,
        frames,
        format: PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap(),
        pattern,
    }
}
fn collect(spec: FixtureSpec, block_frames: usize) -> Vec<i64> {
    let mut source = FixtureSource::new(spec.clone()).unwrap();
    let mut block = PcmBlock::prepare(
        spec.format,
        block_frames,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .unwrap();
    let mut result = vec![];
    while source.read_into(&mut block).unwrap() > 0 {
        result.extend(
            (0..block
                .header()
                .format
                .scalar_count(block.header().frame_count)
                .unwrap())
                .map(|i| block.integer_sample(i).unwrap()),
        );
    }
    result
}

#[test]
fn fixture_patterns_match_independent_vectors() {
    assert_eq!(
        collect(spec(FixturePattern::Boundary, 9), 4),
        vec![-32768, -32767, -1, 0, 1, 32766, 32767, -32768, -32767]
    );
    assert_eq!(
        collect(spec(FixturePattern::SeededNoise, 3), 2),
        vec![19887, -6668, -15025]
    );
    assert_eq!(collect(spec(FixturePattern::Silence, 3), 2), vec![0, 0, 0]);
    let mut stereo = spec(FixturePattern::Impulse, 3);
    stereo.format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    assert_eq!(collect(stereo, 2), vec![8191, 0, 0, 8191, 0, 0]);
}

#[test]
fn generation_is_independent_of_block_partitioning() {
    for fixture in qualification_specs() {
        let one = collect(fixture.clone(), 1);
        for size in [64, 256, 1024] {
            assert_eq!(collect(fixture.clone(), size), one);
        }
        assert_eq!(one.len(), fixture.format.scalar_count(1031).unwrap());
    }
}

#[test]
fn source_rejects_invalid_dimensions_and_accepts_empty_eof() {
    assert!(collect(spec(FixturePattern::Silence, 0), 4).is_empty());
    let mut invalid = spec(FixturePattern::Silence, u64::MAX);
    assert!(FixtureSource::new(invalid.clone()).is_err());
    invalid.frames = 2;
    invalid.format =
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    assert!(FixtureSource::new(invalid).is_err());
    let mut source = FixtureSource::new(spec(FixturePattern::Boundary, 3)).unwrap();
    let other = PcmFormat::new(44100, ChannelLayout::Mono, PcmRepresentation::Signed16).unwrap();
    let mut block =
        PcmBlock::prepare(other, 4, Epochs::default(), MemoryBudget::default()).unwrap();
    assert!(source.read_into(&mut block).is_err());
}
