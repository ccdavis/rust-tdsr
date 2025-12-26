//! Clipboard integration

use crate::{Result, TdsrError};
use arboard::Clipboard;
use log::debug;

/// Copy text to system clipboard
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    debug!("Copying {} chars to clipboard", text.len());

    let mut clipboard = Clipboard::new()
        .map_err(|e| TdsrError::Other(format!("Failed to open clipboard: {}", e)))?;

    clipboard
        .set_text(text)
        .map_err(|e| TdsrError::Other(format!("Failed to copy to clipboard: {}", e)))?;

    Ok(())
}

/// Get text from system clipboard
pub fn get_from_clipboard() -> Result<String> {
    debug!("Getting text from clipboard");

    let mut clipboard = Clipboard::new()
        .map_err(|e| TdsrError::Other(format!("Failed to open clipboard: {}", e)))?;

    clipboard
        .get_text()
        .map_err(|e| TdsrError::Other(format!("Failed to get from clipboard: {}", e)))
}

// TODO: Phase 8 - Implement selection rectangle copying from terminal buffer
