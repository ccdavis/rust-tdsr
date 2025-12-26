# Installation Guide for TDSR (Rust)

This guide covers installing TDSR from source on macOS, Linux, and WSL.

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

**Runtime dependencies:**
```bash
# Speech Dispatcher (for TTS)
sudo apt install speech-dispatcher

# Clipboard support (X11)
sudo apt install xclip

# Clipboard support (Wayland)
sudo apt install wl-clipboard
```

### WSL (Windows Subsystem for Linux)

Same build dependencies as Linux. Runtime uses Windows SAPI automatically - no speech-dispatcher needed. See [WSL.md](WSL.md) for details.

## Building from Source

```bash
cd rust
cargo build --release
# Binary at: target/release/tdsr
```

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
