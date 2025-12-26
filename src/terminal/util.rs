//! Terminal utilities

use crate::Result;
use nix::libc;
use std::os::unix::io::RawFd;

/// Get the terminal size for the given file descriptor
///
/// Screen reader needs to know terminal dimensions to properly
/// size the screen buffer and PTY.
pub fn get_terminal_size(fd: RawFd) -> Result<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };

    let result = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };

    if result == 0 {
        Ok((ws.ws_col, ws.ws_row))
    } else {
        // Default size if ioctl fails
        Ok((80, 24))
    }
}

/// Set raw mode on a terminal file descriptor
///
/// Raw mode is required for the screen reader to capture all keypresses
/// including control characters and escape sequences.
pub fn set_raw_mode(fd: RawFd) -> Result<libc::termios> {
    let original_termios = unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        libc::tcgetattr(fd, &mut termios);
        termios
    };

    let mut raw_termios = original_termios;

    unsafe {
        libc::cfmakeraw(&mut raw_termios);
        libc::tcsetattr(fd, libc::TCSANOW, &raw_termios);
    }

    Ok(original_termios)
}

/// Restore terminal attributes
///
/// Called when screen reader exits to return terminal to normal state
pub fn restore_termios(fd: RawFd, termios: &libc::termios) {
    unsafe {
        libc::tcsetattr(fd, libc::TCSANOW, termios);
    }
}
