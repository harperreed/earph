// ABOUTME: Exposes checked network admission and owned streaming readers.
// ABOUTME: Keeps private URLs and transport errors out of diagnostics.
mod policy;
pub use policy::*;
mod fetch;
pub use fetch::*;
pub mod upnp;
