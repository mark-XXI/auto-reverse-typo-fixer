//! Windows layer: low-level hooks, text injection, layout switching, focus checks.
//!
//! All Win32 access goes through hand-declared FFI in [`sys`] (no external crates), so this
//! crate can be type-checked on any platform; it only *runs* on 64-bit Windows.

#![allow(clippy::missing_safety_doc)]

pub mod app;
pub mod cli;
pub mod focus;
pub mod hook;
pub mod inject;
pub mod layout_win;
pub mod overlay;
pub mod sys;
pub mod uia;
#[cfg(windows)]
pub mod uia_events;
pub mod uia_guard;
