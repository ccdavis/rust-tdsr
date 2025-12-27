//! TDSR - Terminal-based screen reader
//!
//! A console-based screen reader for *nix systems (macOS, Linux, FreeBSD).
//! Provides text-to-speech feedback for terminal applications.

pub mod clipboard;
pub mod error;
pub mod input;
pub mod platform;
pub mod plugins;
pub mod review;
pub mod speech;
pub mod state;
pub mod symbols;
pub mod terminal;

pub use error::{Result, TdsrError};

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_NAME: &str = "tdsr";
