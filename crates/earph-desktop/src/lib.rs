// ABOUTME: Exposes the attach-only desktop client and bounded presentation contracts.
// ABOUTME: Keeps engine ownership and audible policy in the existing shared owner.
#![forbid(unsafe_code)]
uniffi::setup_scaffolding!();
#[cfg(test)]
extern crate self as earph_desktop;
mod projection;
pub use projection::*;
mod client;
pub use client::*;
mod dispatcher;
pub use dispatcher::*;
mod model;
pub use model::*;
pub mod gui;

#[cfg(test)]
#[path = "../tests/network_fixture/mod.rs"]
mod network_fixture;
