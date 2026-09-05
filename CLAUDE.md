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
│   ├── pty.rs           # PTY spawn/resize (portable-pty), foreground process group
│   ├── emulator.rs      # vte parser driver
│   ├── performer.rs     # vte Perform impl → screen + speech buffer (largest file)
│   ├── screen.rs        # Screen buffer + scrollback history, SGR/DECAWM/DECTCEM state
│   ├── cell.rs          # Character cell (char + attributes)
│   ├── attrs.rs         # SGR attributes: colours, bold/dim/underline/reverse
│   └── util.rs          # termios raw mode, terminal size
├── tui/
│   ├── mod.rs           # TuiTracker: screen diffing for full-screen programs (menus, dialogs)
│   ├── diff.rs          # Row/cell diff, changed spans, background statistics
│   ├── frame.rs         # Box-frame detection, titles, decoration stripping
│   └── detect.rs        # Auto-detection of full-screen programs (score + hysteresis)
├── speech/
│   ├── mod.rs           # Speech module exports
│   ├── synth.rs         # Synth trait and backend selection
│   ├── voices.rs        # espeak-ng voice catalogue (shared by both espeak backends)
│   ├── buffer.rs        # Speech buffer accumulation
│   ├── resample.rs      # 2x upsampler (22050 → 44100 Hz) for the in-process backend
│   └── backends/
│       ├── mod.rs       # Backend exports
│       ├── command.rs   # Line-protocol speech server subprocess (macOS server, speech_command)
│       ├── speechd.rs   # Speech Dispatcher client (speech-dispatcher crate); not compiled on macOS
│       ├── windows.rs   # Windows SAPI via PowerShell (WSL)
│       ├── espeak.rs    # Linux/WSL: libespeak-ng in-process + own PulseAudio playback (audio thread)
│       ├── pulseaudio.rs # espeak-ng subprocesses with PulseAudio (fallback)
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

**Terminal Emulation** (`terminal/`): `emulator.rs` drives the `vte` ANSI parser; the custom `Perform` impl lives in `performer.rs` (the largest module) and is where screen-buffer updates and speech-buffer population happen — most output/announcement behavior changes go here. `pty.rs` owns the child PTY. Every `Cell` carries its `Attrs` (SGR: `Color` fg/bg incl. 256-colour and RGB, bold/dim/underline/reverse; `Attrs::effective_bg` swaps on reverse, `is_bright_fg` excludes dark grey). `Screen` tracks the current `sgr`, `cursor_visible` (DECTCEM `?25`) and `autowrap` (DECAWM `?7`; off means the last column is overwritten, never wrapped); erases fill with the current background (bce). Leaving the alternate screen drops the text drawn on it during that burst.

**TUI mode** (`tui/`): full-screen programs (Free Pascal IDE `fp`, Midnight Commander, `dialog`/`whiptail`, ncurses menus) mark the selected item with a colour change, hide or park the cursor and send only the cells that changed, so reading the print stream yields fragments. When active, `TuiTracker` drops the print stream and instead diffs the screen (chars + attrs) against a baseline once output has settled (`State::after_output` arms a flush after `tui_settle` ms, capped at 150 ms; `run_scheduled` runs `State::flush_tui`, which calls `TuiTracker::observe` and speaks `Announcement`s; a keypress drops a pending flush without advancing the baseline, since diffing screens loses nothing). Classification order in `observe`: a freshly drawn frame (all four corners and ≥50% of its border drawn in this burst; sides reaching the last row without a bottom border, like mc's tall File menu, count as a frame running off the screen with `bottom == rows`) is a **menu** (only the highlighted item is spoken, plus the bar item above it) when it hangs under a highlighted menu-bar item or beside an open menu and no visible cursor moved into it, else a **window** (title from the top border, interior lines with decoration stripped, then the focused item); otherwise the best-scoring changed span is the new **highlight** (score: attribute-only change, distinct background on its row / among the changed cells, one-row bar, bright foreground with plain siblings, gained emphasis, learned attrs, cursor on it; penalties for lost emphasis, going back to the row's ordinary rendition, dark-grey/dim text, overlapping the previous highlight); a frameless repaint of more than 3 rows is silent (menu closed); a visible cursor that moved reads its line clipped at vertical frame pieces (row change) or its character (column change), except after typed text; remaining single-row text changes with a real word are spoken as messages unless the row is volatile (changed in 3 of the last 4 bursts: counters, clocks). `detect.rs` decides `auto` mode: enter when the screen has ≥3 rows with ≥3 distinct backgrounds **and** the alternate screen was entered in this burst or auto-wrap is off, or at once when the foreground process name is in `tui_apps` (`tui_mode = apps` uses only the names). A hidden cursor is deliberately not a sign: nano keeps it hidden for most of a second in some states. A decaying score (alt +2, hidden +1, autowrap off +2, multi-bg +2) only decides quietness. Leave when the shell's process group is back in the foreground (`Pty::foreground_pgid` via `tcgetpgrp`, only after a foreign group was seen, so shells without job control are ignored), when the alternate screen is left (a listed name then does not re-enter until the foreground changes, so an exiting program does not flicker), or after 3 quiet bursts. `less`, `nano`, plain `vim` (no third background) do not activate; a vim colour scheme with backgrounds would. When a window, menu or page opens the tracker remembers the screen underneath (`underlays`, 3 deep); a repaint that mostly restores one of them is a close: only the highlight there (and any rewritten message row) is spoken, never the repaint. A window whose frame, title and text were read before is not read again (`windows`). Keys: Alt+t cycles auto/apps/on/off (saved as `tui_mode`), Alt+w reads the innermost frame around the highlight/cursor (else the last window, else the screen), Alt+h repeats the highlight. Arrow keys skip the ordinary delayed line/char reads while TUI mode is active. **Fixtures:** `tools/capture_tui.py` records real programs in a pty, one `.bin` per keystroke under `tests/fixtures/<app>/`; `tests/tui_test.rs` replays them through the same path `main` uses and asserts the spoken text (fp: menus, dropdowns, editor typing/cursor, the SwitchesMode dialog with Tab, the exit prompt; whiptail; mc: panels, Tab, F9 menus, viewer; `ls --color`, `less` and `nano` must stay in ordinary mode). mc needs `-u` (no subshell) and application-mode arrows (`ESC O B`) when recorded in a bare pty. Note Alt+letter keys are TDSR's, so fp menus are reached with F10 in the fixtures. `RUST_LOG=trace` shows every span's score.

**Speech System** (`speech/`):
- `Synth` trait defines speak/cancel/set_rate/set_volume and two ways to pick a voice: `set_voice_idx(idx)` (a menu index into the backend's current list) and `set_voice(id) -> Result<name>` (a persistent id: espeak voice file, Speech Dispatcher voice name). `voice_count()`/`voice_id(idx)` describe the list (both `None` for index-only backends: macOS server, `speech_command`, SAPI); `legacy_voice_id(idx)` says what an old `voice_idx` meant. **Config:** `[speech] voice = <id>` is what the menu saves and what wins at startup; a bare `voice_idx` is migrated to `voice` on backends with ids (espeak: via the old 14-entry table in `voices::legacy_voice_name`, announced as "voice setting updated to ...") and stays the setting on index-only backends. The menu refuses out-of-range or unusable indices without saving on backends with a list, and saves `voice_idx` regardless of send success on index-only backends (the failure may be a dead speech server). All in `State::apply_configured_voice` and `ConfigHandler::set_voice_idx`.
- Backend selection in `synth.rs`: WSL → espeak in-process/espeak subprocess/SAPI/Speech Dispatcher; Linux → Speech Dispatcher/espeak in-process/espeak subprocess; macOS → avfoundation (subprocess) only
- **Speech Dispatcher backend** (`backends/speechd.rs`, feature `native-speech`): talks to the daemon over libspeechd. The connection must be `Mode::Threaded`: the crate's `open` turns notifications on, which libspeechd refuses on a single-mode connection (so single mode never connects). `Priority::Text` for everything so utterances queue in order and `cancel` drops them all. TDSR rate/volume 0-100 map linearly onto the daemon's -100..100 (50 = daemon normal), single characters go through `spd_char` (capitals/symbols per daemon config), the voice list is sorted by name, the config stores the voice name, and the voice is set per client (`SET self`). Voices are those of the daemon's current output module; other synths are chosen in speechd's own config. A module that lists no voices is still used; `--list-voices` then prints a notice and a configured `voice` is passed to the daemon as given. `tdsr --list-voices` on native Linux prints the daemon's list when it answers, else the espeak catalogue. Untested on a real daemon as of 2026-09 (the WSL dev box has none installed).
- **Linux/WSL primary espeak backend is in-process** (`backends/espeak.rs`): `libespeak-ng.so.1` and `libpulse-simple.so.0` are `dlopen`ed (`libloading`, no build deps) on a dedicated audio thread. espeak-ng hands the thread 20 ms PCM chunks; it upsamples them to 44100 Hz (`speech/resample.rs`) and writes them into a 40 ms PulseAudio playback buffer (prebuf 0). `Synth` methods only push onto a queue; `cancel` sets a flag that aborts synthesis at the next chunk and flushes the server buffer. The WSLg RDP sink (module-rdp-sink) forwards audio to Windows in 5 ms blocks with up to 1.3 s outstanding; the Windows side plays ~5-8% slower than the sink sends, so the backlog grows with everything sent (silence included) and drains only while the sink has no stream at all; once saturated the WSLg pulse server wedges (connections refused). So the backend transmits sound only: espeak's pauses (≥60 ms) become closed-stream gaps of the same length, the stream's reported latency is checked every 250 ms of sound and a drain gap inserted above 200 ms, and the stream is closed whenever idle. A keep-alive thread on WSL holds a recording of `@DEFAULT_MONITOR@` so the sink never suspends (it never resets its pacing clock on resume and would burst). Measured floor on WSLg is ~50 ms from write to audible (RDP path); on native Linux it is the sink's latency.
- `backends/pulseaudio.rs` is the fallback when the libraries cannot be loaded: one short-lived espeak-ng subprocess per batch from a worker thread, `cancel` kills it, `-z` for letter-only batches, and a `parec` wake of a suspended WSLg sink before speaking after >4 s of silence. The worker reaps the child with `try_wait` under the same lock that publishes its PID (polling every 5 ms) and clears the PID in that critical section, so `cancel` can never SIGKILL a reused PID.
- **Voices on Linux/WSL** come from espeak-ng itself (`speech/voices.rs`, `VoiceCatalogue`): the in-process backend reads `espeak_ListVoices` (plain, then with `languages = "mb"` for the MBROLA definitions) on the audio thread and hands the list back at init; the subprocess fallback parses `espeak-ng --voices` and `--voices=mb`. Menu indices number espeak-ng's own voices first, then all MBROLA definitions; the config stores the identifier (`gmw/en-US`, `mb/mb-us1`), which `espeak_SetVoiceByName` accepts, and `VoiceCatalogue::find` also resolves file names and language tags (`en-us`, `mb-us1`, the legacy table's names). MBROLA "installed" mirrors espeak-ng's own search (`mbrola` on PATH; a non-empty database at `<espeak data dir from espeak_Info / --version>/mbrola/<name>` or the three `/usr/share/mbrola` layouts): it hides definitions in `--list-voices` and refuses them up front with a spoken message, because espeak-ng and mbrola print complaints to the terminal when a load fails. **In-process voice changes are synchronous:** `EspeakSynth::set_voice` posts a `VoiceRequest`, sets the cancel flag so the current utterance stops, and waits (3 s) for the audio thread's answer; the thread loads the voice *outside* the queue lock (loading MBROLA forks `mbrola`), re-reads the sample rate, reverts to the last good voice on failure, and replies. A name the catalogue lacks is passed to espeak-ng and its verdict returned. `tdsr --list-voices` prints the catalogue on Linux/WSL (`main.rs`, in-process library first, CLI fallback).
- **MBROLA voices are 16000 Hz, espeak-ng's own 22050 Hz.** The audio thread re-reads `espeak_ng_GetSampleRate` after each voice change and opens the PulseAudio stream at twice the current rate, feeding the 2x upsampler; a rate change aborts any open stream.
- **Typed letters** (`Utterance::is_letter`) run into each other: a letter's trailing silence is dropped and the stream is left open (the audio loop closes it after `WSL_LETTER_IDLE_CLOSE` = 150 ms idle on WSL, `IDLE_CLOSE` on native Linux), so typing at 120 ms/key restarts the sink about twice per 20 letters instead of once per letter; a keypress cancelling a letter lets its buffered tail (≤ 40 ms) play instead of flushing it (the flush clicks), while cancelled text is still cut at once (`Player::stop_current`). Two ignored tests measure this on the current machine: `cargo test --release --lib measure_letter_latency -- --ignored --nocapture` (stream open and synthesis times) and `measure_typing` (stream restarts while typing; plays audio).
- **Pauses:** on WSL a pause ≥ 60 ms (and the end of an utterance) is a closed-stream gap; `Player::gap` waits for the buffered sound to play out and closes the stream *before* timing the pause, so the pause is heard in full. On native Linux the sink rewinds when a stream goes away, so pauses are written as silent samples (20 ms slices, cancellable), the stream stays open across utterances, and the audio thread closes it after 500 ms idle (`IDLE_CLOSE`, a `wait_timeout` on the queue).
- `espeak_Terminate` can hang (a lost wake-up in espeak-ng's event thread) when called soon after `espeak_Initialize`; `EspeakSynth::drop` therefore waits at most 500 ms for the audio thread, `list_voices()` leaks the library instead of terminating it, and only one test per process may load the library (`backend_lists_voices_and_follows_their_sample_rate`).
- `handle_pty_output` drains everything the PTY has ready (up to 64 KB per event, via `Pty::has_more`) and then schedules `State::flush_speech` 5 ms later (`schedule_speech_flush`, guarded by `delaying_output`), so output written in several pieces becomes one utterance. A keypress cancels the pending flush and drops the unspoken text. `speak_output` applies repeated-symbol condensing; `speak` trims and skips empty text.
- The performer inserts separators when the cursor jumps (including row changes via CUP), keeps auto-wrapped words whole, and treats text drawn after a carriage return on the same row as a rewrite that replaces that row's queued speech (progress bars are spoken once per flush, not once per redraw).
- Key echo is detected at draw time: `State::last_key` is handed to the performer as `echo_key`; the first character drawn that changes a screen cell and equals it is the shell's echo, kept out of the speech buffer and returned so `main` can speak it as a letter (if `key_echo` is on). Characters that repaint a cell unchanged while a key is pending are queued provisionally (`SpeechBuffer::note_redraw`) and dropped when the echo arrives: zsh backspaces over the word being typed and redraws all of it (`BS c d` for the second letter of `cd`), which would otherwise be read as "cd". This also works when zsh wraps the echo in escape sequences.
- DEC special graphics (`ESC ( 0`, SI/SO) are translated to box-drawing characters (`Screen::map_charset`), so ncurses/tmux borders are not read as "lqqqk".
- Speech backend failures are non-fatal: `State::synth_op` logs them. `CommandSynth` respawns a dead server once and then backs off for a second between attempts.

**External speech servers**: `backends/command.rs` (`CommandSynth`) starts any program and drives it over stdin with the original TDSR line protocol (`s`, `l`, `x`, `r`, `v`, `V`). `[speech] speech_command` or `--speech-command` selects one on any platform; the macOS server below is the same mechanism pointed at our own binary.

**macOS speech is a subprocess** (`backends/avfoundation.rs`). `AVSpeechSynthesizer` only advances its utterance queue while a CFRunLoop is serviced on the process **main thread**; TDSR's main thread parks in mio's kqueue `poll()`, so driving the synth in-process plays only the first utterance and then goes silent. Instead, `spawn_server_synth` re-execs our own binary as `tdsr --speech-server` (detected at the top of `main()`, before any terminal setup) and the child runs `AVSpeechSynthesizer` on its main thread with a real `CFRunLoopRun()`. The parent sends a line protocol over the child's stdin: `s<text>`/`l<text>` (speak), `x` (cancel), `r`/`v`/`V` (rate/volume/voice). This mirrors the original Python TDSR's macOS design. Frameworks (`objc`, `core-foundation`, `core-foundation-sys`) are macOS-only deps in `Cargo.toml`.

Voice selection (priority in `Worker::speak`): explicit `voice_idx` → `AVSpeechSynthesisVoice.speechVoices()[idx]`, resolved and retained once in `select_voice` when the index is set (never per utterance: `speechVoices()` is slow); else the default voice resolved at startup by `find_default_voice()` — **Eloquence, Reed variant** if installed (detected by identifier containing "eloquence", variant via the last identifier component `.reed`, preferring the current language), retained for the process; else `prefersAssistiveTechnologySettings` to follow the VoiceOver voice (the only way to reach Siri voices). Config default `voice_idx` is `-1` → `None`. `tdsr --list-voices` (handled at the top of `main()`, like `--speech-server`) prints voices with indices and flags Eloquence rows.

**Input Handling** (`input/`): Stack-based modal handlers. `HandlerResult::Passthrough` sends key to PTY, `Remove` pops handler. `main.rs` splits each stdin read with `split_keys` (key repeat and fast typing coalesce several keys into one read), forwards terminal replies (`is_terminal_response`: cursor position reports, DA, OSC/DCS replies, focus events) to the PTY without silencing speech, and routes each real key through `dispatch_key`, which is the only place the modal stack is popped/reinserted; keep that logic in the library so it stays testable (`tests/modal_input_test.rs` drives it with a recording synth and a temp config via `Config::load_from` + `State::from_parts`).

**Configuration** (`state/config.rs`): INI format in `~/.tdsr.cfg`. Sections: `[speech]`, `[symbols]`, `[plugins]`, `[commands]`. `Pty::new` applies the user's pre-raw-mode termios to the child PTY so the shell inherits erase/flow-control settings.

### Speech Backend Priority

**WSL:**
1. espeak-ng in-process + own PulseAudio playback (`backends/espeak.rs`, if WSLG available)
2. PulseAudio + espeak-ng subprocesses (if the libraries cannot be loaded)
3. Windows SAPI via PowerShell
4. Speech Dispatcher (fallback)

**Linux:**
1. Speech Dispatcher (`backends/speechd.rs`, if the daemon answers)
2. espeak-ng in-process + own PulseAudio playback
3. PulseAudio + espeak-ng subprocesses

**macOS:**
1. AVFoundation via `tdsr --speech-server` subprocess (see `backends/avfoundation.rs`) — the only backend. The `speech-dispatcher` crate is not a dependency on macOS. If the subprocess cannot start, `create_synth` speaks the error through `/usr/bin/say` (`say_blocking`) and returns it, so TDSR exits with an audible reason.

## Key Bindings

**Alt = ESC prefix.** All `Alt+key` bindings (`keymap.rs`, e.g. `b"\x1bu"`) require the terminal to send Option/Alt as Meta (ESC-prefixed). On macOS this is off by default — Terminal.app: "Use Option as Meta key"; iTerm2: Option key → "Esc+". See README "Terminal setup". This is inherent to terminal input, not configurable in TDSR.

Review cursor navigation uses Alt+key:
- Line: `Alt+u/i/o` (prev/current/next)
- Word: `Alt+j/k/l` (prev/current/next), double-tap `Alt+k` to spell
- Char: `Alt+m/,/.` (prev/current/next), double-tap `Alt+,` for phonetic
- Screen: `Alt+U/O` (top/bottom), `Alt+M/>` (start/end of line). `Alt+U` when already on the top row jumps to the oldest scrolled-off line.
- Scrollback: `Alt+u` from the top row keeps going into lines that scrolled off (`Screen::history`, capped at `MAX_HISTORY` = 2000, recorded only when the main screen's top row scrolls out). `ReviewCursor::above` counts how far above the screen the cursor is; every read goes through `Screen::get_line_at`/`get_char_at`, so word/char review and line copy work there. `Alt+o`/`Alt+O` and cursor tracking bring it back to the screen; `adjust_review_cursor_for_scroll` follows content into the history.
- Config: `Alt+c` (then a letter; `?` lists keys; Enter or Escape leaves; unknown keys are announced), Copy: `Alt+v`, Quiet: `Alt+q`, Cancel: `Alt+x`. `V` takes a voice index: on success it is saved and "confirmed, <voice name>" is spoken (in the new voice); a rejected index speaks the backend's reason and saves nothing. `t` cycles TUI mode.
- TUI mode: `Alt+t` cycle auto/apps/on/off, `Alt+w` read the current window/dialog, `Alt+h` repeat the highlighted item.

- Numeric entry inside the config menu (rate, volume, voice, delay) accepts digits only, echoes each digit, and Escape or Enter on an empty value cancels

## Configuration

File: `~/.tdsr.cfg` (INI format)

```ini
[speech]
rate = 50              # 0-100
volume = 80            # 0-100
voice = gmw/en-US      # Linux/WSL: id from `tdsr --list-voices` (menu saves it); macOS/external: voice_idx = N
cursor_delay = 300     # ms before speaking cursor position
tui_mode = auto        # auto | apps | on | off: screen diffing for full-screen programs

tui_apps = fp,mc       # process names that always get TUI mode
tui_settle = 30        # ms of quiet before the screen is compared (max 1000)
tui_announce = true    # say "TUI mode on/off" when it switches
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
