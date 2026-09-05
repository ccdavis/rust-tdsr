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

    /// Set voice by index into the backend's current voice list (what the
    /// config menu and `--list-voices` number). Backends that know their
    /// voices return an error, phrased to be spoken, for an index that
    /// cannot be used.
    fn set_voice_idx(&mut self, idx: usize) -> Result<()>;

    /// Set voice by the persistent id `voice_id` reported for it (what the
    /// config stores as `voice`). Returns the voice's spoken name. Backends
    /// that only have indices leave the default, an error.
    fn set_voice(&mut self, id: &str) -> Result<String> {
        Err(crate::TdsrError::Speech(format!(
            "this speech backend selects voices by index, not by name ({})",
            id
        )))
    }

    /// How many voices the backend lists, or `None` for a backend that has
    /// no list and can only pass an index on (the macOS server, external
    /// speech commands, SAPI).
    fn voice_count(&self) -> Option<usize> {
        None
    }

    /// Persistent id of the voice at `idx` (an espeak-ng voice file, a
    /// Speech Dispatcher voice name); `None` if the backend has no list or
    /// the index is out of range.
    fn voice_id(&self, idx: usize) -> Option<String> {
        let _ = idx;
        None
    }

    /// The voice an older TDSR meant by a saved `voice_idx = idx`, for
    /// backends whose numbering changed since. Only consulted for a config
    /// that has `voice_idx` but no `voice`; the result is migrated to
    /// `voice`. Defaults to the current meaning of the index.
    fn legacy_voice_id(&self, idx: usize) -> Option<String> {
        self.voice_id(idx)
    }

    /// Speak text to the user
    fn speak(&mut self, text: &str) -> Result<()>;

    /// Speak a single letter/character
    fn letter(&mut self, text: &str) -> Result<()>;

    /// Cancel/silence current speech
    fn cancel(&mut self) -> Result<()>;
}

/// Try the in-process espeak-ng backend (Linux and WSL only).
#[cfg(target_os = "linux")]
fn try_espeak_in_process() -> Option<Box<dyn Synth>> {
    info!("Trying espeak-ng in-process backend...");
    match super::backends::espeak::EspeakSynth::new() {
        Ok(synth) => {
            info!("✓ Successfully initialized espeak-ng in-process backend");
            Some(Box::new(synth))
        }
        Err(e) => {
            info!("✗ espeak-ng in-process backend unavailable: {}", e);
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn try_espeak_in_process() -> Option<Box<dyn Synth>> {
    None
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
/// - AVFoundation, driven by a `tdsr --speech-server` subprocess (the only
///   backend; if it cannot start, the error is spoken through `say` and
///   returned so TDSR exits with a clear message rather than running silent)
///
/// **Any platform:** `speech_command` (config `[speech] speech_command` or
/// `--speech-command`) names an external program that speaks the TDSR line
/// protocol; when set it is used instead of the platform backend.
///
/// All backends provide helpful error messages when unavailable.
pub fn create_synth(speech_command: Option<&str>) -> Result<Box<dyn Synth>> {
    let platform = std::env::consts::OS;

    if let Some(cmd) = speech_command {
        info!("Using external speech server: {}", cmd);
        use super::backends::command::CommandSynth;
        return match CommandSynth::from_command_line(cmd) {
            Ok(synth) => Ok(Box::new(synth)),
            Err(e) => {
                let msg = format!("Could not start speech_command '{}': {}", cmd, e);
                #[cfg(target_os = "macos")]
                super::backends::avfoundation::say_blocking(&msg);
                Err(crate::TdsrError::Speech(msg))
            }
        };
    }

    // Special case: WSL (Linux with Windows interop)
    if platform == "linux" && is_wsl() {
        info!("Detected WSL environment");

        // In-process espeak-ng with our own PulseAudio playback first: it is
        // the only backend that keeps WSLg's audio pipeline responsive.
        if let Some(synth) = try_espeak_in_process() {
            return Ok(synth);
        }

        // Then espeak-ng subprocesses through PulseAudio
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

        // Fall back to Speech Dispatcher (only when built with `native-speech`)
        #[cfg(all(feature = "native-speech", not(target_os = "macos")))]
        {
            info!("Trying Speech Dispatcher backend...");
            use super::backends::speechd::SpeechDispatcherSynth;

            match SpeechDispatcherSynth::new() {
                Ok(synth) => {
                    info!("✓ Successfully initialized Speech Dispatcher backend");
                    return Ok(Box::new(synth));
                }
                Err(e) => {
                    info!("✗ Speech Dispatcher unavailable: {}", e);
                }
            }
        }

        return Err(crate::TdsrError::Speech(
            "No speech backend available on WSL. Tried:\n\
             1. espeak-ng + PulseAudio (install: sudo apt install espeak-ng)\n\
             2. Windows SAPI (PowerShell not available)\n\
             3. Speech Dispatcher (not configured, or built without 'native-speech')"
                .to_string(),
        ));
    }

    // Native Linux: Try Speech Dispatcher first, then PulseAudio
    if platform == "linux" {
        info!("Detected native Linux environment");

        // Try Speech Dispatcher first (standard Linux TTS), if compiled in.
        // Builds without `native-speech` skip straight to PulseAudio + espeak-ng.
        #[cfg(all(feature = "native-speech", not(target_os = "macos")))]
        {
            info!("Trying Speech Dispatcher backend...");
            use super::backends::speechd::SpeechDispatcherSynth;

            match SpeechDispatcherSynth::new() {
                Ok(synth) => {
                    info!("✓ Successfully initialized Speech Dispatcher backend");
                    return Ok(Box::new(synth));
                }
                Err(e) => {
                    info!("✗ Speech Dispatcher unavailable: {}", e);
                    info!("To install: sudo apt install speech-dispatcher");
                }
            }
        }

        // Fall back to espeak-ng: in-process with our own playback, then
        // subprocesses through PulseAudio
        if let Some(synth) = try_espeak_in_process() {
            return Ok(synth);
        }

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
                     2. espeak-ng + PulseAudio (install: sudo apt install espeak-ng)\n\
                     Error: {}",
                    e
                )));
            }
        }
    }

    // macOS: the AVFoundation speech-server subprocess is the only backend.
    // There is no in-process fallback (it would play one utterance and go
    // silent), so on failure speak the reason through the system `say`
    // command — the user is likely blind and cannot read stderr — and bail.
    #[cfg(target_os = "macos")]
    {
        info!("Trying AVFoundation speech-server backend...");
        use super::backends::avfoundation::{say_blocking, spawn_server_synth};

        match spawn_server_synth() {
            Ok(synth) => {
                info!("✓ Successfully initialized AVFoundation speech-server backend");
                Ok(Box::new(synth))
            }
            Err(e) => {
                let msg = format!(
                    "TDSR could not start the macOS speech server and is exiting. {}",
                    e
                );
                log::error!("{}", msg);
                say_blocking(&msg);
                Err(crate::TdsrError::Speech(msg))
            }
        }
    }

    // Other platforms: native tts crate, if compiled in.
    #[cfg(all(feature = "native-speech", not(target_os = "macos")))]
    {
        info!(
            "Creating native speech synthesizer for platform: {}",
            platform
        );
        use super::backends::speechd::SpeechDispatcherSynth;

        match SpeechDispatcherSynth::new() {
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

    #[cfg(all(not(feature = "native-speech"), not(target_os = "macos")))]
    {
        Err(crate::TdsrError::Speech(format!(
            "No speech backend available for platform '{}'. This binary was built \
             without the 'native-speech' feature, so the Speech Dispatcher \
             backend is not included.",
            platform
        )))
    }
}
