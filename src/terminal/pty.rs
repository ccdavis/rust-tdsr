//! Pseudo-terminal (PTY) management
//!
//! The PTY is how the screen reader intercepts terminal I/O.
//! We create a pseudo-terminal, spawn a shell in it, and monitor all
//! input/output to provide speech feedback.

use crate::{Result, TdsrError};
use log::{debug, info};
use nix::unistd::dup;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::io::{FromRawFd, OwnedFd, RawFd};

/// Manages a pseudo-terminal for intercepting shell I/O
///
/// The screen reader sits between the user's terminal and the shell,
/// reading all output to provide speech feedback while passing input through.
pub struct Pty {
    /// Reader for PTY output
    reader: Box<dyn Read + Send>,

    /// Writer for PTY input
    writer: Box<dyn Write + Send>,

    /// The child process (shell) running in the PTY
    _child: Box<dyn Child + Send>,

    /// Duplicated file descriptor for the PTY (for event loop registration)
    /// This is our own copy that stays valid even after master is consumed
    _fd_owner: OwnedFd,

    /// Raw file descriptor for select/mio
    fd: RawFd,

    /// Current terminal size (stored for resize operations)
    _size: PtySize,
}

impl Pty {
    /// Create a new PTY and spawn a shell or specified program
    ///
    /// This sets up the terminal environment that the screen reader monitors.
    /// If no program is specified, spawns the user's default shell.
    pub fn new(program: Option<Vec<String>>, rows: u16, cols: u16) -> Result<Self> {
        let pty_system = native_pty_system();

        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        debug!("Creating PTY with size {}x{}", rows, cols);

        // Create the PTY pair
        let pair = pty_system
            .openpty(size)
            .map_err(|e| TdsrError::Pty(format!("Failed to open PTY: {}", e)))?;

        // Determine what program to run
        let cmd = if let Some(prog) = program {
            // User specified a program
            info!("Spawning specified program: {:?}", prog);
            let mut cmd = CommandBuilder::new(&prog[0]);
            for arg in &prog[1..] {
                cmd.arg(arg);
            }
            cmd
        } else {
            // Spawn user's default shell
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            info!("Spawning default shell: {}", shell);
            CommandBuilder::new(shell)
        };

        // Spawn the child process in the PTY
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| TdsrError::Pty(format!("Failed to spawn child: {}", e)))?;

        // Extract the file descriptor from the master
        let original_fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| TdsrError::Pty("Failed to get PTY file descriptor".to_string()))?;

        debug!("Original PTY file descriptor: {}", original_fd);

        // Duplicate the file descriptor so we have our own copy that won't be closed
        // when the master is consumed by take_writer()
        let dup_fd = dup(original_fd)
            .map_err(|e| TdsrError::Pty(format!("Failed to duplicate fd: {}", e)))?;

        debug!("Duplicated PTY file descriptor: {}", dup_fd);

        // Wrap the duplicated fd in OwnedFd for RAII cleanup
        let fd_owner = unsafe { OwnedFd::from_raw_fd(dup_fd) };
        let fd = fd_owner.as_raw_fd();

        // Clone a reader from the master
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TdsrError::Pty(format!("Failed to get PTY reader: {}", e)))?;

        // Take the writer (this consumes the master, but we already have our dup'd fd)
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TdsrError::Pty(format!("Failed to get PTY writer: {}", e)))?;

        debug!("PTY created successfully with fd {}", fd);

        Ok(Self {
            reader,
            writer,
            _child: child,
            _fd_owner: fd_owner,
            fd,
            _size: size,
        })
    }

    /// Get the file descriptor for the PTY master
    ///
    /// Screen reader uses this fd in the event loop to monitor for new output
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Read output from the PTY
    ///
    /// This is the terminal output that the screen reader will parse and speak
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.reader.read(buf).map_err(TdsrError::Io)
    }

    /// Write input to the PTY
    ///
    /// Passes user keystrokes through to the shell
    pub fn write(&mut self, buf: &[u8]) -> Result<usize> {
        self.writer.write(buf).map_err(TdsrError::Io)
    }

    /// Flush writer
    pub fn flush(&mut self) -> Result<()> {
        self.writer.flush().map_err(TdsrError::Io)
    }

    /// Resize the terminal
    ///
    /// Called when user resizes their terminal window (SIGWINCH).
    /// Screen reader needs to resize both PTY and internal screen buffer.
    /// Note: portable-pty doesn't support runtime resize, so this is a no-op for now
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        debug!(
            "PTY resize requested to {}x{} (not supported by portable-pty)",
            rows, cols
        );
        // TODO: portable-pty doesn't support resizing after creation
        // May need to use nix directly for this
        Ok(())
    }
}
