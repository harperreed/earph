// ABOUTME: Reads bounded correction sources through the shared media parsers on one owned worker.
// ABOUTME: Retains exact text bytes and reports actual cancellation retirement without modifying sources.
use earph_core::{
    dsp_nodes::FirPurpose,
    graph::FiniteGraphScalar,
    profile_control::{ProfileCorrectionContext, ProfileCorrectionInput},
};
use earph_media::{
    FirCorrectionManifest,
    correction_text::{self, CorrectionFormat, CorrectionImport, ImportContext, ImportMetadata},
    decode_fir_wav, read_fir_source,
};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrectionSource {
    Text {
        text: String,
        format: CorrectionFormat,
    },
    TextFile {
        path: String,
        format: CorrectionFormat,
    },
    Fir {
        path: String,
        purpose: FirPurpose,
    },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrectionPreviewRequest {
    pub source: CorrectionSource,
    pub context: ImportContext,
    pub metadata: ImportMetadata,
    pub fir_preamp: Option<FiniteGraphScalar>,
}
impl CorrectionPreviewRequest {
    /// Reconstruct explicit new source text; historical source bytes are unavailable here.
    pub fn from_text_report(bytes: &[u8]) -> Result<Self, String> {
        let report = correction_text::read_correction_report(bytes).map_err(|e| e.to_string())?;
        let text = correction_text::export_correction_text(&report).map_err(|e| e.to_string())?;
        Ok(Self {
            source: CorrectionSource::Text {
                text,
                format: report.source.format,
            },
            context: report.context,
            metadata: report.metadata,
            fir_preamp: None,
        })
    }
}
#[derive(Debug, Clone, PartialEq)]
pub enum CorrectionPreview {
    Text {
        report: Box<CorrectionImport>,
        source_text: String,
    },
    Fir {
        manifest: Box<FirCorrectionManifest>,
        path: String,
    },
}
impl CorrectionPreview {
    pub fn source_sha256(&self) -> &str {
        match self {
            Self::Text { report, .. } => &report.source.sha256,
            Self::Fir { manifest, .. } => manifest.asset.sha256.as_str(),
        }
    }
    pub fn input(&self) -> ProfileCorrectionInput {
        match self {
            Self::Text {
                report,
                source_text,
            } => ProfileCorrectionInput::TextReport {
                report_json: serde_json::to_string(report).expect("typed report serialization"),
                source_text: source_text.clone(),
            },
            Self::Fir { manifest, path } => ProfileCorrectionInput::Fir {
                path: path.clone(),
                purpose: manifest.asset.purpose,
                context: ProfileCorrectionContext {
                    sample_rate_hz: manifest.context.sample_rate_hz,
                    channels: manifest.context.channels,
                    quantum_frames: manifest.context.quantum_frames,
                },
                metadata_json: serde_json::to_string(&manifest.metadata)
                    .expect("typed metadata serialization"),
                preamp_db: manifest.preamp_db,
            },
        }
    }
}
/// One bounded, joinable read/parse job. Cancel never claims the thread has retired.
#[derive(Debug)]
pub struct CorrectionWorker {
    cancel: Arc<AtomicBool>,
    join: Option<JoinHandle<Result<CorrectionPreview, String>>>,
}
impl CorrectionWorker {
    pub fn start(request: CorrectionPreviewRequest) -> Result<Self, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let join = thread::Builder::new()
            .name("earph-correction-preview".into())
            .spawn(move || {
                check(&token)?;
                let result = preview(request);
                check(&token)?;
                result
            })
            .map_err(|e| format!("Cannot start correction preview: {e}"))?;
        Ok(Self {
            cancel,
            join: Some(join),
        })
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn retired(&self) -> bool {
        self.join.is_none()
    }
    pub fn poll(&mut self) -> Option<Result<CorrectionPreview, String>> {
        if !self.join.as_ref().is_some_and(|j| j.is_finished()) {
            return None;
        }
        let result = self
            .join
            .take()
            .unwrap()
            .join()
            .unwrap_or_else(|_| Err("Correction preview worker panicked".into()));
        Some(if self.cancel.load(Ordering::Acquire) {
            Err("Correction preview canceled; worker retired".into())
        } else {
            result
        })
    }
}
impl Drop for CorrectionWorker {
    fn drop(&mut self) {
        self.cancel();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn check(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Correction preview canceled; worker retired".into())
    } else {
        Ok(())
    }
}
fn preview(request: CorrectionPreviewRequest) -> Result<CorrectionPreview, String> {
    match request.source {
        CorrectionSource::Text { text, format } => {
            text_preview(text.into_bytes(), format, request.context, request.metadata)
        }
        CorrectionSource::TextFile { path, format } => {
            validate_path(&path)?;
            let bytes = correction_text::read_correction_file(
                Path::new(&path),
                correction_text::MAX_CORRECTION_BYTES,
            )
            .map_err(|e| e.to_string())?;
            text_preview(bytes, format, request.context, request.metadata)
        }
        CorrectionSource::Fir { path, purpose } => {
            validate_path(&path)?;
            let preamp = request
                .fir_preamp
                .ok_or("FIR requires an explicit finite preamp")?;
            let bytes = read_fir_source(Path::new(&path)).map_err(|e| e.to_string())?;
            let correction = decode_fir_wav(&bytes).map_err(|e| e.to_string())?;
            if correction.reference.purpose != purpose {
                return Err("FIR purpose differs from the requested correction purpose".into());
            }
            let manifest = FirCorrectionManifest::new(
                correction.reference,
                bytes.len(),
                request.context,
                request.metadata,
                preamp,
            )
            .map_err(|e| e.to_string())?;
            Ok(CorrectionPreview::Fir {
                manifest: Box::new(manifest),
                path,
            })
        }
    }
}
fn text_preview(
    bytes: Vec<u8>,
    format: CorrectionFormat,
    context: ImportContext,
    metadata: ImportMetadata,
) -> Result<CorrectionPreview, String> {
    let report = correction_text::import_correction_text(&bytes, format, context, metadata)
        .map_err(|e| e.to_string())?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if report.source.sha256 != digest {
        return Err("Parser source hash differs from the captured bytes".into());
    }
    let source_text = String::from_utf8(bytes).map_err(|_| "Correction source must be UTF-8")?;
    Ok(CorrectionPreview::Text {
        report: Box::new(report),
        source_text,
    })
}
fn validate_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > 1024 || path.contains('\0') {
        Err("Correction path requires 1–1,024 UTF-8 bytes without NUL".into())
    } else {
        Ok(())
    }
}
