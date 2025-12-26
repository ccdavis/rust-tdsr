# Migration Guide: Python TDSR → Rust TDSR

This guide helps you migrate from the Python-based TDSR to the Rust version.

## What's New

- **No Python Required** - Single binary, no interpreter or packages needed
- **Native Speech Synthesis** - Direct bindings to Speech Dispatcher (Linux), AVFoundation (macOS), Windows SAPI (WSL)
- **Faster Startup** - ~500ms vs ~2s with Python
- **Lower Memory** - ~10-20 MB vs ~40-60 MB
- **Smaller Distribution** - ~3 MB binary vs ~50 MB Python + dependencies

## What Stays the Same

- **Configuration** - Your `~/.tdsr.cfg` works unchanged
- **Key Bindings** - All Alt+key shortcuts identical
- **Plugin System** - Same plugin interface (plugins run as subprocesses)
- **Features** - Full functionality preserved

## Migration Steps

### 1. Backup Configuration

```bash
cp ~/.tdsr.cfg ~/.tdsr.cfg.backup
cp -r ~/.tdsr/plugins ~/.tdsr/plugins.backup  # if you have plugins
```

### 2. Build Rust Version

**Linux:**
```bash
sudo apt install libclang-dev libspeechd-dev
cd tdsr/rust
cargo build --release
```

**macOS:**
```bash
cd tdsr/rust
cargo build --release
```

### 3. Test Before Replacing

```bash
./target/release/tdsr --debug

# Try these inside TDSR:
# Alt+i - speak current line
# Alt+c - configuration menu
# Alt+x - cancel speech
```

### 4. Install

```bash
# User installation
mkdir -p ~/.local/bin
cp target/release/tdsr ~/.local/bin/

# Or system-wide
sudo cp target/release/tdsr /usr/local/bin/
```

### 5. Remove Python Version (Optional)

Only after confirming Rust version works:

```bash
pip uninstall tdsr
# or
uv tool uninstall tdsr
```

## Configuration Compatibility

All settings work unchanged:

```ini
[speech]
rate = 50              # Works the same
volume = 80            # Works the same
voice_idx = 0          # Works on all platforms
cursor_delay = 300
process_symbols = false
key_echo = true
cursor_tracking = true

[symbols]
33 = bang              # Works the same

[plugins]
git = g                # Works the same
```

## Plugin Compatibility

Python plugins work without modification:

```python
#!/usr/bin/env python3
import json, sys

input_data = json.loads(sys.stdin.readline())
lines = input_data['lines']
result = ["Things to say"]
print(json.dumps({'speak': result}))
```

Plugins can be written in any language (Python, shell, etc.) - they communicate via JSON over stdin/stdout.

## Troubleshooting

### No Speech Output

```bash
# Linux: ensure Speech Dispatcher is running
systemctl --user start speech-dispatcher
spd-say "test"

# macOS: test system TTS
say "test"

# WSL: test Windows SAPI
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
```

### Config Not Loading

```bash
ls -l ~/.tdsr.cfg
RUST_LOG=debug tdsr --debug
grep -i config tdsr.log
```

### Rollback

```bash
cp ~/.tdsr.cfg.backup ~/.tdsr.cfg
rm ~/.local/bin/tdsr
uv tool install -e .  # reinstall Python version
```

## Performance Comparison

| Metric | Python | Rust |
|--------|--------|------|
| Startup | ~2-3s | ~0.3-0.5s |
| Memory | ~40-60 MB | ~10-20 MB |
| Binary size | ~50 MB (with deps) | ~3 MB |

## Summary

1. Backup config
2. Build Rust version
3. Test it works
4. Replace Python version
5. Clean up (optional)

Your config and plugins work unchanged. Same features, better performance.
