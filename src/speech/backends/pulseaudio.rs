//! PulseAudio backend using espeak-ng
//!
//! This backend is designed for WSL with WSLG support, where PulseAudio
//! is available through /mnt/wslg/PulseServer. It uses espeak-ng for
//! text-to-speech synthesis.
//!
//! Dependencies:
//! - espeak-ng (install with: sudo apt install espeak-ng)
//! - PulseAudio client libraries (usually pre-installed with WSLG)

use crate::platform::is_wsl;
use crate::speech::{Synth, SpeechCommand};
use crate::{Result, TdsrError};
use log::{debug, error, info, warn};
use std::process::{Child, Command, Stdio};

/// PulseAudio backend using espeak-ng
pub struct PulseAudioSynth {
    /// Currently running espeak-ng process
    current_process: Option<Child>,

    /// Cached rate setting (0-100)
    rate: u8,

    /// Cached volume setting (0-100)
    volume: u8,

    /// Voice name for espeak-ng
    voice: String,

    /// Path to espeak-ng
    espeak_path: String,
}

impl PulseAudioSynth {
    /// Setup PulseAudio server environment
    ///
    /// Auto-detects WSLG PulseAudio server and sets PULSE_SERVER if needed.
    /// Returns error with helpful message if PulseAudio is not available.
    fn setup_pulseaudio() -> Result<()> {
        const WSLG_PULSE_PATH: &str = "/mnt/wslg/PulseServer";

        // Check if PULSE_SERVER is already set
        if std::env::var("PULSE_SERVER").is_ok() {
            debug!("PULSE_SERVER already set via environment");
            return Ok(());
        }

        // Try to auto-detect WSLG PulseAudio server
        if std::path::Path::new(WSLG_PULSE_PATH).exists() {
            info!("Auto-detected WSLG PulseAudio server at {}", WSLG_PULSE_PATH);
            std::env::set_var("PULSE_SERVER", WSLG_PULSE_PATH);
            return Ok(());
        }

        // PulseAudio not found - provide helpful error message only on WSL
        if is_wsl() {
            warn!("WSLG PulseAudio server not found at {}", WSLG_PULSE_PATH);
            warn!("Make sure WSLg is installed and running");
            warn!("You can also set the PULSE_SERVER environment variable:");
            warn!("  export PULSE_SERVER=/path/to/pulseaudio");
            return Err(TdsrError::Speech(
                "PulseAudio server not found. Install WSLg or set PULSE_SERVER environment variable.".to_string()
            ));
        }

        // On native Linux, PulseAudio might be available via default socket
        // Let espeak-ng try to connect - it will fail if not available
        debug!("Running on native Linux - PulseAudio will use default configuration");
        Ok(())
    }

    /// Create a new PulseAudio synthesizer
    ///
    /// Verifies espeak-ng and PulseAudio are available
    pub fn new() -> Result<Self> {
        debug!("Creating PulseAudio backend");

        // Setup PulseAudio environment
        Self::setup_pulseaudio()?;

        // Find espeak-ng
        let espeak_path = Self::find_espeak()?;
        debug!("Found espeak-ng at: {}", espeak_path);

        Ok(Self {
            current_process: None,
            rate: 50,  // Default rate
            volume: 80, // Default volume
            voice: "en".to_string(), // Default English voice
            espeak_path,
        })
    }

    /// Find espeak-ng executable
    fn find_espeak() -> Result<String> {
        let paths = vec!["espeak-ng", "/usr/bin/espeak-ng"];

        for path in paths {
            if let Ok(output) = Command::new(path)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
            {
                if output.success() {
                    return Ok(path.to_string());
                }
            }
        }

        Err(TdsrError::Speech(
            "espeak-ng not found. Install with: sudo apt install espeak-ng".to_string()
        ))
    }

    /// Convert TDSR rate (0-100) to espeak speed (80-450 wpm)
    fn rate_to_espeak_speed(tdsr_rate: u8) -> u16 {
        // TDSR 0 = 80 wpm (very slow)
        // TDSR 50 = 175 wpm (default)
        // TDSR 100 = 450 wpm (very fast)
        80 + ((tdsr_rate as u16) * 370 / 100)
    }

    /// Convert TDSR volume (0-100) to espeak amplitude (0-200)
    fn volume_to_espeak_amplitude(tdsr_volume: u8) -> u8 {
        // Scale 0-100 to 0-200
        ((tdsr_volume as u16 * 200) / 100) as u8
    }

    /// Get voice name by index
    fn get_voice_by_idx(idx: usize) -> &'static str {
        const VOICES: &[&str] = &[
            "en",       // 0: Default English
            "en-us",    // 1: US English
            "en-gb",    // 2: British English
            "en-sc",    // 3: Scottish English
            "es",       // 4: Spanish
            "fr",       // 5: French
            "de",       // 6: German
            "it",       // 7: Italian
            "pt",       // 8: Portuguese
            "ru",       // 9: Russian
        ];

        VOICES.get(idx).unwrap_or(&"en")
    }

    /// Cancel any currently running speech process
    fn cancel_process(&mut self) {
        if let Some(mut child) = self.current_process.take() {
            debug!("Killing espeak-ng process");
            match child.kill() {
                Ok(_) => {
                    let _ = child.wait(); // Clean up zombie
                }
                Err(e) => {
                    debug!("Failed to kill espeak-ng process: {}", e);
                }
            }
        }
    }

    /// Speak text using espeak-ng
    fn speak_internal(&mut self, text: &str, is_letter: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        // Cancel any current speech
        self.cancel_process();

        let speed = Self::rate_to_espeak_speed(self.rate);
        let amplitude = Self::volume_to_espeak_amplitude(self.volume);

        let mut cmd = Command::new(&self.espeak_path);
        cmd.arg("-v").arg(&self.voice);
        cmd.arg("-s").arg(speed.to_string());
        cmd.arg("-a").arg(amplitude.to_string());

        // For letters, add spacing
        let text_to_speak = if is_letter {
            format!(" {} ", text)
        } else {
            text.to_string()
        };

        cmd.arg(text_to_speak);

        // PULSE_SERVER is already set in new() and will be inherited by subprocess
        // Spawn espeak-ng process
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => {
                self.current_process = Some(child);
                debug!("espeak-ng process started");
                Ok(())
            }
            Err(e) => {
                error!("Failed to spawn espeak-ng: {}", e);
                Err(TdsrError::Speech(format!("Failed to start espeak-ng: {}", e)))
            }
        }
    }
}

impl Synth for PulseAudioSynth {
    fn send(&mut self, cmd: SpeechCommand) -> Result<()> {
        match cmd {
            SpeechCommand::Speak(text) => self.speak(&text),
            SpeechCommand::Letter(ch) => self.letter(&ch.to_string()),
            SpeechCommand::Cancel => self.cancel(),
            SpeechCommand::SetRate(rate) => self.set_rate(rate),
            SpeechCommand::SetVolume(vol) => self.set_volume(vol),
            SpeechCommand::SetVoiceIdx(idx) => self.set_voice_idx(idx),
        }
    }

    fn set_rate(&mut self, rate: u8) -> Result<()> {
        debug!("Setting rate to {}", rate);
        self.rate = rate;
        Ok(())
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        debug!("Setting volume to {}", volume);
        self.volume = volume;
        Ok(())
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let voice = Self::get_voice_by_idx(idx);
        debug!("Setting voice to {} (index {})", voice, idx);
        self.voice = voice.to_string();
        Ok(())
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        debug!("Speaking: {}", text);
        self.speak_internal(text, false)
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        debug!("Speaking letter: {}", text);
        self.speak_internal(text, true)
    }

    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        self.cancel_process();
        Ok(())
    }
}

impl Drop for PulseAudioSynth {
    fn drop(&mut self) {
        debug!("Shutting down PulseAudio backend");
        self.cancel_process();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_conversion() {
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(0), 80);    // Slowest
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(50), 265);  // Normal
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(100), 450); // Fastest
    }

    #[test]
    fn test_volume_conversion() {
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(0), 0);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(50), 100);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(100), 200);
    }

    #[test]
    fn test_voice_selection() {
        assert_eq!(PulseAudioSynth::get_voice_by_idx(0), "en");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(1), "en-us");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(2), "en-gb");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(999), "en"); // Out of range defaults to en
    }

    #[test]
    fn test_create_pulseaudio_synth() {
        match PulseAudioSynth::new() {
            Ok(_) => println!("✓ PulseAudio backend available"),
            Err(e) => println!("⚠ PulseAudio backend not available: {}", e),
        }
    }
}
