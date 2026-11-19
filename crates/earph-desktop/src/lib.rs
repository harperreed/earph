// ABOUTME: Exposes the attach-only desktop client and bounded presentation contracts.
// ABOUTME: Keeps engine ownership and audible policy in the existing shared owner.
#![forbid(unsafe_code)]
mod projection;
pub use projection::*;
mod client;
pub use client::*;
mod dispatcher;
pub use dispatcher::*;
mod model;
pub use model::*;
pub mod gui;
