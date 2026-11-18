// ABOUTME: Exposes explicit ALSA routes and owned PCM output.
// ABOUTME: Keeps platform pointers behind a private native boundary.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unsafe_code)]
mod contract;
pub use contract::*;
mod evidence;
pub use evidence::validate_matrix;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod native;
#[cfg(target_os = "linux")]
mod output;
#[cfg(target_os = "linux")]
pub use native::{devices, library_version};
#[cfg(target_os = "linux")]
pub use output::AlsaOutput;
