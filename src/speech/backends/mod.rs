//! Platform-specific speech backends

// Native TTS backend using the tts crate (cross-platform)
pub mod native;

// Windows SAPI backend for WSL
pub mod windows;

// PulseAudio backend using espeak-ng for WSL/WSLG
pub mod pulseaudio;
