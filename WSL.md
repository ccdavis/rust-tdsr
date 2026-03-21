# TDSR on WSL (Windows Subsystem for Linux)

TDSR automatically detects WSL and selects the best available speech backend.

## Speech Backend Priority

On WSL, TDSR tries speech backends in this order:

1. **PulseAudio + espeak-ng** (lowest latency, best for interactive use)
2. **Windows SAPI** via PowerShell (higher latency, but uses Windows voices)
3. **Speech Dispatcher** (fallback)

The PulseAudio backend requires WSLG (included in modern WSL2) and `espeak-ng`. This is the recommended setup for the best interactive experience.

## Features

- **Low-latency speech** - PulseAudio + espeak-ng provides responsive feedback
- **MBROLA voice support** - Install MBROLA for higher quality voices
- **Multiple fallbacks** - Automatically falls back to SAPI or Speech Dispatcher
- **Rate and volume control** - Full speech configuration support

## Requirements

### WSL Setup

WSL 2 with WSLG support (Windows 11, or Windows 10 with recent updates).

### Build Dependencies

```bash
# Ubuntu/Debian
sudo apt install libclang-dev libspeechd-dev
```

### Runtime Dependencies

```bash
# Required: espeak-ng for speech
sudo apt install espeak-ng

# Optional: MBROLA for higher quality voices
sudo apt install mbrola mbrola-us1 mbrola-us2
```

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
voice_idx = 0       # Voice index (see below)
```

### Voice Selection (espeak-ng / PulseAudio backend)

Set `voice_idx` in your config or use the config menu (Alt+c → V):

| Index | Voice | Notes |
|-------|-------|-------|
| 0 | en | Default English (espeak-ng built-in) |
| 1 | en-us | US English (espeak-ng built-in) |
| 10 | mb-us1 | US English Female (requires `mbrola mbrola-us1`) |
| 11 | mb-us2 | US English Male (requires `mbrola mbrola-us2`) |

See the [README](README.md#voice-selection-linux--wsl-with-espeak-ng-backend) for the full voice table.

### Installing MBROLA Voices

MBROLA voices use diphone synthesis and sound more natural than the default espeak-ng formant voices:

```bash
sudo apt install mbrola mbrola-us1 mbrola-us2
```

Then set in `~/.tdsr.cfg`:
```ini
[speech]
voice_idx = 10    # MBROLA US English Female
```

## How It Works

```
TDSR (Rust binary)
    ↓
PulseAudio + espeak-ng backend
    ↓
Persistent espeak-ng process (reads stdin line-by-line)
    ↓
WSLG PulseAudio → Windows audio output
```

TDSR detects WSL by checking `/proc/version` for "microsoft" or "wsl".

For the fallback SAPI backend:
```
TDSR → PowerShell process → .NET System.Speech.Synthesis → Windows audio
```

## Known Issue: Audio Quality on WSLG

You may notice scratchy or static-sounding speech on WSL2 with WSLG. This affects all voices equally (espeak-ng built-in and MBROLA) and is **not** a TDSR bug — espeak-ng sounds fine on native Linux with the same voices.

### Root Cause

The WSLG audio pipeline resamples all audio through a low-quality algorithm:

```
espeak-ng (22050 Hz mono)
    ↓
PulseAudio client
    ↓
WSLG PulseAudio server (resamples to 44100 Hz stereo using speex-float-1)
    ↓
RDP audio transport (module-rdp-sink)
    ↓
Windows audio output
```

The WSLG PulseAudio server uses `speex-float-1`, the lowest quality resampler, and its configuration is on a read-only filesystem (`/mnt/wslg/distro/etc/pulse/daemon.conf`) that cannot be modified by users.

### What Doesn't Help

These have been tested and do not improve the audio quality:

- Changing voices (espeak-ng or MBROLA — all sound equally scratchy)
- Setting `PULSE_LATENCY_MSEC` (buffer size doesn't affect resampling quality)
- Piping through `paplay` (resampling still happens server-side)
- Creating a local `~/.config/pulse/daemon.conf` (only affects a local daemon, not the WSLG server)

### What Might Help

- **Updating WSL** (`wsl --update` from Windows) — Microsoft may improve the default resampler in future releases
- **Native Linux** — espeak-ng sounds clean on native Linux where PulseAudio uses better resampling or no resampling is needed
- **Different Windows audio drivers** — audio driver quality can compound the issue

The speech is fully functional despite the audio artifacts. If clear audio is critical, consider running TDSR on a native Linux installation where this issue does not occur.

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

If PulseAudio + espeak-ng fails, TDSR falls back to:
1. Windows SAPI via PowerShell (higher latency)
2. Speech Dispatcher (if installed)

## See Also

- [README.md](README.md) - General documentation
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - More troubleshooting help
- [INSTALL.md](INSTALL.md) - Installation guide
