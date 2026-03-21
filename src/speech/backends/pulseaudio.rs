//! PulseAudio backend using espeak-ng
//!
//! This backend is designed for WSL with WSLG support, where PulseAudio
//! is available through /mnt/wslg/PulseServer. It uses espeak-ng for
//! text-to-speech synthesis.
//!
//! Uses a persistent espeak-ng process with --stdin to avoid the overhead
//! and audio artifacts of spawning a new process for every utterance.
//! Speech is queued by writing lines to stdin; cancel() kills the process
//! and a new one is started on the next speak() call.
//!
//! Dependencies:
//! - espeak-ng (install with: sudo apt install espeak-ng)
//! - PulseAudio client libraries (usually pre-installed with WSLG)

use crate::platform::is_wsl;
use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, info, warn};
use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};

/// PulseAudio backend using a persistent espeak-ng process
pub struct PulseAudioSynth {
    /// Persistent espeak-ng process (reads from stdin via --stdin flag)
    process: Option<Child>,

    /// Stdin pipe to the persistent process
    stdin: Option<ChildStdin>,

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
            info!(
                "Auto-detected WSLG PulseAudio server at {}",
                WSLG_PULSE_PATH
            );
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
            process: None,
            stdin: None,
            rate: 50,                // Default rate
            volume: 80,              // Default volume
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
            "espeak-ng not found. Install with: sudo apt install espeak-ng".to_string(),
        ))
    }

    /// Convert TDSR rate (0-100) to espeak speed (80-450 wpm)
    fn rate_to_espeak_speed(tdsr_rate: u8) -> u16 {
        // TDSR 0 = 80 wpm (very slow)
        // TDSR 50 = 175 wpm (default)
        // TDSR 100 = 450 wpm (very fast)
        80 + ((tdsr_rate as u16) * 370 / 100)
    }

    /// Convert TDSR volume (0-100) to espeak amplitude (0-100)
    fn volume_to_espeak_amplitude(tdsr_volume: u8) -> u8 {
        // Direct mapping: TDSR 0-100 → espeak 0-100
        // espeak-ng's default amplitude is 100; values above 100 cause
        // clipping and distortion (scratchy/static audio)
        tdsr_volume
    }

    /// Get voice name by index
    ///
    /// Indices 0-9: espeak-ng built-in voices (always available)
    /// Indices 10+: MBROLA voices (require mbrola + voice data packages)
    fn get_voice_by_idx(idx: usize) -> &'static str {
        const VOICES: &[&str] = &[
            "en",     // 0: Default English
            "en-us",  // 1: US English
            "en-gb",  // 2: British English
            "en-sc",  // 3: Scottish English
            "es",     // 4: Spanish
            "fr",     // 5: French
            "de",     // 6: German
            "it",     // 7: Italian
            "pt",     // 8: Portuguese
            "ru",     // 9: Russian
            "mb-us1", // 10: MBROLA US English Female (apt: mbrola mbrola-us1)
            "mb-us2", // 11: MBROLA US English Male (apt: mbrola mbrola-us2)
            "mb-us3", // 12: MBROLA US English Male 2 (apt: mbrola mbrola-us3)
            "mb-en1", // 13: MBROLA British English Male (apt: mbrola mbrola-en1)
        ];

        VOICES.get(idx).unwrap_or(&"en")
    }

    /// Start a persistent espeak-ng process with --stdin
    ///
    /// The process reads text from stdin line-by-line and speaks each line.
    /// This avoids the overhead and audio artifacts of spawning a new process
    /// for every utterance, and allows speech to be properly queued.
    fn start_process(&mut self) -> Result<()> {
        self.stop_process();

        let speed = Self::rate_to_espeak_speed(self.rate);
        let amplitude = Self::volume_to_espeak_amplitude(self.volume);

        // Don't pass --stdin: it waits for EOF before speaking.
        // Without --stdin or a text argument, espeak-ng reads from stdin
        // line by line, speaking each line as it arrives.
        let mut cmd = Command::new(&self.espeak_path);
        cmd.arg("-v")
            .arg(&self.voice)
            .arg("-s")
            .arg(speed.to_string())
            .arg("-a")
            .arg(amplitude.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        // Increase PulseAudio buffer to reduce crackling/static artifacts,
        // especially on WSLG where the audio transport adds latency
        cmd.env("PULSE_LATENCY_MSEC", "60");

        let mut child = cmd
            .spawn()
            .map_err(|e| TdsrError::Speech(format!("Failed to start espeak-ng: {}", e)))?;

        self.stdin = child.stdin.take();
        self.process = Some(child);
        debug!("Started persistent espeak-ng process");
        Ok(())
    }

    /// Stop the persistent espeak-ng process
    fn stop_process(&mut self) {
        // Drop stdin first to close the pipe
        self.stdin = None;

        if let Some(mut child) = self.process.take() {
            debug!("Stopping espeak-ng process");
            let _ = child.kill();
            let _ = child.wait(); // Clean up zombie
        }
    }

    /// Ensure the persistent process is running, restarting if needed
    fn ensure_process(&mut self) -> Result<()> {
        let needs_restart = match &mut self.process {
            Some(child) => match child.try_wait() {
                Ok(Some(_)) => {
                    // Process exited
                    self.process = None;
                    self.stdin = None;
                    true
                }
                Ok(None) => self.stdin.is_none(), // Running but no stdin pipe
                Err(_) => {
                    self.process = None;
                    self.stdin = None;
                    true
                }
            },
            None => true,
        };

        if needs_restart {
            self.start_process()?;
        }
        Ok(())
    }

    /// Speak text by writing to the persistent espeak-ng process stdin
    ///
    /// Unlike the old approach of spawning a new process per utterance,
    /// this writes text as a line to the persistent process. espeak-ng
    /// queues and speaks each line in order. This means multiple rapid
    /// speak() calls (e.g., multi-line command output) are all heard.
    fn speak_internal(&mut self, text: &str, is_letter: bool) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        self.ensure_process()?;

        let text_to_speak = if is_letter {
            format!(" {} ", text)
        } else {
            text.to_string()
        };

        // Try to write to the process; if the pipe is broken, restart and retry
        if let Some(ref mut stdin) = self.stdin {
            if let Err(e) = writeln!(stdin, "{}", text_to_speak) {
                warn!("espeak-ng pipe broken (restarting): {}", e);
                self.start_process()?;
                if let Some(ref mut stdin) = self.stdin {
                    writeln!(stdin, "{}", text_to_speak).map_err(|e| {
                        TdsrError::Speech(format!("Failed to write to espeak-ng: {}", e))
                    })?;
                    stdin.flush().map_err(|e| {
                        TdsrError::Speech(format!("Failed to flush espeak-ng stdin: {}", e))
                    })?;
                }
            } else {
                stdin.flush().map_err(|e| {
                    TdsrError::Speech(format!("Failed to flush espeak-ng stdin: {}", e))
                })?;
            }
        }

        Ok(())
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
        // Restart process to apply new rate
        if self.process.is_some() {
            self.start_process()?;
        }
        Ok(())
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        debug!("Setting volume to {}", volume);
        self.volume = volume;
        // Restart process to apply new volume
        if self.process.is_some() {
            self.start_process()?;
        }
        Ok(())
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let voice = Self::get_voice_by_idx(idx);
        debug!("Setting voice to {} (index {})", voice, idx);
        self.voice = voice.to_string();
        // Restart process to apply new voice
        if self.process.is_some() {
            self.start_process()?;
        }
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
        self.stop_process();
        Ok(())
    }
}

impl Drop for PulseAudioSynth {
    fn drop(&mut self) {
        debug!("Shutting down PulseAudio backend");
        self.stop_process();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_conversion() {
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(0), 80); // Slowest
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(50), 265); // Normal
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(100), 450); // Fastest
    }

    #[test]
    fn test_volume_conversion() {
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(0), 0);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(50), 50);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(100), 100);
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
