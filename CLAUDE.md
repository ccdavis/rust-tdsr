# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

TDSR is a console-based screen reader for *nix systems (Linux, macOS, FreeBSD, WSL). It uses a pseudo-terminal (PTY) to intercept terminal I/O and provides text-to-speech feedback via native speech synthesis. It is an AI-assisted Rust port of Tyler Spivey's [Python TDSR](https://github.com/tspivey/tdsr); currently alpha.

The crate builds both a binary (`src/main.rs`) and a library (`src/lib.rs`). Tests link against the library, so logic that needs test coverage lives in `lib`-exported modules, not in `main.rs`.

## Development Commands

```bash
# Build
cargo build --release

# Run tests
cargo test

# Run with debug logging
cargo run -- --debug

# Build script (checks dependencies)
./build.sh
./build.sh --no-test
./build.sh --clean
```

## Architecture

### Module Structure

```
src/
├── main.rs              # Entry point and mio event loop
├── lib.rs               # Library exports (what tests link against)
├── platform.rs          # Platform detection (WSL, Linux, macOS)
├── symbols.rs           # Symbol-to-word mapping
├── clipboard.rs         # Clipboard copy (arboard)
├── error.rs             # Crate error types
├── terminal/
│   ├── mod.rs           # Module exports
│   ├── pty.rs           # PTY spawn/resize (portable-pty)
│   ├── emulator.rs      # vte parser driver
│   ├── performer.rs     # vte Perform impl → screen + speech buffer (largest file)
│   ├── screen.rs        # Screen buffer + scrollback history
│   ├── cell.rs          # Character cell
│   └── util.rs          # termios raw mode, terminal size
├── speech/
│   ├── mod.rs           # Speech module exports
│   ├── synth.rs         # Synth trait and backend selection
│   ├── buffer.rs        # Speech buffer accumulation
│   └── backends/
│       ├── mod.rs       # Backend exports
│       ├── command.rs   # Line-protocol speech server subprocess (macOS server, speech_command)
│       ├── native.rs    # tts crate (Speech Dispatcher); not compiled on macOS
│       ├── windows.rs   # Windows SAPI via PowerShell (WSL)
│       ├── pulseaudio.rs # espeak-ng with PulseAudio (WSLG fallback)
│       └── avfoundation.rs # macOS: subprocess speech server (primary)
├── input/
│   ├── mod.rs           # Input module exports
│   ├── handler.rs       # KeyHandler trait, HandlerResult
│   ├── keymap.rs        # Alt+key to action mapping
│   ├── tokenize.rs      # Split one stdin read into key sequences
│   ├── dispatch.rs      # Route a key to modal stack or default handler
│   ├── default_handler.rs # Main navigation handler
│   ├── config_handler.rs  # Alt+c config menu
│   ├── buffer_handler.rs  # Text input for config values
│   └── copy_handler.rs    # Alt+v copy mode
├── state/
│   ├── mod.rs           # AppState, global state management
│   ├── config.rs        # Config loading/saving (~/.tdsr.cfg)
│   └── phonetics.rs     # NATO phonetic alphabet
├── review/
│   └── mod.rs           # Review cursor navigation
└── plugins/
    └── mod.rs           # Plugin subprocess protocol (JSON)
```

### Key Components

**Event Loop** (`main.rs`): Uses `mio` to poll stdin and the PTY with a 100 ms timeout (select() on WSL). SIGWINCH sets a flag checked each iteration; an `Interrupted` poll result is treated as a timeout, not an error. The loop ends on PTY EOF/EIO or when `Pty::try_wait` reports the shell gone (after a short drain), and `run()` returns the shell's exit status, which becomes TDSR's.

**Terminal Emulation** (`terminal/`): `emulator.rs` drives the `vte` ANSI parser; the custom `Perform` impl lives in `performer.rs` (the largest module) and is where screen-buffer updates and speech-buffer population happen — most output/announcement behavior changes go here. `pty.rs` owns the child PTY.

**Speech System** (`speech/`):
- `Synth` trait defines speak/cancel/set_rate/set_volume/set_voice
- Backend selection in `synth.rs`: WSL → PulseAudio/SAPI/native; Linux → native/PulseAudio; macOS → avfoundation (subprocess), native fallback
- PulseAudio backend uses a persistent espeak-ng process (stdin line-by-line mode) for proper speech queuing
- Supports MBROLA voices (indices 10+) for higher quality speech on Linux/WSL
- `handle_pty_output` drains everything the PTY has ready (up to 64 KB per event, via `Pty::has_more`) and then schedules `State::flush_speech` 5 ms later (`schedule_speech_flush`, guarded by `delaying_output`), so output written in several pieces becomes one utterance. A keypress cancels the pending flush and drops the unspoken text. `speak_output` applies repeated-symbol condensing; `speak` trims and skips empty text.
- The performer inserts separators when the cursor jumps (including row changes via CUP), keeps auto-wrapped words whole, and treats text drawn after a carriage return on the same row as a rewrite that replaces that row's queued speech (progress bars are spoken once per flush, not once per redraw).
- Key echo is detected at draw time: `State::last_key` is handed to the performer as `echo_key`; the first character drawn that changes a screen cell and equals it is the shell's echo, kept out of the speech buffer and returned so `main` can speak it as a letter (if `key_echo` is on). Characters that repaint a cell unchanged while a key is pending are queued provisionally (`SpeechBuffer::note_redraw`) and dropped when the echo arrives: zsh backspaces over the word being typed and redraws all of it (`BS c d` for the second letter of `cd`), which would otherwise be read as "cd". This also works when zsh wraps the echo in escape sequences.
- DEC special graphics (`ESC ( 0`, SI/SO) are translated to box-drawing characters (`Screen::map_charset`), so ncurses/tmux borders are not read as "lqqqk".
- Speech backend failures are non-fatal: `State::synth_op` logs them. `CommandSynth` respawns a dead server once and then backs off for a second between attempts.

**External speech servers**: `backends/command.rs` (`CommandSynth`) starts any program and drives it over stdin with the original TDSR line protocol (`s`, `l`, `x`, `r`, `v`, `V`). `[speech] speech_command` or `--speech-command` selects one on any platform; the macOS server below is the same mechanism pointed at our own binary.

**macOS speech is a subprocess** (`backends/avfoundation.rs`), not the `tts` crate. `AVSpeechSynthesizer` only advances its utterance queue while a CFRunLoop is serviced on the process **main thread**; TDSR's main thread parks in mio's kqueue `poll()`, so driving the synth in-process plays only the first utterance and then goes silent. Instead, `spawn_server_synth` re-execs our own binary as `tdsr --speech-server` (detected at the top of `main()`, before any terminal setup) and the child runs `AVSpeechSynthesizer` on its main thread with a real `CFRunLoopRun()`. The parent sends a line protocol over the child's stdin: `s<text>`/`l<text>` (speak), `x` (cancel), `r`/`v`/`V` (rate/volume/voice). This mirrors the original Python TDSR's macOS design. Frameworks (`objc`, `core-foundation`, `core-foundation-sys`) are macOS-only deps in `Cargo.toml`.

Voice selection (priority in `Worker::speak`): explicit `voice_idx` → `AVSpeechSynthesisVoice.speechVoices()[idx]`, resolved and retained once in `select_voice` when the index is set (never per utterance: `speechVoices()` is slow); else the default voice resolved at startup by `find_default_voice()` — **Eloquence, Reed variant** if installed (detected by identifier containing "eloquence", variant via the last identifier component `.reed`, preferring the current language), retained for the process; else `prefersAssistiveTechnologySettings` to follow the VoiceOver voice (the only way to reach Siri voices). Config default `voice_idx` is `-1` → `None`. `tdsr --list-voices` (handled at the top of `main()`, like `--speech-server`) prints voices with indices and flags Eloquence rows.

**Input Handling** (`input/`): Stack-based modal handlers. `HandlerResult::Passthrough` sends key to PTY, `Remove` pops handler. `main.rs` splits each stdin read with `split_keys` (key repeat and fast typing coalesce several keys into one read), forwards terminal replies (`is_terminal_response`: cursor position reports, DA, OSC/DCS replies, focus events) to the PTY without silencing speech, and routes each real key through `dispatch_key`, which is the only place the modal stack is popped/reinserted; keep that logic in the library so it stays testable (`tests/modal_input_test.rs` drives it with a recording synth and a temp config via `Config::load_from` + `State::from_parts`).

**Configuration** (`state/config.rs`): INI format in `~/.tdsr.cfg`. Sections: `[speech]`, `[symbols]`, `[plugins]`, `[commands]`. `Pty::new` applies the user's pre-raw-mode termios to the child PTY so the shell inherits erase/flow-control settings.

### Speech Backend Priority

**WSL:**
1. PulseAudio + espeak-ng (if WSLG available)
2. Windows SAPI via PowerShell
3. Speech Dispatcher (fallback)

**Linux:**
1. Speech Dispatcher (via tts crate)
2. PulseAudio + espeak-ng

**macOS:**
1. AVFoundation via `tdsr --speech-server` subprocess (see `backends/avfoundation.rs`) — the only backend. The `tts` crate is not a dependency on macOS (its in-process AVFoundation path plays one utterance and goes silent). If the subprocess cannot start, `create_synth` speaks the error through `/usr/bin/say` (`say_blocking`) and returns it, so TDSR exits with an audible reason.

## Key Bindings

**Alt = ESC prefix.** All `Alt+key` bindings (`keymap.rs`, e.g. `b"\x1bu"`) require the terminal to send Option/Alt as Meta (ESC-prefixed). On macOS this is off by default — Terminal.app: "Use Option as Meta key"; iTerm2: Option key → "Esc+". See README "Terminal setup". This is inherent to terminal input, not configurable in TDSR.

Review cursor navigation uses Alt+key:
- Line: `Alt+u/i/o` (prev/current/next)
- Word: `Alt+j/k/l` (prev/current/next), double-tap `Alt+k` to spell
- Char: `Alt+m/,/.` (prev/current/next), double-tap `Alt+,` for phonetic
- Screen: `Alt+U/O` (top/bottom), `Alt+M/>` (start/end of line). `Alt+U` when already on the top row jumps to the oldest scrolled-off line.
- Scrollback: `Alt+u` from the top row keeps going into lines that scrolled off (`Screen::history`, capped at `MAX_HISTORY` = 2000, recorded only when the main screen's top row scrolls out). `ReviewCursor::above` counts how far above the screen the cursor is; every read goes through `Screen::get_line_at`/`get_char_at`, so word/char review and line copy work there. `Alt+o`/`Alt+O` and cursor tracking bring it back to the screen; `adjust_review_cursor_for_scroll` follows content into the history.
- Config: `Alt+c` (then a letter; `?` lists keys; Enter or Escape leaves; unknown keys are announced), Copy: `Alt+v`, Quiet: `Alt+q`, Cancel: `Alt+x`
- Numeric entry inside the config menu (rate, volume, voice, delay) accepts digits only, echoes each digit, and Escape or Enter on an empty value cancels

## Configuration

File: `~/.tdsr.cfg` (INI format)

```ini
[speech]
rate = 50              # 0-100
volume = 80            # 0-100
voice_idx = 0          # Voice index
cursor_delay = 300     # ms before speaking cursor position
speech_command = python3 ~/my_server.py   # optional external speech server
process_symbols = false
key_echo = true
cursor_tracking = true

[symbols]
33 = bang              # ! → "bang"

[plugins]
my_plugin = d          # Alt+d runs ~/.tdsr/plugins/my_plugin.py

[commands]
my_plugin = ^git\b     # Only run after git commands
```

## Plugin System

Plugins are executables that receive JSON on stdin and return JSON on stdout:

**Input:** `{"lines": ["line3", "line2", "line1"], "last_command": "ls"}`
**Output:** `{"speak": ["text to speak"]}`

Plugins live in `~/.tdsr/plugins/`. For a plugin named `foo`, TDSR runs `foo` directly if it is an executable file (any language, via its shebang), otherwise `python3 foo.py`. Dots in the name map to subdirectories (`me.foo` → `me/foo`). Plugins run on a worker thread (`PluginManager::execute_plugin_async`); the event loop drains results via `State::poll_plugin_results` each iteration and speaks them, so keys and output are never blocked. A plugin is killed after `PLUGIN_TIMEOUT` (10 s) and the timeout is spoken as a plugin error.

## Build Dependencies

**Linux:** `libclang-dev`, `libspeechd-dev`
**macOS:** None (uses system frameworks)
**WSL:** Same as Linux (uses Windows SAPI at runtime)

## Testing

```bash
cargo test                    # All tests
cargo test speech             # Speech tests only
cargo test -- --nocapture     # See output
```

Tests handle missing TTS gracefully for CI environments.
