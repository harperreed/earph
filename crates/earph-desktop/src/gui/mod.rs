// ABOUTME: Hosts the native local controller over the existing desktop client.
// ABOUTME: Keeps view workflows and child lifecycle separate from engine policy.
mod app;
mod connect;
mod correction;
mod diagnostics;
mod dsp;
mod graph_editor;
mod host;
mod output;
mod package_files;
mod packages;
mod profile_files;
mod profiles;
mod queue;
mod receipts;
mod signal_path;
mod status;
mod transport;
mod workflow;
pub use app::{GuiApp, run};
pub use host::HostStatus;
pub use profile_files::{
    CorrectionPreview, CorrectionPreviewRequest, CorrectionSource, CorrectionWorker,
};
pub use workflow::{GuiController, Receipt, UiIntent};

use earph_core::ids::RequestId;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

fn request_id() -> Result<RequestId, String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let counter = NEXT
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| "Request identity counter exhausted".to_owned())?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    RequestId::new(format!("gui-{:x}-{time:x}-{counter:x}", std::process::id()))
        .map_err(|e| e.to_string())
}
