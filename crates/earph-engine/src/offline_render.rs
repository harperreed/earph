// ABOUTME: Captures explicit local graph processing without opening a native output device.
// ABOUTME: Preserves partial evidence and separates source, DSP, file acceptance, and actual bytes.
use crate::stream_render::{PreparedStreamRender, SourceSpan, StreamRenderConfig};
use earph_core::{
    EngineError, ErrorCode,
    audio::{PcmFormat, PcmRepresentation},
    gain::GainRamp,
    ids::Epochs,
};
use earph_dsp::{
    CompileContext, CompiledStreamStage, FirCoefficients, GraphBudget, GraphDocument, GraphError,
    NodeKind, ProcessingPolicy, compile_stream, stream::StreamBudget,
};
use earph_media::LocalFile;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    os::{
        fd::AsFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

const LIMIT: u64 = 256 << 20;
const FIXED_BYTES: u64 = 16 << 20;
const GRAPH_BYTES: u64 = 128 << 10;
pub const USAGE: &str = "render-graph --graph GRAPH_JSON --output NEW_DIR --sample-format signed16|signed24|signed32 --gain-db DB --ramp-frames N --quantum N [--runtime PRIVATE_ASSET_DIR] [--] INPUT...";
const IO: EngineError = EngineError::new(
    ErrorCode::OutputFault,
    "offline capture or report I/O failed",
);
const INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "invalid explicit offline render arguments or graph",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "offline render exceeds its bounded resource allowance",
);

pub struct RenderGraphOptions {
    pub graph: PathBuf,
    pub output: PathBuf,
    pub runtime: Option<PathBuf>,
    pub inputs: Vec<PathBuf>,
    pub sample_format: PcmRepresentation,
    pub gain_db: f64,
    pub ramp_frames: u32,
    pub quantum_frames: usize,
}
#[derive(Debug, Serialize)]
#[serde(tag = "domain", content = "detail", rename_all = "snake_case")]
pub enum OfflineRenderError {
    Engine(EngineError),
    Graph(GraphError),
}
impl From<EngineError> for OfflineRenderError {
    fn from(e: EngineError) -> Self {
        Self::Engine(e)
    }
}
impl From<GraphError> for OfflineRenderError {
    fn from(e: GraphError) -> Self {
        Self::Graph(e)
    }
}
#[derive(Debug, Serialize)]
pub struct OfflineResources {
    pub resident_bytes: u64,
    pub preparation_peak_bytes: u64,
    pub wrapper_bridge_bytes: u64,
    pub fixed_reader_asset_report_reserve_bytes: u64,
    pub qualification: &'static str,
}
#[derive(Debug, Serialize)]
pub struct SrcObservation {
    pub real_input_frames: u64,
    pub library_input_frames: u64,
    pub library_calls: u64,
    pub generated_frames: u64,
    pub emitted_frames: u64,
    pub guard_frames: u64,
    pub explicit_padding_input_frames: u64,
    pub nominal_output_frames: u64,
    pub truncated_library_output_delay_frames: Option<usize>,
}
#[derive(Debug, Serialize)]
pub struct StageObservation {
    pub index: usize,
    pub input_frames: u64,
    pub output_frames: u64,
    pub work_units: u64,
    pub src: Option<SrcObservation>,
}
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OfflineFinalizationStage {
    CaptureSync,
    CaptureReadback,
    ReportEncode,
    ReportCreate,
    ReportWrite,
    ReportSync,
    ReportDirectorySync,
    ReportRepairPreserve,
    ReportRepairWrite,
    ReportRepairSync,
    ReportRepairDirectorySync,
}
#[derive(Debug, Serialize)]
pub struct OfflineFinalizationError {
    pub stage: OfflineFinalizationStage,
    pub error: EngineError,
}
#[derive(Debug, Serialize)]
pub struct OfflineGraphReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub input_format: Option<PcmFormat>,
    pub output_format: Option<PcmFormat>,
    pub gain_db: Option<f64>,
    pub ramp_frames: Option<u32>,
    pub quantum_frames: Option<usize>,
    pub quantization: &'static str,
    pub graph_hash: Option<String>,
    pub stages: Vec<CompiledStreamStage>,
    pub observations: Vec<StageObservation>,
    pub resources: Option<OfflineResources>,
    pub source_frames: u64,
    pub dsp_output_frames: u64,
    pub acknowledged_output_frames: u64,
    pub capture_frames: Option<u64>,
    pub files_completed: usize,
    pub capture_sha256: Option<String>,
    pub capture_bytes: Option<u64>,
    pub partial_frame_bytes: Option<u64>,
    pub saturation_count: u64,
    pub graph_peak: f64,
    pub prequantization_peak: f64,
    pub output_peak: f64,
    pub error: Option<OfflineRenderError>,
    pub finalization_errors: Vec<OfflineFinalizationError>,
}
impl Default for OfflineGraphReport {
    fn default() -> Self {
        Self {
            schema_version: 1,
            status: "fail",
            input_format: None,
            output_format: None,
            gain_db: None,
            ramp_frames: None,
            quantum_frames: None,
            quantization: "round ties away; saturate; no dither",
            graph_hash: None,
            stages: Vec::new(),
            observations: Vec::new(),
            resources: None,
            source_frames: 0,
            dsp_output_frames: 0,
            acknowledged_output_frames: 0,
            capture_frames: None,
            files_completed: 0,
            capture_sha256: None,
            capture_bytes: None,
            partial_frame_bytes: None,
            saturation_count: 0,
            graph_peak: 0.0,
            prequantization_peak: 0.0,
            output_peak: 0.0,
            error: None,
            finalization_errors: Vec::new(),
        }
    }
}

/// Parse only explicit offline options. No path enters the result or error text.
pub fn command(args: &[OsString]) -> OfflineGraphReport {
    match parse(args) {
        Ok(options) => render_graph(&options),
        Err(error) => OfflineGraphReport {
            error: Some(error.into()),
            ..OfflineGraphReport::default()
        },
    }
}
fn parse(args: &[OsString]) -> Result<RenderGraphOptions, EngineError> {
    let mut values = BTreeMap::new();
    let mut inputs = Vec::new();
    let mut index = 0;
    let mut positional = false;
    while index < args.len() {
        let arg = &args[index];
        if !positional && arg == "--" {
            positional = true;
            index += 1;
            continue;
        }
        if !positional && arg.as_encoded_bytes().starts_with(b"--") {
            let key = arg.to_str().ok_or(INVALID)?;
            if !matches!(
                key,
                "--graph"
                    | "--output"
                    | "--sample-format"
                    | "--gain-db"
                    | "--ramp-frames"
                    | "--quantum"
                    | "--runtime"
            ) {
                return Err(INVALID);
            }
            index += 1;
            let value = args.get(index).ok_or(INVALID)?.clone();
            if values.insert(key, value).is_some() {
                return Err(INVALID);
            }
        } else {
            inputs.push(PathBuf::from(arg));
        }
        index += 1;
    }
    let text = |key| values.get(key).and_then(|v| v.to_str()).ok_or(INVALID);
    let sample_format = match text("--sample-format")? {
        "signed16" => PcmRepresentation::Signed16,
        "signed24" => PcmRepresentation::Signed24,
        "signed32" => PcmRepresentation::Signed32,
        _ => return Err(INVALID),
    };
    Ok(RenderGraphOptions {
        graph: PathBuf::from(values.get("--graph").ok_or(INVALID)?),
        output: PathBuf::from(values.get("--output").ok_or(INVALID)?),
        runtime: values.get("--runtime").map(PathBuf::from),
        inputs,
        sample_format,
        gain_db: text("--gain-db")?.parse().map_err(|_| INVALID)?,
        ramp_frames: text("--ramp-frames")?.parse().map_err(|_| INVALID)?,
        quantum_frames: text("--quantum")?.parse().map_err(|_| INVALID)?,
    })
}
fn graph(path: &Path) -> Result<GraphDocument, OfflineRenderError> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|_| INVALID)?
        .take(GRAPH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > GRAPH_BYTES {
        return Err(BUDGET.into());
    }
    let graph: GraphDocument = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if graph.nodes.len() > 128
        || graph.edges.len() > 256
        || graph.retained_capacity_bytes()? as u64 > GRAPH_BYTES
    {
        return Err(BUDGET.into());
    }
    Ok(graph)
}
fn coefficients(
    graph: &GraphDocument,
    runtime: Option<&Path>,
) -> Result<Vec<FirCoefficients>, OfflineRenderError> {
    let mut references = BTreeMap::new();
    for node in &graph.nodes {
        if let NodeKind::Convolution { asset } = &node.kind {
            asset.validate()?;
            if let Some(previous) = references.insert(asset.sha256.clone(), asset)
                && previous != asset
            {
                return Err(INVALID.into());
            }
        }
    }
    let declared = references.values().try_fold(0u64, |bytes, reference| {
        let format = PcmFormat::new(
            reference.sample_rate_hz,
            reference.channels,
            PcmRepresentation::Float64,
        )?;
        bytes
            .checked_add(format.storage_bytes(reference.taps_per_channel as u64)? as u64)
            .and_then(|value| value.checked_add(1024))
            .ok_or(BUDGET)
    })?;
    if declared > LIMIT - FIXED_BYTES {
        return Err(BUDGET.into());
    }
    let mut assets = Vec::with_capacity(references.len());
    for reference in references.values() {
        let correction = crate::assets::resolve(runtime.ok_or(INVALID)?, reference)?;
        assets.push(FirCoefficients::new(correction.reference, correction.taps)?);
    }
    Ok(assets)
}
fn output_format(
    graph: &GraphDocument,
    representation: PcmRepresentation,
) -> Result<PcmFormat, EngineError> {
    let output = graph
        .nodes
        .iter()
        .find(|n| n.id == graph.output)
        .ok_or(INVALID)?;
    PcmFormat::new(
        output.format.sample_rate_hz,
        output.format.channels,
        representation,
    )
}
/// Capture one continuous stream. Every failure preserves the directory and partial files.
/// Keep the first render error and list finalization failures separately.
/// A single repair preserves prior report bytes before changing the owned report.
/// Arbitrary I/O faults can prevent repair; inspect its failure stage rather than assuming disk/stdout agreement.
pub fn render_graph(options: &RenderGraphOptions) -> OfflineGraphReport {
    let mut report = OfflineGraphReport::default();
    let mut capture = None;
    let mut owned_directory = false;
    let result = (|| -> Result<(), OfflineRenderError> {
        if options.inputs.is_empty()
            || options.inputs.len() > 256
            || options.quantum_frames == 0
            || options.quantum_frames > 8192
        {
            return Err(BUDGET.into());
        }
        options.sample_format.integer_bounds()?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&options.output)
            .map_err(|_| IO)?;
        owned_directory = true;
        capture = Some(
            OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(options.output.join("capture.pcm"))
                .map_err(|_| IO)?,
        );
        let document = graph(&options.graph)?;
        let epochs = Epochs {
            session: 1,
            stream: 1,
        };
        let mut first = Some(LocalFile::open(&options.inputs[0], epochs)?);
        let input_format = first.as_ref().ok_or(INVALID)?.format();
        let native_format = output_format(&document, options.sample_format)?;
        let mut gain = GainRamp::new(native_format)?;
        gain.set_target(Some(options.gain_db), options.ramp_frames)?;
        report.input_format = Some(input_format);
        report.output_format = Some(native_format);
        report.gain_db = Some(options.gain_db);
        report.ramp_frames = Some(options.ramp_frames);
        report.quantum_frames = Some(options.quantum_frames);
        let assets = coefficients(&document, options.runtime.as_deref())?;
        let context = CompileContext {
            input_format: PcmFormat::new(
                input_format.sample_rate_hz(),
                input_format.channels(),
                PcmRepresentation::Float64,
            )?,
            output_format: PcmFormat::new(
                native_format.sample_rate_hz(),
                native_format.channels(),
                PcmRepresentation::Float64,
            )?,
            quantum_frames: options.quantum_frames,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget {
                max_buffer_bytes: LIMIT - FIXED_BYTES,
                ..GraphBudget::default()
            },
        };
        let compiled = compile_stream(
            &document,
            &context,
            &assets,
            StreamBudget {
                max_resident_bytes: LIMIT - FIXED_BYTES,
                max_preparation_bytes: LIMIT - FIXED_BYTES,
                ..StreamBudget::default()
            },
        )?;
        report.graph_hash = Some(compiled.content_hash().into());
        report.stages = compiled.stages().to_vec();
        let mut renderer = PreparedStreamRender::prepare(
            compiled.into_stream_plan(),
            StreamRenderConfig {
                input_format,
                native_format,
                period_frames: options.quantum_frames,
                queue_frames: options.quantum_frames,
                epochs,
                budget_bytes: LIMIT - FIXED_BYTES,
            },
            Arc::new(AtomicBool::new(false)),
        )?;
        let resources = renderer.resources();
        report.resources = Some(OfflineResources {
            resident_bytes: resources.resident_bytes + FIXED_BYTES,
            preparation_peak_bytes: resources.preparation_peak_bytes + FIXED_BYTES,
            wrapper_bridge_bytes: resources.bridge_bytes,
            fixed_reader_asset_report_reserve_bytes: FIXED_BYTES,
            qualification: "bounded requested component bytes; allocator overhead/RSS, realtime, DAC and acoustic qualification unknown",
        });
        renderer.set_gain_target(Some(options.gain_db), options.ramp_frames)?;
        let file = capture.as_mut().ok_or(IO)?;
        let mut packed = vec![0; native_format.payload_bytes(options.quantum_frames as u64)?];
        let processing = (|| -> Result<(), EngineError> {
            for (index, path) in options.inputs.iter().enumerate() {
                let mut source = if index == 0 {
                    first.take().ok_or(INVALID)?
                } else {
                    LocalFile::open(path, epochs)?
                };
                if source.format() != input_format {
                    return Err(EngineError::new(
                        ErrorCode::FormatUnsupported,
                        "offline source joins require identical decoded formats",
                    ));
                }
                while let Some(block) = source.next_block()? {
                    let mut offset = 0;
                    while offset < block.header().frame_count as usize {
                        let step = renderer.pump(Some(SourceSpan {
                            block: &block,
                            offset,
                            frames: (block.header().frame_count as usize - offset)
                                .min(options.quantum_frames),
                        }))?;
                        offset += step.consumed_frames;
                        let wrote = write_periods(&mut renderer, file, &mut packed, &mut report)?;
                        if step.work_units == 0
                            && step.consumed_frames == 0
                            && step.produced_frames == 0
                            && !wrote
                        {
                            return Err(EngineError::new(
                                ErrorCode::InternalFault,
                                "offline stream made no bounded progress",
                            ));
                        }
                    }
                }
                report.files_completed += 1;
            }
            while !renderer.finished() {
                let step = renderer.finish()?;
                let wrote = write_periods(&mut renderer, file, &mut packed, &mut report)?;
                if !renderer.finished()
                    && step.work_units == 0
                    && step.produced_frames == 0
                    && !wrote
                {
                    return Err(EngineError::new(
                        ErrorCode::InternalFault,
                        "offline finish made no bounded progress",
                    ));
                }
            }
            Ok(())
        })();
        report.source_frames = renderer.stream().stats().source_frames;
        report.dsp_output_frames = renderer.stream().stats().output_frames;
        for index in 0..report.stages.len() {
            let stats = renderer.stream().stage_stats(index).ok_or(INVALID)?;
            let src = renderer.stream().src_stats(index).map(|s| SrcObservation {
                real_input_frames: s.real_input_frames,
                library_input_frames: s.library_input_frames,
                library_calls: s.library_calls,
                generated_frames: s.generated_frames,
                emitted_frames: s.emitted_frames,
                guard_frames: s.guard_frames,
                explicit_padding_input_frames: s.explicit_padding_input_frames,
                nominal_output_frames: s.nominal_output_frames,
                truncated_library_output_delay_frames: renderer
                    .stream()
                    .src_metadata(index)
                    .map(|m| m.truncated_output_delay),
            });
            report.observations.push(StageObservation {
                index,
                input_frames: stats.input_frames,
                output_frames: stats.output_frames,
                work_units: stats.work_units,
                src,
            });
        }
        processing?;
        Ok(())
    })();
    report.error = result.err();
    finalize(
        &mut report,
        capture.as_mut(),
        owned_directory.then_some(options.output.as_path()),
        &mut |directory| File::open(directory)?.sync_all(),
    );
    report
}
fn finalization_error(report: &mut OfflineGraphReport, stage: OfflineFinalizationStage) {
    report.status = "fail";
    if report.error.is_none() {
        report.error = Some(IO.into());
    }
    // The fixed finalization phases and single repair attempt bound this history.
    report
        .finalization_errors
        .push(OfflineFinalizationError { stage, error: IO });
}
fn finalize(
    report: &mut OfflineGraphReport,
    capture: Option<&mut File>,
    directory: Option<&Path>,
    sync_directory: &mut impl FnMut(&Path) -> std::io::Result<()>,
) {
    use OfflineFinalizationStage::*;
    if let Some(file) = capture {
        if file.sync_all().is_err() {
            finalization_error(report, CaptureSync);
        }
        if readback(file, report).is_err() {
            finalization_error(report, CaptureReadback);
        }
    }
    report.status = if report.error.is_none() {
        "pass"
    } else {
        "fail"
    };
    let Some(directory) = directory else {
        return;
    };
    let bytes = match serde_json::to_vec_pretty(&report) {
        Ok(bytes) => bytes,
        Err(_) => {
            finalization_error(report, ReportEncode);
            return;
        }
    };
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(directory.join("report.json"))
    {
        Ok(file) => file,
        Err(_) => {
            finalization_error(report, ReportCreate);
            return;
        }
    };
    if write_file(&mut file, &bytes).is_err() {
        finalization_error(report, ReportWrite);
        return;
    }
    let failure = if file.sync_all().is_err() {
        Some(ReportSync)
    } else if sync_directory(directory).is_err() {
        Some(ReportDirectorySync)
    } else {
        None
    };
    if let Some(stage) = failure {
        finalization_error(report, stage);
        repair_report(report, &mut file, &bytes, directory, sync_directory);
    }
}
// Repair once through the owned handle. Arbitrary I/O faults can still prevent disk/stdout agreement.
fn repair_report(
    report: &mut OfflineGraphReport,
    file: &mut File,
    original: &[u8],
    directory: &Path,
    sync_directory: &mut impl FnMut(&Path) -> std::io::Result<()>,
) {
    use OfflineFinalizationStage::*;
    let preserve = (|| -> Result<(), EngineError> {
        let mut backup = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(directory.join("report.before-finalization.json"))
            .map_err(|_| IO)?;
        write_file(&mut backup, original)?;
        backup.sync_all().map_err(|_| IO)?;
        sync_directory(directory).map_err(|_| IO)
    })();
    if preserve.is_err() {
        finalization_error(report, ReportRepairPreserve);
        return;
    }
    let correction = (|| -> Result<(), EngineError> {
        let bytes = serde_json::to_vec_pretty(&report).map_err(|_| IO)?;
        file.rewind().map_err(|_| IO)?;
        write_file(file, &bytes)?;
        // This handle belongs to the create-new report. Its complete prior bytes are preserved above.
        file.set_len(bytes.len() as u64).map_err(|_| IO)
    })();
    if correction.is_err() {
        finalization_error(report, ReportRepairWrite);
        return;
    }
    if file.sync_all().is_err() {
        finalization_error(report, ReportRepairSync);
        return;
    }
    if sync_directory(directory).is_err() {
        finalization_error(report, ReportRepairDirectorySync);
    }
}

/// Write exactly one JSON line and flush through the caller's locked fd writer.
/// Buffered writers must have no pending bytes before this call.
pub fn write_json_report(
    report: &impl serde::Serialize,
    output: &mut (impl std::io::Write + std::os::fd::AsFd),
) -> Result<(), EngineError> {
    let mut bytes = serde_json::to_vec(report).map_err(|_| IO)?;
    bytes.push(b'\n');
    write_file(output, &bytes)?;
    output.flush().map_err(|_| IO)
}

// Check the actual fd offset, append flag, and soft limit before every write or retry.
fn write_file(output: &mut (impl Write + AsFd), mut bytes: &[u8]) -> Result<(), EngineError> {
    while !bytes.is_empty() {
        let metadata = rustix::fs::fstat(output.as_fd()).map_err(|_| IO)?;
        let regular = rustix::fs::FileType::from_raw_mode(metadata.st_mode).is_file();
        let admitted = if regular {
            let position = rustix::fs::seek(output.as_fd(), rustix::fs::SeekFrom::Current(0))
                .map_err(|_| IO)?;
            let flags = rustix::fs::fcntl_getfl(output.as_fd()).map_err(|_| IO)?;
            let offset = if flags.contains(rustix::fs::OFlags::APPEND) {
                u64::try_from(metadata.st_size).map_err(|_| IO)?
            } else {
                position
            };
            rustix::process::getrlimit(rustix::process::Resource::Fsize)
                .current
                .map_or(bytes.len(), |limit| {
                    usize::try_from(limit.saturating_sub(offset))
                        .unwrap_or(usize::MAX)
                        .min(bytes.len())
                })
        } else {
            bytes.len()
        };
        if admitted == 0 {
            return Err(IO);
        }
        let written = match output.write(&bytes[..admitted]) {
            Ok(0) => return Err(IO),
            Ok(written) => written,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(IO),
        };
        // Commit each regular-file chunk before inspecting its next actual offset.
        if regular {
            output.flush().map_err(|_| IO)?;
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn write_periods(
    renderer: &mut PreparedStreamRender,
    file: &mut File,
    packed: &mut [u8],
    report: &mut OfflineGraphReport,
) -> Result<bool, EngineError> {
    let mut wrote = false;
    while let Some(period) = renderer.stage()? {
        let bytes = renderer.period().pack_le(packed)?;
        write_file(file, &packed[..bytes])?;
        renderer.accepted(period.frames)?;
        report.acknowledged_output_frames = report
            .acknowledged_output_frames
            .checked_add(period.frames as u64)
            .ok_or(BUDGET)?;
        renderer.observe(
            report.acknowledged_output_frames,
            report.acknowledged_output_frames,
        )?;
        report.saturation_count = report
            .saturation_count
            .checked_add(period.saturation_count)
            .ok_or(BUDGET)?;
        report.graph_peak = report.graph_peak.max(period.graph_peak);
        report.prequantization_peak = report.prequantization_peak.max(period.prequantization_peak);
        report.output_peak = report.output_peak.max(period.output_peak);
        wrote = true;
    }
    Ok(wrote)
}
fn readback(file: &mut File, report: &mut OfflineGraphReport) -> Result<(), EngineError> {
    file.rewind().map_err(|_| IO)?;
    let mut buffer = [0; 8192];
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    loop {
        let size = file.read(&mut buffer).map_err(|_| IO)?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
        bytes = bytes.checked_add(size as u64).ok_or(BUDGET)?;
    }
    report.capture_bytes = Some(bytes);
    report.capture_sha256 = Some(
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    );
    if let Some(format) = report.output_format {
        let width = format.payload_bytes(1)? as u64;
        report.capture_frames = Some(bytes / width);
        report.partial_frame_bytes = Some(bytes % width);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "earph-finalize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        directory
    }
    #[test]
    fn directory_sync_failure_corrects_the_owned_report_and_preserves_original_bytes() {
        let directory = directory();
        let mut report = OfflineGraphReport::default();
        let mut calls = 0;
        finalize(&mut report, None, Some(&directory), &mut |path| {
            calls += 1;
            if calls == 1 {
                Err(std::io::Error::other("controlled directory sync failure"))
            } else {
                File::open(path)?.sync_all()
            }
        });
        assert_eq!(report.status, "fail");
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            stored["status"], "fail",
            "finalization left a passing report at its final name"
        );
        let original = fs::read(directory.join("report.before-finalization.json")).unwrap();
        let expected = OfflineGraphReport {
            status: "pass",
            ..Default::default()
        };
        assert_eq!(original, serde_json::to_vec_pretty(&expected).unwrap());
        assert_eq!(stored, serde_json::to_value(&report).unwrap());
        assert_eq!(
            calls, 3,
            "repair did not durably preserve its backup and failed report"
        );
    }
    #[test]
    fn readback_failure_preserves_the_first_render_error_and_records_its_stage() {
        let directory = directory();
        let path = directory.join("capture.pcm");
        let mut capture = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        capture.write_all(b"actual capture bytes").unwrap();
        let first = EngineError::new(ErrorCode::FormatUnsupported, "first source-format failure");
        let mut report = OfflineGraphReport {
            error: Some(first.into()),
            ..Default::default()
        };
        finalize(
            &mut report,
            Some(&mut capture),
            Some(&directory),
            &mut |path| File::open(path)?.sync_all(),
        );
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["error"]["detail"]["code"], "FORMAT_UNSUPPORTED");
        assert_eq!(value["finalization_errors"][0]["stage"], "capture_readback");
        assert!(report.capture_bytes.is_none());
        assert_eq!(fs::read(path).unwrap(), b"actual capture bytes");
    }
    #[test]
    fn existing_report_collision_fails_without_changing_the_unowned_file() {
        let directory = directory();
        let path = directory.join("report.json");
        fs::write(&path, b"preserved existing evidence").unwrap();
        let mut report = OfflineGraphReport::default();
        finalize(&mut report, None, Some(&directory), &mut |path| {
            File::open(path)?.sync_all()
        });
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["status"], "fail");
        assert_eq!(value["error"]["detail"]["code"], "OUTPUT_FAULT");
        assert_eq!(value["finalization_errors"][0]["stage"], "report_create");
        assert_eq!(fs::read(path).unwrap(), b"preserved existing evidence");
    }
    #[test]
    fn repair_preservation_failure_is_explicit_and_never_changes_existing_artifacts() {
        let directory = directory();
        let backup = directory.join("report.before-finalization.json");
        fs::write(&backup, b"preserved collision").unwrap();
        let mut report = OfflineGraphReport::default();
        let mut calls = 0;
        finalize(&mut report, None, Some(&directory), &mut |_| {
            calls += 1;
            Err(std::io::Error::other("controlled directory sync failure"))
        });
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["status"], "fail");
        assert_eq!(
            value["finalization_errors"][1]["stage"],
            "report_repair_preserve"
        );
        assert_eq!(fs::read(backup).unwrap(), b"preserved collision");
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("report.json")).unwrap()).unwrap();
        assert_eq!(
            stored["status"], "pass",
            "failed preservation must leave the owned prior bytes intact"
        );
        assert_eq!(calls, 1);
    }
    #[test]
    fn final_repair_directory_sync_failure_is_reported_without_another_repair_attempt() {
        let directory = directory();
        let mut report = OfflineGraphReport::default();
        let mut calls = 0;
        finalize(&mut report, None, Some(&directory), &mut |path| {
            calls += 1;
            if calls == 1 || calls == 3 {
                Err(std::io::Error::other("controlled directory sync failure"))
            } else {
                File::open(path)?.sync_all()
            }
        });
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(
            value["finalization_errors"][1]["stage"],
            "report_repair_directory_sync"
        );
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("report.json")).unwrap()).unwrap();
        assert_eq!(stored["status"], "fail");
        assert_ne!(stored, value);
        assert!(directory.join("report.before-finalization.json").is_file());
        assert_eq!(calls, 3);
    }
    #[test]
    fn json_line_writer_handles_regular_and_socket_fds_and_actual_flush_failure() {
        let directory = directory();
        let path = directory.join("line.json");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let report = serde_json::json!({"status":"fail","detail":"line\nbreak"});
        let mut expected = serde_json::to_vec(&report).unwrap();
        expected.push(b'\n');
        write_json_report(&report, &mut file).unwrap();
        assert_eq!(fs::read(&path).unwrap(), expected);
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        write_json_report(&report, &mut writer).unwrap();
        let mut bytes = vec![0; expected.len()];
        reader.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
        struct BufferedFile(std::io::BufWriter<File>);
        impl AsFd for BufferedFile {
            fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
                self.0.get_ref().as_fd()
            }
        }
        impl Write for BufferedFile {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0.flush()
            }
        }
        let mut read_only = BufferedFile(std::io::BufWriter::new(File::open(&path).unwrap()));
        assert_eq!(
            write_json_report(&report, &mut read_only).unwrap_err().code,
            ErrorCode::OutputFault
        );
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}
