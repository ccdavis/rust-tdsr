//! TDSR - Terminal-based screen reader
//!
//! A console-based screen reader for *nix systems (macOS, Linux, FreeBSD).
//! Provides text-to-speech feedback for terminal applications.

pub mod error;
pub mod terminal;
pub mod speech;
pub mod input;
pub mod state;
pub mod review;
pub mod clipboard;
pub mod symbols;
pub mod plugins;
pub mod platform;

pub use error::{Result, TdsrError};

/// Version information
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_NAME: &str = "tdsr";
