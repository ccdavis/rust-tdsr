//! Speech through an external process driven over its stdin.
//!
//! This is the original TDSR speech-server design: the synthesizer is a
//! separate program that reads a line protocol on stdin. On macOS the
//! program is our own binary in `--speech-server` mode; users can also point
//! `[speech] speech_command` (or `--speech-command`) at any program that
//! speaks the protocol:
//!
//! ```text
//!   s<text>\n   speak text
//!   l<text>\n   speak a letter/character
//!   x\n         cancel/silence immediately
//!   r<0-100>\n  set rate
//!   v<0-100>\n  set volume
//!   V<idx>\n    set voice index
//! ```
//!
//! If the process dies, the next command respawns it once; if that fails,
//! further attempts are held off for a second so a broken synth cannot stall
//! the keyboard, and speech stays silent until it recovers.

use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, info, warn};
use std::io::{self, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Minimum interval between respawn attempts after one has failed.
const RESPAWN_BACKOFF: Duration = Duration::from_secs(1);

/// Synth that drives a line-protocol speech server subprocess.
pub struct CommandSynth {
    program: String,
    args: Vec<String>,
    child: Option<Child>,
    // Cached settings so they can be re-applied if the child is respawned.
    rate: Option<u8>,
    volume: Option<u8>,
    voice_idx: Option<usize>,
    /// When the last respawn attempt failed, if it did.
    last_spawn_failure: Option<Instant>,
}

impl CommandSynth {
    /// Start `program args...` as the speech server.
    pub fn new(program: impl Into<String>, args: Vec<String>) -> Result<Self> {
        let mut synth = Self {
            program: program.into(),
            args,
            child: None,
            rate: None,
            volume: None,
            voice_idx: None,
            last_spawn_failure: None,
        };
        synth.spawn()?;
        info!("Speech server started: {} {:?}", synth.program, synth.args);
        Ok(synth)
    }

    /// Start from a shell-like command line ("prog arg 'quoted arg'").
    pub fn from_command_line(command: &str) -> Result<Self> {
        let mut words = split_command(command).into_iter();
        let program = words
            .next()
            .ok_or_else(|| TdsrError::Speech("speech_command is empty".to_string()))?;
        Self::new(program, words.collect())
    }

    fn spawn(&mut self) -> Result<()> {
        // Reap a previous (dead) child before replacing it, or it lingers as
        // a zombie until we exit.
        if let Some(mut old) = self.child.take() {
            let _ = old.kill();
            let _ = old.wait();
        }

        let child = Command::new(&self.program)
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                TdsrError::Speech(format!(
                    "Failed to start speech server '{}': {}",
                    self.program, e
                ))
            })?;
        self.child = Some(child);
        self.last_spawn_failure = None;

        // Re-apply cached settings (best effort; a fresh child has defaults).
        if let Some(r) = self.rate {
            let _ = self.try_write(&format!("r{}", r));
        }
        if let Some(v) = self.volume {
            let _ = self.try_write(&format!("v{}", v));
        }
        if let Some(i) = self.voice_idx {
            let _ = self.try_write(&format!("V{}", i));
        }
        Ok(())
    }

    /// Write one protocol line, respawning the child once if the pipe broke.
    fn write_line(&mut self, line: &str) -> Result<()> {
        if self.try_write(line).is_ok() {
            return Ok(());
        }
        if let Some(failed_at) = self.last_spawn_failure {
            if failed_at.elapsed() < RESPAWN_BACKOFF {
                return Err(TdsrError::Speech("speech server down".to_string()));
            }
        }
        debug!("Speech server pipe broken; respawning");
        if let Err(e) = self.spawn() {
            self.last_spawn_failure = Some(Instant::now());
            warn!("Speech server respawn failed: {}", e);
            return Err(e);
        }
        self.try_write(line)
            .map_err(|e| TdsrError::Speech(format!("Speech server write failed: {}", e)))
    }

    fn try_write(&mut self, line: &str) -> io::Result<()> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "no speech server"))?;
        let stdin = child.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "speech server stdin closed")
        })?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }
}

impl Drop for CommandSynth {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Strip framing-breaking newlines; the server handles the rest of the cleanup.
fn sanitize(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

impl Synth for CommandSynth {
    fn send(&mut self, cmd: SpeechCommand) -> Result<()> {
        match cmd {
            SpeechCommand::Speak(t) => self.speak(&t),
            SpeechCommand::Letter(c) => self.letter(&c.to_string()),
            SpeechCommand::Cancel => self.cancel(),
            SpeechCommand::SetRate(r) => self.set_rate(r),
            SpeechCommand::SetVolume(v) => self.set_volume(v),
            SpeechCommand::SetVoiceIdx(i) => self.set_voice_idx(i),
        }
    }

    fn set_rate(&mut self, rate: u8) -> Result<()> {
        self.rate = Some(rate);
        self.write_line(&format!("r{}", rate))
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        self.volume = Some(volume);
        self.write_line(&format!("v{}", volume))
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        self.voice_idx = Some(idx);
        self.write_line(&format!("V{}", idx))
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        let text = sanitize(text);
        if text.trim().is_empty() {
            return Ok(());
        }
        self.write_line(&format!("s{}", text))
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        let text = sanitize(text);
        if text.is_empty() {
            return Ok(());
        }
        self.write_line(&format!("l{}", text))
    }

    fn cancel(&mut self) -> Result<()> {
        self.write_line("x")
    }
}

/// Split a command line into words the way a POSIX shell would for the
/// simple cases: whitespace separates words, single and double quotes group,
/// backslash escapes the next character outside single quotes.
pub fn split_command(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'') | (None, '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_word = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_command_handles_quotes_and_escapes() {
        assert_eq!(split_command("say -v Alex"), vec!["say", "-v", "Alex"]);
        assert_eq!(
            split_command("python3 '/my dir/server.py' \"a b\" c\\ d"),
            vec!["python3", "/my dir/server.py", "a b", "c d"]
        );
        assert_eq!(split_command("  x  ''  "), vec!["x", ""]);
        assert!(split_command("   ").is_empty());
    }

    #[test]
    fn missing_program_fails_cleanly() {
        let err = CommandSynth::new("/nonexistent/tdsr-speech", vec![]).err();
        assert!(matches!(err, Some(TdsrError::Speech(_))));
        assert!(CommandSynth::from_command_line("   ").is_err());
    }

    #[test]
    fn talks_to_a_cat_process_and_survives_its_death() {
        // `cat` is a fine stand-in for a speech server: it reads stdin.
        let mut synth = CommandSynth::new("cat", vec![]).unwrap();
        synth.set_rate(50).unwrap();
        synth.speak("hello").unwrap();
        assert!(
            synth.speak("   ").is_ok(),
            "whitespace-only text is dropped"
        );
        // Kill it behind our back; the next write respawns it.
        let pid = synth.child.as_ref().unwrap().id();
        unsafe {
            nix::libc::kill(pid as i32, nix::libc::SIGKILL);
        }
        let _ = synth.child.as_mut().unwrap().wait();
        std::thread::sleep(Duration::from_millis(50));
        assert!(synth.cancel().is_ok());
        assert_ne!(synth.child.as_ref().unwrap().id(), pid, "respawned");
    }
}
