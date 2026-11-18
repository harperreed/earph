// ABOUTME: Measures prepared rendering against fixed graph and CPU margin contracts.
// ABOUTME: Preserves raw durations and verifies receipts independently from their summaries.

use earph_core::{
    audio::{
        BlockHeader, ChannelLayout, MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation,
        PcmSamples, TimeDomain,
    },
    dsp_nodes::{FirAssetReference, FirPurpose, FirSha256, PeqFilter, SrcPreset},
    ids::Epochs,
};
use earph_dsp::{stream::StreamBudget, *};
use earph_engine::stream_render::{PreparedStreamRender, SourceSpan, StreamRenderConfig};
#[path = "../src/render.rs"]
mod render;
use render::PreparedRender;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    hint::black_box,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Instant,
};
#[path = "support/allocations.rs"]
mod allocations;

type Result<T> = std::result::Result<T, String>;
const RATE: u64 = 48000;
const MAX_ARTIFACT_BYTES: u64 = 128 << 20;
const CSV_HEADER: &str =
    "index,duration_ns,pump_calls,source_accepted_frames,generated_frames,submitted_frames\n";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Sample {
    duration_ns: u64,
    pump_calls: u64,
    source_accepted_frames: u64,
    generated_frames: u64,
    submitted_frames: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Summary {
    minimum_ns: u64,
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    p999_ns: u64,
    maximum_ns: u64,
    total_ns: u64,
    deadline_misses: u64,
    margin_pass: bool,
}
impl Summary {
    fn from_samples(samples: &[Sample], quantum: usize) -> Result<Self> {
        if samples.is_empty() || ![48, 64, 256].contains(&quantum) {
            return Err("invalid statistics geometry".into());
        }
        let mut durations: Vec<_> = samples.iter().map(|s| s.duration_ns).collect();
        durations.sort_unstable();
        let rank = |numerator: usize| -> Result<u64> {
            let index = durations
                .len()
                .checked_mul(numerator)
                .and_then(|n| n.checked_add(999))
                .ok_or("rank overflow")?
                / 1000
                - 1;
            Ok(durations[index])
        };
        let period_numerator = quantum as u128 * 1_000_000_000;
        let p999_ns = rank(999)?;
        Ok(Self {
            minimum_ns: durations[0],
            p50_ns: rank(500)?,
            p95_ns: rank(950)?,
            p99_ns: rank(990)?,
            p999_ns,
            maximum_ns: *durations.last().unwrap(),
            total_ns: durations
                .iter()
                .try_fold(0_u64, |a, n| a.checked_add(*n).ok_or("duration overflow"))?,
            deadline_misses: durations
                .iter()
                .filter(|n| u128::from(**n) * u128::from(RATE) > period_numerator)
                .count() as u64,
            margin_pass: u128::from(p999_ns) * u128::from(RATE) * 10 < period_numerator * 7,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Workload {
    SameRateGain,
    SameRatePeq10,
    SameRateFir4096,
    ResponsiveRoundtripCorrection,
    QualityRoundtripCorrection,
}
#[derive(Debug, Clone, Copy)]
struct Case {
    workload: Workload,
    quantum: usize,
}
impl Case {
    fn name(self) -> String {
        format!(
            "{}_q{}",
            serde_json::to_value(self.workload)
                .unwrap()
                .as_str()
                .unwrap(),
            self.quantum
        )
    }
    fn streaming(self) -> bool {
        matches!(
            self.workload,
            Workload::ResponsiveRoundtripCorrection | Workload::QualityRoundtripCorrection
        )
    }
}
fn cases() -> Vec<Case> {
    [
        Workload::SameRateGain,
        Workload::SameRatePeq10,
        Workload::SameRateFir4096,
        Workload::ResponsiveRoundtripCorrection,
        Workload::QualityRoundtripCorrection,
    ]
    .into_iter()
    .flat_map(|workload| [64, 256].map(|quantum| Case { workload, quantum }))
    .collect()
}
fn finite(value: f64) -> FiniteGraphScalar {
    FiniteGraphScalar::new(value).unwrap()
}
fn format(representation: PcmRepresentation) -> PcmFormat {
    PcmFormat::new(RATE as u32, ChannelLayout::Stereo, representation).unwrap()
}
fn authored(case: Case) -> Result<(GraphDocument, Vec<FirCoefficients>)> {
    let mut nodes = Vec::new();
    let mut add = |id: &str, kind: NodeKind, rate| {
        nodes.push(Node {
            id: NodeId::new(id).unwrap(),
            kind,
            format: PortFormat {
                domain: SampleDomain::Pcm,
                sample_rate_hz: rate,
                channels: ChannelLayout::Stereo,
                precision: PcmRepresentation::Float64,
                max_quantum_frames: 512,
            },
        });
    };
    add("input", NodeKind::Input, RATE as u32);
    if case.streaming() {
        let preset = if case.workload == Workload::QualityRoundtripCorrection {
            SrcPreset::Quality
        } else {
            SrcPreset::Responsive
        };
        add(
            "down",
            NodeKind::Src {
                input_rate_hz: 48000,
                preset,
                chunk_frames: case.quantum,
            },
            44100,
        );
        add(
            "up",
            NodeKind::Src {
                input_rate_hz: 44100,
                preset,
                chunk_frames: case.quantum,
            },
            48000,
        );
    }
    let mut assets = Vec::new();
    if case.workload == Workload::SameRateGain {
        add(
            "gain",
            NodeKind::Gain {
                gain_linear: finite(0.5),
            },
            48000,
        );
    }
    if case.streaming() || case.workload == Workload::SameRateFir4096 {
        let count = if case.streaming() { 1024 } else { 4096 };
        let taps: Vec<Vec<f64>> = (0..2)
            .map(|channel| {
                (0..count)
                    .map(|k| {
                        if (k + channel) % 2 == 0 {
                            0.5 / count as f64
                        } else {
                            -0.5 / count as f64
                        }
                    })
                    .collect()
            })
            .collect();
        let mut bytes = Vec::with_capacity(count * 2 * 8);
        for k in 0..count {
            for channel in &taps {
                bytes.extend_from_slice(&channel[k].to_le_bytes());
            }
        }
        let reference = FirAssetReference {
            sha256: FirSha256::new(hash(&bytes)).unwrap(),
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            taps_per_channel: count,
            purpose: FirPurpose::Correction,
        };
        add(
            "fir",
            NodeKind::Convolution {
                asset: reference.clone(),
            },
            48000,
        );
        assets.push(FirCoefficients::new(reference, taps).map_err(|e| e.to_string())?);
    }
    let frequencies = [
        80., 125., 200., 315., 500., 800., 1250., 2000., 3150., 5000.,
    ];
    let filters = if case.workload == Workload::SameRatePeq10 {
        &frequencies[..]
    } else if case.streaming() {
        &frequencies[6..]
    } else {
        &frequencies[0..0]
    };
    for (index, frequency) in filters.iter().enumerate() {
        add(
            &format!("peq{index}"),
            NodeKind::Peq {
                filter: PeqFilter::Peaking {
                    frequency_hz: finite(*frequency),
                    q: finite(0.707),
                    gain_db: finite(-3.),
                },
            },
            48000,
        );
    }
    add("output", NodeKind::Output, 48000);
    let edges = nodes
        .windows(2)
        .map(|n| Edge {
            from: n[0].id.clone(),
            to: n[1].id.clone(),
            from_port: 0,
            to_port: 0,
        })
        .collect();
    Ok((
        GraphDocument {
            schema_version: 1,
            output: NodeId::new("output").unwrap(),
            nodes,
            edges,
        },
        assets,
    ))
}
enum Renderer {
    Same(Box<PreparedRender>),
    Stream(Box<PreparedStreamRender>),
}
struct Producer {
    renderer: Renderer,
    input: PcmBlock,
    input_offset: usize,
    case: Case,
    source: u64,
    generated: u64,
    submitted: u64,
    saturation: u64,
    checksum: u64,
    document: GraphDocument,
    graph_hash: String,
    implementation: String,
    metadata: Value,
    resources: Value,
    source_hash: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Finish {
    terminal: bool,
    source_accepted_frames: u64,
    generated_frames: u64,
    submitted_frames: u64,
    saturation_count: u64,
    tail_calls: u64,
    tail_elapsed_ns: u64,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Progress {
    source_accepted_frames: u64,
    generated_frames: u64,
    submitted_frames: u64,
}
impl Progress {
    fn add(self, other: Self) -> Result<Self> {
        Ok(Self {
            source_accepted_frames: self
                .source_accepted_frames
                .checked_add(other.source_accepted_frames)
                .ok_or("source total overflow")?,
            generated_frames: self
                .generated_frames
                .checked_add(other.generated_frames)
                .ok_or("generated total overflow")?,
            submitted_frames: self
                .submitted_frames
                .checked_add(other.submitted_frames)
                .ok_or("submitted total overflow")?,
        })
    }
}
fn witness(period: &PcmBlock, offset: usize, frames: usize, checksum: &mut u64) -> Result<()> {
    for index in [offset * 2, (offset + frames) * 2 - 1] {
        let value = black_box(
            period
                .integer_sample(index)
                .ok_or("missing output sample")?,
        );
        *checksum = checksum.wrapping_mul(0x100000001b3) ^ value as u64;
    }
    Ok(())
}
impl Producer {
    fn prepare(case: Case) -> Result<Self> {
        let (document, assets) = authored(case)?;
        let context = CompileContext {
            input_format: format(PcmRepresentation::Float64),
            output_format: format(PcmRepresentation::Float64),
            quantum_frames: case.quantum,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        };
        let native = format(PcmRepresentation::Signed24);
        let (renderer, graph_hash, implementation, metadata, resources) = if case.streaming() {
            let plan = compile_stream(&document, &context, &assets, StreamBudget::default())
                .map_err(|e| e.to_string())?;
            let hash = plan.content_hash().to_owned();
            let version = plan.implementation_version().to_owned();
            let metadata = serde_json::to_value(plan.stages()).map_err(|e| e.to_string())?;
            let mut render = PreparedStreamRender::prepare(
                plan.into_stream_plan(),
                StreamRenderConfig {
                    input_format: native,
                    native_format: native,
                    period_frames: case.quantum,
                    queue_frames: case.quantum * 4,
                    epochs: Epochs::default(),
                    budget_bytes: 256 << 20,
                },
                Arc::new(AtomicBool::new(false)),
            )
            .map_err(|e| e.to_string())?;
            render
                .set_gain_target(Some(-6.020599913279624), 0)
                .map_err(|e| e.to_string())?;
            let r = render.resources();
            (
                Renderer::Stream(Box::new(render)),
                hash,
                version,
                metadata,
                json!({"resident_bytes":r.resident_bytes,"preparation_peak_bytes":r.preparation_peak_bytes,"bridge_bytes":r.bridge_bytes,"replay_frames":r.replay_frames}),
            )
        } else {
            let plan =
                compile_with_assets(&document, &context, &assets).map_err(|e| e.to_string())?;
            let hash = plan.content_hash().to_owned();
            let version = plan.implementation_version().to_owned();
            let metadata =
                json!({"latency_frames":plan.latency_frames(),"tail_frames":plan.tail_frames()});
            let mut render = PreparedRender::prepare(
                plan,
                native,
                native,
                Epochs::default(),
                MemoryBudget::default(),
            )
            .map_err(|e| format!("{:?}", e))?;
            render
                .set_gain_target(Some(-6.020599913279624), 0)
                .map_err(|e| format!("{:?}", e))?;
            let r = render.resources();
            (
                Renderer::Same(Box::new(render)),
                hash,
                version,
                metadata,
                json!({"audio_payload_bytes":r.audio_payload_bytes}),
            )
        };
        let mut seed = 0x2396efb3_u32;
        let samples: Vec<i32> = (0..case.quantum * 2)
            .map(|_| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                ((seed >> 16) as i32 - 32768) * 4
            })
            .collect();
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let input = PcmBlock::new(
            BlockHeader {
                format: native,
                epochs: Epochs::default(),
                sequence: 0,
                frame_position: 0,
                frame_count: case.quantum as u64,
                time_domain: TimeDomain::OfflineFrames,
            },
            PcmSamples::Signed24(samples),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            renderer,
            input,
            input_offset: 0,
            case,
            source: 0,
            generated: 0,
            submitted: 0,
            saturation: 0,
            checksum: 0xcbf29ce484222325,
            document,
            graph_hash,
            implementation,
            metadata,
            resources,
            source_hash: hash(&bytes),
        })
    }
    fn render_period(&mut self) -> Result<Sample> {
        let before = (self.source, self.generated, self.submitted);
        let mut pumps = 0;
        match &mut self.renderer {
            Renderer::Same(render) => {
                let report = render
                    .process(&self.input, 0, self.case.quantum)
                    .map_err(|e| format!("{:?}", e))?;
                if report.valid_frames != self.case.quantum
                    || report.source_frames != self.case.quantum
                {
                    return Err("same-rate period count changed".into());
                }
                self.source += report.source_frames as u64;
                self.generated += report.valid_frames as u64;
                self.submitted += report.valid_frames as u64;
                self.saturation += report.saturation_count;
                witness(render.period(), 0, report.valid_frames, &mut self.checksum)?;
                pumps = 1;
            }
            Renderer::Stream(render) => {
                let target = self.submitted + self.case.quantum as u64;
                for _ in 0..4096 {
                    if let Some(report) = render.stage().map_err(|e| e.to_string())? {
                        let offset = render.period_offset();
                        let count =
                            (report.frames - offset).min((target - self.submitted) as usize);
                        if count == 0 {
                            return Err("empty staged output".into());
                        }
                        if offset == 0 {
                            self.saturation += report.saturation_count;
                        }
                        witness(render.period(), offset, count, &mut self.checksum)?;
                        render.accepted(count).map_err(|e| e.to_string())?;
                        self.submitted += count as u64;
                        render
                            .observe(self.submitted, self.submitted)
                            .map_err(|e| e.to_string())?;
                        if self.submitted == target {
                            break;
                        }
                    } else {
                        let step = render
                            .pump(Some(SourceSpan {
                                block: &self.input,
                                offset: self.input_offset,
                                frames: self.case.quantum - self.input_offset,
                            }))
                            .map_err(|e| e.to_string())?;
                        pumps += 1;
                        self.input_offset += step.consumed_frames;
                        if self.input_offset == self.case.quantum {
                            self.input_offset = 0;
                        }
                    }
                }
                self.source = render.stream().stats().source_frames;
                self.generated = render.stream().stats().output_frames;
                if self.submitted != target {
                    return Err("output demand exceeded bounded scheduler calls".into());
                }
            }
        }
        if self.saturation != 0 {
            return Err("reference workload saturated".into());
        }
        Ok(Sample {
            duration_ns: 0,
            pump_calls: pumps,
            source_accepted_frames: self.source - before.0,
            generated_frames: self.generated - before.1,
            submitted_frames: self.submitted - before.2,
        })
    }
    fn finish(&mut self) -> Result<Finish> {
        let started = Instant::now();
        let mut calls = 0;
        let mut terminal = false;
        for _ in 0..1_048_576 {
            calls += 1;
            match &mut self.renderer {
                Renderer::Same(render) => {
                    let report = render.finish().map_err(|e| format!("{:?}", e))?;
                    self.generated += report.valid_frames as u64;
                    self.submitted += report.valid_frames as u64;
                    self.saturation += report.saturation_count;
                    if report.valid_frames > 0 {
                        witness(render.period(), 0, report.valid_frames, &mut self.checksum)?;
                    }
                    if report.tail_finished {
                        terminal = true;
                        break;
                    }
                }
                Renderer::Stream(render) => {
                    if let Some(report) = render.stage().map_err(|e| e.to_string())? {
                        let offset = render.period_offset();
                        let count = report.frames - offset;
                        if offset == 0 {
                            self.saturation += report.saturation_count;
                        }
                        witness(render.period(), offset, count, &mut self.checksum)?;
                        render.accepted(count).map_err(|e| e.to_string())?;
                        self.submitted += count as u64;
                        render
                            .observe(self.submitted, self.submitted)
                            .map_err(|e| e.to_string())?;
                    } else if render.finished() {
                        terminal = true;
                        break;
                    } else {
                        render.finish().map_err(|e| e.to_string())?;
                    }
                    self.source = render.stream().stats().source_frames;
                    self.generated = render.stream().stats().output_frames;
                }
            }
        }
        if !terminal || self.generated != self.submitted || self.saturation != 0 {
            return Err("invalid terminal rendering counts".into());
        }
        Ok(Finish {
            terminal,
            source_accepted_frames: self.source,
            generated_frames: self.generated,
            submitted_frames: self.submitted,
            saturation_count: self.saturation,
            tail_calls: calls,
            tail_elapsed_ns: elapsed(started)?,
        })
    }
    fn source_accepted(&self) -> u64 {
        self.source
    }
    fn progress(&self) -> Progress {
        Progress {
            source_accepted_frames: self.source,
            generated_frames: self.generated,
            submitted_frames: self.submitted,
        }
    }
    fn submitted(&self) -> u64 {
        self.submitted
    }
    fn output_checksum(&self) -> u64 {
        self.checksum
    }
}
fn elapsed(started: Instant) -> Result<u64> {
    u64::try_from(started.elapsed().as_nanos()).map_err(|_| "elapsed duration overflow".into())
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunConfig {
    qualification: bool,
    warmup: usize,
    measured: usize,
}
impl RunConfig {
    fn smoke() -> Self {
        Self {
            qualification: false,
            warmup: 8,
            measured: 16,
        }
    }
    fn qualification() -> Self {
        Self {
            qualification: true,
            warmup: 4096,
            measured: 100000,
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    path: String,
    bytes: u64,
    sha256: String,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseReceipt {
    name: String,
    workload: Workload,
    quantum: usize,
    measured_count: usize,
    warmup_count: usize,
    graph: GraphDocument,
    graph_hash: String,
    implementation_version: String,
    stage_metadata: Value,
    resources: Value,
    source_packet_sha256: String,
    cold_priming_ns: u64,
    before_measurement: Progress,
    after_measurement: Progress,
    summary: Summary,
    csv: Artifact,
    finish: Finish,
    output_checksum: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema_version: u32,
    status: String,
    config: RunConfig,
    build_provenance: Value,
    harness_source_sha256: String,
    binary_sha256: String,
    hardware: Value,
    scope: String,
    sample_rate_hz: u64,
    timer_calibration: Summary,
    cases: Vec<CaseReceipt>,
}
impl Receipt {
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.sample_rate_hz != RATE
            || self.cases.len() != 10
            || self.config.measured == 0
            || self.config.measured > 100000
            || self.config.warmup > 4096
        {
            return Err("invalid receipt geometry".into());
        }
        if self.config.qualification
            && (self.config.measured != 100000
                || self.config.warmup != 4096
                || self.build_provenance["source_dirty"] != false
                || self.build_provenance["profile"] != "release")
        {
            return Err("qualification source or sample binding failed".into());
        }
        let mut names = std::collections::BTreeSet::new();
        let mut bytes = 0_u64;
        for (actual, expected) in self.cases.iter().zip(cases()) {
            if actual.name != expected.name()
                || actual.workload != expected.workload
                || actual.quantum != expected.quantum
                || !names.insert(&actual.name)
                || actual.measured_count != self.config.measured
                || actual.warmup_count != self.config.warmup
                || actual.csv.path != format!("{}.csv", expected.name())
                || !actual.finish.terminal
                || actual.finish.generated_frames != actual.finish.submitted_frames
                || actual.finish.saturation_count != 0
                || actual.finish.source_accepted_frames
                    != actual.after_measurement.source_accepted_frames
                || actual.finish.generated_frames < actual.after_measurement.generated_frames
                || actual.finish.submitted_frames < actual.after_measurement.submitted_frames
                || actual.before_measurement.submitted_frames
                    != (self.config.warmup as u64 + 1) * actual.quantum as u64
                || actual.after_measurement.submitted_frames
                    != (self.config.warmup as u64 + 1 + self.config.measured as u64)
                        * actual.quantum as u64
            {
                return Err("case binding failed".into());
            }
            for digest in [
                &actual.graph_hash,
                &actual.source_packet_sha256,
                &actual.csv.sha256,
            ] {
                if digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err("invalid receipt digest".into());
                }
            }
            bytes = bytes
                .checked_add(actual.csv.bytes)
                .ok_or("artifact size overflow")?;
        }
        if bytes > MAX_ARTIFACT_BYTES {
            return Err("artifact byte budget exceeded".into());
        }
        let status = if !self.config.qualification {
            "smoke"
        } else if self.cases.iter().all(|c| c.summary.margin_pass) {
            "passed"
        } else {
            "failed"
        };
        if self.status != status {
            return Err("receipt status disagrees with measurements".into());
        }
        Ok(())
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())
}
fn hardware() -> Value {
    let command = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_owned())
    };
    let cpu = if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("model name\t: ").map(str::to_owned))
        })
    } else {
        command("sysctl", &["-n", "machdep.cpu.brand_string"])
    };
    let affinity = fs::read_to_string("/proc/self/status").ok().and_then(|s| {
        s.lines()
            .find_map(|l| l.strip_prefix("Cpus_allowed_list:\t").map(str::to_owned))
    });
    json!({"os_kernel_arch":command("uname",&["-srm"]),"cpu_model":cpu,"logical_cpus":std::thread::available_parallelism().ok().map(usize::from),"allowed_cpus":affinity,"cpu0_governor":fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor").ok().map(|s|s.trim().to_owned()),"load_average":fs::read_to_string("/proc/loadavg").ok().map(|s|s.split_whitespace().take(3).collect::<Vec<_>>().join(" ")),"policy_changes":false})
}
fn produce(directory: &Path, config: RunConfig) -> Result<Receipt> {
    if config.measured == 0 || config.measured > 100000 || config.warmup > 4096 {
        return Err("invalid run bounds".into());
    }
    let provenance = earph_harness::build_provenance();
    if config.qualification
        && (cfg!(debug_assertions)
            || provenance["source_dirty"] != false
            || provenance["profile"] != "release")
    {
        return Err("qualification requires a clean release build".into());
    }
    fs::DirBuilder::new()
        .mode(0o700)
        .create(directory)
        .map_err(|e| e.to_string())?;
    let run = (|| {
        let mut calibration = Vec::with_capacity(10000);
        for _ in 0..10000 {
            let start = Instant::now();
            calibration.push(Sample {
                duration_ns: elapsed(start)?,
                pump_calls: 0,
                source_accepted_frames: 0,
                generated_frames: 0,
                submitted_frames: 0,
            });
        }
        let mut receipt=Receipt {schema_version:1,status:"smoke".into(),config,build_provenance:provenance,harness_source_sha256:hash(include_bytes!("cpu_qualification.rs")),binary_sha256:hash(&fs::read(std::env::current_exe().map_err(|e|e.to_string())?).map_err(|e|e.to_string())?),hardware:hardware(),scope:"Prepared software rendering includes conversion, DSP, gain, quantization, output witnesses, and streaming replay ledger work. It excludes decoding, worker scheduling, native I/O, callbacks, driver queues, and acoustic output. Streaming acknowledgments are harness ledger actions.".into(),sample_rate_hz:RATE,timer_calibration:Summary::from_samples(&calibration,64)?,cases:Vec::with_capacity(10)};
        for case in cases() {
            let mut producer =
                Producer::prepare(case).map_err(|e| format!("{}: {e}", case.name()))?;
            let cold = Instant::now();
            producer.render_period()?;
            let cold_priming_ns = elapsed(cold)?;
            for _ in 0..config.warmup {
                producer.render_period()?;
            }
            let before_measurement = producer.progress();
            let mut samples = Vec::with_capacity(config.measured);
            for _ in 0..config.measured {
                let started = Instant::now();
                let mut sample = producer.render_period()?;
                sample.duration_ns = elapsed(started)?;
                samples.push(sample);
            }
            let summary = Summary::from_samples(&samples, case.quantum)?;
            let after_measurement = producer.progress();
            let finish = producer.finish()?;
            let mut csv = Vec::with_capacity(config.measured * 64);
            csv.extend_from_slice(CSV_HEADER.as_bytes());
            for (i, sample) in samples.iter().enumerate() {
                writeln!(
                    csv,
                    "{i},{},{},{},{},{}",
                    sample.duration_ns,
                    sample.pump_calls,
                    sample.source_accepted_frames,
                    sample.generated_frames,
                    sample.submitted_frames
                )
                .map_err(|e| e.to_string())?;
            }
            if csv.len() as u64 > MAX_ARTIFACT_BYTES / 10 {
                return Err("case artifact byte budget exceeded".into());
            }
            let path = format!("{}.csv", case.name());
            write_new(&directory.join(&path), &csv)?;
            receipt.cases.push(CaseReceipt {
                name: case.name(),
                workload: case.workload,
                quantum: case.quantum,
                measured_count: config.measured,
                warmup_count: config.warmup,
                graph: producer.document,
                graph_hash: producer.graph_hash,
                implementation_version: producer.implementation,
                stage_metadata: producer.metadata,
                resources: producer.resources,
                source_packet_sha256: producer.source_hash,
                cold_priming_ns,
                before_measurement,
                after_measurement,
                summary,
                csv: Artifact {
                    path,
                    bytes: csv.len() as u64,
                    sha256: hash(&csv),
                },
                finish,
                output_checksum: producer.checksum,
            });
        }
        receipt.status = if !config.qualification {
            "smoke"
        } else if receipt.cases.iter().all(|c| c.summary.margin_pass) {
            "passed"
        } else {
            "failed"
        }
        .into();
        receipt.validate()?;
        write_new(
            &directory.join("cpu-qualification.json"),
            &serde_json::to_vec_pretty(&receipt).map_err(|e| e.to_string())?,
        )?;
        validate_receipt(directory)?;
        Ok(receipt)
    })();
    if let Err(error) = &run {
        let _ = write_new(
            &directory.join("failed.json"),
            &serde_json::to_vec(&json!({"status":"failed","reason":error}))
                .map_err(|e| e.to_string())?,
        );
    }
    run
}
fn validate_receipt(directory: &Path) -> Result<Receipt> {
    let bytes = earph_harness::read_bounded(&directory.join("cpu-qualification.json"), 1 << 20)?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    receipt.validate()?;
    for case in &receipt.cases {
        let csv = earph_harness::read_bounded(
            &directory.join(&case.csv.path),
            MAX_ARTIFACT_BYTES as usize / 10,
        )?;
        if csv.len() as u64 != case.csv.bytes || hash(&csv) != case.csv.sha256 {
            return Err("CSV byte identity changed".into());
        }
        let text = std::str::from_utf8(&csv).map_err(|e| e.to_string())?;
        if !text.starts_with(CSV_HEADER) || !text.ends_with('\n') {
            return Err("CSV framing changed".into());
        }
        let mut samples = Vec::with_capacity(case.measured_count);
        for (i, line) in text[CSV_HEADER.len()..].lines().enumerate() {
            if i >= case.measured_count {
                return Err("extra CSV sample".into());
            }
            let numbers: Vec<u64> = line
                .split(',')
                .map(|v| v.parse().map_err(|_| "invalid CSV number".to_owned()))
                .collect::<Result<_>>()?;
            if numbers.len() != 6 || numbers[0] != i as u64 || numbers[5] != case.quantum as u64 {
                return Err("CSV sample contract changed".into());
            }
            samples.push(Sample {
                duration_ns: numbers[1],
                pump_calls: numbers[2],
                source_accepted_frames: numbers[3],
                generated_frames: numbers[4],
                submitted_frames: numbers[5],
            });
        }
        let totals = samples.iter().try_fold(Progress::default(), |sum, s| {
            sum.add(Progress {
                source_accepted_frames: s.source_accepted_frames,
                generated_frames: s.generated_frames,
                submitted_frames: s.submitted_frames,
            })
        })?;
        if case.before_measurement.add(totals)? != case.after_measurement
            || samples.len() != case.measured_count
            || Summary::from_samples(&samples, case.quantum)? != case.summary
        {
            return Err("CSV statistics changed".into());
        }
    }
    Ok(receipt)
}
fn test_directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/cpu-qualification-tests");
    fs::create_dir_all(&root).unwrap();
    root.join(format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
#[ignore = "writes release CPU qualification receipts; set EARPH_CPU_QUALIFICATION_OUTPUT to a new private directory"]
fn release_cpu_qualification_receipts() {
    let output = std::env::var_os("EARPH_CPU_QUALIFICATION_OUTPUT")
        .expect("set EARPH_CPU_QUALIFICATION_OUTPUT to a new directory");
    let receipt = produce(Path::new(&output), RunConfig::qualification()).unwrap();
    assert_eq!(
        receipt.status, "passed",
        "CPU margin gate failed; preserve the written receipt and raw samples"
    );
}

#[test]
fn nearest_rank_retains_the_tail_that_p99_does_not_describe() {
    let mut samples: Vec<_> = (1..=10000)
        .map(|duration_ns| Sample {
            duration_ns,
            pump_calls: 1,
            source_accepted_frames: 64,
            generated_frames: 64,
            submitted_frames: 64,
        })
        .collect();
    for sample in &mut samples[9989..] {
        sample.duration_ns = 1_000_000;
    }
    let summary = Summary::from_samples(&samples, 64).unwrap();
    assert_eq!(summary.p50_ns, 5000);
    assert_eq!(summary.p99_ns, 9900);
    assert_eq!(summary.p999_ns, 1_000_000);
    assert_eq!(summary.maximum_ns, 1_000_000);
    assert_eq!(summary.deadline_misses, 0);
    assert!(!summary.margin_pass);
    assert!(Summary::from_samples(&[], 64).is_err());
    assert!(Summary::from_samples(&samples, 0).is_err());
}

#[test]
fn margin_boundary_is_strict_and_deadline_misses_have_a_separate_meaning() {
    let sample = |duration_ns| Sample {
        duration_ns,
        pump_calls: 1,
        source_accepted_frames: 48,
        generated_frames: 48,
        submitted_frames: 48,
    };
    assert!(
        Summary::from_samples(&[sample(699_999)], 48)
            .unwrap()
            .margin_pass
    );
    let exact = Summary::from_samples(&[sample(700_000)], 48).unwrap();
    assert!(!exact.margin_pass);
    assert_eq!(exact.deadline_misses, 0);
    let deadlines = Summary::from_samples(&[sample(1_000_000), sample(1_000_001)], 48).unwrap();
    assert_eq!(deadlines.deadline_misses, 1);
}

#[test]
fn fixed_workloads_render_real_periods_and_finish_without_processing_allocations() {
    for case in cases() {
        let mut producer = Producer::prepare(case).unwrap();
        producer.render_period().unwrap();
        let (_, counts) = allocations::tracked(|| {
            for _ in 0..16 {
                let sample = producer.render_period().unwrap();
                assert_eq!(sample.submitted_frames, case.quantum as u64);
            }
        });
        assert_eq!(counts, allocations::AllocationCounts::default());
        assert!(producer.source_accepted() > 0);
        assert_eq!(producer.submitted(), 17 * case.quantum as u64);
        assert_ne!(producer.output_checksum(), 0);
        let finish = producer.finish().unwrap();
        assert!(finish.terminal);
        assert_eq!(finish.source_accepted_frames, producer.source_accepted());
        assert_eq!(finish.generated_frames, finish.submitted_frames);
        assert_eq!(finish.saturation_count, 0);
        if let Renderer::Same(render) = &mut producer.renderer {
            assert_eq!(render.graph().lifecycle(), GraphLifecycle::Finished);
            render
                .reset(Epochs {
                    session: 0,
                    stream: 1,
                })
                .unwrap();
            assert_eq!(render.graph().lifecycle(), GraphLifecycle::Prepared);
            render.cancel();
            assert_eq!(render.graph().lifecycle(), GraphLifecycle::Cancelled);
        }
    }
}

#[test]
fn receipt_consumer_checks_actual_rows_hashes_and_totals_after_serialization() {
    let directory = test_directory();
    let receipt = produce(&directory, RunConfig::smoke()).unwrap();
    validate_receipt(&directory).unwrap();
    let mut inconsistent = serde_json::to_value(&receipt).unwrap();
    inconsistent["cases"][0]["finish"]["source_accepted_frames"] =
        serde_json::json!(receipt.cases[0].finish.source_accepted_frames + 1);
    let inconsistent: Receipt = serde_json::from_value(inconsistent).unwrap();
    assert!(inconsistent.validate().is_err());
    let mut document = serde_json::to_value(&receipt).unwrap();
    document["cases"][0]["measured_count"] = serde_json::json!(999);
    let invalid: Receipt = serde_json::from_value(document).unwrap();
    assert!(invalid.validate().is_err());
    let first_csv = directory.join(&receipt.cases[0].csv.path);
    let mut csv = std::fs::read(&first_csv).unwrap();
    csv.push(b'0');
    std::fs::write(&first_csv, csv).unwrap();
    assert!(validate_receipt(&directory).is_err());
    assert!(produce(&directory, RunConfig::smoke()).is_err());
}
