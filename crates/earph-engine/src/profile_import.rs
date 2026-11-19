// ABOUTME: Prepares verified correction reports and FIR provenance through the owner worker.
// ABOUTME: Keeps original sources unpublished until the complete catalog passes admission.
use super::*;
use earph_core::{
    dsp_nodes::FirAssetReference,
    graph::{FiniteGraphScalar, QuantizationPolicy},
    ids::ProfileId,
    profile_control::{
        CorrectionNormalization, ProfileCorrectionContext, ProfileCorrectionInput, ProfileDraft,
    },
    profiles::{ProfileCorrectionKind, ProfileCorrectionReference},
};
use earph_media::{
    FirCorrectionManifest, MAX_FIR_MANIFEST_BYTES,
    correction_text::{
        CorrectionImport, ImportContext, ImportError, ImportMetadata, MAX_CORRECTION_BYTES,
        MAX_REPORT_BYTES, import_correction_text, read_correction_report,
    },
    read_fir_manifest,
};

const IMPORT_IO_BYTES: u64 = earph_media::MAX_FIR_WAV_BYTES as u64 + 8 * 1024 * 1024;
type LoadedCorrection = (
    Arc<Vec<ProfileCorrectionObject>>,
    Option<Arc<ProfileCorrectionPreview>>,
);

fn import_error(error: ImportError) -> EngineError {
    EngineError::new(
        match error.code {
            "RESOURCE_LIMIT" => ErrorCode::ResourceBudgetExceeded,
            "UNSUPPORTED_FILTER" | "UNSUPPORTED_FIELD" | "UNSUPPORTED_FORMAT" => {
                ErrorCode::CapabilityUnsupported
            }
            _ => ErrorCode::AssetInvalid,
        },
        error.message,
    )
}
fn io_admission(retained: u64, declared: u64) -> Result<(), EngineError> {
    if IMPORT_IO_BYTES > declared
        || retained
            .checked_add(IMPORT_IO_BYTES)
            .is_none_or(|n| n > LIMIT)
    {
        return Err(BUDGET);
    }
    Ok(())
}
pub(super) fn import_asset(
    runtime: &Path,
    path: &str,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<FirAssetReference, EngineError> {
    io_admission(retained, LIMIT)?;
    check(cancel)?;
    let reference = assets::import_owned(runtime, Path::new(path))?;
    check(cancel)?;
    Ok(reference)
}
fn verified_report(report_json: &[u8], source: &[u8]) -> Result<CorrectionImport, EngineError> {
    let report = read_correction_report(report_json).map_err(import_error)?;
    let rebuilt = import_correction_text(
        source,
        report.source.format,
        report.context,
        report.metadata.clone(),
    )
    .map_err(import_error)?;
    if rebuilt != report {
        return Err(INVALID);
    }
    Ok(report)
}
fn context(value: ImportContext) -> ProfileCorrectionContext {
    ProfileCorrectionContext {
        sample_rate_hz: value.sample_rate_hz,
        channels: value.channels,
        quantum_frames: value.quantum_frames,
    }
}
pub(super) fn preview(
    context: ProfileCorrectionContext,
    preamp_db: FiniteGraphScalar,
    bounds: Vec<f64>,
) -> Result<Arc<ProfileCorrectionPreview>, EngineError> {
    let bounds = bounds
        .into_iter()
        .map(FiniteGraphScalar::new)
        .collect::<Result<Vec<_>, _>>()?;
    let required = ProfileCorrectionPreview::minimum_headroom_db(&bounds);
    let value = ProfileCorrectionPreview {
        context,
        preamp_db,
        required_headroom_db: FiniteGraphScalar::new(required)?,
        channel_peak_bounds: bounds,
        normalization: CorrectionNormalization::Preserved,
    };
    value.validate()?;
    Ok(Arc::new(value))
}
fn json_object(
    kind: ProfileCorrectionKind,
    bytes: &[u8],
) -> Result<ProfileCorrectionObject, EngineError> {
    Ok(ProfileCorrectionObject {
        kind,
        object_sha256: hash(bytes)?,
        document_json: Arc::from(std::str::from_utf8(bytes).map_err(|_| INVALID)?),
    })
}
fn intent(graph: earph_core::graph::GraphDocument) -> Arc<GraphIntent> {
    Arc::new(GraphIntent {
        graph: Arc::new(graph),
        processing: GraphProcessingRequest::Float64 {
            quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
        },
    })
}
pub(super) fn prepare_import(
    runtime: &Path,
    id: ProfileId,
    revision: u64,
    draft: &ProfileDraft,
    input: &ProfileCorrectionInput,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<PreparedProfile, EngineError> {
    io_admission(retained, draft.latency.max_preparation_bytes)?;
    check(cancel)?;
    let (graph, object, context, preamp, source) = match input {
        ProfileCorrectionInput::TextReport {
            report_json,
            source_text,
        } => {
            let report = verified_report(report_json.as_bytes(), source_text.as_bytes())?;
            let bytes = serde_json::to_vec(&report).map_err(|_| INVALID)?;
            if bytes.len() > MAX_REPORT_BYTES {
                return Err(BUDGET);
            }
            let object = json_object(ProfileCorrectionKind::TextReport, &bytes)?;
            let preamp = FiniteGraphScalar::new(report.preamp_db)?;
            (
                intent(report.graph),
                object,
                context(report.context),
                preamp,
                Some(PreparedCorrectionSource::Text(
                    source_text.as_bytes().to_vec(),
                )),
            )
        }
        ProfileCorrectionInput::Fir {
            path,
            context: declared,
            metadata_json,
            preamp_db,
            ..
        } => {
            declared.validate()?;
            let metadata: ImportMetadata =
                serde_json::from_str(metadata_json).map_err(|_| INVALID)?;
            let source = assets::PreparedFirSource::read(Path::new(path))?;
            check(cancel)?;
            let manifest = FirCorrectionManifest::new(
                source.coefficients().reference().clone(),
                source.source_bytes(),
                ImportContext {
                    sample_rate_hz: declared.sample_rate_hz,
                    channels: declared.channels,
                    quantum_frames: declared.quantum_frames,
                },
                metadata,
                *preamp_db,
            )?;
            let bytes = serde_json::to_vec(&manifest).map_err(|_| INVALID)?;
            if bytes.len() > MAX_FIR_MANIFEST_BYTES {
                return Err(BUDGET);
            }
            (
                intent(manifest.graph()?),
                json_object(ProfileCorrectionKind::FirManifest, &bytes)?,
                *declared,
                *preamp_db,
                Some(PreparedCorrectionSource::Fir(source)),
            )
        }
    };
    check(cancel)?;
    let graph_bytes = serde_json::to_vec(&*graph).map_err(|_| INVALID)?;
    if graph_bytes.len() > MAX_GRAPH_INTENT_BYTES {
        return Err(BUDGET);
    }
    let mut profile = draft.document(id, revision, hash(&graph_bytes)?)?;
    profile.corrections.push(ProfileCorrectionReference {
        kind: object.kind,
        object_sha256: object.object_sha256,
    });
    let inputs = PreparedInputs::new(&profile, graph_bytes, source)?;
    let bounds =
        validate_prepared_intent(runtime, &profile, &graph, retained, cancel, Some(&inputs))?;
    let preview = preview(context, preamp, bounds)?;
    prepare_record(
        profile,
        graph,
        Arc::new(vec![object]),
        Some(preview),
        inputs,
    )
}
pub(super) fn load(
    runtime: &Path,
    profile: &ProfileDocument,
    graph: &GraphIntent,
    retained: u64,
    cancel: &AtomicBool,
) -> Result<LoadedCorrection, EngineError> {
    if profile.corrections.is_empty() {
        return Ok((Arc::new(Vec::new()), None));
    }
    io_admission(retained, profile.latency.max_preparation_bytes)?;
    let directory = directory(runtime, false)?;
    let (objects, context) = reconstruct(
        profile,
        graph,
        |hash, kind, limit| read_object(&directory, hash, kind, limit),
        |reference| {
            let (correction, size) = assets::resolve_with_size(runtime, reference)?;
            drop(correction);
            Ok(size)
        },
    )?;
    let bounds = validate_intent(runtime, profile, graph, retained, cancel)?;
    Ok((
        objects,
        context
            .map(|(context, preamp)| preview(context, preamp, bounds))
            .transpose()?,
    ))
}
type ReconstructedCorrection = (
    Arc<Vec<ProfileCorrectionObject>>,
    Option<(ProfileCorrectionContext, FiniteGraphScalar)>,
);
pub(super) fn reconstruct(
    profile: &ProfileDocument,
    graph: &GraphIntent,
    mut read: impl FnMut(ProfileObjectHash, &str, usize) -> Result<Vec<u8>, EngineError>,
    mut verify_fir: impl FnMut(&FirAssetReference) -> Result<usize, EngineError>,
) -> Result<ReconstructedCorrection, EngineError> {
    if profile.corrections.is_empty() {
        return Ok((Arc::new(Vec::new()), None));
    }
    if profile.corrections.len() != 1 {
        return Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "one imported correction chain is supported per profile",
        ));
    }
    let reference = &profile.corrections[0];
    let (bytes, expected, context, preamp) = match reference.kind {
        ProfileCorrectionKind::TextReport => {
            let bytes = read(reference.object_sha256, "text-report", MAX_REPORT_BYTES)?;
            let report = read_correction_report(&bytes).map_err(import_error)?;
            let source = read(
                ProfileObjectHash::try_from(report.source.sha256.clone())?,
                "text-source",
                MAX_CORRECTION_BYTES,
            )?;
            let report = verified_report(&bytes, &source)?;
            (
                bytes,
                intent(report.graph),
                context(report.context),
                FiniteGraphScalar::new(report.preamp_db)?,
            )
        }
        ProfileCorrectionKind::FirManifest => {
            let bytes = read(
                reference.object_sha256,
                "fir-manifest",
                MAX_FIR_MANIFEST_BYTES,
            )?;
            let manifest = read_fir_manifest(&bytes)?;
            let size = verify_fir(&manifest.asset)?;
            if size != manifest.source_bytes {
                return Err(INVALID);
            }
            (
                bytes,
                intent(manifest.graph()?),
                context(manifest.context),
                manifest.preamp_db,
            )
        }
    };
    if expected.as_ref() != graph {
        return Err(INVALID);
    }
    let object = json_object(reference.kind, &bytes)?;
    Ok((Arc::new(vec![object]), Some((context, preamp))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_admission_refuses_insufficient_declared_scratch_before_parsing() {
        let mut draft: ProfileDraft = serde_json::from_value(serde_json::json!({"name":"Owned", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"owned","manual_setup":null},
            "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":1,"max_preparation_bytes":1,"max_latency_frames":null},"corrections":[]})).unwrap();
        let input = ProfileCorrectionInput::TextReport {
            report_json: "not JSON".into(),
            source_text: "unparsed source".into(),
        };
        let result = prepare_import(
            Path::new("/unused"),
            ProfileId::new("owned").unwrap(),
            1,
            &draft,
            &input,
            0,
            &AtomicBool::new(false),
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::ResourceBudgetExceeded);
        let mut profile = draft
            .document(
                ProfileId::new("owned").unwrap(),
                1,
                ProfileObjectHash::try_from("0".repeat(64)).unwrap(),
            )
            .unwrap();
        profile.corrections.push(ProfileCorrectionReference {
            kind: ProfileCorrectionKind::TextReport,
            object_sha256: ProfileObjectHash::try_from("0".repeat(64)).unwrap(),
        });
        let graph:GraphIntent = serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"output":"out","nodes":[],"edges":[]},"processing":{"kind":"integer_bypass"}})).unwrap();
        assert_eq!(
            load(
                Path::new("/unused"),
                &profile,
                &graph,
                0,
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .code,
            ErrorCode::ResourceBudgetExceeded
        );
        draft.latency.max_preparation_bytes = LIMIT;
        assert_eq!(
            prepare_import(
                Path::new("/unused"),
                ProfileId::new("owned").unwrap(),
                1,
                &draft,
                &input,
                0,
                &AtomicBool::new(false)
            )
            .unwrap_err()
            .code,
            ErrorCode::AssetInvalid
        );
    }
}
