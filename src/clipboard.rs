//! Clipboard integration
//!
//! - **WSL**: writes go to the Windows clipboard (`clip.exe` / PowerShell
//!   `Set-Clipboard`) first, falling back to `arboard`.
//! - **Native Linux**: prefers persistent clipboard tools (`wl-copy` on
//!   Wayland, `xclip`/`xsel` on X11) because short-lived `arboard` X11/Wayland
//!   connections lose clipboard ownership on drop when no clipboard manager is
//!   running; falls back to `arboard`.
//! - **macOS**: uses `arboard` (`NSPasteboard`) directly.

use crate::platform::is_wsl;
use crate::{Result, TdsrError};
use arboard::Clipboard;
use log::debug;
use std::io::Write;
use std::process::{Command, Stdio};

/// Copy text to system clipboard
pub fn copy_to_clipboard(text: &str) -> Result<()> {
    debug!("Copying {} chars to clipboard", text.len());

    if is_wsl() {
        match wsl_copy_to_clipboard(text) {
            Ok(()) => return Ok(()),
            Err(e) => debug!("WSL Windows clipboard copy failed ({}), trying arboard", e),
        }
    } else {
        #[cfg(target_os = "linux")]
        match linux_copy_to_clipboard(text) {
            Ok(()) => return Ok(()),
            Err(e) => debug!("Linux CLI clipboard copy failed ({}), trying arboard", e),
        }
    }

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

    if is_wsl() {
        match wsl_get_from_clipboard() {
            Ok(text) => return Ok(text),
            Err(e) => debug!("WSL Windows clipboard read failed ({}), trying arboard", e),
        }
    } else {
        #[cfg(target_os = "linux")]
        match linux_get_from_clipboard() {
            Ok(text) => return Ok(text),
            Err(e) => debug!("Linux CLI clipboard read failed ({}), trying arboard", e),
        }
    }

    let mut clipboard = Clipboard::new()
        .map_err(|e| TdsrError::Other(format!("Failed to open clipboard: {}", e)))?;

    clipboard
        .get_text()
        .map_err(|e| TdsrError::Other(format!("Failed to get from clipboard: {}", e)))
}

/// Pipe `bytes` into the first candidate executable that spawns and exits 0.
fn pipe_to_program(candidates: &[&str], args: &[&str], bytes: &[u8]) -> Result<()> {
    let mut last_err = None;
    for prog in candidates {
        match Command::new(prog)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(mut stdin) = child.stdin.take() {
                    if let Err(e) = stdin.write_all(bytes) {
                        let _ = child.wait();
                        last_err = Some(format!("{} write failed: {}", prog, e));
                        continue;
                    }
                }
                match child.wait() {
                    Ok(status) if status.success() => return Ok(()),
                    Ok(status) => last_err = Some(format!("{} exited with {}", prog, status)),
                    Err(e) => last_err = Some(format!("{} wait failed: {}", prog, e)),
                }
            }
            Err(e) => last_err = Some(format!("{}: {}", prog, e)),
        }
    }
    Err(TdsrError::Other(last_err.unwrap_or_else(|| {
        "clipboard helper unavailable".to_string()
    })))
}

/// Copy `text` to the Windows clipboard from WSL.
///
/// Pure ASCII uses `clip.exe` directly (near-instant, no codepage loss);
/// non-ASCII uses PowerShell with explicit UTF-8 input so Unicode survives,
/// falling back to `clip.exe` if PowerShell is unavailable.
fn wsl_copy_to_clipboard(text: &str) -> Result<()> {
    const CLIP: &[&str] = &["clip.exe", "/mnt/c/Windows/System32/clip.exe"];
    const PS: &[&str] = &[
        "powershell.exe",
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
    ];
    if text.is_ascii() {
        return pipe_to_program(CLIP, &[], text.as_bytes());
    }
    pipe_to_program(
        PS,
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::InputEncoding = [Text.Encoding]::UTF8; \
             Set-Clipboard -Value ([Console]::In.ReadToEnd())",
        ],
        text.as_bytes(),
    )
    .or_else(|_| pipe_to_program(CLIP, &[], text.as_bytes()))
}

/// Read the Windows clipboard from WSL via PowerShell (UTF-8).
fn wsl_get_from_clipboard() -> Result<String> {
    const PS: &[&str] = &[
        "powershell.exe",
        "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
    ];
    let mut last_err = None;
    for prog in PS {
        match Command::new(prog)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        {
            Ok(out) if out.status.success() => {
                return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
            }
            Ok(out) => last_err = Some(format!("{} exited with {}", prog, out.status)),
            Err(e) => last_err = Some(format!("{}: {}", prog, e)),
        }
    }
    Err(TdsrError::Other(last_err.unwrap_or_else(|| {
        "powershell.exe unavailable".to_string()
    })))
}

/// Copy `text` on native Linux via `wl-copy` (Wayland) or `xclip`/`xsel` (X11).
/// These fork a daemon to hold clipboard ownership after exiting; keeping
/// stdout/stderr set to `Stdio::null()` avoids blocking on inherited pipes.
#[cfg(target_os = "linux")]
fn linux_copy_to_clipboard(text: &str) -> Result<()> {
    let bytes = text.as_bytes();
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && pipe_to_program(&["wl-copy"], &[], bytes).is_ok()
    {
        return Ok(());
    }
    if std::env::var_os("DISPLAY").is_some() {
        if pipe_to_program(&["xclip"], &["-selection", "clipboard"], bytes).is_ok() {
            return Ok(());
        }
        if pipe_to_program(&["xsel"], &["--clipboard", "--input"], bytes).is_ok() {
            return Ok(());
        }
    }
    Err(TdsrError::Other(
        "no native Linux clipboard helper available".to_string(),
    ))
}

/// Read the clipboard on native Linux via `wl-paste` (Wayland) or `xclip`/`xsel` (X11).
#[cfg(target_os = "linux")]
fn linux_get_from_clipboard() -> Result<String> {
    let run = |prog: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(prog)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        if let Some(s) = run("wl-paste", &["--no-newline"]) {
            return Ok(s);
        }
    }
    if std::env::var_os("DISPLAY").is_some() {
        if let Some(s) = run("xclip", &["-selection", "clipboard", "-o"]) {
            return Ok(s);
        }
        if let Some(s) = run("xsel", &["--clipboard", "--output"]) {
            return Ok(s);
        }
    }
    Err(TdsrError::Other(
        "no native Linux clipboard helper available".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_to_program_falls_back_to_working_candidate() {
        assert!(pipe_to_program(&["nonexistent-tdsr-clip-cmd", "cat"], &[], b"hello").is_ok());
        assert!(pipe_to_program(&["nonexistent-tdsr-clip-cmd"], &[], b"hello").is_err());
    }
}
