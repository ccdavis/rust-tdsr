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
use std::time::Duration;
use tdsr::input::{
    create_default_keymap, dispatch_key, is_terminal_response, split_keys, DefaultKeyHandler,
};
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

    // macOS speech-server mode: AVSpeechSynthesizer needs a serviced run loop
    // on the process main thread, so we re-exec ourselves as a child that does
    // exactly that. This must run before any terminal/PTY setup and never
    // returns (it owns this process's main thread). The logger is already up
    // so a `--debug` child appends to the same tdsr.log as the parent.
    #[cfg(target_os = "macos")]
    if args
        .iter()
        .any(|arg| arg == tdsr::speech::backends::avfoundation::SPEECH_SERVER_FLAG)
    {
        tdsr::speech::backends::avfoundation::run_speech_server();
    }

    // List installed macOS voices and exit (for choosing `voice_idx`).
    #[cfg(target_os = "macos")]
    if args
        .iter()
        .any(|arg| arg == tdsr::speech::backends::avfoundation::LIST_VOICES_FLAG)
    {
        tdsr::speech::backends::avfoundation::list_voices();
    }

    // Run the application and exit with the shell's status
    match run() {
        Ok(code) => process::exit(code),
        Err(e) => {
            error!("Fatal error: {}", e);
            eprintln!("tdsr: {}", e);
            process::exit(1);
        }
    }
}

fn run() -> Result<i32> {
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

    // Parse command line arguments: our own flags, then the program to run
    let mut speech_command: Option<String> = None;
    let mut program_args: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--debug" | "-d" | "--speech-server" | "--list-voices" => {}
            "--speech-command" => speech_command = args.next(),
            _ => program_args.push(arg),
        }
    }
    let program = if program_args.is_empty() {
        None
    } else {
        Some(program_args)
    };

    // Load configuration and initialize state
    // State holds all screen reader settings and UI state
    let mut state = State::new(cols, rows, speech_command)?;
    info!("State initialized - config from {:?}", state.config.path());

    // Create PTY and spawn shell
    // This is the core of the screen reader - we sit between user and shell
    let mut pty = Pty::new(program, rows, cols, Some(&original_termios))?;
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

    info!("TDSR {} ready", tdsr::VERSION);
    info!("Configuration loaded: {}", state.config.path().display());
    info!("  Process symbols: {}", state.config.process_symbols());
    info!("  Key echo: {}", state.config.key_echo());
    info!("  Cursor tracking: {}", state.config.cursor_tracking());
    info!("  Symbols loaded: {}", state.config.symbols.len());

    // Main event loop
    // Screen reader continuously monitors for:
    // - User input (to pass to shell)
    // - Shell output (to parse and speak)
    // - Window resize (to update dimensions)
    // - The shell exiting
    let mut child_exited = false;
    let mut stdin_failed = false;
    'main: loop {
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

        // Speak results from plugins that finished on their worker threads
        if let Err(e) = state.poll_plugin_results() {
            error!("Error delivering plugin result: {}", e);
        }

        // Notice when the shell exits even if a background job still holds
        // the PTY open (which would otherwise keep us alive forever).
        if !child_exited && pty.try_wait()?.is_some() {
            info!("Shell exited; draining remaining output");
            child_exited = true;
        }

        // Wait at most 100ms so resize/scheduled checks stay responsive; once
        // the shell is gone, just a short grace period for its last output.
        let max_wait = Duration::from_millis(if child_exited { 50 } else { 100 });
        let timeout = state
            .time_until_next_scheduled()
            .map_or(max_wait, |d| d.min(max_wait));

        let mut pty_event = false;

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

            let mut timeout = TimeVal::milliseconds(timeout.as_millis() as i64);

            match select(None, Some(&mut read_fds), None, None, Some(&mut timeout)) {
                Ok(_n) => {
                    if read_fds.contains(stdin_borrowed) {
                        if let Err(e) =
                            handle_stdin(&mut pty, &mut state, &mut emulator, &mut default_handler)
                        {
                            error!("stdin error: {}", e);
                            stdin_failed = true;
                            break 'main;
                        }
                    }
                    if read_fds.contains(pty_borrowed) {
                        pty_event = true;
                        // WSL keeps its original one-read-per-readiness cadence
                        // (see the drain note in handle_pty_output).
                        if !handle_pty_output(&mut pty, &mut emulator, &mut state, false)? {
                            info!("PTY closed (shell exited)");
                            break 'main;
                        }
                    }
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
            // Regular mode: Use mio for I/O monitoring.
            // mio does not retry on EINTR. A SIGWINCH (terminal resize) that
            // lands while we're parked in kqueue/epoll surfaces here as an
            // Interrupted error; treat it like a timeout and loop around so the
            // RESIZE_PENDING check at the top of the loop picks it up.
            if let Err(e) = poll.poll(events, Some(timeout)) {
                if e.kind() == io::ErrorKind::Interrupted {
                    debug!("poll() interrupted by signal");
                    continue;
                }
                return Err(e.into());
            }

            for event in events.iter() {
                match event.token() {
                    STDIN => {
                        if let Err(e) =
                            handle_stdin(&mut pty, &mut state, &mut emulator, &mut default_handler)
                        {
                            error!("stdin error: {}", e);
                            stdin_failed = true;
                            break 'main;
                        }
                    }
                    PTY => {
                        pty_event = true;
                        if !handle_pty_output(&mut pty, &mut emulator, &mut state, true)? {
                            info!("PTY closed (shell exited)");
                            break 'main;
                        }
                    }
                    _ => {}
                }
            }
        }

        if child_exited && !pty_event {
            debug!("No more output after shell exit");
            break;
        }
    }

    if stdin_failed {
        // Our own terminal went away; don't wait on a shell that may still be
        // running (it will get SIGHUP when the PTY closes).
        return Ok(1);
    }

    let code = pty.wait()?;
    info!("Shell exited with status {}", code);
    Ok(code as i32)
}

/// Handle user input from stdin
///
/// Screen reader intercepts keystrokes to implement navigation commands.
/// Keys not handled by screen reader are passed through to the shell.
///
/// A zero-length read means our terminal went away; that is reported as an
/// error so the event loop stops instead of spinning on a readable EOF.
fn handle_stdin(
    pty: &mut Pty,
    state: &mut State,
    emulator: &mut Emulator,
    default_handler: &mut DefaultKeyHandler,
) -> Result<()> {
    let mut buf = [0u8; 4096];

    let n = io::stdin().read(&mut buf)?;
    if n == 0 {
        return Err(tdsr::TdsrError::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "terminal closed (EOF on stdin)",
        )));
    }

    let input = &buf[..n];

    // One read can hold several keystrokes (auto-repeat, fast typing, paste).
    // Split it into key sequences so each one is matched on its own.
    let keys = split_keys(input);

    // Any keypress silences speech and cancels pending cursor-tracking speech.
    // Terminal replies to application queries (cursor position, device
    // attributes, focus events) are not keypresses and leave speech alone.
    if keys.iter().any(|k| !is_terminal_response(k)) {
        state.cancel_speech()?;
        state.clear_delayed_functions();
    }

    for key in keys {
        if is_terminal_response(key) {
            pty.write(key)?;
            continue;
        }
        match dispatch_key(key, state, emulator, default_handler) {
            Ok(true) => {
                track_typed_key(state, key);
                pty.write(key)?;
            }
            Ok(false) => {}
            // A failing command (e.g. the config file could not be saved)
            // is reported, not fatal.
            Err(e) => {
                error!("Key command failed: {}", e);
                let _ = state.speak("error");
            }
        }
    }

    Ok(())
}

/// Record a key that is being forwarded to the shell, for key echo and for
/// tracking the current command line (used by plugins).
fn track_typed_key(state: &mut State, key: &[u8]) {
    if key.len() != 1 {
        // Escape sequence, multi-byte character, paste: not a single typed key
        state.last_key = None;
        return;
    }
    let ch = key[0] as char;
    if ch.is_ascii_graphic() || ch == ' ' {
        state.last_key = Some(ch);
        // Accumulate typed characters to track the current command
        state.input_line.push(ch);
        return;
    }
    state.last_key = None;
    match ch {
        // Enter - save accumulated input as last_command
        '\r' | '\n' => {
            if !state.input_line.is_empty() {
                state.last_command = std::mem::take(&mut state.input_line);
            }
        }
        // Backspace - remove last character from input line
        '\x08' | '\x7f' => {
            state.input_line.pop();
        }
        // Ctrl+C or Ctrl+U - clear input line
        '\x03' | '\x15' => state.input_line.clear(),
        _ => {}
    }
}

/// Handle output from PTY
///
/// This is the core screen reader function - we parse terminal output,
/// update the screen buffer, and queue new content for speech.
///
/// When `drain` is set, everything the PTY has ready is read in one go
/// (bounded by `MAX_DRAIN`) so output written in several pieces becomes one
/// utterance. Draining relies on `Pty::has_more` (a non-blocking `poll`)
/// being accurate: a false "readable" would make the next blocking `read`
/// hang the loop. That is trustworthy on native Linux and macOS, but WSL is
/// exactly where TTY fd polling has been unreliable, so `main` passes
/// `drain = false` on WSL. There the deferred flush (below) still coalesces
/// output across event-loop iterations, so speech is not fragmented.
///
/// Returns `Ok(false)` once the PTY has closed (the shell exited): macOS
/// reports that as a zero-length read, Linux as `EIO`.
fn handle_pty_output(
    pty: &mut Pty,
    emulator: &mut Emulator,
    state: &mut State,
    drain: bool,
) -> Result<bool> {
    /// Upper bound on bytes handled per event, so a flood can't starve the
    /// keyboard for long (the loop comes straight back for the rest).
    const MAX_DRAIN: usize = 64 * 1024;

    let mut buf = [0u8; 4096];
    let mut total = 0;

    // Save cursor position before processing
    let old_cursor = emulator.cursor();

    loop {
        let n = match pty.read(&mut buf) {
            Ok(0) => return Ok(false),
            Ok(n) => n,
            Err(tdsr::TdsrError::Io(ref e)) if e.raw_os_error() == Some(libc::EIO) => {
                return Ok(false)
            }
            Err(e) => return Err(e),
        };
        total += n;
        let output = &buf[..n];

        // Echo output to user's terminal (passthrough)
        io::stdout().write_all(output)?;
        io::stdout().flush()?;

        // Parse output and update screen buffer + speech buffer. Key echo is
        // detected while drawing: the first character drawn after a keypress
        // that equals the typed key is the shell echoing it.
        let line_pause = state.config.line_pause();
        let echoed = emulator.process_with_speech(
            output,
            &mut state.speech_buffer,
            &mut state.last_drawn,
            line_pause,
            &mut state.last_key,
        )?;
        if let Some(ch) = echoed {
            if state.config.key_echo() {
                state.speak_char(ch)?;
            }
        }

        if !drain || total >= MAX_DRAIN || !pty.has_more() {
            break;
        }
    }

    if state.quiet || state.temp_silence {
        // Not reading automatically right now: keep the screen, drop the text
        state.speech_buffer.drain_lines();
        state.speech_buffer.flush();
    } else if state.speech_buffer.has_pending_lines() || !state.speech_buffer.is_empty() {
        state.schedule_speech_flush();
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

    Ok(true)
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
