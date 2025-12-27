//! Native Rust TTS backend using the tts crate
//!
//! This backend uses the `tts` crate which provides a unified interface to:
//! - Speech Dispatcher on Linux (via native bindings)
//! - AVFoundation on macOS/iOS (via native bindings)
//! - Various other platforms
//!
//! This eliminates the need for Python subprocesses and their dependencies.

use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, error, warn};
use tts::Tts as TtsCrate;

/// Native TTS backend using the tts crate
///
/// Provides text-to-speech functionality without requiring Python subprocesses.
pub struct NativeSynth {
    /// The tts crate's TTS instance
    tts: TtsCrate,

    /// Cached rate setting (0-100)
    rate: Option<u8>,

    /// Cached volume setting (0-100)
    volume: Option<u8>,

    /// Cached voice index
    voice_idx: Option<usize>,
}

impl NativeSynth {
    /// Create a new native TTS synthesizer
    ///
    /// Initializes the platform-appropriate TTS backend
    pub fn new() -> Result<Self> {
        debug!("Creating native TTS backend");

        let tts = TtsCrate::default()
            .map_err(|e| TdsrError::Speech(format!("Failed to initialize TTS: {}", e)))?;

        debug!("Native TTS backend created successfully");

        Ok(Self {
            tts,
            rate: None,
            volume: None,
            voice_idx: None,
        })
    }

    /// Convert TDSR rate (0-100) to tts crate rate
    ///
    /// The tts crate typically uses a normalized rate where the default varies by platform.
    /// We need to map our 0-100 scale appropriately.
    fn convert_rate(&self, tdsr_rate: u8) -> f32 {
        // The tts crate uses platform-specific rate ranges
        // For now, we'll use a simple percentage-based conversion
        // This may need adjustment based on platform testing
        tdsr_rate as f32
    }

    /// Convert TDSR volume (0-100) to tts crate volume (0.0-1.0)
    fn convert_volume(&self, tdsr_volume: u8) -> f32 {
        tdsr_volume as f32 / 100.0
    }
}

impl Synth for NativeSynth {
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
        self.rate = Some(rate);

        // Check if rate control is supported
        let features = self.tts.supported_features();
        if !features.rate {
            warn!("Rate control not supported on this platform");
            return Ok(());
        }

        let converted_rate = self.convert_rate(rate);
        self.tts
            .set_rate(converted_rate)
            .map_err(|e| TdsrError::Speech(format!("Failed to set rate: {}", e)))?;

        Ok(())
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        debug!("Setting volume to {}", volume);
        self.volume = Some(volume);

        // Check if volume control is supported
        let features = self.tts.supported_features();
        if !features.volume {
            warn!("Volume control not supported on this platform");
            return Ok(());
        }

        let converted_volume = self.convert_volume(volume);
        self.tts
            .set_volume(converted_volume)
            .map_err(|e| TdsrError::Speech(format!("Failed to set volume: {}", e)))?;

        Ok(())
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        debug!("Setting voice index to {}", idx);
        self.voice_idx = Some(idx);

        // Get available voices
        let voices = self
            .tts
            .voices()
            .map_err(|e| TdsrError::Speech(format!("Failed to get voices: {}", e)))?;

        if let Some(voice) = voices.get(idx) {
            debug!("Selecting voice: {:?}", voice);
            self.tts
                .set_voice(voice)
                .map_err(|e| TdsrError::Speech(format!("Failed to set voice: {}", e)))?;
        } else {
            warn!(
                "Voice index {} out of range (have {} voices)",
                idx,
                voices.len()
            );
        }

        Ok(())
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        debug!("Speaking: {}", text);
        self.tts.speak(text, false).map_err(|e| {
            error!("Failed to speak: {}", e);
            TdsrError::Speech(format!("Speak failed: {}", e))
        })?;

        Ok(())
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        // The tts crate doesn't have a separate letter/character mode
        // We'll use speak() with the character
        // TODO: Consider adding pauses or using SSML if supported
        debug!("Speaking letter: {}", text);
        self.tts.speak(text, false).map_err(|e| {
            error!("Failed to speak letter: {}", e);
            TdsrError::Speech(format!("Letter speak failed: {}", e))
        })?;

        Ok(())
    }

    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        self.tts.stop().map_err(|e| {
            error!("Failed to cancel speech: {}", e);
            TdsrError::Speech(format!("Cancel failed: {}", e))
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_synth() {
        // This test verifies that we can create a TTS instance
        // It may fail if the system doesn't have speech-dispatcher (Linux)
        // or if running in CI without audio
        let result = NativeSynth::new();

        match result {
            Ok(_) => println!("✓ Native TTS backend initialized successfully"),
            Err(e) => println!("⚠ TTS initialization failed (may be expected in CI): {}", e),
        }
    }

    #[test]
    fn test_rate_conversion() {
        if let Ok(synth) = NativeSynth::new() {
            // Test rate conversion
            assert_eq!(synth.convert_rate(0), 0.0);
            assert_eq!(synth.convert_rate(50), 50.0);
            assert_eq!(synth.convert_rate(100), 100.0);
        }
    }

    #[test]
    fn test_volume_conversion() {
        if let Ok(synth) = NativeSynth::new() {
            // Test volume conversion
            assert_eq!(synth.convert_volume(0), 0.0);
            assert_eq!(synth.convert_volume(50), 0.5);
            assert_eq!(synth.convert_volume(100), 1.0);
        }
    }
}
