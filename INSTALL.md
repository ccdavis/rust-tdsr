# Installation Guide for TDSR (Rust)

This guide covers installing TDSR from source on macOS, Linux, and WSL.

## Quick Setup (recommended)

A helper script installs every build and runtime prerequisite for your
platform (macOS, WSL, or native Linux with apt / dnf / pacman / zypper):

```bash
./install-deps.sh              # install build + runtime deps
./install-deps.sh --with-mbrola   # also install higher-quality MBROLA voices
./install-deps.sh --dry-run    # show what would be installed, change nothing
```

Then build:

```bash
./build.sh        # or: cargo build --release
```

The rest of this document describes the prerequisites the script installs,
for manual setup or unsupported distributions.

## Prerequisites

### All Platforms

- **Rust**: 1.70 or later
  ```bash
  # Install rustup if needed
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

  # Verify version
  rustc --version
  ```

### macOS

No additional dependencies - uses built-in AVFoundation for speech.

### Linux

**Build dependencies:**
```bash
# Debian/Ubuntu
sudo apt install libclang-dev libspeechd-dev

# Fedora/RHEL
sudo dnf install clang-devel speech-dispatcher-devel

# Arch
sudo pacman -S clang speech-dispatcher
```

> **Note:** the build needs **speech-dispatcher >= 0.10**. On older distros
> (e.g. Ubuntu 20.04) these build dependencies are not needed — build the
> espeak-ng-only variant instead (see
> [Older distributions](#older-distributions-speech-dispatcher--010) below).

**Runtime dependencies:**
```bash
# Debian/Ubuntu
sudo apt install speech-dispatcher espeak-ng xclip wl-clipboard

# Fedora/RHEL
sudo dnf install speech-dispatcher espeak-ng xclip wl-clipboard

# Arch
sudo pacman -S speech-dispatcher espeak-ng xclip wl-clipboard
```

- **speech-dispatcher** — primary Linux speech backend.
- **espeak-ng** — used by the PulseAudio fallback backend. Note it must be
  the `-ng` variant; the legacy `espeak` package is not used.
- **xclip** (X11) or **wl-clipboard** (Wayland) — for the Alt+v copy feature.

### WSL (Windows Subsystem for Linux)

Same build dependencies as Linux. Runtime uses Windows SAPI automatically - no speech-dispatcher needed. See [WSL.md](WSL.md) for details.

## Building from Source

```bash
cd rust
cargo build --release
# Binary at: target/release/tdsr
```

Prefer `./build.sh`, which checks dependencies and automatically picks the
right speech backend for your system (see below).

### Older distributions (speech-dispatcher < 0.10)

The default build uses the `tts` crate, which links Speech Dispatcher and
requires **libspeechd >= 0.10** at build time. Some distributions ship an
older release — notably **Ubuntu 20.04** (speech-dispatcher 0.9.1) — where the
default build fails to compile (`SPD_PUNCT_MOST` / enum mismatches in the
`speech-dispatcher` crate).

On those systems, build the **espeak-ng-only** variant. It omits Speech
Dispatcher and uses the low-latency PulseAudio + espeak-ng backend instead —
no libclang or libspeechd needed:

```bash
cargo build --release --no-default-features
# or, equivalently:
./build.sh --espeak-only
```

`./build.sh` with no arguments detects an old/missing speech-dispatcher and
selects this variant automatically. The only runtime requirement is
`espeak-ng` (installed by `./install-deps.sh`).

## Installation Methods

### Method 1: Cargo Install (Recommended)

```bash
cargo install --path .
# Installs to ~/.cargo/bin/tdsr
```

Ensure `~/.cargo/bin` is in your PATH.

### Method 2: Manual Copy

```bash
# System-wide
sudo cp target/release/tdsr /usr/local/bin/

# User-local
mkdir -p ~/.local/bin
cp target/release/tdsr ~/.local/bin/
```

## Verification

```bash
tdsr
```

You should hear: "TDSR, presented by Lighthouse of San Francisco"

Quick navigation test:
- Run `ls` then press `Alt+i` - speaks current line
- Press `Alt+u` / `Alt+o` - navigate lines
- Press `Alt+c` then `ESC` - test config menu

## Troubleshooting

### Speech Not Working (Linux)

```bash
# Ensure Speech Dispatcher is running
systemctl --user start speech-dispatcher
spd-say "test"
```

### Speech Not Working (macOS)

```bash
say "test"
```

### Build Errors

**"libclang not found":**
```bash
sudo apt install libclang-dev
```

**"speechd.h not found":**
```bash
sudo apt install libspeechd-dev
```

See [TROUBLESHOOTING.md](TROUBLESHOOTING.md) for more help.

## Updating

```bash
git pull
cargo build --release
cargo install --path . --force
```

## Uninstalling

```bash
cargo uninstall tdsr
rm ~/.tdsr.cfg  # Optional: remove config
```
