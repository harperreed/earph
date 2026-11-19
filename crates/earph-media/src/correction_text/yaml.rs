// ABOUTME: Adapts real YAML parser events into a bounded CamillaDSP filter fragment.
// ABOUTME: Rejects aliases, tags, duplicate fields, and unsupported processing without executing source directives.
use super::*;
use serde_json::{Map, Number, Value, json};
use yaml_rust2::{
    parser::{Event, Parser},
    scanner::TScalarStyle,
};

const MAX_YAML_EVENTS: usize = 4096;
const MAX_YAML_DEPTH: usize = 12;

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    events: usize,
}
impl<'a> Reader<'a> {
    fn next(&mut self) -> Result<Event, ImportError> {
        self.events += 1;
        if self.events > MAX_YAML_EVENTS {
            return Err(error("RESOURCE_LIMIT", "YAML exceeds 4096 parser events"));
        }
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|_| error("MALFORMED_YAML", "invalid YAML syntax"))
    }
    fn value(&mut self, event: Event, depth: usize) -> Result<Value, ImportError> {
        if depth > MAX_YAML_DEPTH {
            return Err(error(
                "RESOURCE_LIMIT",
                "YAML nesting exceeds twelve levels",
            ));
        }
        match event {
            Event::Scalar(value, style, anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(error(
                        "UNSUPPORTED_YAML",
                        "YAML anchors and tags are unsupported",
                    ));
                }
                if value.len() > 1024 {
                    return Err(error("RESOURCE_LIMIT", "YAML scalar exceeds 1024 bytes"));
                }
                if style != TScalarStyle::Plain {
                    return Ok(Value::String(value));
                }
                match value.as_str() {
                    "" | "null" | "Null" | "NULL" | "~" => return Ok(Value::Null),
                    "true" | "True" | "TRUE" => return Ok(Value::Bool(true)),
                    "false" | "False" | "FALSE" => return Ok(Value::Bool(false)),
                    ".nan" | ".NaN" | ".NAN" | ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf"
                    | "+.INF" | "-.inf" | "-.Inf" | "-.INF" => {
                        return Err(error("NONFINITE_VALUE", "YAML numbers must be finite"));
                    }
                    _ => {}
                }
                if let Ok(v) = value.parse::<i64>() {
                    return Ok(Value::Number(v.into()));
                }
                if let Ok(v) = value.parse::<u64>() {
                    return Ok(Value::Number(v.into()));
                }
                if let Ok(v) = value.parse::<f64>() {
                    return Number::from_f64(v)
                        .map(Value::Number)
                        .ok_or_else(|| error("NONFINITE_VALUE", "YAML numbers must be finite"));
                }
                Ok(Value::String(value))
            }
            Event::MappingStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(error(
                        "UNSUPPORTED_YAML",
                        "YAML anchors and tags are unsupported",
                    ));
                }
                let mut map = Map::new();
                loop {
                    let event = self.next()?;
                    if event == Event::MappingEnd {
                        break;
                    }
                    let key = match self.value(event, depth + 1)? {
                        Value::String(s) => s,
                        _ => {
                            return Err(error(
                                "UNSUPPORTED_YAML",
                                "YAML mapping keys must be strings",
                            ));
                        }
                    };
                    if key == "<<" {
                        return Err(error("UNSUPPORTED_YAML", "YAML merge keys are unsupported"));
                    }
                    if map.contains_key(&key) {
                        return Err(error("DUPLICATE_FIELD", "YAML mapping keys must be unique"));
                    }
                    let event = self.next()?;
                    let value = self.value(event, depth + 1)?;
                    map.insert(key, value);
                }
                Ok(Value::Object(map))
            }
            Event::SequenceStart(anchor, tag) => {
                if anchor != 0 || tag.is_some() {
                    return Err(error(
                        "UNSUPPORTED_YAML",
                        "YAML anchors and tags are unsupported",
                    ));
                }
                let mut values = Vec::new();
                loop {
                    let event = self.next()?;
                    if event == Event::SequenceEnd {
                        break;
                    }
                    values.push(self.value(event, depth + 1)?);
                }
                Ok(Value::Array(values))
            }
            Event::Alias(_) => Err(error("UNSUPPORTED_YAML", "YAML aliases are unsupported")),
            _ => Err(error("MALFORMED_YAML", "unexpected YAML document boundary")),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fragment {
    filters: BTreeMap<String, Value>,
    pipeline: Vec<Value>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Definition {
    #[serde(rename = "type")]
    kind: String,
    parameters: Value,
    #[serde(default)]
    description: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QEqualizer {
    #[serde(rename = "type")]
    kind: String,
    freq: f64,
    gain: f64,
    q: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gain {
    gain: f64,
    #[serde(default)]
    scale: Option<GainScale>,
    #[serde(default)]
    inverted: Option<bool>,
    #[serde(default)]
    mute: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    channels: Option<Vec<usize>>,
    names: Vec<String>,
    #[serde(default)]
    bypassed: Option<bool>,
    #[serde(default)]
    description: Option<String>,
}

fn typed<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ImportError> {
    serde_json::from_value(value).map_err(|_| {
        error(
            "MALFORMED_FORMAT",
            "CamillaDSP fragment has missing, duplicate, unknown, or wrongly typed fields",
        )
    })
}

pub(super) fn parse(
    source: &str,
    context: ImportContext,
    metadata: ImportMetadata,
) -> Result<CorrectionImport, ImportError> {
    if source.lines().any(|line| line.starts_with('%')) {
        return Err(error("UNSUPPORTED_YAML", "YAML directives are unsupported"));
    }
    let mut reader = Reader {
        parser: Parser::new_from_str(source),
        events: 0,
    };
    if reader.next() != Ok(Event::StreamStart) || reader.next() != Ok(Event::DocumentStart) {
        return Err(error("MALFORMED_YAML", "expected one YAML document"));
    }
    let first = reader.next()?;
    let document = reader.value(first, 0)?;
    if reader.next() != Ok(Event::DocumentEnd) || reader.next() != Ok(Event::StreamEnd) {
        return Err(error(
            "UNSUPPORTED_YAML",
            "exactly one YAML document is supported",
        ));
    }
    let fragment: Fragment = typed(document)?;
    if fragment.filters.len() > MAX_CORRECTION_FILTERS
        || fragment.pipeline.len() > MAX_CORRECTION_FILTERS
    {
        return Err(error(
            "RESOURCE_LIMIT",
            "CamillaDSP definitions or steps exceed 32",
        ));
    }
    let mut imported = empty(CorrectionFormat::CamillaDsp, context, metadata);
    for (name, value) in fragment.filters {
        validate_name(&name)?;
        let definition: Definition = typed(value)?;
        let filter = match definition.kind.as_str() {
            "Biquad" => {
                let kind = definition
                    .parameters
                    .get("type")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        error("MALFORMED_FORMAT", "a biquad requires an explicit type")
                    })?;
                if definition.parameters.get("slope").is_some() {
                    return Err(error(
                        "UNSUPPORTED_FILTER",
                        "shelf slope controls require a separately qualified design; use explicit Q",
                    ));
                }
                if !matches!(kind, "Peaking" | "Lowshelf" | "Highshelf") {
                    return Err(error(
                        "UNSUPPORTED_FILTER",
                        "only Peaking, Lowshelf, and Highshelf biquads with explicit Q are supported",
                    ));
                }
                if definition.parameters.get("bandwidth").is_some() {
                    return Err(error(
                        "UNSUPPORTED_FILTER",
                        "bandwidth controls require a separately qualified conversion",
                    ));
                }
                let parameters: QEqualizer = typed(definition.parameters)?;
                match parameters.kind.as_str() {
                    "Lowshelf" => ImportedFilter::LowShelfQ {
                        frequency_hz: parameters.freq,
                        gain_db: parameters.gain,
                        q: parameters.q,
                    },
                    "Highshelf" => ImportedFilter::HighShelfQ {
                        frequency_hz: parameters.freq,
                        gain_db: parameters.gain,
                        q: parameters.q,
                    },
                    _ => ImportedFilter::Peaking {
                        frequency_hz: parameters.freq,
                        gain_db: parameters.gain,
                        q: parameters.q,
                    },
                }
            }
            "Gain" => {
                let parameters: Gain = typed(definition.parameters)?;
                ImportedFilter::Gain {
                    gain: parameters.gain,
                    scale: parameters.scale.unwrap_or(GainScale::Decibel),
                    inverted: parameters.inverted.unwrap_or(false),
                    mute: parameters.mute.unwrap_or(false),
                }
            }
            _ => {
                return Err(error(
                    "UNSUPPORTED_FILTER",
                    "only Q-controlled equalizer biquads and static Gain filters are supported",
                ));
            }
        };
        imported.definitions.push(FilterDefinition {
            name,
            description: definition.description,
            filter: Some(filter),
        });
    }
    for value in fragment.pipeline {
        if value.get("type").and_then(Value::as_str) != Some("Filter") {
            return Err(error(
                "UNSUPPORTED_PIPELINE",
                "only ordered Filter pipeline steps are supported",
            ));
        }
        let step: Step = typed(value)?;
        if step.kind != "Filter" {
            return Err(error("UNSUPPORTED_PIPELINE", "unsupported pipeline step"));
        }
        imported.pipeline.push(FilterStep {
            channels: step
                .channels
                .unwrap_or_else(|| (0..context.channels.count()).collect()),
            bypassed: step.bypassed.unwrap_or(false),
            names: step.names,
            description: step.description,
        });
    }
    Ok(imported)
}

/// JSON is a YAML 1.2 subset and preserves all normalized fragment parameters without an emitter heuristic.
pub(super) fn export(imported: &CorrectionImport) -> Result<String, ImportError> {
    let mut filters = Map::new();
    for definition in &imported.definitions {
        let mut value = match definition.filter.as_ref().ok_or_else(|| {
            error(
                "INVALID_REPORT",
                "CamillaDSP definitions cannot contain empty REW slots",
            )
        })? {
            ImportedFilter::Peaking {
                frequency_hz,
                gain_db,
                q,
            } => {
                json!({"type":"Biquad","parameters":{"type":"Peaking","freq":frequency_hz,"gain":gain_db,"q":q}})
            }
            ImportedFilter::LowShelfQ {
                frequency_hz,
                gain_db,
                q,
            } => {
                json!({"type":"Biquad","parameters":{"type":"Lowshelf","freq":frequency_hz,"gain":gain_db,"q":q}})
            }
            ImportedFilter::HighShelfQ {
                frequency_hz,
                gain_db,
                q,
            } => {
                json!({"type":"Biquad","parameters":{"type":"Highshelf","freq":frequency_hz,"gain":gain_db,"q":q}})
            }
            ImportedFilter::Gain {
                gain,
                scale,
                inverted,
                mute,
            } => {
                json!({"type":"Gain","parameters":{"gain":gain,"scale":scale,"inverted":inverted,"mute":mute}})
            }
        };
        if let Some(description) = &definition.description {
            value["description"] = json!(description);
        }
        filters.insert(definition.name.clone(), value);
    }
    let pipeline: Vec<_> = imported.pipeline.iter().map(|step| {
        let mut value = json!({"type":"Filter","channels":step.channels,"names":step.names,"bypassed":step.bypassed});
        if let Some(description) = &step.description {
            value["description"] = json!(description);
        }
        value
    }).collect();
    serde_json::to_string_pretty(&json!({"filters":filters,"pipeline":pipeline}))
        .map_err(|_| error("INVALID_REPORT", "cannot export CamillaDSP parameters"))
}
