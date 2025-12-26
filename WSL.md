# TDSR on WSL (Windows Subsystem for Linux)

TDSR automatically detects WSL and uses Windows SAPI for speech synthesis.

## Features

- **Native Windows speech** - Uses your installed Windows TTS voices
- **No additional setup** - Works out of the box with WSL interop
- **Voice selection** - Choose from installed Windows voices via `voice_idx`
- **Rate and volume control** - Full speech configuration support

## Requirements

### WSL Setup

WSL 1 or WSL 2 with Windows interop enabled (default).

Verify interop works:
```bash
cmd.exe /c ver
```

If you get "command not found", enable interop in `/etc/wsl.conf`:
```ini
[interop]
enabled = true
appendWindowsPath = true
```
Then restart WSL: `wsl.exe --shutdown`

### Build Dependencies

```bash
# Ubuntu/Debian
sudo apt install libclang-dev libspeechd-dev
```

Note: `libspeechd-dev` is only needed for building. The WSL backend uses Windows SAPI at runtime.

## Building

```bash
cd rust
cargo build --release
```

## Testing Speech

```bash
# Quick test
./target/release/tdsr

# You should hear the startup message via Windows TTS
# Press Alt+i to speak current line
```

Manual Windows SAPI test:
```bash
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
```

## Configuration

Your `~/.tdsr.cfg` works the same as on Linux/macOS:

```ini
[speech]
rate = 50           # 0=slowest, 50=normal, 100=fastest
volume = 80         # 0=quietest, 100=loudest
voice_idx = 0       # Windows voice index (0 = default)
```

### Changing Windows Voice

Option 1: Use `voice_idx` in TDSR config (press Alt+c, then 'V')

Option 2: Change system default in Windows Settings → Time & Language → Speech

Common Windows voices:
- Microsoft David (English US, male)
- Microsoft Zira (English US, female)
- Plus any additional voices you've installed

## How It Works

```
TDSR (Rust binary)
    ↓
Windows SAPI Backend
    ↓
Persistent PowerShell process
    ↓
.NET System.Speech.Synthesis
    ↓
Windows audio output
```

TDSR detects WSL by checking `/proc/version` for "microsoft" or "wsl".

## Troubleshooting

### No Speech Output

1. Verify Windows interop:
   ```bash
   powershell.exe -Command "echo 'test'"
   ```

2. Test SAPI directly:
   ```bash
   powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
   ```

3. Check TDSR logs:
   ```bash
   RUST_LOG=debug ./target/release/tdsr --debug
   cat tdsr.log | grep -i "windows\|wsl"
   ```

### PowerShell Not Found

```bash
# Add Windows to PATH
echo 'export PATH="$PATH:/mnt/c/Windows/System32/WindowsPowerShell/v1.0"' >> ~/.bashrc
source ~/.bashrc
```

### Fallback Behavior

If Windows SAPI fails, TDSR falls back to:
1. PulseAudio + espeak-ng (if WSLG available)
2. Speech Dispatcher (if installed)

## See Also

- [README.md](README.md) - General documentation
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - More troubleshooting help
- [INSTALL.md](INSTALL.md) - Installation guide
