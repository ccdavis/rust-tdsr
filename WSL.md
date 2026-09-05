# TDSR on WSL (Windows Subsystem for Linux)

TDSR automatically detects WSL and selects the best available speech backend.

## Speech Backend Priority

On WSL, TDSR tries speech backends in this order:

1. **espeak-ng in-process + PulseAudio** (lowest latency, best for interactive use; falls back to espeak-ng subprocesses)
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

# Optional: parec (pulseaudio-utils), used only by the subprocess fallback
# backend to wake WSLg's audio sink after a pause (see "How It Works")
sudo apt install pulseaudio-utils

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
Audio thread: libespeak-ng (in-process) → 20 ms PCM chunks → 44100 Hz
    ↓
TDSR's own PulseAudio playback stream (40 ms buffer, opened only while
sound is playing; pauses are silent gaps with the stream closed)
    ↓
WSLG PulseAudio → RDP audio (module-rdp-sink, 5 ms blocks) → Windows audio output
```

TDSR detects WSL by checking `/proc/version` for "microsoft" or "wsl".

If `libespeak-ng.so.1` or `libpulse-simple.so.0` cannot be loaded, TDSR
falls back to running `espeak-ng` as a subprocess per utterance (and then to
Windows SAPI and Speech Dispatcher).

### Why TDSR manages the audio stream itself on WSL

WSLg's RDP audio sink forwards audio to Windows in 5 ms blocks and allows up
to 1.3 s of them to be outstanding. The Windows side plays those blocks a
few percent slower than the sink sends them, so during continuous audio the
backlog on the Windows side grows by tens of milliseconds per second. It
grows with silence too: an open stream that carries nothing still makes the
sink send silence. Audio already forwarded cannot be cancelled, so a key
echo typed after a long read would only be heard after the backlog, and once
the backlog saturates the WSLg PulseAudio server stops responding to clients
altogether (speech goes dead until WSLg's `pulseaudio` is restarted, e.g.
with `wsl --shutdown`).

The backlog only shrinks while the sink has no stream at all, so TDSR
transmits sound and nothing else. espeak-ng's own pauses (between clauses
and at the end of each line) are not sent as silent samples: the stream is
closed for their duration and reopened for the next sound, so the cadence is
unchanged but every pause lets Windows catch up. While sound is streaming,
the stream's reported latency (on WSL, the client's measured playback delay)
is checked every 250 ms and a short silent gap inserted if it exceeds
200 ms. The stream is closed whenever nothing is playing, and `cancel`
discards the 40 ms the server holds, so what you hear after a key press is
at most a few tens of milliseconds of the old speech.

The sink also never resets its pacing clock when it resumes from suspend
(PulseAudio suspends it 5 s after the last stream closes), and the first
stream after such a pause bursts a backlog to Windows. TDSR keeps the sink
awake, without sending anything, by holding a recording of the sink's
monitor source for as long as it runs.

What remains is the RDP audio path itself: measured on WSL2, a sample
written to PulseAudio is heard roughly 45 to 55 ms later, and that floor
cannot be lowered from inside WSL. On native Linux the same backend gets
the sound card's latency instead, typically 10 to 30 ms.

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
