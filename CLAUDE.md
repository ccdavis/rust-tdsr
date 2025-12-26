# CLAUDE.md

Guidance for Claude Code when working with this repository.

## Project Overview

TDSR is a console-based screen reader for *nix systems (Linux, macOS, FreeBSD, WSL). It uses a pseudo-terminal (PTY) to intercept terminal I/O and provides text-to-speech feedback via native speech synthesis.

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
├── main.rs              # Entry point and event loop
├── lib.rs               # Library exports
├── platform.rs          # Platform detection (WSL, Linux, macOS)
├── symbols.rs           # Symbol-to-word mapping
├── terminal/
│   ├── mod.rs           # PTY setup, terminal performer
│   ├── screen.rs        # Screen buffer
│   └── cell.rs          # Character cell
├── speech/
│   ├── mod.rs           # Speech module exports
│   ├── synth.rs         # Synth trait and backend selection
│   ├── buffer.rs        # Speech buffer accumulation
│   └── backends/
│       ├── mod.rs       # Backend exports
│       ├── native.rs    # tts crate (Speech Dispatcher/AVFoundation)
│       ├── windows.rs   # Windows SAPI via PowerShell (WSL)
│       └── pulseaudio.rs # espeak-ng with PulseAudio (WSLG fallback)
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

**Terminal Emulation** (`terminal/`): Uses `vte` crate for ANSI parsing. Custom `Perform` impl populates speech buffer on screen updates.

**Speech System** (`speech/`):
- `Synth` trait defines speak/cancel/set_rate/set_volume/set_voice
- Backend selection in `synth.rs`: WSL → SAPI/PulseAudio/native; Linux → native/PulseAudio; macOS → native
- Speech buffer accumulates text, flushed on cursor movement or timer

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
1. AVFoundation (via tts crate)

## Key Bindings

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
