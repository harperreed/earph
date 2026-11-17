// ABOUTME: Generates bounded integer fixtures with stable seeds and manifests.
// ABOUTME: Keeps expected sample patterns independent of the offline renderer.
use crate::{read_bounded, sha256};
use earph_core::{EngineError, ErrorCode, audio::*, ids::Epochs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    path::Path,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixturePattern {
    Silence,
    Impulse,
    Boundary,
    SeededNoise,
}
impl FixturePattern {
    fn name(self) -> &'static str {
        match self {
            Self::Silence => "silence",
            Self::Impulse => "impulse",
            Self::Boundary => "boundary",
            Self::SeededNoise => "seeded_noise",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureSpec {
    pub seed: u64,
    pub frames: u64,
    pub format: PcmFormat,
    pub pattern: FixturePattern,
}

pub struct FixtureSource {
    spec: FixtureSpec,
    position: u64,
    sequence: u64,
    random_state: u64,
}
impl FixtureSource {
    pub fn new(spec: FixtureSpec) -> Result<Self, EngineError> {
        spec.format.representation().integer_bounds()?;
        MemoryBudget::default().admit(spec.format.storage_bytes(spec.frames)?)?;
        Ok(Self {
            random_state: spec.seed,
            spec,
            position: 0,
            sequence: 0,
        })
    }
    pub fn read_into(&mut self, destination: &mut PcmBlock) -> Result<usize, EngineError> {
        if destination.header().format != self.spec.format {
            return Err(EngineError::new(
                ErrorCode::FormatUnsupported,
                "fixture and buffer formats differ",
            ));
        }
        let frames = (self.spec.frames - self.position).min(destination.capacity_frames() as u64);
        if frames == 0 {
            return Ok(0);
        }
        let next_sequence = self.sequence.checked_add(1).ok_or(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "fixture sequence exhausted",
        ))?;
        destination.set_span(
            destination.header().epochs,
            self.sequence,
            self.position,
            frames,
        )?;
        let channels = self.spec.format.channels().count();
        let (min, max) = self.spec.format.representation().integer_bounds()?;
        for index in 0..self.spec.format.scalar_count(frames)? {
            let frame = self.position + index as u64 / channels as u64;
            let channel = index % channels;
            let value = match self.spec.pattern {
                FixturePattern::Silence => 0,
                FixturePattern::Impulse => {
                    if frame == channel as u64 {
                        max / 4
                    } else {
                        0
                    }
                }
                FixturePattern::Boundary => [min, min + 1, -1, 0, 1, max - 1, max]
                    [((frame * channels as u64 + channel as u64) % 7) as usize],
                FixturePattern::SeededNoise => {
                    let noise = splitmix64(&mut self.random_state);
                    min + (noise & (max - min) as u64) as i64
                }
            };
            destination.set_integer_sample(index, value)?;
        }
        self.position += frames;
        self.sequence = next_sequence;
        Ok(frames as usize)
    }
}

// SplitMix64 uses wrapping unsigned arithmetic; it is a fixture PRNG, not cryptography.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

pub fn qualification_specs() -> Vec<FixtureSpec> {
    let mut specs = Vec::new();
    for representation in [
        PcmRepresentation::Signed16,
        PcmRepresentation::Signed24,
        PcmRepresentation::Signed32,
    ] {
        for channels in [ChannelLayout::Mono, ChannelLayout::Stereo] {
            for rate in [44100, 48000, 96000] {
                for pattern in [
                    FixturePattern::Silence,
                    FixturePattern::Impulse,
                    FixturePattern::Boundary,
                    FixturePattern::SeededNoise,
                ] {
                    specs.push(FixtureSpec {
                        seed: 2389,
                        frames: 1031,
                        format: PcmFormat::new(rate, channels, representation)
                            .expect("positive fixed rate"),
                        pattern,
                    });
                }
            }
        }
    }
    specs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureFile {
    pub schema_version: u32,
    pub spec: FixtureSpec,
    pub payload_file: String,
    pub payload_bytes: usize,
    pub payload_sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureBundle {
    pub schema_version: u32,
    pub fixtures: Vec<FixtureFile>,
}

pub fn load_fixture(path: &Path) -> Result<(FixtureFile, PcmBlock), String> {
    load_fixture_with_budget(path, MemoryBudget::default())
}

pub fn load_fixture_with_budget(
    path: &Path,
    budget: MemoryBudget,
) -> Result<(FixtureFile, PcmBlock), String> {
    let manifest: FixtureFile = serde_json::from_slice(&read_bounded(path, 64 * 1024)?)
        .map_err(|_| "invalid fixture manifest")?;
    if manifest.schema_version != 1 {
        return Err("unsupported fixture schema".into());
    }
    FixtureSource::new(manifest.spec.clone()).map_err(|e| e.to_string())?;
    let expected = manifest
        .spec
        .format
        .payload_bytes(manifest.spec.frames)
        .map_err(|e| e.to_string())?;
    if expected != manifest.payload_bytes {
        return Err("fixture size differs from declared format".into());
    }
    let storage = manifest
        .spec
        .format
        .storage_bytes(manifest.spec.frames)
        .map_err(|e| e.to_string())?;
    let peak = expected
        .checked_add(storage)
        .ok_or("fixture buffer size overflow")?;
    budget.admit(peak).map_err(|e| e.to_string())?;
    let relative = Path::new(&manifest.payload_file);
    if relative.components().count() != 1
        || !matches!(
            relative.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("fixture payload must be a local filename".into());
    }
    let payload = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(relative);
    let metadata = fs::symlink_metadata(&payload).map_err(|_| "cannot inspect fixture payload")?;
    if !metadata.file_type().is_file() || metadata.len() != expected as u64 {
        return Err("invalid fixture payload file or length".into());
    }
    let bytes = read_bounded(&payload, expected)?;
    if sha256(&bytes) != manifest.payload_sha256 {
        return Err("fixture payload hash mismatch".into());
    }
    let block = PcmBlock::from_le_bytes(
        BlockHeader {
            epochs: Epochs::default(),
            sequence: 0,
            frame_position: 0,
            frame_count: manifest.spec.frames,
            format: manifest.spec.format,
            time_domain: TimeDomain::OfflineFrames,
        },
        &bytes,
    )
    .map_err(|e| e.to_string())?;
    Ok((manifest, block))
}

pub fn write_bundle(output: &Path) -> Result<FixtureBundle, String> {
    fs::create_dir(output)
        .map_err(|_| "cannot create fixture directory; destination must be new".to_string())?;
    let mut fixtures = Vec::new();
    for spec in qualification_specs() {
        let stem = format!(
            "{:?}-{:?}-{}-{}",
            spec.format.representation(),
            spec.format.channels(),
            spec.format.sample_rate_hz(),
            spec.pattern.name()
        )
        .to_lowercase();
        let payload_file = format!("{stem}.pcm");
        let mut file = File::create_new(output.join(&payload_file))
            .map_err(|_| "cannot create fixture payload")?;
        let mut source = FixtureSource::new(spec.clone()).map_err(|e| e.to_string())?;
        let mut block =
            PcmBlock::prepare(spec.format, 256, Epochs::default(), MemoryBudget::default())
                .map_err(|e| e.to_string())?;
        let mut buffer = vec![0; spec.format.payload_bytes(256).map_err(|e| e.to_string())?];
        let mut digest = Sha256::new();
        while source.read_into(&mut block).map_err(|e| e.to_string())? > 0 {
            let used = block.pack_le(&mut buffer).map_err(|e| e.to_string())?;
            file.write_all(&buffer[..used])
                .map_err(|_| "cannot write fixture payload")?;
            digest.update(&buffer[..used]);
        }
        file.sync_all().map_err(|_| "cannot sync fixture payload")?;
        let fixture = FixtureFile {
            schema_version: 1,
            payload_file,
            payload_bytes: spec
                .format
                .payload_bytes(spec.frames)
                .map_err(|e| e.to_string())?,
            spec,
            payload_sha256: digest
                .finalize()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        };
        fs::write(
            output.join(format!("{stem}.json")),
            serde_json::to_vec_pretty(&fixture).map_err(|e| e.to_string())?,
        )
        .map_err(|_| "cannot write fixture manifest")?;
        fixtures.push(fixture);
    }
    let bundle = FixtureBundle {
        schema_version: 1,
        fixtures,
    };
    let manifest = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
    fs::write(output.join("manifest.json"), &manifest)
        .map_err(|_| "cannot write fixture bundle manifest")?;
    fs::write(
        output.join("manifest.sha256"),
        format!("{}\n", sha256(&manifest)),
    )
    .map_err(|_| "cannot write manifest hash")?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    #[test]
    fn splitmix64_matches_independent_unsigned_vectors() {
        let mut state = 0;
        assert_eq!(super::splitmix64(&mut state), 0xe220a8397b1dcdaf);
        assert_eq!(super::splitmix64(&mut state), 0x6e789e6aa1b965f4);
        assert_eq!(super::splitmix64(&mut state), 0x06c45d188009454f);
    }
}
