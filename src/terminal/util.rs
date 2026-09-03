//! Terminal utilities

use crate::{Result, TdsrError};
use nix::libc;
use std::os::unix::io::RawFd;

/// Size assumed when the terminal reports none (or reports zero columns/rows,
/// which happens under `script`, some CI runners and detached sessions).
const FALLBACK_SIZE: (u16, u16) = (80, 24);

/// Get the terminal size for the given file descriptor as (cols, rows).
///
/// Screen reader needs to know terminal dimensions to properly size the
/// screen buffer and PTY. Never returns a zero dimension: the screen buffer
/// code assumes at least one column and one row.
pub fn get_terminal_size(fd: RawFd) -> Result<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };

    // Safety: TIOCGWINSZ writes a winsize into the pointed-to struct.
    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };

    if result == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Ok((ws.ws_col, ws.ws_row))
    } else {
        Ok(FALLBACK_SIZE)
    }
}

/// Set raw mode on a terminal file descriptor, returning the previous
/// attributes so they can be restored on exit.
///
/// Raw mode is required for the screen reader to capture all keypresses
/// including control characters and escape sequences.
pub fn set_raw_mode(fd: RawFd) -> Result<libc::termios> {
    let mut original_termios: libc::termios = unsafe { std::mem::zeroed() };

    // Safety: tcgetattr fills the struct; we check its result so we never
    // "restore" zeroed attributes later.
    if unsafe { libc::tcgetattr(fd, &mut original_termios) } != 0 {
        return Err(TdsrError::Io(std::io::Error::last_os_error()));
    }

    let mut raw_termios = original_termios;

    // Safety: cfmakeraw/tcsetattr operate on the struct we own.
    unsafe {
        libc::cfmakeraw(&mut raw_termios);
        if libc::tcsetattr(fd, libc::TCSANOW, &raw_termios) != 0 {
            return Err(TdsrError::Io(std::io::Error::last_os_error()));
        }
    }

    Ok(original_termios)
}

/// Restore terminal attributes
///
/// Called when screen reader exits to return terminal to normal state
pub fn restore_termios(fd: RawFd, termios: &libc::termios) {
    // Safety: restoring attributes previously obtained from tcgetattr.
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, termios);
    }
}
