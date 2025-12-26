//! Platform detection utilities

use std::fs;

/// Detect if running in WSL (Windows Subsystem for Linux)
///
/// Checks for WSL-specific indicators in /proc/version and environment variables.
pub fn is_wsl() -> bool {
    // Check for WSL-specific indicators in /proc/version
    if let Ok(contents) = fs::read_to_string("/proc/version") {
        let lower = contents.to_lowercase();
        if lower.contains("microsoft") || lower.contains("wsl") {
            return true;
        }
    }

    // Check for WSL environment variable
    std::env::var("WSL_DISTRO_NAME").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_wsl() {
        // This test just verifies the function doesn't panic
        // The actual result depends on the platform
        let _ = is_wsl();
    }
}
