// ABOUTME: Owns bounded graph intent and resolved processing plans.
// ABOUTME: Keeps graph compilation independent from the session authority.
#![forbid(unsafe_code)]
mod compile;
pub mod convolution;
mod model;
pub mod peq;
mod rate_compile;
pub mod resample;
mod runtime;
pub mod stream;
pub use compile::{compile, compile_with_assets, compile_with_input_bounds};
pub use model::*;
pub use rate_compile::{
    CompiledStreamPlan, CompiledStreamStage, CompiledStreamStageKind, compile_stream,
};
pub use runtime::{GraphLifecycle, MeterObservation, PreparedGraph};
