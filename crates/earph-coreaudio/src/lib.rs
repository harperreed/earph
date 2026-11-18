// ABOUTME: Defines explicit device inspection and precision assessment for Core Audio.
// ABOUTME: Keeps native handles and framework calls outside the shared core.
#![deny(unsafe_code)]
mod contract;
pub use contract::*;
mod redaction;
pub use redaction::redact_record;
mod playback;
pub use playback::*;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod native;
#[cfg(target_os = "macos")]
pub use native::{SilentRegistration, devices, inspect};
