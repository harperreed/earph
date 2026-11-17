// ABOUTME: Exercises deterministic PCM production and offline consumption.
// ABOUTME: Uses independent sample vectors instead of renderer-derived expectations.
use earph_core::{audio::*, ids::Epochs};
use earph_harness::fixture::*;

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
