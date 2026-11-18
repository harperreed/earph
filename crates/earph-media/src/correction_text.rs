// ABOUTME: Imports bounded correction definitions into product-owned graph intent and provenance.
// ABOUTME: Preserves source controls without applying profiles, changing volume, or opening outputs.
mod text;
mod yaml;
use earph_core::{
    audio::{ChannelLayout, PcmRepresentation},
    dsp_nodes::PeqFilter,
    graph::{
        Edge, FiniteGraphScalar, GraphDocument, MAX_GRAPH_EDGES, MAX_GRAPH_NODES, Node, NodeKind,
        PortFormat,
    },
    ids::NodeId,
    safety::SampleDomain,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    fs::File,
    io::{Read, Seek},
    path::Path,
};

pub const MAX_CORRECTION_BYTES: usize = 64 * 1024;
pub const MAX_CORRECTION_FILTERS: usize = 32;
pub const MAX_REPORT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorrectionFormat {
    #[serde(rename = "rew")]
    Rew,
    #[serde(rename = "autoeq")]
    AutoEq,
    #[serde(rename = "camilladsp")]
    CamillaDsp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportContext {
    pub sample_rate_hz: u32,
    pub channels: ChannelLayout,
    pub quantum_frames: usize,
}
impl ImportContext {
    pub fn validate(self) -> Result<(), ImportError> {
        validate_context(self)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportMetadata {
    pub headphone_model: Option<String>,
    pub measurement_provider: Option<String>,
    pub target: Option<String>,
    pub parameter_set: Option<String>,
    pub revision: Option<String>,
    pub redistribution_license: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProvenance {
    pub format: CorrectionFormat,
    pub sha256: String,
    pub bytes: usize,
    pub application_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GainScale {
    #[serde(rename = "dB")]
    Decibel,
    #[serde(rename = "linear")]
    Linear,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ImportedFilter {
    Peaking {
        frequency_hz: f64,
        gain_db: f64,
        q: f64,
    },
    LowShelfQ {
        frequency_hz: f64,
        gain_db: f64,
        q: f64,
    },
    HighShelfQ {
        frequency_hz: f64,
        gain_db: f64,
        q: f64,
    },
    Gain {
        gain: f64,
        scale: GainScale,
        inverted: bool,
        mute: bool,
    },
}
impl ImportedFilter {
    fn node(&self, rate: u32) -> Result<NodeKind, ImportError> {
        match *self {
            Self::Peaking {
                frequency_hz,
                gain_db,
                q,
            } => equalizer_node(
                PeqFilter::Peaking {
                    frequency_hz: scalar(frequency_hz)?,
                    gain_db: scalar(gain_db)?,
                    q: scalar(q)?,
                },
                rate,
            ),
            Self::LowShelfQ {
                frequency_hz,
                gain_db,
                q,
            } => equalizer_node(
                PeqFilter::LowShelfQ {
                    frequency_hz: scalar(frequency_hz)?,
                    gain_db: scalar(gain_db)?,
                    q: scalar(q)?,
                },
                rate,
            ),
            Self::HighShelfQ {
                frequency_hz,
                gain_db,
                q,
            } => equalizer_node(
                PeqFilter::HighShelfQ {
                    frequency_hz: scalar(frequency_hz)?,
                    gain_db: scalar(gain_db)?,
                    q: scalar(q)?,
                },
                rate,
            ),
            Self::Gain {
                gain,
                scale,
                inverted,
                mute,
            } => {
                let limit = if scale == GainScale::Decibel {
                    150.0
                } else {
                    10.0
                };
                if !gain.is_finite() || gain.abs() > limit {
                    return Err(error(
                        "PARAMETER_RANGE",
                        "static gain exceeds the declared source range",
                    ));
                }
                let value = if mute {
                    0.0
                } else {
                    (if scale == GainScale::Decibel {
                        10.0_f64.powf(gain / 20.0)
                    } else {
                        gain
                    }) * if inverted { -1.0 } else { 1.0 }
                };
                Ok(NodeKind::Gain {
                    gain_linear: scalar(value)?,
                })
            }
        }
    }
}

fn equalizer_node(filter: PeqFilter, rate: u32) -> Result<NodeKind, ImportError> {
    filter.validate(rate).map_err(|_| {
        error(
            "PARAMETER_RANGE",
            "equalizer frequency, gain, or Q exceeds Core limits",
        )
    })?;
    Ok(NodeKind::Peq { filter })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDefinition {
    pub name: String,
    pub description: Option<String>,
    pub filter: Option<ImportedFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterStep {
    pub channels: Vec<usize>,
    pub bypassed: bool,
    pub names: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicationPolicy {
    PipelineOnlyRetainInactiveDefinitions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStatus {
    Validated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorrectionImport {
    pub schema_version: u32,
    pub status: ImportStatus,
    pub source: SourceProvenance,
    pub context: ImportContext,
    pub metadata: ImportMetadata,
    pub preamp_db: f64,
    pub preamp_explicit: bool,
    pub definitions: Vec<FilterDefinition>,
    pub pipeline: Vec<FilterStep>,
    pub application_policy: ApplicationPolicy,
    pub graph: GraphDocument,
    pub semantic_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportError {
    pub code: &'static str,
    pub message: &'static str,
    pub line: Option<usize>,
}
impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)?;
        if let Some(line) = self.line {
            write!(f, " at line {line}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ImportError {}
fn error(code: &'static str, message: &'static str) -> ImportError {
    ImportError {
        code,
        message,
        line: None,
    }
}
fn at_line(mut e: ImportError, line: usize) -> ImportError {
    e.line = Some(line);
    e
}
fn scalar(value: f64) -> Result<FiniteGraphScalar, ImportError> {
    FiniteGraphScalar::new(value)
        .map_err(|_| error("NONFINITE_VALUE", "numeric parameters must be finite"))
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Import explicit source syntax without reading any referenced file or applying its graph.
pub fn import_correction_text(
    bytes: &[u8],
    format: CorrectionFormat,
    context: ImportContext,
    metadata: ImportMetadata,
) -> Result<CorrectionImport, ImportError> {
    validate_context(context)?;
    validate_metadata(&metadata)?;
    if bytes.len() > MAX_CORRECTION_BYTES {
        return Err(error("RESOURCE_LIMIT", "correction input exceeds 64 KiB"));
    }
    let source = std::str::from_utf8(bytes)
        .map_err(|_| error("INVALID_ENCODING", "correction input must be UTF-8"))?;
    if source
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        || source.lines().any(|l| l.len() > 4096)
    {
        return Err(error(
            "RESOURCE_LIMIT",
            "correction text contains unsupported controls or an excessive line",
        ));
    }
    let mut imported = match format {
        CorrectionFormat::Rew | CorrectionFormat::AutoEq => {
            text::parse(source, format, context, metadata)?
        }
        CorrectionFormat::CamillaDsp => yaml::parse(source, context, metadata)?,
    };
    imported.source.sha256 = hash(bytes);
    imported.source.bytes = bytes.len();
    imported.graph = build_graph(&imported)?;
    imported.semantic_sha256 = semantic_hash(&imported)?;
    Ok(imported)
}

fn empty(
    format: CorrectionFormat,
    context: ImportContext,
    metadata: ImportMetadata,
) -> CorrectionImport {
    CorrectionImport {
        schema_version: 1,
        status: ImportStatus::Validated,
        source: SourceProvenance {
            format,
            sha256: String::new(),
            bytes: 0,
            application_version: None,
        },
        context,
        metadata,
        preamp_db: 0.0,
        preamp_explicit: false,
        definitions: Vec::new(),
        pipeline: Vec::new(),
        application_policy: ApplicationPolicy::PipelineOnlyRetainInactiveDefinitions,
        graph: GraphDocument {
            schema_version: 1,
            nodes: Vec::new(),
            edges: Vec::new(),
            output: NodeId::new("output").expect("static node identifier"),
        },
        semantic_sha256: String::new(),
    }
}

fn validate_context(context: ImportContext) -> Result<(), ImportError> {
    if context.sample_rate_hz == 0 || context.quantum_frames == 0 || context.quantum_frames > 8192 {
        return Err(error(
            "INVALID_CONTEXT",
            "positive rate and a quantum from 1 through 8192 are required",
        ));
    }
    Ok(())
}
fn validate_string(value: &str, limit: usize) -> Result<(), ImportError> {
    if value.len() > limit
        || value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
    {
        return Err(error(
            "RESOURCE_LIMIT",
            "metadata or label exceeds its declared bound",
        ));
    }
    Ok(())
}
fn validate_metadata(m: &ImportMetadata) -> Result<(), ImportError> {
    for value in [
        &m.headphone_model,
        &m.measurement_provider,
        &m.target,
        &m.parameter_set,
        &m.revision,
        &m.redistribution_license,
    ]
    .into_iter()
    .flatten()
    {
        validate_string(value, 256)?;
    }
    Ok(())
}
fn validate_name(name: &str) -> Result<(), ImportError> {
    validate_string(name, 128)?;
    if name.is_empty() || name.contains(['$', '/', '\\']) || name.chars().any(char::is_control) {
        return Err(error(
            "UNSUPPORTED_NAME",
            "filter labels cannot contain paths, substitutions, or controls",
        ));
    }
    Ok(())
}

fn semantic_hash(imported: &CorrectionImport) -> Result<String, ImportError> {
    let bytes = serde_json::to_vec(&(
        "earph-correction/1",
        imported.context,
        &imported.metadata,
        imported.preamp_db,
        imported.preamp_explicit,
        &imported.definitions,
        &imported.pipeline,
    ))
    .map_err(|_| error("INVALID_REPORT", "correction report cannot be serialized"))?;
    Ok(hash(&bytes))
}

fn build_graph(imported: &CorrectionImport) -> Result<GraphDocument, ImportError> {
    validate_context(imported.context)?;
    validate_metadata(&imported.metadata)?;
    if imported.definitions.is_empty() {
        return Err(error(
            "NO_FILTERS",
            "correction input contains no filter definitions",
        ));
    }
    if imported.definitions.len() > MAX_CORRECTION_FILTERS
        || imported.pipeline.len() > MAX_CORRECTION_FILTERS
        || !imported.preamp_db.is_finite()
        || imported.preamp_db.abs() > 150.0
    {
        return Err(error(
            "RESOURCE_LIMIT",
            "correction definitions, steps, or preamp exceed their limits",
        ));
    }
    if imported.source.format == CorrectionFormat::CamillaDsp
        && (imported.preamp_db != 0.0 || imported.preamp_explicit)
    {
        return Err(error(
            "INVALID_REPORT",
            "CamillaDSP fragments express gain through explicit filters",
        ));
    }
    if imported.source.format != CorrectionFormat::CamillaDsp {
        text::validate_report(imported)?;
    }
    let mut definitions = BTreeMap::new();
    for definition in &imported.definitions {
        validate_name(&definition.name)?;
        if let Some(description) = &definition.description {
            validate_string(description, 1024)?;
        }
        if definitions
            .insert(
                definition.name.as_str(),
                definition
                    .filter
                    .as_ref()
                    .map(|f| f.node(imported.context.sample_rate_hz))
                    .transpose()?,
            )
            .is_some()
        {
            return Err(error("DUPLICATE_FILTER", "filter names must be unique"));
        }
    }
    let mut channels: Vec<Vec<NodeKind>> = vec![Vec::new(); imported.context.channels.count()];
    let mut operations = 0_usize;
    for step in &imported.pipeline {
        if let Some(description) = &step.description {
            validate_string(description, 1024)?;
        }
        if step.names.len() > MAX_CORRECTION_FILTERS
            || step.channels.len() > channels.len()
            || step.channels.iter().any(|c| *c >= channels.len())
            || step.channels.iter().collect::<BTreeSet<_>>().len() != step.channels.len()
        {
            return Err(error(
                "INVALID_CHANNELS",
                "pipeline channels must be unique members of the declared layout",
            ));
        }
        operations = operations
            .checked_add(step.names.len())
            .ok_or_else(|| error("RESOURCE_LIMIT", "pipeline operation count overflow"))?;
        if operations > MAX_CORRECTION_FILTERS {
            return Err(error(
                "RESOURCE_LIMIT",
                "pipeline contains more than 32 filter references",
            ));
        }
        for name in &step.names {
            let kind = definitions
                .get(name.as_str())
                .ok_or_else(|| error("MISSING_FILTER", "pipeline names an undefined filter"))?;
            if !step.bypassed
                && let Some(kind) = kind
            {
                for ch in &step.channels {
                    channels[*ch].push(kind.clone());
                }
            }
        }
    }
    let format = PortFormat {
        domain: SampleDomain::Pcm,
        sample_rate_hz: imported.context.sample_rate_hz,
        channels: imported.context.channels,
        precision: PcmRepresentation::Float64,
        max_quantum_frames: imported.context.quantum_frames,
    };
    let mut graph = GraphDocument {
        schema_version: 1,
        nodes: Vec::with_capacity(MAX_GRAPH_NODES),
        edges: Vec::with_capacity(MAX_GRAPH_EDGES),
        output: NodeId::new("output").expect("static identifier"),
    };
    add_node(&mut graph, "input", NodeKind::Input, format)?;
    add_node(
        &mut graph,
        "preamp",
        NodeKind::Gain {
            gain_linear: scalar(10.0_f64.powf(imported.preamp_db / 20.0))?,
        },
        format,
    )?;
    add_edge(&mut graph, "input", "preamp", 0)?;
    if channels.len() == 1 || channels[0] == channels[1] {
        let mut previous = String::from("preamp");
        for (index, kind) in channels[0].iter().enumerate() {
            let name = format!("filter-{index}");
            add_node(&mut graph, &name, kind.clone(), format)?;
            add_edge(&mut graph, &previous, &name, 0)?;
            previous = name;
        }
        add_node(&mut graph, "output", NodeKind::Output, format)?;
        add_edge(&mut graph, &previous, "output", 0)?;
    } else {
        let mono = PortFormat {
            channels: ChannelLayout::Mono,
            ..format
        };
        add_node(&mut graph, "sum", NodeKind::Sum, format)?;
        for (ch, kinds) in channels.iter().enumerate() {
            let select = format!("channel-{ch}-select");
            add_node(
                &mut graph,
                &select,
                NodeKind::Matrix {
                    input_channels: ChannelLayout::Stereo,
                    coefficients: vec![scalar(f64::from(ch == 0))?, scalar(f64::from(ch == 1))?],
                },
                mono,
            )?;
            add_edge(&mut graph, "preamp", &select, 0)?;
            let mut previous = select;
            for (index, kind) in kinds.iter().enumerate() {
                let name = format!("channel-{ch}-filter-{index}");
                add_node(&mut graph, &name, kind.clone(), mono)?;
                add_edge(&mut graph, &previous, &name, 0)?;
                previous = name;
            }
            let output = format!("channel-{ch}-output");
            add_node(
                &mut graph,
                &output,
                NodeKind::Matrix {
                    input_channels: ChannelLayout::Mono,
                    coefficients: vec![scalar(f64::from(ch == 0))?, scalar(f64::from(ch == 1))?],
                },
                format,
            )?;
            add_edge(&mut graph, &previous, &output, 0)?;
            add_edge(&mut graph, &output, "sum", ch)?;
        }
        add_node(&mut graph, "output", NodeKind::Output, format)?;
        add_edge(&mut graph, "sum", "output", 0)?;
    }
    graph.validate_structure().map_err(|_| {
        error(
            "RESOURCE_LIMIT",
            "imported graph exceeds the Core structural budget",
        )
    })?;
    Ok(graph)
}

fn add_node(
    graph: &mut GraphDocument,
    name: &str,
    kind: NodeKind,
    format: PortFormat,
) -> Result<(), ImportError> {
    if graph.nodes.len() >= MAX_GRAPH_NODES {
        return Err(error(
            "RESOURCE_LIMIT",
            "imported graph exceeds the node budget",
        ));
    }
    graph.nodes.push(Node {
        id: NodeId::new(name)
            .map_err(|_| error("INVALID_REPORT", "invalid generated node identifier"))?,
        kind,
        format,
    });
    Ok(())
}
fn add_edge(
    graph: &mut GraphDocument,
    from: &str,
    to: &str,
    to_port: usize,
) -> Result<(), ImportError> {
    if graph.edges.len() >= MAX_GRAPH_EDGES {
        return Err(error(
            "RESOURCE_LIMIT",
            "imported graph exceeds the edge budget",
        ));
    }
    graph.edges.push(Edge {
        from: NodeId::new(from)
            .map_err(|_| error("INVALID_REPORT", "invalid generated source identifier"))?,
        to: NodeId::new(to)
            .map_err(|_| error("INVALID_REPORT", "invalid generated target identifier"))?,
        from_port: 0,
        to_port,
    });
    Ok(())
}

/// Validate semantic agreement before accepting a previously exported import report.
pub fn read_correction_report(bytes: &[u8]) -> Result<CorrectionImport, ImportError> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(error("RESOURCE_LIMIT", "correction report exceeds 256 KiB"));
    }
    let imported: CorrectionImport = serde_json::from_slice(bytes).map_err(|_| {
        error(
            "INVALID_REPORT",
            "correction report has invalid or unknown fields",
        )
    })?;
    validate_report(&imported)?;
    Ok(imported)
}
fn validate_report(imported: &CorrectionImport) -> Result<(), ImportError> {
    if imported.schema_version != 1
        || imported.source.sha256.len() != 64
        || !imported
            .source
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || imported.source.bytes > MAX_CORRECTION_BYTES
    {
        return Err(error(
            "INVALID_REPORT",
            "invalid correction schema or source identity",
        ));
    }
    if let Some(version) = &imported.source.application_version {
        validate_string(version, 128)?;
    }
    if semantic_hash(imported)? != imported.semantic_sha256
        || build_graph(imported)? != imported.graph
    {
        return Err(error(
            "INVALID_REPORT",
            "graph or semantic fingerprint differs from imported parameters",
        ));
    }
    Ok(())
}

/// Export the same source dialect without rounding its numeric controls.
pub fn export_correction_text(imported: &CorrectionImport) -> Result<String, ImportError> {
    validate_report(imported)?;
    let output = match imported.source.format {
        CorrectionFormat::Rew | CorrectionFormat::AutoEq => text::export(imported),
        CorrectionFormat::CamillaDsp => yaml::export(imported),
    }?;
    if output.len() > MAX_CORRECTION_BYTES {
        return Err(error(
            "RESOURCE_LIMIT",
            "exported correction text exceeds 64 KiB",
        ));
    }
    Ok(output)
}

/// Read only the requested regular file. The parser never opens paths inside its contents.
pub fn read_correction_file(path: &Path, limit: usize) -> Result<Vec<u8>, ImportError> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| {
        error(
            "SOURCE_UNAVAILABLE",
            "cannot open the requested correction file",
        )
    })?;
    let mut file = File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|_| error("SOURCE_UNAVAILABLE", "cannot inspect the correction file"))?;
    if !metadata.is_file() {
        return Err(error(
            "SOURCE_UNAVAILABLE",
            "correction input must be a regular file",
        ));
    }
    let size = usize::try_from(metadata.len())
        .map_err(|_| error("RESOURCE_LIMIT", "correction source size overflow"))?;
    if size > limit {
        return Err(error(
            "RESOURCE_LIMIT",
            "correction source exceeds its byte limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| error("RESOURCE_LIMIT", "cannot reserve correction source bytes"))?;
    bytes.resize(size, 0);
    file.rewind()
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|_| error("SOURCE_CHANGED", "correction source changed during reading"))?;
    if file
        .read(&mut [0_u8; 1])
        .map_err(|_| error("SOURCE_CHANGED", "cannot complete correction source read"))?
        != 0
    {
        return Err(error(
            "SOURCE_CHANGED",
            "correction source grew during reading",
        ));
    }
    Ok(bytes)
}
