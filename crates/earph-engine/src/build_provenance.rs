// ABOUTME: Decodes the Engine's immutable compiled package record through Core's strict contract.
// ABOUTME: Shares one typed record without inspecting a runtime checkout or executable.
use earph_core::provenance::{BuildUnavailableReason, RunningBuildProvenance};
use std::sync::{Arc, OnceLock};

pub fn build_provenance() -> Arc<RunningBuildProvenance> {
    static BUILD: OnceLock<Arc<RunningBuildProvenance>> = OnceLock::new();
    Arc::clone(BUILD.get_or_init(|| {
        let build = RunningBuildProvenance::decode(include_bytes!(concat!(
            env!("OUT_DIR"),
            "/build-provenance.json"
        )))
        .and_then(|build| {
            build.validate_for_engine()?;
            Ok(build)
        })
        .unwrap_or_else(|_| {
            RunningBuildProvenance::unavailable(BuildUnavailableReason::InvalidRecord)
        });
        Arc::new(build)
    }))
}
