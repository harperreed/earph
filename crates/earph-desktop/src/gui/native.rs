// ABOUTME: Exposes the existing desktop controller to native system-control hosts.
// ABOUTME: Captures bounded render bases and keeps IPC, safety, and cleanup in Rust.
mod actions;
mod snapshot;
use super::{GuiController, graph_editor::GraphIntentEditor, profiles};
use crate::{ProfileRecordView, StateView};
use std::{
    collections::VecDeque,
    fmt,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

const CAPTURE_LIMIT: usize = 64;

#[derive(Debug, uniffi::Error)]
pub enum NativeError {
    InvalidAction { detail: String },
    Internal { detail: String },
}
impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAction { detail } | Self::Internal { detail } => f.write_str(detail),
        }
    }
}
impl std::error::Error for NativeError {}
fn invalid(detail: impl Into<String>) -> NativeError {
    NativeError::InvalidAction {
        detail: detail.into(),
    }
}

#[derive(Clone)]
struct RenderBasis {
    token: String,
    queue: bool,
    state: Arc<StateView>,
    profile: Option<Arc<ProfileRecordView>>,
    fir_asset: Option<earph_core::dsp_nodes::FirAssetReference>,
    review: ReviewCapture,
}
#[derive(Clone)]
struct ReviewCapture {
    diagnostic: Option<earph_core::ids::RequestId>,
    diagnostic_directory: String,
    package: Option<Arc<crate::ProfilePackageResultView>>,
    package_directory: String,
    package_destination: String,
    object: Option<(earph_core::profile_packages::PackageObject, String)>,
    objects_read: Vec<earph_core::profile_packages::PackageObject>,
    acknowledgments: (bool, bool, bool),
}
#[derive(Default)]
struct NativeState {
    controller: GuiController,
    generation: u64,
    captures: VecDeque<RenderBasis>,
    graph: GraphIntentEditor,
    profile: profiles::Draft,
    package_directory: String,
    correction_request: Option<super::CorrectionPreviewRequest>,
}
impl NativeState {
    fn graph_review_available(&self) -> bool {
        self.profile
            .graph_review_required()
            .is_none_or(|reference| self.controller.profile_review_available(reference))
    }
    fn capture(&mut self, state: Option<Arc<StateView>>, queue: bool) -> Option<String> {
        let state = state?;
        let token = format!(
            "{}:{}",
            if queue { "queue" } else { "state" },
            self.generation
        );
        if self.captures.len() == CAPTURE_LIMIT {
            self.captures.pop_front();
        }
        self.captures.push_back(RenderBasis {
            token: token.clone(),
            queue,
            state,
            profile: self
                .controller
                .profiles
                .record
                .as_ref()
                .map(|p| p.value.clone()),
            fir_asset: self.controller.fir_asset().cloned(),
            review: self.review_capture(),
        });
        Some(token)
    }
    fn basis(&self, token: &str, queue: bool) -> Result<RenderBasis, NativeError> {
        self.captures
            .iter()
            .find(|basis| basis.token == token && basis.queue == queue)
            .cloned()
            .ok_or_else(|| {
                invalid("Rendered context expired or has the wrong kind; observe and submit again")
            })
    }
    fn review_capture(&self) -> ReviewCapture {
        let review = &self.controller.package_review;
        ReviewCapture {
            diagnostic: self
                .controller
                .diagnostics
                .preview
                .as_ref()
                .map(|(id, _)| id.clone()),
            diagnostic_directory: self.controller.diagnostics.directory.clone(),
            package: self
                .controller
                .profiles
                .package
                .as_ref()
                .map(|view| view.value.clone()),
            package_directory: self.package_directory.clone(),
            package_destination: review.destination.clone(),
            object: review
                .displayed
                .as_ref()
                .map(|value| (value.object.clone(), value.path.clone())),
            objects_read: review.read.clone(),
            acknowledgments: (review.exact, review.private, review.measurements),
        }
    }
    fn owner_scope_matches(&self, basis: &RenderBasis) -> bool {
        self.controller.state().is_some_and(|state| {
            state.owner_id == basis.state.owner_id && state.zone_id == basis.state.zone_id
        })
    }
    fn diagnostic_review_context(&self, token: &str) -> Result<(), NativeError> {
        let basis = self.basis(token, false)?;
        let current = self.review_capture();
        if !self.owner_scope_matches(&basis)
            || basis.review.diagnostic != current.diagnostic
            || basis.review.diagnostic_directory != current.diagnostic_directory
        {
            return Err(invalid(
                "Rendered diagnostic preview or destination changed; review the displayed bytes again",
            ));
        }
        Ok(())
    }
    fn package_review_context(
        &self,
        token: &str,
        displayed: bool,
        acknowledgments: bool,
    ) -> Result<(), NativeError> {
        let basis = self.basis(token, false)?;
        let current = self.review_capture();
        if !self.owner_scope_matches(&basis)
            || basis.review.package != current.package
            || basis.review.package_directory != current.package_directory
            || basis.review.package_destination != current.package_destination
            || (displayed && basis.review.object != current.object)
            || (acknowledgments
                && (basis.review.objects_read != current.objects_read
                    || basis.review.acknowledgments != current.acknowledgments))
        {
            return Err(invalid(
                "Rendered package candidate, object, or destination changed; review the displayed bytes again",
            ));
        }
        Ok(())
    }
}

/// Own this handle and call its methods on one background queue, including final release.
#[derive(uniffi::Object)]
pub struct NativeController {
    state: Mutex<NativeState>,
}
#[uniffi::export]
impl NativeController {
    /// Creates local presentation only. Owner launch and attachment remain explicit actions.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(NativeState::default()),
        })
    }
    /// Advances the existing controller without waiting for IPC and returns owned view bytes.
    pub fn tick(&self) -> Result<String, NativeError> {
        let mut state = self.lock()?;
        state.controller.tick(Instant::now());
        state.render()
    }
    /// Captures the current view without issuing an Engine refresh.
    pub fn snapshot(&self) -> Result<String, NativeError> {
        self.lock()?.render()
    }
    /// Admits one bounded local action. Sent operations retain their existing uncertainty semantics.
    pub fn send(&self, action_json: String) -> Result<String, NativeError> {
        let action = actions::parse(&action_json)?;
        let mut state = self.lock()?;
        state.dispatch(action)?;
        state.render()
    }
}
impl NativeController {
    fn lock(&self) -> Result<MutexGuard<'_, NativeState>, NativeError> {
        self.state.lock().map_err(|_| NativeError::Internal {
            detail: "Native controller lock is poisoned".into(),
        })
    }
}
