# Changelog

All notable changes to the Rust implementation of TDSR.

## [0.1.0] - 2025

### Complete Rust Implementation

**Core Features:**
- PTY management with `portable-pty`
- VTE terminal emulation with `vte` crate
- Screen buffer with review cursor navigation
- INI configuration loading/saving (`~/.tdsr.cfg`)
- Symbol processing and regex building
- Wide character support via `unicode-width`

**Speech System:**
- Native TTS using `tts` crate - no Python required
- Linux: Speech Dispatcher backend
- macOS: AVFoundation backend
- WSL: Windows SAPI via PowerShell interop
- PulseAudio + espeak-ng fallback for WSLG
- Rate, volume, and voice selection on all platforms

**Input System:**
- Modal key handler stack
- 33+ key bindings matching Python version
- Config menu (Alt+c)
- Copy mode (Alt+v)
- Double-tap detection for spelling/phonetics

**Review Navigation:**
- Line, word, character navigation
- Screen edge navigation
- NATO phonetic alphabet
- Repeated symbols condensing

**Plugin System:**
- JSON subprocess protocol
- Language-agnostic (Python, shell, etc.)
- Command filtering via regex

**Testing:**
- Unit tests across all modules
- CI-compatible (handles missing TTS)

### Technical Details

**Dependencies:**
- vte 0.13 - Terminal emulation
- portable-pty 0.8 - Cross-platform PTY
- tts 0.26 - Native speech synthesis
- mio 1.0 - Event-driven I/O
- unicode-width 0.2 - Character width
- arboard 3.4 - Clipboard
- rust-ini 0.21 - Configuration
- regex 1.10 - Pattern matching
- serde/serde_json - Plugin JSON

**Performance:**
- ~500ms startup (vs ~2s Python)
- ~10-20 MB memory (vs ~40-60 MB Python)
- ~3 MB binary size

### Migration from Python

**Compatible:**
- Configuration file format (`~/.tdsr.cfg`)
- All key bindings
- Symbol definitions
- Plugin system (JSON protocol)

**Changed:**
- Native TTS instead of Python subprocess
- Single binary distribution

## Links

- [Repository](https://github.com/tspivey/tdsr)
- [Issues](https://github.com/tspivey/tdsr/issues)
