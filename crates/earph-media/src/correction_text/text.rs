// ABOUTME: Parses the documented REW Generic and AutoEq parametric text subsets.
// ABOUTME: Retains disabled filters and rejects alternate controls rather than approximating them.
use super::*;

pub(super) fn parse(
    source: &str,
    format: CorrectionFormat,
    context: ImportContext,
    metadata: ImportMetadata,
) -> Result<CorrectionImport, ImportError> {
    let mut imported = empty(format, context, metadata);
    let (mut header, mut equaliser, mut measurement_label) = (false, false, false);
    for (index, line) in source.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let result = (|| {
            if format == CorrectionFormat::Rew && line == "Filter Settings file" {
                if header || !imported.definitions.is_empty() {
                    return Err(error(
                        "MALFORMED_FORMAT",
                        "one REW header must precede its filters",
                    ));
                }
                header = true;
                return Ok(());
            }
            if line.starts_with("Preamp:") {
                if imported.preamp_explicit || !imported.definitions.is_empty() {
                    return Err(error(
                        "INVALID_PREAMP",
                        "one preamp must precede the filters",
                    ));
                }
                let tokens: Vec<_> = line.split_ascii_whitespace().collect();
                let mut position = 1;
                if tokens.first() != Some(&"Preamp:") {
                    return Err(error("MALFORMED_FORMAT", "invalid preamp field"));
                }
                imported.preamp_db = quantity(&tokens, &mut position, "dB")?;
                if position != tokens.len() {
                    return Err(error("MALFORMED_FORMAT", "unexpected preamp controls"));
                }
                imported.preamp_explicit = true;
                return Ok(());
            }
            if line.starts_with("Filter ") {
                if header && !equaliser {
                    return Err(error(
                        "UNSUPPORTED_EQUALISER",
                        "a REW header must select Generic before its filters",
                    ));
                }
                if format == CorrectionFormat::AutoEq && !imported.preamp_explicit {
                    return Err(error(
                        "INVALID_PREAMP",
                        "AutoEq parametric settings require an explicit preamp",
                    ));
                }
                if imported.definitions.len() >= MAX_CORRECTION_FILTERS {
                    return Err(error(
                        "RESOURCE_LIMIT",
                        "text input contains more than 32 filter definitions",
                    ));
                }
                let tokens: Vec<_> = line.split_ascii_whitespace().collect();
                if tokens.len() < 4 || tokens[0] != "Filter" {
                    return Err(error("MALFORMED_FORMAT", "invalid filter line"));
                }
                let number = tokens[1]
                    .strip_suffix(':')
                    .and_then(|n| n.parse::<usize>().ok())
                    .filter(|n| *n > 0 && *n <= 999)
                    .ok_or_else(|| {
                        error(
                            "MALFORMED_FORMAT",
                            "filter identifiers must be positive decimal numbers through 999",
                        )
                    })?;
                let bypassed = match tokens[2] {
                    "ON" => false,
                    "OFF" => true,
                    _ => return Err(error("MALFORMED_FORMAT", "filter state must be ON or OFF")),
                };
                let filter = match tokens[3] {
                    "None" if tokens.len() == 4 && format == CorrectionFormat::Rew => None,
                    "PK" | "LSC" | "HSC"
                        if tokens[3] == "PK" || format == CorrectionFormat::AutoEq =>
                    {
                        if tokens[3] != "PK" && tokens.get(4) != Some(&"Fc") {
                            return Err(error(
                                "UNSUPPORTED_FILTER",
                                "shelf slope forms are unsupported; use explicit center frequency and Q",
                            ));
                        }
                        let mut position = 4;
                        word(&tokens, &mut position, "Fc")?;
                        let frequency_hz = quantity(&tokens, &mut position, "Hz")?;
                        word(&tokens, &mut position, "Gain")?;
                        let gain_db = quantity(&tokens, &mut position, "dB")?;
                        word(&tokens, &mut position, "Q")?;
                        let q =
                            number_value(tokens.get(position).ok_or_else(|| {
                                error("MALFORMED_FORMAT", "missing equalizer Q")
                            })?)?;
                        position += 1;
                        if position != tokens.len() {
                            return Err(error("MALFORMED_FORMAT", "unexpected equalizer controls"));
                        }
                        Some(match tokens[3] {
                            "LSC" => ImportedFilter::LowShelfQ {
                                frequency_hz,
                                gain_db,
                                q,
                            },
                            "HSC" => ImportedFilter::HighShelfQ {
                                frequency_hz,
                                gain_db,
                                q,
                            },
                            _ => ImportedFilter::Peaking {
                                frequency_hz,
                                gain_db,
                                q,
                            },
                        })
                    }
                    "LSC" | "HSC" | "LS" | "HS" | "LSQ" | "HSQ" => {
                        return Err(error(
                            "UNSUPPORTED_FILTER",
                            "only explicit AutoEq LSC/HSC Q shelf forms are supported",
                        ));
                    }
                    _ => {
                        return Err(error(
                            "UNSUPPORTED_FILTER",
                            "filter type is unsupported by the selected text dialect",
                        ));
                    }
                };
                let name = number.to_string();
                if imported.definitions.iter().any(|d| d.name == name) {
                    return Err(error(
                        "DUPLICATE_FILTER",
                        "filter identifiers must be unique",
                    ));
                }
                imported.definitions.push(FilterDefinition {
                    name: name.clone(),
                    description: None,
                    filter,
                });
                imported.pipeline.push(FilterStep {
                    channels: (0..context.channels.count()).collect(),
                    bypassed,
                    names: vec![name],
                    description: None,
                });
                return Ok(());
            }
            if format == CorrectionFormat::Rew && imported.definitions.is_empty() {
                if let Some(version) = line.strip_prefix("Room EQ V") {
                    if !header
                        || !version.starts_with(|c: char| c.is_ascii_digit())
                        || !version
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || " .-_()".contains(c))
                        || imported.source.application_version.is_some()
                    {
                        return Err(error("MALFORMED_FORMAT", "invalid REW application header"));
                    }
                    validate_string(version, 128)?;
                    imported.source.application_version = Some(version.into());
                    return Ok(());
                }
                if let Some(value) = line.strip_prefix("Equaliser:") {
                    if value.trim() != "Generic" || equaliser {
                        return Err(error(
                            "UNSUPPORTED_EQUALISER",
                            "only the REW Generic PK text dialect is supported",
                        ));
                    }
                    equaliser = true;
                    return Ok(());
                }
                if header
                    && (line.starts_with("Dated:")
                        || line.starts_with("Notes:")
                        || line.starts_with("Target level:")
                        || line == "Full range"
                        || line.starts_with("Bass limited ")
                        || line.starts_with("Subwoofer "))
                {
                    return Ok(());
                }
                if header
                    && equaliser
                    && !measurement_label
                    && !line.contains([':', '/', '\\'])
                    && !["Preamp", "Filter", "Include", "Channel", "Device", "Copy"]
                        .iter()
                        .any(|prefix| line.starts_with(prefix))
                {
                    measurement_label = true;
                    return Ok(());
                }
            }
            Err(error(
                "UNKNOWN_FIELD",
                "unknown text line or unsupported include, channel, or processing directive",
            ))
        })();
        result.map_err(|e| at_line(e, index + 1))?;
    }
    if format == CorrectionFormat::AutoEq && !imported.preamp_explicit {
        return Err(error(
            "INVALID_PREAMP",
            "AutoEq parametric settings require an explicit preamp",
        ));
    }
    Ok(imported)
}

fn word(tokens: &[&str], position: &mut usize, wanted: &str) -> Result<(), ImportError> {
    if tokens.get(*position) != Some(&wanted) {
        return Err(error(
            "MALFORMED_FORMAT",
            "missing or unsupported numeric control",
        ));
    }
    *position += 1;
    Ok(())
}
fn quantity(tokens: &[&str], position: &mut usize, unit: &str) -> Result<f64, ImportError> {
    let token = tokens
        .get(*position)
        .ok_or_else(|| error("MALFORMED_FORMAT", "missing numeric quantity"))?;
    *position += 1;
    if let Some(number) = token.strip_suffix(unit)
        && !number.is_empty()
    {
        return number_value(number);
    }
    let value = number_value(token)?;
    word(tokens, position, unit)?;
    Ok(value)
}
fn number_value(token: &str) -> Result<f64, ImportError> {
    if token.len() > 64 {
        return Err(error("RESOURCE_LIMIT", "numeric token exceeds 64 bytes"));
    }
    let value = token.parse::<f64>().map_err(|_| {
        error(
            "INVALID_NUMBER",
            "expected an explicit decimal numeric value",
        )
    })?;
    if !value.is_finite() {
        return Err(error(
            "NONFINITE_VALUE",
            "numeric parameters must be finite",
        ));
    }
    Ok(value)
}

pub(super) fn validate_report(imported: &CorrectionImport) -> Result<(), ImportError> {
    if imported.pipeline.len() != imported.definitions.len()
        || (!imported.preamp_explicit && imported.preamp_db != 0.0)
        || (imported.source.format == CorrectionFormat::AutoEq && !imported.preamp_explicit)
    {
        return Err(error(
            "INVALID_REPORT",
            "text reports require the source preamp and one ordered step per definition",
        ));
    }
    for (definition, step) in imported.definitions.iter().zip(&imported.pipeline) {
        let representable = match &definition.filter {
            Some(ImportedFilter::Peaking { .. }) => true,
            Some(ImportedFilter::LowShelfQ { .. } | ImportedFilter::HighShelfQ { .. }) => {
                imported.source.format == CorrectionFormat::AutoEq
            }
            None => imported.source.format == CorrectionFormat::Rew,
            Some(ImportedFilter::Gain { .. }) => false,
        };
        if step.names != [definition.name.clone()]
            || step.channels != (0..imported.context.channels.count()).collect::<Vec<_>>()
            || step.description.is_some()
            || definition.description.is_some()
            || definition
                .name
                .parse::<usize>()
                .ok()
                .filter(|n| *n > 0 && *n <= 999 && n.to_string() == definition.name)
                .is_none()
            || !representable
        {
            return Err(error(
                "INVALID_REPORT",
                "text report controls cannot be represented by its source dialect",
            ));
        }
    }
    Ok(())
}

pub(super) fn export(imported: &CorrectionImport) -> Result<String, ImportError> {
    let mut output = String::new();
    if imported.preamp_explicit {
        output.push_str(&format!("Preamp: {} dB\n", imported.preamp_db));
    }
    for (definition, step) in imported.definitions.iter().zip(&imported.pipeline) {
        let state = if step.bypassed { "OFF" } else { "ON" };
        match &definition.filter {
            Some(ImportedFilter::Peaking {
                frequency_hz,
                gain_db,
                q,
            }) => output.push_str(&format!(
                "Filter {}: {state} PK Fc {frequency_hz} Hz Gain {gain_db} dB Q {q}\n",
                definition.name
            )),
            Some(ImportedFilter::LowShelfQ {
                frequency_hz,
                gain_db,
                q,
            }) => output.push_str(&format!(
                "Filter {}: {state} LSC Fc {frequency_hz} Hz Gain {gain_db} dB Q {q}\n",
                definition.name
            )),
            Some(ImportedFilter::HighShelfQ {
                frequency_hz,
                gain_db,
                q,
            }) => output.push_str(&format!(
                "Filter {}: {state} HSC Fc {frequency_hz} Hz Gain {gain_db} dB Q {q}\n",
                definition.name
            )),
            None if imported.source.format == CorrectionFormat::Rew => {
                output.push_str(&format!("Filter {}: {state} None\n", definition.name))
            }
            _ => {
                return Err(error(
                    "INVALID_REPORT",
                    "text export cannot represent this filter kind",
                ));
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_controls_preserve_precision_and_reject_locale_or_nonfinite_values() {
        assert_eq!(
            number_value("0.12345678901234566").unwrap().to_bits(),
            0.12345678901234566_f64.to_bits()
        );
        for token in ["NaN", "inf", "-inf", "1e309"] {
            assert_eq!(number_value(token).unwrap_err().code, "NONFINITE_VALUE");
        }
        assert_eq!(number_value("0,7").unwrap_err().code, "INVALID_NUMBER");
        assert_eq!(
            number_value(&"1".repeat(65)).unwrap_err().code,
            "RESOURCE_LIMIT"
        );
    }

    #[test]
    fn edited_reports_reject_text_ids_that_change_during_export_and_reimport() {
        let context = ImportContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Mono,
            quantum_frames: 64,
        };
        let source = b"Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 1\n";
        let original = import_correction_text(
            source,
            CorrectionFormat::AutoEq,
            context,
            ImportMetadata::default(),
        )
        .unwrap();
        for name in ["01", "+1"] {
            let mut edited = original.clone();
            edited.definitions[0].name = name.into();
            edited.pipeline[0].names[0] = name.into();
            edited.semantic_sha256 = semantic_hash(&edited).unwrap();
            assert_eq!(
                read_correction_report(&serde_json::to_vec(&edited).unwrap())
                    .unwrap_err()
                    .code,
                "INVALID_REPORT"
            );
        }
    }
}
