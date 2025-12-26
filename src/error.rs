//! Error types for TDSR

use std::io;
use thiserror::Error;

/// Main error type for TDSR
#[derive(Error, Debug)]
pub enum TdsrError {
    #[error("Terminal error: {0}")]
    Terminal(String),

    #[error("PTY error: {0}")]
    Pty(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Speech synthesis error: {0}")]
    Speech(String),

    #[error("Plugin error: {0}")]
    Plugin(String),

    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("INI parse error: {0}")]
    IniParse(String),

    #[error("Invalid UTF-8: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),

    #[error("{0}")]
    Other(String),
}

/// Result type alias for TDSR operations
pub type Result<T> = std::result::Result<T, TdsrError>;

impl From<String> for TdsrError {
    fn from(s: String) -> Self {
        TdsrError::Other(s)
    }
}

impl From<&str> for TdsrError {
    fn from(s: &str) -> Self {
        TdsrError::Other(s.to_string())
    }
}

impl From<serde_json::Error> for TdsrError {
    fn from(e: serde_json::Error) -> Self {
        TdsrError::Plugin(format!("JSON error: {}", e))
    }
}
