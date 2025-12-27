//! TDSR main entry point
//!
//! The screen reader's main loop monitors three sources:
//! 1. stdin (user keyboard input) - passed to shell
//! 2. PTY output (shell output) - parsed and spoken
//! 3. Signals (SIGWINCH for resize) - updates screen size

use log::{debug, error, info};
use mio::{Events, Interest, Poll, Token};
use nix::libc;
use nix::sys::signal::{self, SigHandler, Signal};
use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use tdsr::input::{create_default_keymap, DefaultKeyHandler, HandlerAction};
use tdsr::platform::is_wsl;
use tdsr::state::State;
use tdsr::terminal::{get_terminal_size, restore_termios, set_raw_mode, Emulator, Pty};
use tdsr::Result;

/// Token for stdin in mio poll
const STDIN: Token = Token(0);
/// Token for PTY in mio poll
const PTY: Token = Token(1);

/// Global flag set by SIGWINCH handler
static RESIZE_PENDING: AtomicBool = AtomicBool::new(false);

/// SIGWINCH handler - sets flag when terminal is resized
extern "C" fn handle_sigwinch(_: libc::c_int) {
    RESIZE_PENDING.store(true, Ordering::Relaxed);
}

fn main() {
    // Parse command line arguments
    let args: Vec<String> = std::env::args().collect();
    let debug_mode = args.iter().any(|arg| arg == "--debug" || arg == "-d");

    // Initialize logger
    if debug_mode {
        // Debug mode: write to tdsr.log file
        use std::fs::OpenOptions;
        match OpenOptions::new()
            .create(true)
            .append(true)
            .open("tdsr.log")
        {
            Ok(log_file) => {
                env_logger::Builder::new()
                    .filter_level(log::LevelFilter::Debug)
                    .target(env_logger::Target::Pipe(Box::new(log_file)))
                    .init();
            }
            Err(e) => {
                eprintln!("Warning: Failed to open tdsr.log for debug logging: {}", e);
                eprintln!("Continuing without file logging...");
                // Initialize basic logging to stderr
                env_logger::Builder::new()
                    .filter_level(log::LevelFilter::Warn)
                    .init();
            }
        }

        info!(
            "TDSR version {} starting (debug mode, logging to tdsr.log)",
            tdsr::VERSION
        );
    } else {
        // Normal mode: minimal logging to stderr, only errors
        env_logger::Builder::from_default_env()
            .filter_level(log::LevelFilter::Error)
            .init();
    }

    // Run the application
    if let Err(e) = run() {
        error!("Fatal error: {}", e);
        process::exit(1);
    }
}

fn run() -> Result<()> {
    debug!("Initializing TDSR");

    // Verify stdin is a TTY
    // TDSR requires interactive terminal access
    let stdin_fd = io::stdin().as_raw_fd();
    if unsafe { libc::isatty(stdin_fd) } == 0 {
        eprintln!("Error: TDSR requires an interactive terminal (stdin is not a TTY)");
        eprintln!("Usage: Run TDSR directly in a terminal, not through pipes or redirects");
        eprintln!("Example: tdsr");
        process::exit(1);
    }

    // Get stdin fd and set raw mode
    // Raw mode lets screen reader capture all keystrokes including Ctrl+C
    let original_termios = set_raw_mode(stdin_fd)?;

    // Ensure we restore terminal on exit
    let _guard = TermiosGuard {
        fd: stdin_fd,
        termios: original_termios,
    };

    // Get current terminal size
    let (cols, rows) = get_terminal_size(stdin_fd)?;
    info!("Terminal size: {}x{}", cols, rows);

    // Parse command line arguments (filter out --debug flag)
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--debug" && arg != "-d")
        .collect();
    let program = if args.is_empty() { None } else { Some(args) };

    // Load configuration and initialize state
    // State holds all screen reader settings and UI state
    let mut state = State::new(cols, rows)?;
    info!("State initialized - config from {:?}", state.config.path());

    // Create PTY and spawn shell
    // This is the core of the screen reader - we sit between user and shell
    let mut pty = Pty::new(program, rows, cols)?;
    info!("PTY created, shell spawned");

    // Create terminal emulator
    // This maintains the screen buffer for review cursor navigation
    let mut emulator = Emulator::new(cols, rows);

    // Create default key handler for screen reader commands
    // This processes Alt+key combinations for navigation
    let keymap = create_default_keymap();
    info!("Key handler initialized with {} bindings", keymap.len());
    let mut default_handler = DefaultKeyHandler::new(keymap);

    // Set up signal handler for window resize
    unsafe {
        signal::signal(Signal::SIGWINCH, SigHandler::Handler(handle_sigwinch)).map_err(|e| {
            tdsr::TdsrError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("Failed to set SIGWINCH handler: {}", e),
            ))
        })?;
    }

    // Set up event loop
    // We monitor stdin and PTY for I/O
    let pty_fd = pty.as_raw_fd();

    // WSL doesn't support epoll on TTY file descriptors, so use select() instead
    let use_select = is_wsl();

    // Set up event loop infrastructure based on platform
    let mut mio_poll = if !use_select {
        debug!("Using mio::Poll for event loop");
        let poll = Poll::new()?;

        // Register stdin for reading
        let mut stdin_source = mio::unix::SourceFd(&stdin_fd);
        poll.registry()
            .register(&mut stdin_source, STDIN, Interest::READABLE)?;

        // Register PTY for reading
        let mut pty_source = mio::unix::SourceFd(&pty_fd);
        poll.registry()
            .register(&mut pty_source, PTY, Interest::READABLE)?;

        Some((poll, Events::with_capacity(128)))
    } else {
        debug!("Using select() for event loop (WSL mode)");
        None
    };

    info!("TDSR ready - entering event loop");

    // Speak welcome message
    state.speak("TDSR, presented by Lighthouse of San Francisco")?;

    println!("TDSR {} ready - All phases complete!", tdsr::VERSION);
    println!("Configuration loaded: {}", state.config.path().display());
    println!("  Process symbols: {}", state.config.process_symbols());
    println!("  Key echo: {}", state.config.key_echo());
    println!("  Cursor tracking: {}", state.config.cursor_tracking());
    println!("  Symbols loaded: {}", state.config.symbols.len());
    println!("Speech synthesizer initialized");
    println!("PTY running, screen buffer active");
    println!("Type 'exit' to quit");

    // Main event loop
    // Screen reader continuously monitors for:
    // - User input (to pass to shell)
    // - Shell output (to parse and speak)
    // - Window resize (to update dimensions)
    loop {
        // Check for pending resize
        if RESIZE_PENDING.swap(false, Ordering::Relaxed) {
            // Terminal was resized - update everything
            let (new_cols, new_rows) = get_terminal_size(stdin_fd)?;
            info!("Terminal resized to {}x{}", new_cols, new_rows);

            pty.resize(new_rows, new_cols)?;
            emulator.resize(new_cols, new_rows);
            state.resize(new_cols, new_rows);
        }

        // Run any scheduled functions that are ready
        let screen = emulator.screen();
        if let Err(e) = state.run_scheduled(screen) {
            error!("Error running scheduled function: {}", e);
        }

        if use_select {
            // WSL mode: Use select() for I/O monitoring
            use nix::sys::select::{select, FdSet};
            use nix::sys::time::{TimeVal, TimeValLike};
            use std::os::unix::io::BorrowedFd;

            // Create borrowed FDs for select (must be created each iteration)
            let stdin_borrowed = unsafe { BorrowedFd::borrow_raw(stdin_fd) };
            let pty_borrowed = unsafe { BorrowedFd::borrow_raw(pty_fd) };

            // Rebuild FdSet each iteration (select() modifies it)
            let mut read_fds = FdSet::new();
            read_fds.insert(stdin_borrowed);
            read_fds.insert(pty_borrowed);

            // Calculate timeout based on scheduled functions or use default
            let mut timeout = if let Some(delay) = state.time_until_next_scheduled() {
                // Use the delay until next scheduled function, max 100ms
                let ms = delay.as_millis().min(100) as i64;
                TimeVal::milliseconds(ms)
            } else {
                // No scheduled functions, use 100ms default for resize checks
                TimeVal::milliseconds(100)
            };

            match select(None, Some(&mut read_fds), None, None, Some(&mut timeout)) {
                Ok(_n) => {
                    if read_fds.contains(stdin_borrowed) {
                        if let Err(e) =
                            handle_stdin(&mut pty, &mut state, &mut emulator, &mut default_handler)
                        {
                            error!("stdin error: {}", e);
                            return Ok(());
                        }
                    }
                    if read_fds.contains(pty_borrowed) {
                        if let Err(e) = handle_pty_output(&mut pty, &mut emulator, &mut state) {
                            if e.to_string().contains("EOF") {
                                info!("PTY closed (shell exited)");
                                return Ok(());
                            }
                            error!("PTY error: {}", e);
                            return Err(e);
                        }
                    }
                }
                Err(nix::errno::Errno::EBADF) => {
                    error!("select() failed: Bad file descriptor");
                    return Err(tdsr::TdsrError::Io(io::Error::from_raw_os_error(9)));
                }
                Err(nix::errno::Errno::EINTR) => {
                    debug!("select() interrupted by signal");
                }
                Err(e) => {
                    error!("select() error: {:?}", e);
                    return Err(tdsr::TdsrError::Io(io::Error::from_raw_os_error(e as i32)));
                }
            }
        } else if let Some((ref mut poll, ref mut events)) = mio_poll {
            // Regular mode: Use mio for I/O monitoring
            // Calculate timeout based on scheduled functions or use default
            let timeout = state
                .time_until_next_scheduled()
                .map(|d| d.min(std::time::Duration::from_millis(100)))
                .or(Some(std::time::Duration::from_millis(100)));

            poll.poll(events, timeout)?;

            for event in events.iter() {
                match event.token() {
                    STDIN => {
                        if let Err(e) =
                            handle_stdin(&mut pty, &mut state, &mut emulator, &mut default_handler)
                        {
                            error!("stdin error: {}", e);
                            return Ok(());
                        }
                    }
                    PTY => {
                        if let Err(e) = handle_pty_output(&mut pty, &mut emulator, &mut state) {
                            if e.to_string().contains("EOF") {
                                info!("PTY closed (shell exited)");
                                return Ok(());
                            }
                            error!("PTY error: {}", e);
                            return Err(e);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Handle user input from stdin
///
/// Screen reader intercepts keystrokes to implement navigation commands.
/// Keys not handled by screen reader are passed through to the shell.
fn handle_stdin(
    pty: &mut Pty,
    state: &mut State,
    emulator: &mut Emulator,
    default_handler: &mut DefaultKeyHandler,
) -> Result<()> {
    let mut buf = [0u8; 4096];

    let n = io::stdin().read(&mut buf)?;
    if n == 0 {
        return Ok(());
    }

    let input = &buf[..n];

    // Cancel any pending speech and clear delayed functions
    state.cancel_speech()?;
    state.clear_delayed_functions();

    // Process through handler stack if there are modal handlers active
    // (e.g., config menu, copy mode, buffer input)
    if !state.handlers.is_empty() {
        // Temporarily pop the handler to avoid borrow checker issues
        if let Some(mut handler) = state.handlers.pop() {
            let action = handler.process_with_context(input, state, emulator)?;

            match action {
                HandlerAction::Passthrough => {
                    // Push handler back (it wants to stay active)
                    state.handlers.push(handler);
                    // Track the last key for key_echo feature
                    if input.len() == 1 {
                        let ch = input[0] as char;
                        if ch.is_ascii_graphic() || ch == ' ' {
                            state.last_key = Some(ch);
                        }
                    } else {
                        state.last_key = None;
                    }
                    // Pass key through to shell
                    pty.write(input)?;
                }
                HandlerAction::Remove => {
                    // Handler removed itself, don't push back
                }
                HandlerAction::Handled => {
                    // Push handler back (it wants to stay active)
                    state.handlers.push(handler);
                }
            }
        }
        return Ok(());
    }

    // No modal handlers - process with default screen reader bindings
    let action = default_handler.process_key(input, state, emulator)?;

    match action {
        HandlerAction::Passthrough => {
            // Not a screen reader command - pass to shell
            // Track the last key for key_echo feature
            if input.len() == 1 {
                let ch = input[0] as char;
                if ch.is_ascii_graphic() || ch == ' ' {
                    state.last_key = Some(ch);
                }
            } else {
                // Multi-byte input (escape sequence, etc.) - clear last key
                state.last_key = None;
            }
            pty.write(input)?;
        }
        HandlerAction::Handled => {
            // Screen reader command was executed
        }
        HandlerAction::Remove => {
            // This shouldn't happen for default handler
        }
    }

    Ok(())
}

/// Handle output from PTY
///
/// This is the core screen reader function - we parse terminal output,
/// update the screen buffer, and speak new content automatically.
fn handle_pty_output(pty: &mut Pty, emulator: &mut Emulator, state: &mut State) -> Result<()> {
    let mut buf = [0u8; 4096];

    let n = pty.read(&mut buf)?;
    if n == 0 {
        return Err("EOF from PTY".into());
    }

    let output = &buf[..n];

    // Save cursor position before processing
    let old_cursor = emulator.cursor();

    // Echo output to user's terminal (passthrough)
    io::stdout().write_all(output)?;
    io::stdout().flush()?;

    // Parse output and update screen buffer + speech buffer
    // If quiet mode or temp_silence is active, don't add to speech
    if !state.quiet && !state.temp_silence {
        let line_pause = state.config.line_pause();
        let key_echo = state.config.key_echo();
        let last_key = state.last_key;

        emulator.process_with_speech(
            output,
            &mut state.speech_buffer,
            &mut state.last_drawn,
            line_pause,
        )?;

        // Key echo: if output is just the last typed character being echoed,
        // speak it as a character (phonetically) instead of normal speech
        if key_echo {
            if let Some(typed_key) = last_key {
                // Check if output is just the single character we typed
                // (terminals typically echo the character immediately)
                if output.len() == 1 && output[0] as char == typed_key {
                    // Speak the character and skip normal speech buffer
                    state.speak_char(typed_key)?;
                    state.speech_buffer.flush(); // Discard the character from buffer
                    state.last_key = None;
                    return Ok(());
                }
            }
        }

        // Clear last_key after processing (echo window has passed)
        state.last_key = None;

        // If line_pause is enabled, speak each line separately
        if line_pause && state.speech_buffer.has_pending_lines() {
            for line in state.speech_buffer.drain_lines() {
                if !line.is_empty() {
                    state.speak(&line)?;
                }
            }
        }

        // Flush any remaining buffer content to TTS
        if !state.speech_buffer.is_empty() {
            let text = state.speech_buffer.flush();
            state.speak(&text)?;
        }
    } else {
        // Quiet mode - just update screen buffer without speech
        emulator.process(output)?;
    }

    // Adjust review cursor for any scrolling that occurred
    let scroll_offset = emulator.screen_mut().take_scroll_offset();
    if scroll_offset != 0 {
        let rows = emulator.screen().size.1;
        state.adjust_review_cursor_for_scroll(scroll_offset, rows);
    }

    // Update review cursor if cursor tracking is enabled and cursor moved
    let new_cursor = emulator.cursor();
    if old_cursor != new_cursor {
        state.update_review_cursor_from_terminal(new_cursor);
    }

    Ok(())
}

/// RAII guard to restore terminal on exit
///
/// Ensures terminal is always returned to normal mode even if screen reader crashes
struct TermiosGuard {
    fd: RawFd,
    termios: libc::termios,
}

impl Drop for TermiosGuard {
    fn drop(&mut self) {
        restore_termios(self.fd, &self.termios);
        debug!("Terminal attributes restored");
    }
}
