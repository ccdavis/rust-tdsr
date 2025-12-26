# TDSR Troubleshooting Guide

## Quick Diagnostics

### 1. Test System Speech

**Linux:**
```bash
spd-say "Hello from speech dispatcher"
```

**macOS:**
```bash
say "Hello from macOS"
```

**WSL:**
```bash
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Hello')"
```

### 2. Check TDSR Logs

```bash
RUST_LOG=debug tdsr --debug
cat tdsr.log | grep -i "speech\|error"
```

## No Speech Output

### Linux

**1. Install Speech Dispatcher:**
```bash
sudo apt install speech-dispatcher
```

**2. Start the service:**
```bash
systemctl --user start speech-dispatcher
systemctl --user enable speech-dispatcher
```

**3. Configure Speech Dispatcher:**
```bash
spd-conf
```

**4. Check audio output:**
```bash
speaker-test -t wav -c 2
```

**5. Check audio group permissions:**
```bash
groups | grep audio
sudo usermod -aG audio $USER
# Log out and back in
```

### macOS

**1. Test system speech:**
```bash
say "test"
```

**2. Check macOS version (requires 10.14+):**
```bash
sw_vers
```

**3. Check accessibility permissions:**
System Preferences → Security & Privacy → Privacy → Accessibility

### WSL

**1. Verify Windows interop:**
```bash
powershell.exe -Command "echo 'test'"
```

**2. Test Windows SAPI:**
```bash
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
```

**3. Add PowerShell to PATH if needed:**
```bash
echo 'export PATH="$PATH:/mnt/c/Windows/System32/WindowsPowerShell/v1.0"' >> ~/.bashrc
source ~/.bashrc
```

## Build Errors

### "libclang not found"

```bash
# Ubuntu/Debian
sudo apt install libclang-dev

# Fedora
sudo dnf install clang-devel

# Arch
sudo pacman -S clang
```

### "speechd.h not found"

```bash
# Ubuntu/Debian
sudo apt install libspeechd-dev

# Fedora
sudo dnf install speech-dispatcher-devel

# Arch
sudo pacman -S speech-dispatcher
```

## Speech Rate/Volume Issues

### Adjust in TDSR

Press `Alt+c`, then:
- `r` for rate (0-100)
- `v` for volume (0-100)

### Edit Config Directly

```ini
# ~/.tdsr.cfg
[speech]
rate = 50
volume = 80
```

### Configure Speech Dispatcher (Linux)

```bash
mkdir -p ~/.config/speech-dispatcher
nano ~/.config/speech-dispatcher/speechd.conf
# Set DefaultRate and DefaultVolume
systemctl --user restart speech-dispatcher
```

## Voice Selection

### Linux

```bash
spd-conf  # Select output module and voice
```

### macOS

```bash
say -v '?'  # List available voices
# In TDSR: Alt+c → V → enter voice index
```

### WSL

In TDSR: `Alt+c` → `V` → enter voice index, or change Windows default in Settings → Time & Language → Speech

## Speech Cuts Off

**Possible causes:**
- Rapid key input during speech
- High system load
- Audio buffer issues

**Solutions:**
- Use `Alt+x` to cancel before new input
- Check `top` for high CPU usage
- Try different Speech Dispatcher backend: `spd-conf`

## TTS Initialization Failure

**Linux:**
```bash
# Kill and restart Speech Dispatcher
killall speech-dispatcher
systemctl --user start speech-dispatcher

# Clear cache
rm -rf ~/.cache/speech-dispatcher
```

**macOS:**
- Toggle System Preferences → Accessibility → Spoken Content off and on
- Try different voice: `say -v Alex "test"`

## Performance Issues

### Slow Startup

```bash
time tdsr -c "echo test"
# Should be < 500ms
```

If slow, check Speech Dispatcher startup time.

### High Memory

```bash
ps aux | grep tdsr
# Should be < 50 MB
```

## Debug Logging

```bash
# Full trace logging
RUST_LOG=trace tdsr --debug 2>&1 | tee full.log

# Check specific components
grep -i "speech\|synth" tdsr.log
grep -i "error\|warning" tdsr.log
```

## Reporting Issues

Include:
1. OS and version: `uname -a`
2. TDSR logs: `cat tdsr.log`
3. Steps to reproduce
4. System speech test results

## See Also

- [INSTALL.md](INSTALL.md) - Installation guide
- [WSL.md](WSL.md) - WSL-specific help
- [TESTING.md](TESTING.md) - Testing procedures
