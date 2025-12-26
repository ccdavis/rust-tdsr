# TDSR Testing Guide

## Prerequisites

### Linux
```bash
# Ensure Speech Dispatcher is running
systemctl --user start speech-dispatcher
spd-say "Test"
```

### macOS
```bash
say "Test"
```

### WSL
```bash
powershell.exe -Command "Add-Type -AssemblyName System.Speech; (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak('Test')"
```

## Automated Tests

```bash
cd rust

# All tests
cargo test

# Speech-specific tests
cargo test speech
```

Tests are designed to pass even without TTS available (for CI environments).

## Manual Testing

### Build and Run

```bash
cargo build --release
./target/release/tdsr --debug
```

### Basic Speech

1. Press `Alt+i` - speak current line
2. Press `Alt+k` - speak current word
3. Press `Alt+k` twice - spell word
4. Press `Alt+,` - speak current character
5. Press `Alt+,` twice - phonetic spelling
6. Press `Alt+x` - cancel speech
7. Press `Alt+q` - toggle quiet mode

### Configuration

1. Press `Alt+c` - open config menu
2. Try `r` for rate (0-100)
3. Try `v` for volume (0-100)
4. Press `ESC` to exit
5. Restart TDSR and verify settings persisted

### Navigation

```bash
# Generate test content
ls -la

# Navigate:
# Alt+u/o - previous/next line
# Alt+j/l - previous/next word
# Alt+m/. - previous/next character
# Alt+U/O - top/bottom of screen
```

### Unicode

```bash
cat > /tmp/test.txt << 'EOF'
Hello World
世界你好
Café naïve
EOF

cat /tmp/test.txt
# Navigate and verify speech works for all characters
```

## Performance

```bash
# Startup time (should be < 500ms)
time ./target/release/tdsr -c "echo test"

# Memory (should be < 50 MB)
ps aux | grep tdsr

# Binary size
ls -lh target/release/tdsr
```

## Debug Logging

```bash
RUST_LOG=debug tdsr --debug
cat tdsr.log
```

## Reporting Issues

Include:
1. OS: `uname -a`
2. Logs: `cat tdsr.log`
3. System speech test result
4. Steps to reproduce

## See Also

- [TROUBLESHOOTING.md](TROUBLESHOOTING.md) - Common issues
- [README.md](README.md) - Usage documentation
