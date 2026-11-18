// ABOUTME: Owns bounded graph intent and resolved processing plans.
// ABOUTME: Keeps graph compilation independent from the session authority.
#![forbid(unsafe_code)]
mod compile;
pub mod convolution;
mod model;
pub mod peq;
pub mod resample;
mod runtime;
pub use compile::compile;
pub use model::*;
pub use runtime::{GraphLifecycle, MeterObservation, PreparedGraph};
