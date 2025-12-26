//! Windows TTS backend using SAPI (System.Speech.Synthesis)
//!
//! This backend is primarily designed for WSL (Windows Subsystem for Linux),
//! where speech-dispatcher may not be available but Windows SAPI can be
//! accessed through PowerShell.
//!
//! Uses a persistent PowerShell process that reads commands from stdin,
//! similar to the Python backend architecture. Commands:
//! - s<text>: Speak text asynchronously
//! - l<char>: Speak letter/character
//! - x: Cancel current speech immediately
//! - r<rate>: Set rate (0-100, converted to SAPI -10 to 10)
//! - v<volume>: Set volume (0-100)
//! - V<idx>: Set voice by index

use crate::speech::{Synth, SpeechCommand};
use crate::{Result, TdsrError};
use log::{debug, error};
use std::io::Write;
use std::process::{Child, Command, Stdio};

/// Windows SAPI backend for WSL
///
/// Communicates with Windows TTS through a persistent PowerShell process.
pub struct WindowsSynth {
    /// Persistent PowerShell process running the speech server
    process: Option<Child>,

    /// Cached rate setting (0-100)
    rate: u8,

    /// Cached volume setting (0-100)
    volume: u8,

    /// Path to powershell.exe
    powershell_path: String,
}

impl WindowsSynth {
    /// Create a new Windows SAPI synthesizer
    ///
    /// Verifies PowerShell is available and spawns persistent speech process
    pub fn new() -> Result<Self> {
        debug!("Creating Windows SAPI backend");

        // Find PowerShell
        let powershell_path = Self::find_powershell()?;
        debug!("Found PowerShell at: {}", powershell_path);

        // Test that SAPI is available
        Self::test_sapi(&powershell_path)?;

        let mut synth = Self {
            process: None,
            rate: 50,  // Default rate
            volume: 80, // Default volume
            powershell_path,
        };

        // Start the persistent speech process
        synth.start_speech_process()?;

        Ok(synth)
    }

    /// Find PowerShell executable (WSL interop)
    fn find_powershell() -> Result<String> {
        // Try common paths
        let paths = vec![
            "powershell.exe",
            "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
        ];

        for path in paths {
            if let Ok(output) = Command::new(path)
                .arg("-Command")
                .arg("$PSVersionTable.PSVersion")
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
            "PowerShell not found. WSL interop may not be enabled.".to_string()
        ))
    }

    /// Test that Windows SAPI is available
    fn test_sapi(powershell_path: &str) -> Result<()> {
        let test_cmd = "Add-Type -AssemblyName System.Speech";

        let output = Command::new(powershell_path)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(test_cmd)
            .output()
            .map_err(|e| TdsrError::Speech(format!("Failed to test SAPI: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(TdsrError::Speech(format!(
                "Windows SAPI not available: {}",
                stderr
            )));
        }

        debug!("Windows SAPI test successful");
        Ok(())
    }

    /// Start the persistent PowerShell speech server process
    fn start_speech_process(&mut self) -> Result<()> {
        debug!("Starting persistent PowerShell speech process");

        // PowerShell script that runs a speech server loop
        // Reads commands from stdin and processes them
        let script = r#"
Add-Type -AssemblyName System.Speech
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$synth.Rate = 0
$synth.Volume = 80

# Read commands from stdin in a loop
while ($line = [Console]::ReadLine()) {
    if ($line -eq $null) { break }
    if ($line.Length -eq 0) { continue }

    $cmd = $line[0]
    $arg = $line.Substring(1)

    switch ($cmd) {
        's' {
            # Speak text asynchronously - cancel any previous speech first
            if ($arg) {
                $synth.SpeakAsyncCancelAll()
                [void]$synth.SpeakAsync($arg)
            }
        }
        'l' {
            # Speak letter/character - cancel any previous speech first
            if ($arg) {
                $synth.SpeakAsyncCancelAll()
                [void]$synth.SpeakAsync($arg)
            }
        }
        'x' {
            # Cancel all speech immediately
            $synth.SpeakAsyncCancelAll()
        }
        'r' {
            # Set rate (convert from 0-100 to -10 to 10)
            if ($arg) {
                $tdsrRate = [int]$arg
                $sapiRate = [Math]::Round(($tdsrRate - 50) / 5.0)
                $synth.Rate = [Math]::Max(-10, [Math]::Min(10, $sapiRate))
            }
        }
        'v' {
            # Set volume (0-100, same scale)
            if ($arg) {
                $vol = [int]$arg
                $synth.Volume = [Math]::Max(0, [Math]::Min(100, $vol))
            }
        }
        'V' {
            # Set voice by index
            if ($arg) {
                $idx = [int]$arg
                $voices = $synth.GetInstalledVoices()
                if ($idx -ge 0 -and $idx -lt $voices.Count) {
                    $synth.SelectVoice($voices[$idx].VoiceInfo.Name)
                }
            }
        }
        'q' {
            # Quit command
            break
        }
    }
}
"#;

        let child = Command::new(&self.powershell_path)
            .arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                error!("Failed to spawn PowerShell speech process: {}", e);
                TdsrError::Speech(format!("Failed to start speech process: {}", e))
            })?;

        debug!("PowerShell speech process started with PID: {:?}", child.id());
        self.process = Some(child);
        Ok(())
    }

    /// Send a command to the PowerShell speech process
    fn send_command(&mut self, cmd: &str) -> Result<()> {
        if let Some(ref mut child) = self.process {
            if let Some(ref mut stdin) = child.stdin {
                writeln!(stdin, "{}", cmd)
                    .map_err(|e| {
                        error!("Failed to write command to speech process: {}", e);
                        TdsrError::Speech(format!("Failed to send command: {}", e))
                    })?;
                stdin.flush()
                    .map_err(|e| {
                        error!("Failed to flush stdin: {}", e);
                        TdsrError::Speech(format!("Failed to flush command: {}", e))
                    })?;
                Ok(())
            } else {
                Err(TdsrError::Speech("Speech process stdin not available".to_string()))
            }
        } else {
            Err(TdsrError::Speech("Speech process not running".to_string()))
        }
    }

    /// Escape text for PowerShell (handle special characters)
    fn escape_text(text: &str) -> String {
        // For stdin commands, we mainly need to handle newlines
        // which could terminate the command prematurely
        text.replace(['\n', '\r'], " ")
    }
}

impl Synth for WindowsSynth {
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
        self.send_command(&format!("r{}", rate))
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        debug!("Setting volume to {}", volume);
        self.volume = volume;
        self.send_command(&format!("v{}", volume))
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        debug!("Setting voice index to {}", idx);
        self.send_command(&format!("V{}", idx))
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let escaped_text = Self::escape_text(text);
        debug!("Speaking: {}", escaped_text);
        self.send_command(&format!("s{}", escaped_text))
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }

        let escaped_text = Self::escape_text(text);
        debug!("Speaking letter: {}", escaped_text);
        self.send_command(&format!("l{}", escaped_text))
    }

    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        self.send_command("x")
    }
}

impl Drop for WindowsSynth {
    fn drop(&mut self) {
        debug!("Shutting down Windows SAPI backend");

        // Try to send quit command
        if let Err(e) = self.send_command("q") {
            debug!("Failed to send quit command: {}", e);
        }

        // Kill the process if it's still running
        if let Some(mut child) = self.process.take() {
            match child.kill() {
                Ok(_) => {
                    debug!("PowerShell speech process terminated");
                    let _ = child.wait(); // Clean up zombie process
                }
                Err(e) => {
                    debug!("Failed to kill PowerShell process: {}", e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_text() {
        assert_eq!(WindowsSynth::escape_text("Hello"), "Hello");
        assert_eq!(WindowsSynth::escape_text("Hello\nWorld"), "Hello World");
        assert_eq!(WindowsSynth::escape_text("Line1\r\nLine2"), "Line1  Line2");
    }

    #[test]
    fn test_create_windows_synth() {
        // This will only work in WSL with Windows interop
        match WindowsSynth::new() {
            Ok(_) => println!("✓ Windows SAPI backend available"),
            Err(e) => println!("⚠ Windows SAPI not available (expected outside WSL): {}", e),
        }
    }
}
