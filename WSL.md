# TDSR on WSL (Windows Subsystem for Linux)

TDSR automatically detects WSL and selects the best available speech backend.

## Speech Backend Priority

On WSL, TDSR tries speech backends in this order:

1. **Multi-engine in-process backend + WSLg PulseAudio** (default: loads every installed engine — `espeak-ng`, `MBROLA`, `Piper`, `RHVoice`, `SVOX Pico`, and optional `DECtalk` — with **Alt+s** live switching and TDSR's WSLg low-latency audio management)
2. **espeak-ng in-process + PulseAudio** (single-engine fallback)
3. **espeak-ng subprocesses + PulseAudio** (fallback if `libespeak-ng` or `libpulse-simple` cannot load)
4. **Windows SAPI** via PowerShell (higher latency, uses Windows voices)
5. **Speech Dispatcher** (fallback)

The PulseAudio path requires WSLg (included in modern WSL2) and automatically detects `/mnt/wslg/PulseServer` even if `PULSE_SERVER` is not set in your shell. Setting `[speech] backend = alsa` in `~/.tdsr.cfg` with the default ALSA device also routes through this same WSLg-managed PulseAudio output on WSL2.

## Quick Setup (All Voices on WSL2)

```bash
# 1. Install build & runtime prerequisites
./install-deps.sh

# 2. Install all voice engines (espeak-ng, MBROLA, RHVoice, SVOX Pico)
#    and download English Piper neural voices (~760 MB)
./install-voices.sh

# 3. Build TDSR with Piper support
./build.sh --piper        # or: cargo build --release --features piper

# 4. List all available engines and voices, then launch TDSR
./target/release/tdsr --list-voices
./target/release/tdsr
```

Press **Alt+s** inside TDSR to cycle through every installed engine (`e speak`, `M brola`, `Piper`, `R H voice`, `Pico`, and `DEC talk` if linked), and **Alt+a** to step through the voices of the current engine (automatically saved to `~/.tdsr.cfg`). No `~/.tdsr.cfg` edits are required on WSL2.

## Features

- **All engines out of the box** - Switch live between espeak-ng, MBROLA, Piper, RHVoice, Pico (and optional DECtalk) with **Alt+s**
- **Low-latency speech** - TDSR-managed PulseAudio avoids WSLg RDP audio backlog drift and post-suspend lag
- **Windows clipboard integration** - Copy commands (`Alt+v`, `Alt+r`) write directly to the Windows clipboard via `clip.exe` / PowerShell (falling back to `arboard`)
- **TUI screen tracking** - Auto-detects full-screen programs such as the Free Pascal IDE (`fp`), Midnight Commander (`mc`), and `whiptail`
- **Multiple fallbacks** - Automatically falls back to subprocess espeak-ng, Windows SAPI, or Speech Dispatcher
- **Rate and volume control** - Per-engine speech rates (`rate`, `mbrola_rate`, `piper_rate`, `rhvoice_rate`, `pico_rate`, `dectalk_rate`) and global volume

## Requirements

### WSL Setup

WSL 2 with WSLg support (Windows 11, or Windows 10 with recent updates). Use **Windows Terminal** rather than legacy ConHost so `Alt+key` and `F10` sequences reach TDSR and TUI programs cleanly.

### Build Dependencies

```bash
# Ubuntu/Debian
sudo apt install libclang-dev libspeechd-dev pkg-config build-essential
```

### Runtime Dependencies

```bash
# Minimal (espeak-ng only):
sudo apt install espeak-ng

# Recommended (all engines + Piper voices):
./install-voices.sh
```

## Building

```bash
# Standard build (auto-enables Piper if ~/.local/share/piper-voices has models)
./build.sh

# Explicitly include Piper neural voices
./build.sh --piper
# or
cargo build --release --features piper
```

## Testing Speech

```bash
# List every engine and voice TDSR found
./target/release/tdsr --list-voices

# Quick test
./target/release/tdsr

# You should hear the startup message
# Press Alt+i to speak current line, Alt+s to switch speech engines
```

Manual Windows SAPI test:
```bash
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
```

## Configuration

Your `~/.tdsr.cfg` works the same as on Linux/macOS:

```ini
[speech]
engine = piper                 # engine at start-up: espeak, mbrola, piper, rhvoice, pico, dectalk
rate = 50                      # espeak-ng rate: 0=slowest, 50=normal, 100=fastest
volume = 80                    # 0=quietest, 100=loudest
voice = gmw/en-US              # espeak-ng voice from `tdsr --list-voices`
piper_voice = en_US-ryan-high  # Piper voice in ~/.local/share/piper-voices (without .onnx)
rhvoice_voice = slt            # RHVoice voice
pico_voice = en-GB             # SVOX Pico voice
mbrola_voice = us2             # MBROLA voice
```

### Voice Selection

Run `tdsr --list-voices` for every espeak-ng voice and every installed engine's voices (`piper:*`, `rhvoice:*`, `pico:*`, `mbrola:*`, `dectalk:*`), then either press `Alt+c`, `V`, type the index and press Enter inside TDSR (saved under that engine's config key), or edit `~/.tdsr.cfg` directly.

An older `voice_idx` line keeps its old meaning and is migrated on first start. See the [README](README.md#voice-selection-linux--wsl) for details.

### TUI Programs (`fp`, `mc`, `whiptail`) on WSL2

TUI screen tracking is enabled automatically (`tui_mode = auto`, `tui_apps = fp,mc`). When you run `fp`:

- TDSR announces `"TUI mode on"` and reads menu highlights, dialogs, editor lines, and focus changes instead of raw screen repaints.
- Use **F10** and the arrow keys to navigate `fp`'s menus (TDSR captures its own `Alt+<key>` bindings such as `Alt+O`, `Alt+S`, `Alt+X`, `Alt+W`, `Alt+H`; `Alt+F` passes through).
- Press **Alt+w** to re-read the current window/dialog, **Alt+h** to repeat the highlighted item, and **Alt+t** to cycle TUI mode (`auto`, `apps`, `on`, `off`).
- If a very large screen repaint ever splits across two announcements, increase `tui_settle` slightly (e.g., `tui_settle = 50` in `[speech]`).

## How It Works

```
TDSR (Rust binary)
    ↓
Audio thread: in-process engine (espeak-ng / MBROLA / Piper / RHVoice / Pico / DECtalk)
    → 20 ms PCM slices → 2x windowed-sinc upsampling (44100 Hz for 22050 Hz engines)
    ↓
TDSR's own PulseAudio playback stream (40 ms buffer, opened only while
sound is playing; pauses are silent gaps with the stream closed)
    ↓
WSLg PulseAudio → RDP audio (module-rdp-sink, 5 ms blocks) → Windows audio output
```

TDSR detects WSL by checking `/proc/version` for "microsoft" or "wsl" (or `WSL_DISTRO_NAME`).

If `libpulse-simple.so.0` or WSLg's PulseAudio server cannot be reached, TDSR falls back to running `espeak-ng` as a subprocess, then Windows SAPI, then Speech Dispatcher.

### Why TDSR manages the audio stream itself on WSL

WSLg's RDP audio sink forwards audio to Windows in 5 ms blocks and allows up to 1.3 s of them to be outstanding. The Windows side plays those blocks a few percent slower than the sink sends them, so during continuous audio the backlog on the Windows side grows by tens of milliseconds per second. It grows with silence too: an open stream that carries nothing still makes the sink send silence. Audio already forwarded cannot be cancelled, so a key echo typed after a long read would only be heard after the backlog, and once the backlog saturates the WSLg PulseAudio server stops responding to clients altogether (speech goes dead until WSLg's `pulseaudio` is restarted, e.g. with `wsl --shutdown`).

The backlog only shrinks while the sink has no stream at all, so TDSR transmits sound and nothing else. Pauses between clauses and at the end of each line are not sent as silent samples: the stream is closed for their duration and reopened for the next sound, so the cadence is unchanged but every pause lets Windows catch up. While sound is streaming, the stream's reported latency (on WSL, the client's measured playback delay) is checked every 250 ms and a short silent gap inserted if it exceeds 200 ms. The stream is closed whenever nothing is playing, and `cancel` discards the 40 ms the server holds, so what you hear after a key press is at most a few tens of milliseconds of the old speech.

The sink also never resets its pacing clock when it resumes from suspend (PulseAudio suspends it 5 s after the last stream closes), and the first stream after such a pause bursts a backlog to Windows. TDSR keeps the sink awake, without sending anything, by holding a recording of the sink's monitor source for as long as it runs.

What remains is the RDP audio path itself: measured on WSL2, a sample written to PulseAudio is heard roughly 45 to 55 ms later, and that floor cannot be lowered from inside WSL. On native Linux the same backend gets the sound card's latency instead, typically 10 to 30 ms.

For the fallback SAPI backend:
```
TDSR → PowerShell process → .NET System.Speech.Synthesis → Windows audio
```

## Audio Quality & Notes on WSLg

### Resampling

WSLg's PulseAudio server uses `speex-float-1` (a low-quality resampler) whenever a client's sample rate does not match the 44,100 Hz RDP sink. TDSR's PulseAudio player applies a clean 2× windowed-sinc FIR upsampler ([src/speech/resample.rs](src/speech/resample.rs)) before writing to PulseAudio:

- **espeak-ng** and standard **Piper** voices (22,050 Hz) are delivered at **44,100 Hz**, bypassing WSLg's resampler completely.
- **MBROLA** / **Pico** (16,000 Hz → 32,000 Hz) and **RHVoice** (24,000 Hz → 48,000 Hz) are 2× oversampled first, which reduces resampling artifacts significantly compared to raw 16 kHz output.

### Piper Prosody on Ubuntu 24.04

Ubuntu 24.04 ships `espeak-ng` 1.51, which exports `espeak_TextToPhonemes` but not `espeak_TextToPhonemesWithTerminator` (added upstream in 2025). Piper works cleanly with the fallback, though question/exclamation punctuation prosody is slightly flatter than with a newer `espeak-ng`.

## Troubleshooting

### No Speech Output

1. Check WSLg PulseAudio socket:
   ```bash
   ls -la /mnt/wslg/PulseServer
   echo "$PULSE_SERVER"
   ```
   If `/mnt/wslg/PulseServer` is missing or stuck, run `wsl --shutdown` from Windows PowerShell and reopen your WSL terminal.

2. Verify Windows interop (for SAPI fallback and Windows clipboard):
   ```bash
   powershell.exe -Command "echo 'test'"
   ```

3. Test SAPI directly:
   ```bash
   powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
   ```

4. Check TDSR logs:
   ```bash
   ./target/release/tdsr --debug
   grep -iE "speech|engine|pulse|wsl" tdsr.log
   ```

### PowerShell Not Found

```bash
# Add Windows to PATH
echo 'export PATH="$PATH:/mnt/c/Windows/System32/WindowsPowerShell/v1.0"' >> ~/.bashrc
source ~/.bashrc
```

### Fallback Behavior

If the multi-engine PulseAudio backend fails, TDSR falls back to:
1. Single-engine in-process espeak-ng + PulseAudio
2. Subprocess espeak-ng + PulseAudio
3. Windows SAPI via PowerShell (higher latency)
4. Speech Dispatcher (if installed)

## See Also

- [README.md](README.md) - General documentation
- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - More troubleshooting help
- [INSTALL.md](INSTALL.md) - Installation guide
