// ABOUTME: Captures compatible local tracks as one exact integer PCM stream.
// ABOUTME: Preserves partial evidence and never overwrites existing output folders.
use crate::LocalFile;
use earph_core::{EngineError, ErrorCode, audio::PcmFormat, ids::Epochs};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};

const IO: EngineError = EngineError::new(
    ErrorCode::OutputFault,
    "cannot create or finish offline capture",
);

#[derive(Debug, Serialize)]
pub struct RenderReport {
    pub schema_version: u32,
    pub status: &'static str,
    pub format: Option<PcmFormat>,
    pub frames: u64,
    pub files_completed: usize,
    pub capture_sha256: String,
    pub capture_bytes: u64,
    pub partial_frame_bytes: u64,
    pub error: Option<EngineError>,
}

pub fn render(
    paths: &[PathBuf],
    output: &Path,
    seek_frame: u64,
) -> Result<RenderReport, EngineError> {
    if paths.is_empty() || paths.len() > 256 {
        return Err(EngineError::new(
            ErrorCode::ResourceBudgetExceeded,
            "render needs one to 256 local files",
        ));
    }
    fs::create_dir(output).map_err(|_| IO)?;
    let mut capture = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(output.join("capture.pcm"))
        .map_err(|_| IO)?;
    let mut report = RenderReport {
        schema_version: 1,
        status: "pass",
        format: None,
        frames: 0,
        files_completed: 0,
        capture_sha256: String::new(),
        capture_bytes: 0,
        partial_frame_bytes: 0,
        error: None,
    };
    let mut packed = vec![0; 65536 * 2 * 4];
    let result = (|| -> Result<(), EngineError> {
        for (index, path) in paths.iter().enumerate() {
            let mut file = LocalFile::open(
                path,
                Epochs {
                    session: 1,
                    stream: 1,
                },
            )?;
            if report.format.is_some_and(|f| f != file.format()) {
                return Err(EngineError::new(
                    ErrorCode::FidelityPolicyConflict,
                    "track transition needs explicit format negotiation",
                ));
            }
            report.format = Some(file.format());
            if index == 0 && seek_frame != 0 {
                file.seek(
                    seek_frame,
                    Epochs {
                        session: 1,
                        stream: 2,
                    },
                )?;
            }
            while let Some(block) = file.next_block()? {
                let used = block.pack_le(&mut packed)?;
                capture.write_all(&packed[..used]).map_err(|_| IO)?;
            }
            report.files_completed += 1;
        }
        Ok(())
    })();
    report.error = result.err();
    if let Err(_error) = capture.sync_all() {
        report.error = Some(IO);
    }
    if report.error.is_some() {
        report.status = "fail";
    }
    // A failed write_all can leave a partial frame. Hash what actually reached the file.
    capture.rewind().map_err(|_| IO)?;
    let mut digest = Sha256::new();
    loop {
        let size = capture.read(&mut packed).map_err(|_| IO)?;
        if size == 0 {
            break;
        }
        digest.update(&packed[..size]);
        report.capture_bytes = report.capture_bytes.checked_add(size as u64).ok_or(IO)?;
    }
    if let Some(format) = report.format {
        let frame_bytes = format.payload_bytes(1)? as u64;
        report.frames = report.capture_bytes / frame_bytes;
        report.partial_frame_bytes = report.capture_bytes % frame_bytes;
    }
    report.capture_sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let bytes = serde_json::to_vec_pretty(&report).map_err(|_| IO)?;
    let partial = output.join("report.partial");
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&partial)
        .map_err(|_| IO)?;
    file.write_all(&bytes).map_err(|_| IO)?;
    file.sync_all().map_err(|_| IO)?;
    fs::rename(partial, output.join("report.json")).map_err(|_| IO)?;
    Ok(report)
}
