//! Speech synthesizer abstraction
//!
//! Provides a unified interface for text-to-speech across platforms.
//! The screen reader uses this to speak all output to the user.

use crate::platform::is_wsl;
use crate::Result;
use log::info;

/// Commands sent to speech backend
///
/// These map to the protocol used by Python backend scripts
#[derive(Debug, Clone)]
pub enum SpeechCommand {
    /// Speak a string of text
    Speak(String),
    /// Speak a single character (letter)
    Letter(char),
    /// Cancel/silence current speech
    Cancel,
    /// Set speech rate (0-100)
    SetRate(u8),
    /// Set speech volume (0-100)
    SetVolume(u8),
    /// Set voice index (backend-specific)
    SetVoiceIdx(usize),
}

/// Speech synthesizer trait
///
/// All backends implement this to provide text-to-speech.
/// The screen reader calls these methods to provide audio feedback.
pub trait Synth: Send {
    /// Send a raw command to the backend
    fn send(&mut self, cmd: SpeechCommand) -> Result<()>;

    /// Set speech rate (0-100, where 50 is normal)
    fn set_rate(&mut self, rate: u8) -> Result<()>;

    /// Set speech volume (0-100)
    fn set_volume(&mut self, volume: u8) -> Result<()>;

    /// Set voice by index (platform-specific)
    fn set_voice_idx(&mut self, idx: usize) -> Result<()>;

    /// Speak text to the user
    fn speak(&mut self, text: &str) -> Result<()>;

    /// Speak a single letter/character
    fn letter(&mut self, text: &str) -> Result<()>;

    /// Cancel/silence current speech
    fn cancel(&mut self) -> Result<()>;
}

/// Create a platform-appropriate speech synthesizer
///
/// Automatically detects the environment and selects the best backend:
///
/// **WSL (Windows Subsystem for Linux):**
/// 1. PulseAudio + espeak-ng (lowest latency, direct audio)
/// 2. Windows SAPI via PowerShell (if espeak-ng not installed)
/// 3. Speech Dispatcher (if SAPI unavailable)
///
/// **Native Linux:**
/// 1. Speech Dispatcher (standard Linux TTS, respects system preferences)
/// 2. PulseAudio + espeak-ng (fallback if Speech Dispatcher unavailable)
///
/// **macOS:**
/// - AVFoundation (via tts crate native bindings)
///
/// All backends provide helpful error messages when unavailable.
pub fn create_synth() -> Result<Box<dyn Synth>> {
    let platform = std::env::consts::OS;

    // Special case: WSL (Linux with Windows interop)
    if platform == "linux" && is_wsl() {
        info!("Detected WSL environment");

        // Try PulseAudio backend first (best performance on WSL with WSLG)
        info!("Trying PulseAudio + espeak-ng backend...");
        use super::backends::pulseaudio::PulseAudioSynth;

        match PulseAudioSynth::new() {
            Ok(synth) => {
                info!("✓ Successfully initialized PulseAudio backend");
                return Ok(Box::new(synth));
            }
            Err(e) => {
                info!("✗ PulseAudio backend unavailable: {}", e);
            }
        }

        // Fall back to Windows SAPI
        info!("Trying Windows SAPI backend...");
        use super::backends::windows::WindowsSynth;

        match WindowsSynth::new() {
            Ok(synth) => {
                info!("✓ Successfully initialized Windows SAPI backend");
                return Ok(Box::new(synth));
            }
            Err(e) => {
                info!("✗ Windows SAPI backend unavailable: {}", e);
            }
        }

        // Fall back to Speech Dispatcher
        info!("Trying Speech Dispatcher backend...");
        use super::backends::native::NativeSynth;

        match NativeSynth::new() {
            Ok(synth) => {
                info!("✓ Successfully initialized Speech Dispatcher backend");
                return Ok(Box::new(synth));
            }
            Err(e) => {
                return Err(crate::TdsrError::Speech(format!(
                    "No speech backend available on WSL. Tried:\n\
                     1. PulseAudio + espeak-ng (install: sudo apt install espeak-ng)\n\
                     2. Windows SAPI (PowerShell not available)\n\
                     3. Speech Dispatcher (not configured)\n\
                     Error: {}",
                    e
                )));
            }
        }
    }

    // Native Linux: Try Speech Dispatcher first, then PulseAudio
    if platform == "linux" {
        info!("Detected native Linux environment");

        // Try Speech Dispatcher first (standard Linux TTS)
        info!("Trying Speech Dispatcher backend...");
        use super::backends::native::NativeSynth;

        match NativeSynth::new() {
            Ok(synth) => {
                info!("✓ Successfully initialized Speech Dispatcher backend");
                return Ok(Box::new(synth));
            }
            Err(e) => {
                info!("✗ Speech Dispatcher unavailable: {}", e);
                info!("To install: sudo apt install speech-dispatcher");
            }
        }

        // Fall back to PulseAudio + espeak-ng
        info!("Trying PulseAudio + espeak-ng backend...");
        use super::backends::pulseaudio::PulseAudioSynth;

        match PulseAudioSynth::new() {
            Ok(synth) => {
                info!("✓ Successfully initialized PulseAudio backend");
                return Ok(Box::new(synth));
            }
            Err(e) => {
                return Err(crate::TdsrError::Speech(format!(
                    "No speech backend available on Linux. Tried:\n\
                     1. Speech Dispatcher (install: sudo apt install speech-dispatcher)\n\
                     2. PulseAudio + espeak-ng (install: sudo apt install espeak-ng)\n\
                     Error: {}",
                    e
                )));
            }
        }
    }

    // macOS and other platforms
    info!(
        "Creating native speech synthesizer for platform: {}",
        platform
    );
    use super::backends::native::NativeSynth;

    match NativeSynth::new() {
        Ok(synth) => {
            info!("✓ Successfully initialized native TTS backend");
            Ok(Box::new(synth))
        }
        Err(e) => Err(crate::TdsrError::Speech(format!(
            "Failed to initialize speech backend for platform '{}': {}",
            platform, e
        ))),
    }
}
