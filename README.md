# TDSR - Terminal-based Screen Reader (Rust Edition)

A console-based screen reader for *nix systems (macOS, Linux, FreeBSD) written in Rust. TDSR sits between you and your shell, providing text-to-speech feedback for terminal applications. It supports running in Windows in a WSL-2 console.

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)

This is a version of the TDSR Python terminal screen reader re-written in Rust. It's currently at an alpha state. Mostly functioning, but  with some bugs, not tested in all environments yet.

Tyler Spivey created the  [Original Python TDSR](https://github.com/tspivey/tdsr)and all credit goes to him for the design and features.

`rust-tdsr` is an AI-written translation from Python. It's mostly done by Claude Code / Sonnet 4.5 and Opus 4.5 with reviews from Codex GPT 5.2 and Gemini CLI using 3.0 Pro. My main contributions are:
* I know Rust and Python
* I'm a screen reader user and a heavy terminal user

So, I was able to prompt, test and guide the coding agents to a working solution.

## What's interesting about this version?

It supports low-latency speech support in WSL. The original used an approach that I couldn't get to work in WSL. My first attempts to produce speech with WSL used a pipe to Power Shell that worked, but with a lot of latency, making it practically fairly useless. This version uses Windows SAPI voices directly, and has fall-backs to Pulseaudio + Espeak, and then to speech-dispatcher.

Also, it's nice to have a single binary to deploy wherever you need a talking terminal rather than relying on Python. It should be easy to get running. It also requires fewer resources than the Python version which could matter on a very small machine.

## Screen Reader Features

- ✅ **Review Cursor Navigation** - Navigate screen content independently of the terminal cursor
- ✅ **Speech Synthesis** - Platform-native speech (macOS AVFoundation, Linux Speech Dispatcher)
- ✅ **Symbol Processing** - Convert special characters to words ("!" → "bang", "$" → "dollar")
- ✅ **Copy/Selection** - Copy lines, screen content, or selected regions to clipboard
- ✅ **Plugin System** - Extend functionality with external scripts (Python, shell, etc.)
- ✅ **Configuration** - Customizable key bindings, speech settings, and symbols
- ✅ **Wide Character Support** - Proper handling of CJK characters and emoji

## Installation

### Prerequisites

**macOS:**
- Rust 1.70 or later (for building)
- macOS 10.14+ (for AVFoundation speech support)

**Linux:**
- Rust 1.70 or later (for building)
- Build dependencies: `sudo apt install libclang-dev libspeechd-dev`
- Runtime dependencies:
  - Speech Dispatcher: `sudo apt install speech-dispatcher`
  - Clipboard: `xclip` (X11) or `wl-copy` (Wayland)
  - Optional (better voices): `sudo apt install espeak-ng mbrola mbrola-us1 mbrola-us2`

**WSL (Windows Subsystem for Linux):**
- Rust 1.70 or later (for building)
- Build dependencies: same as Linux
- Runtime: Uses PulseAudio + espeak-ng (via WSLG), falls back to Windows SAPI or Speech Dispatcher
- Speech runs espeak-ng in-process (`libespeak-ng`, installed with `espeak-ng`) and plays through WSLg's PulseAudio itself; see [WSL.md](WSL.md) for why that matters on WSL
- Optional: `sudo apt install pulseaudio-utils` (`parec`, used only by the subprocess fallback backend to wake WSLg's audio sink after a pause)
- Optional (better voices): `sudo apt install mbrola mbrola-us1 mbrola-us2`
- See [WSL.md](WSL.md) for details

### Building from Source

On Linux/WSL you can install all build and runtime prerequisites with the
helper script (supports apt / dnf / pacman / zypper; no-op extras on macOS):

```bash
./install-deps.sh        # add --with-mbrola for higher-quality voices
```

Then build:

```bash
cd rust
cargo build --release
```

The binary will be at `target/release/tdsr`.

> **Older distributions (e.g. Ubuntu 20.04):** the default build needs
> speech-dispatcher >= 0.10. Where it's older, build the espeak-ng-only variant
> with `cargo build --release --no-default-features` (or `./build.sh
> --espeak-only`) — this uses the low-latency PulseAudio + espeak-ng backend and
> needs no libclang/libspeechd. See [INSTALL.md](INSTALL.md) for details.

### Installation

```bash
# Install to ~/.cargo/bin (ensure it's in your PATH)
cargo install --path .

# Or copy manually
sudo cp target/release/tdsr /usr/local/bin/
```

## Quick Start

```bash
# Run with default shell
tdsr

# Run with specific program
tdsr bash
tdsr zsh

# Run with debug logging (writes to tdsr.log)
tdsr --debug
```

TDSR will speak "TDSR, presented by Lighthouse of San Francisco" when ready.

**Note:** By default, TDSR runs quietly without log output. Use `--debug` or `-d` to enable detailed logging to `tdsr.log`.

## Terminal setup

TDSR's review and command keys are triggered with **Alt** (the Meta key) — e.g. `Alt+u`/`Alt+i`/`Alt+o` to read the previous/current/next line. A terminal screen reader only sees the raw byte stream from your terminal; there is no "Alt" byte. By convention, Alt/Meta+key is sent as an **Escape prefix** (`ESC` followed by the key), and that is exactly what TDSR's key bindings match.

Most terminals do **not** send Escape for the Option/Alt key by default — on macOS, Option produces accented characters (Option+u = ¨) or triggers system shortcuts instead. You must tell your terminal to treat Option/Alt as Meta:

- **macOS Terminal.app:** Settings → Profiles → **Keyboard** → check **"Use Option as Meta key."**
- **macOS iTerm2:** Settings → Profiles → **Keys** → set **Left Option key** (and Right, if you like) to **Esc+**.
- **Linux / GNOME Terminal, Konsole, etc.:** Alt is normally already sent as Meta; no change needed. If your terminal has an "Alt sends Escape" / "Meta sends Escape" option, ensure it is enabled.

If the navigation keys print accented characters or open other apps instead of speaking, this setting is the cause.

(The original Python TDSR required the same setting — this is inherent to how terminal key input works, not specific to this port.)

## Configuration

Configuration file: `~/.tdsr.cfg` (INI format)

### Speech Settings

TDSR uses native speech synthesis:
- **Linux:** Speech Dispatcher, or espeak-ng (in-process, own PulseAudio playback) as fallback
- **WSL:** espeak-ng in-process with its own PulseAudio playback (via WSLG), then espeak-ng subprocesses, Windows SAPI, Speech Dispatcher
- **macOS:** AVFoundation (built into macOS 10.14+)

Any platform can instead use an external speech server: set `speech_command`
(or pass `--speech-command "..."` on the command line) to a program that reads
the TDSR line protocol on stdin (`s<text>`, `l<char>`, `x` cancel, `r<0-100>`,
`v<0-100>`, `V<index>`, one command per line). This is the same protocol the
original Python TDSR's speech servers used, so those scripts work unchanged.

```ini
[speech]
speech_command = python3 /path/to/my_server.py   # optional external synth
rate = 50           # Speech rate: 0 (slowest) to 100 (fastest), default 50
volume = 80         # Volume: 0 (quietest) to 100 (loudest), default 80
voice_idx = 0       # Voice index (see voice table below)
cursor_delay = 300  # Milliseconds before speaking cursor position
process_symbols = false  # Convert symbols to words
key_echo = true     # Speak characters as you type
cursor_tracking = true   # Speak when cursor moves
line_pause = true        # Pause between lines
repeated_symbols = false
repeated_symbols_values = -=!#
prompt = .*         # Regex for prompt (plugin system)
```

### Voice Selection (macOS)

By default macOS TDSR uses **Eloquence** (the **Reed** variant) if it is installed — the responsive synthesizer many screen-reader users prefer — and otherwise **follows your VoiceOver voice** (including premium "Siri" voices). Eloquence is a free download in **System Settings → Accessibility → Spoken Content → System Voice → Manage Voices** (search for "Eloquence"). It is matched to your current language when possible.

To use a *specific* voice instead, list the installed voices and their indices:

```bash
tdsr --list-voices
```

This prints every voice available to TDSR with its index, name, language, and quality (`default` / `enhanced` / `premium`); Eloquence voices are flagged with `<- eloquence`. `enhanced`/`premium` voices sound far better than the default ones.

Then set the index in `~/.tdsr.cfg` (or via the config menu, Alt+c → V):

```ini
[speech]
voice_idx = 144     # an index from `tdsr --list-voices`
```

To return to the default (Eloquence if installed, else VoiceOver), remove `voice_idx` or set it to `-1`.

**Note:** `--list-voices` shows the voices Apple exposes to apps. VoiceOver's own Siri voices are *not* in that list — the only way to use them is the default (VoiceOver-follow) setting, i.e. no `voice_idx` and no Eloquence installed.

### Voice Selection (Linux / WSL with espeak-ng backend)

Set `voice_idx` in `~/.tdsr.cfg` or change via the config menu (Alt+c → V).

**Built-in espeak-ng voices (always available):**

| Index | Voice | Description |
|-------|-------|-------------|
| 0 | en | Default English |
| 1 | en-us | US English |
| 2 | en-gb | British English |
| 3 | en-sc | Scottish English |
| 4 | es | Spanish |
| 5 | fr | French |
| 6 | de | German |
| 7 | it | Italian |
| 8 | pt | Portuguese |
| 9 | ru | Russian |

**MBROLA voices (higher quality, require additional packages):**

| Index | Voice | Description | Package |
|-------|-------|-------------|---------|
| 10 | mb-us1 | US English Female | `mbrola mbrola-us1` |
| 11 | mb-us2 | US English Male | `mbrola mbrola-us2` |
| 12 | mb-us3 | US English Male 2 | `mbrola mbrola-us3` |
| 13 | mb-en1 | British English Male | `mbrola mbrola-en1` |

MBROLA voices use diphone synthesis and generally sound more natural than the default espeak-ng formant voices. To install on Debian/Ubuntu:

```bash
sudo apt install mbrola mbrola-us1 mbrola-us2
```

Then set your preferred voice in `~/.tdsr.cfg`:

```ini
[speech]
voice_idx = 10    # MBROLA US English Female
```

**Quick speech test:**
```bash
# Linux - ensure Speech Dispatcher is running
systemctl --user status speech-dispatcher
systemctl --user start speech-dispatcher
spd-say "Testing speech"

# WSL - test Windows SAPI
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Testing speech')"

# macOS - test system TTS
say "Testing speech"
```

### Symbol Definitions

```ini
[symbols]
33 = bang           # !
36 = dollar         # $
64 = at             # @
# See default config for full list
```

### Plugins

```ini
[plugins]
example = d         # Alt+d runs example plugin

[commands]
example = ls.*      # Only run after 'ls' commands (optional)
```

## Key Bindings

### Line Navigation
- `Alt+u` - Previous line
- `Alt+i` - Current line
- `Alt+o` - Next line

### Word Navigation
- `Alt+j` - Previous word
- `Alt+k` - Current word
- `Alt+k, Alt+k` - Spell current word
- `Alt+l` - Next word

### Character Navigation
- `Alt+m` - Previous character
- `Alt+,` - Current character
- `Alt+,, Alt+,` - Say character phonetically
- `Alt+.` - Next character

### Screen Navigation
- `Alt+U` - Top of screen (press again to jump to the oldest scrolled-off line)
- `Alt+O` - Bottom of screen
- `Alt+M` - Start of line
- `Alt+>` - End of line

Output that scrolls off the top of the screen is not lost: `Alt+u` keeps
going up into the scrollback (the last 2000 lines), where line, word and
character review all work, and `Alt+o` or `Alt+O` come back down.

### Modes & Actions
- `Alt+c` - Configuration menu
- `Alt+q` - Toggle quiet mode
- `Alt+r` - Start/end selection (then Alt+r again to copy)
- `Alt+v` - Copy mode (then 'l' for line, 's' for screen)
- `Alt+x` - Silence speech

## Configuration Menu (Alt+c)

- `r` - Set speech rate
- `v` - Set volume
- `V` - Set voice index
- `p` - Toggle process symbols
- `d` - Set cursor tracking delay
- `e` - Toggle key echo
- `c` - Toggle cursor tracking
- `l` - Toggle line pause
- `s` - Toggle repeated symbols
- `ESC` - Exit config menu

## Copy Mode (Alt+v)

1. Press `Alt+v` to enter copy mode
2. Press:
   - `l` - Copy current line
   - `s` - Copy entire screen
   - Any other key - Exit

## Selection Mode (Alt+r)

1. Press `Alt+r` to start selection
2. Navigate with review cursor
3. Press `Alt+r` again to copy selected region

## Plugins

Plugins are external scripts that analyze terminal output and provide custom speech feedback.

See [plugins/README.md](plugins/README.md) for detailed documentation.

### Quick Example

Create `~/.tdsr/plugins/my_plugin.py`:

```python
#!/usr/bin/env python3
import json, sys

input_data = json.loads(sys.stdin.readline())
lines = input_data['lines']

result = [f"Found {len(lines)} lines"]
print(json.dumps({'speak': result}))
```

Configure in `~/.tdsr.cfg`:

```ini
[plugins]
my_plugin = d
```

Press `Alt+d` to run!

## Architecture

### Components

- **Terminal Emulation** - `vte` crate for ANSI sequence parsing
- **PTY Management** - `portable-pty` for cross-platform PTY
- **Screen Buffer** - 2D buffer for review cursor navigation
- **Speech System** - Native TTS backends:
  - Linux: Speech Dispatcher via `tts` crate, or PulseAudio + espeak-ng
  - WSL: PulseAudio + espeak-ng (WSLG), Windows SAPI fallback, Speech Dispatcher fallback
  - macOS: AVFoundation, driven by a `tdsr --speech-server` subprocess (the `tts` crate is not used on macOS)
  - Optional: MBROLA voices for higher quality speech (Linux/WSL)
- **Input Handling** - Modal key handler stack
- **Plugin System** - JSON subprocess protocol

### Module Structure

```
src/
├── terminal/       # PTY, screen buffer, vte integration
├── speech/         # TTS abstraction and backends
├── input/          # Key handlers and keymap
├── state/          # Application state and config
├── plugins/        # Plugin system
├── review/         # Review cursor
└── main.rs         # Event loop
```

## Development

### Running Tests

```bash
cargo test
```

### Debug Logging

```bash
RUST_LOG=debug tdsr 2> tdsr.log
```

### Code Quality

```bash
cargo fmt
cargo clippy
```

## Troubleshooting

### Speech Not Working (macOS)

TDSR speaks through a helper process, `tdsr --speech-server`, that it starts
from its own executable. If that helper cannot be started, TDSR announces the
reason through the system `say` command and exits (it never runs silently).

```bash
# Test system speech works
say "Hello from macOS"

# Test the helper directly: it should speak "hello" and exit when stdin closes
printf 'shello\n' | tdsr --speech-server

# If that works but TDSR doesn't, run with debug logging (the helper logs to
# the same tdsr.log):
tdsr --debug
grep -i speech tdsr.log

# Check macOS version (requires 10.14+)
sw_vers
```

### Speech Not Working (Linux)

```bash
# 1. Test Speech Dispatcher daemon is running
systemctl --user status speech-dispatcher
systemctl --user start speech-dispatcher

# 2. Test Speech Dispatcher works
spd-say "Hello from Speech Dispatcher"

# 3. If not installed
sudo apt install speech-dispatcher

# 4. If still not working, run TDSR with debug logging
RUST_LOG=debug tdsr --debug
cat tdsr.log | grep -i speech

# 5. Check audio output
speaker-test -t wav -c 2
```

### Build Errors

**"libclang not found" (Linux):**
```bash
sudo apt install libclang-dev
```

**"speechd.h not found" (Linux):**
```bash
sudo apt install libspeechd-dev
```

For complete troubleshooting, see [TROUBLESHOOTING.md](TROUBLESHOOTING.md)

### Clipboard Not Working (Linux)

```bash
# X11
sudo apt install xclip

# Wayland
sudo apt install wl-clipboard
```

## Differences from Python Version

- **Native Speech** - No Python required; direct Speech Dispatcher/AVFoundation/SAPI bindings
- **Better Performance** - ~500ms startup (vs ~2s), ~15 MB memory (vs ~50 MB)
- **Single Binary** - ~3 MB executable, copy anywhere
- **Same Config** - Your `~/.tdsr.cfg` works unchanged
- **Same Keys** - All Alt+key shortcuts identical
- **Plugins** - Same interface, run as JSON subprocesses

See [MIGRATION.md](MIGRATION.md) for upgrade guide.

## Contributing

Contributions welcome!

1. Run tests: `cargo test`
2. Format code: `cargo fmt`
3. Check lints: `cargo clippy`
4. Update docs as needed

## License

GPL-3.0-or-later

## Credits

- Original Python version by Tyler Spivey
- Rust port by the TDSR community
- Presented by Lighthouse of San Francisco

## More Documentation

- [WSL.md](WSL.md) - Windows Subsystem for Linux
- [INSTALL.md](INSTALL.md) - Installation guide
- [MIGRATION.md](MIGRATION.md) - Change from Python version
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - Common issues
- [plugins/README.md](plugins/README.md) - Plugin development

## Links

- [Original Python TDSR](https://github.com/tspivey/tdsr)
