// ABOUTME: Owns bounded graph intent and resolved processing plans.
// ABOUTME: Keeps graph compilation independent from the session authority.
#![forbid(unsafe_code)]
mod compile;
mod model;
mod runtime;
pub use compile::compile;
pub use model::*;
pub use runtime::{GraphLifecycle, MeterObservation, PreparedGraph};
