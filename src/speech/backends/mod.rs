//! Platform-specific speech backends

// Native TTS backend using the tts crate (Speech Dispatcher on Linux and other
// Unixes). Optional: only compiled when the `native-speech` feature is enabled,
// and never on macOS, where it cannot work (see avfoundation.rs).
#[cfg(all(feature = "native-speech", not(target_os = "macos")))]
pub mod native;

// External speech server driven over stdin (macOS server, custom commands)
pub mod command;

// Windows SAPI backend for WSL
pub mod windows;

// PulseAudio backend using espeak-ng for WSL/WSLG
pub mod pulseaudio;

// macOS AVFoundation backend, run as a `tdsr --speech-server` subprocess
#[cfg(target_os = "macos")]
pub mod avfoundation;
