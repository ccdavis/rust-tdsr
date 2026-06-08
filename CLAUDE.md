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
│   ├── screen.rs        # Screen buffer
│   ├── cell.rs          # Character cell
│   └── util.rs          # termios raw mode, terminal size
├── speech/
│   ├── mod.rs           # Speech module exports
│   ├── synth.rs         # Synth trait and backend selection
│   ├── buffer.rs        # Speech buffer accumulation
│   └── backends/
│       ├── mod.rs       # Backend exports
│       ├── native.rs    # tts crate (Speech Dispatcher; macOS fallback)
│       ├── windows.rs   # Windows SAPI via PowerShell (WSL)
│       ├── pulseaudio.rs # espeak-ng with PulseAudio (WSLG fallback)
│       └── avfoundation.rs # macOS: subprocess speech server (primary)
├── input/
│   ├── mod.rs           # Input module exports
│   ├── handler.rs       # KeyHandler trait, HandlerResult
│   ├── keymap.rs        # Alt+key to action mapping
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

**Event Loop** (`main.rs`): Uses `mio` to poll stdin, PTY, and signal pipe. Handles SIGWINCH for terminal resize.

**Terminal Emulation** (`terminal/`): `emulator.rs` drives the `vte` ANSI parser; the custom `Perform` impl lives in `performer.rs` (the largest module) and is where screen-buffer updates and speech-buffer population happen — most output/announcement behavior changes go here. `pty.rs` owns the child PTY.

**Speech System** (`speech/`):
- `Synth` trait defines speak/cancel/set_rate/set_volume/set_voice
- Backend selection in `synth.rs`: WSL → PulseAudio/SAPI/native; Linux → native/PulseAudio; macOS → avfoundation (subprocess), native fallback
- PulseAudio backend uses a persistent espeak-ng process (stdin line-by-line mode) for proper speech queuing
- Supports MBROLA voices (indices 10+) for higher quality speech on Linux/WSL
- Speech buffer accumulates text, flushed on cursor movement or timer

**macOS speech is a subprocess** (`backends/avfoundation.rs`), not the `tts` crate. `AVSpeechSynthesizer` only advances its utterance queue while a CFRunLoop is serviced on the process **main thread**; TDSR's main thread parks in mio's kqueue `poll()`, so driving the synth in-process plays only the first utterance and then goes silent. Instead, `AvServerSynth` re-execs our own binary as `tdsr --speech-server` (detected at the top of `main()`, before any terminal setup) and the child runs `AVSpeechSynthesizer` on its main thread with a real `CFRunLoopRun()`. The parent sends a line protocol over the child's stdin: `s<text>`/`l<text>` (speak), `x` (cancel), `r`/`v`/`V` (rate/volume/voice). This mirrors the original Python TDSR's macOS design. Frameworks (`objc`, `core-foundation`, `core-foundation-sys`) are macOS-only deps in `Cargo.toml`.

Voice selection (priority in `Worker::speak`): explicit `voice_idx` → `AVSpeechSynthesisVoice.speechVoices()[idx]`; else the default voice resolved at startup by `find_default_voice()` — **Eloquence, Reed variant** if installed (detected by identifier containing "eloquence", variant via the last identifier component `.reed`, preferring the current language), retained for the process; else `prefersAssistiveTechnologySettings` to follow the VoiceOver voice (the only way to reach Siri voices). Config default `voice_idx` is `-1` → `None`. `tdsr --list-voices` (handled at the top of `main()`, like `--speech-server`) prints voices with indices and flags Eloquence rows.

**Input Handling** (`input/`): Stack-based modal handlers. `HandlerResult::Passthrough` sends key to PTY, `Remove` pops handler.

**Configuration** (`state/config.rs`): INI format in `~/.tdsr.cfg`. Sections: `[speech]`, `[symbols]`, `[plugins]`, `[commands]`.

### Speech Backend Priority

**WSL:**
1. PulseAudio + espeak-ng (if WSLG available)
2. Windows SAPI via PowerShell
3. Speech Dispatcher (fallback)

**Linux:**
1. Speech Dispatcher (via tts crate)
2. PulseAudio + espeak-ng

**macOS:**
1. AVFoundation via `tdsr --speech-server` subprocess (see `backends/avfoundation.rs`)
2. tts crate (Speech Dispatcher / AppKit) — fallback if the subprocess can't spawn

## Key Bindings

**Alt = ESC prefix.** All `Alt+key` bindings (`keymap.rs`, e.g. `b"\x1bu"`) require the terminal to send Option/Alt as Meta (ESC-prefixed). On macOS this is off by default — Terminal.app: "Use Option as Meta key"; iTerm2: Option key → "Esc+". See README "Terminal setup". This is inherent to terminal input, not configurable in TDSR.

Review cursor navigation uses Alt+key:
- Line: `Alt+u/i/o` (prev/current/next)
- Word: `Alt+j/k/l` (prev/current/next), double-tap `Alt+k` to spell
- Char: `Alt+m/,/.` (prev/current/next), double-tap `Alt+,` for phonetic
- Screen: `Alt+U/O` (top/bottom), `Alt+M/>` (start/end of line)
- Config: `Alt+c`, Copy: `Alt+v`, Quiet: `Alt+q`, Cancel: `Alt+x`

## Configuration

File: `~/.tdsr.cfg` (INI format)

```ini
[speech]
rate = 50              # 0-100
volume = 80            # 0-100
voice_idx = 0          # Voice index
cursor_delay = 300     # ms before speaking cursor position
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

Plugins can be Python, shell scripts, or any executable.

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
