//! Platform-specific speech backends

// Speech Dispatcher client (Linux and other Unixes). Optional: only compiled
// when the `native-speech` feature is enabled, and never on macOS.
#[cfg(all(feature = "native-speech", not(target_os = "macos")))]
pub mod speechd;

// External speech server driven over stdin (macOS server, custom commands)
pub mod command;

// Windows SAPI backend for WSL
pub mod windows;

// espeak-ng in-process with TDSR-managed PulseAudio playback (Linux/WSL)
#[cfg(target_os = "linux")]
pub mod espeak;

// PulseAudio backend using espeak-ng subprocesses for WSL/WSLG (fallback)
pub mod pulseaudio;

// macOS AVFoundation backend, run as a `tdsr --speech-server` subprocess
#[cfg(target_os = "macos")]
pub mod avfoundation;
