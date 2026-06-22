//! dogma — a terminal UI for Nerve.
//!
//! This crate is split into a library (this file) plus a thin binary
//! (`src/main.rs` → `dogma`). The library layout exists so integration
//! tests under `tests/` can reuse the wire-decoding types directly.

// v0.1 has intentional scaffolding for v0.2+ tabs (PendingInteraction,
// per-session cursor, side panel actions, etc.). The unused fields/methods
// will get pulled in as those features land.
#![allow(dead_code)]

pub mod api;
pub mod app;
pub mod auth;
pub mod cli;
pub mod config;
pub mod model;
pub mod ui;
pub mod view;
