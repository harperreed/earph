// ABOUTME: Verifies bounded original package source bytes without modifying files or publishing content.
// ABOUTME: Uses the shared regular-file reader and joinable local worker lifecycle for exact review.
use super::profile_files::{LocalFileWorker, check};
use crate::PackageReviewSourceView;
use earph_core::{
    dsp_nodes::FirAssetReference,
    profile_packages::{MAX_PACKAGE_BYTES, PackageObject, PackageObjectKind},
};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::atomic::AtomicBool};

pub(super) struct ObjectBytes {
    pub object: PackageObject,
    pub path: String,
    pub bytes: Vec<u8>,
    pub wav: Option<FirAssetReference>,
}
pub(super) enum PackageRead {
    Object(Box<ObjectBytes>),
    Verified,
}
pub(super) fn object(
    source: PackageReviewSourceView,
) -> Result<LocalFileWorker<PackageRead>, String> {
    LocalFileWorker::start("package object review", move |cancel| {
        let bytes = read(&source, cancel)?;
        let wav = if source.object.kind == PackageObjectKind::FirWav {
            Some(
                earph_media::decode_fir_wav(&bytes)
                    .map_err(|e| e.to_string())?
                    .reference,
            )
        } else {
            std::str::from_utf8(&bytes).map_err(|_| "Package text object is not UTF-8")?;
            None
        };
        Ok(PackageRead::Object(Box::new(ObjectBytes {
            object: source.object,
            path: source.local_path,
            bytes,
            wav,
        })))
    })
}
pub(super) fn verify(
    sources: Vec<PackageReviewSourceView>,
) -> Result<LocalFileWorker<PackageRead>, String> {
    LocalFileWorker::start("package source verification", move |cancel| {
        let total = sources.iter().try_fold(0u64, |n, s| {
            n.checked_add(s.object.bytes)
                .ok_or("Package review byte total overflow")
        })?;
        if total > MAX_PACKAGE_BYTES
            || sources.len() > earph_core::profile_packages::MAX_PACKAGE_OBJECTS
        {
            return Err("Package review exceeds its declared resource budget".into());
        }
        for source in sources {
            read(&source, cancel)?;
        }
        Ok(PackageRead::Verified)
    })
}
fn read(source: &PackageReviewSourceView, cancel: &AtomicBool) -> Result<Vec<u8>, String> {
    check(cancel)?;
    earph_core::profile_control::validate_package_path(&source.local_path)
        .map_err(|e| e.to_string())?;
    let type_limit = match source.object.kind {
        PackageObjectKind::Profile => earph_core::profiles::MAX_PROFILE_BYTES,
        PackageObjectKind::Graph => earph_core::graph::MAX_GRAPH_INTENT_BYTES,
        PackageObjectKind::TextReport => earph_media::correction_text::MAX_REPORT_BYTES,
        PackageObjectKind::TextSource => earph_media::correction_text::MAX_CORRECTION_BYTES,
        PackageObjectKind::FirManifest => earph_media::MAX_FIR_MANIFEST_BYTES,
        PackageObjectKind::FirWav => earph_media::MAX_FIR_WAV_BYTES,
    };
    if source.object.bytes == 0 || source.object.bytes > type_limit as u64 {
        return Err("Invalid package review source or byte budget".into());
    }
    let limit = usize::try_from(source.object.bytes)
        .map_err(|_| "Package object size cannot fit this host")?;
    let bytes =
        earph_media::correction_text::read_correction_file(Path::new(&source.local_path), limit)
            .map_err(|e| e.to_string())?;
    let digest: String = Sha256::digest(&bytes)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect();
    if bytes.len() != limit || digest != source.object.sha256.as_str() {
        return Err("Package source bytes changed or differ from the immutable manifest; all review acknowledgments are invalid".into());
    }
    check(cancel)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn finish(worker: &mut LocalFileWorker<PackageRead>) -> Result<PackageRead, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(result) = worker.poll() {
                return result;
            }
            assert!(Instant::now() < deadline, "package worker did not retire");
            std::thread::yield_now();
        }
    }
    #[test]
    fn original_byte_review_preserves_owned_sources_and_reverification_refuses_changed_bytes() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/desktop-packages-workflows/fixtures")
            .join(format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("owned-text-source.txt");
        let bytes = b"# Owned public source\r\nPreamp: -6 dB\r\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\r\n";
        std::fs::write(&path, bytes).unwrap();
        let hash: String = Sha256::digest(bytes)
            .iter()
            .map(|v| format!("{v:02x}"))
            .collect();
        let source = PackageReviewSourceView {
            object: PackageObject {
                kind: PackageObjectKind::TextSource,
                sha256: earph_core::profiles::ProfileObjectHash::try_from(hash).unwrap(),
                bytes: bytes.len() as u64,
            },
            local_path: path.to_str().unwrap().into(),
        };
        let mut worker = object(source.clone()).unwrap();
        let PackageRead::Object(displayed) = finish(&mut worker).unwrap() else {
            panic!()
        };
        assert_eq!(displayed.bytes, bytes);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let mut worker = verify(vec![source.clone()]).unwrap();
        assert!(matches!(finish(&mut worker), Ok(PackageRead::Verified)));
        // Only this generated fixture changes, demonstrating post-review mutation.
        std::fs::write(&path, b"changed").unwrap();
        let mut worker = verify(vec![source.clone()]).unwrap();
        assert!(finish(&mut worker).is_err());
        let mut worker = object(source).unwrap();
        worker.cancel();
        assert!(finish(&mut worker).is_err());
        assert!(worker.retired());
        assert_eq!(std::fs::read(path).unwrap(), b"changed");
    }
}
