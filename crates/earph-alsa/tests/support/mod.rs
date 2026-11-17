// ABOUTME: Creates owned ALSA test artifacts under the ignored target directory.
// ABOUTME: Retains every artifact for inspection without deleting files.
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn directory() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/alsa-tests");
    std::fs::create_dir_all(&root).unwrap();
    loop {
        let path = root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return path,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create ALSA test directory: {error}"),
        }
    }
}
