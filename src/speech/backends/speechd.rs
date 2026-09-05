//! Speech Dispatcher backend (Linux and other Unixes).
//!
//! Talks SSIP to the user's `speech-dispatcher` daemon through libspeechd
//! (the `speech-dispatcher` crate), which autospawns the daemon if it is not
//! running. The daemon owns the synthesizer choice (espeak-ng, RHVoice,
//! Piper, Voxin, ...) and the audio output; TDSR only sends text.
//!
//! What this backend gets right that a generic TTS wrapper does not:
//! - TDSR's 0-100 rate and volume are mapped onto Speech Dispatcher's
//!   -100..100 scales with TDSR's 50 at the daemon's normal rate, so the
//!   whole range is reachable.
//! - Single characters go through `spd_char`, so key echo says "capital A"
//!   and symbol names the way the daemon is configured to.
//! - The voice list is sorted, so `voice_idx` means the same thing from one
//!   run to the next, and a bad index is refused with a spoken reason.
//!
//! Compiled only with the `native-speech` feature (needs libspeechd headers
//! at build time) and never on macOS.

use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, info, warn};
use speech_dispatcher::{Connection, Mode, Priority};
use std::fmt::Write as _;

/// Name libspeechd registers with the daemon (shows up in `spd-say -L`
/// style tooling and per-client configuration).
const CLIENT_NAME: &str = "tdsr";

/// A voice the daemon's current output module offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpdVoice {
    pub name: String,
    pub language: String,
    pub variant: Option<String>,
}

impl SpdVoice {
    /// Spoken description: name, then language and variant if they add
    /// anything.
    pub fn describe(&self) -> String {
        let mut s = self.name.clone();
        if !self.language.is_empty() && !self.name.eq_ignore_ascii_case(&self.language) {
            let _ = write!(s, ", {}", self.language);
        }
        if let Some(v) = self
            .variant
            .as_deref()
            .filter(|v| !v.is_empty() && *v != "none")
        {
            let _ = write!(s, ", {}", v);
        }
        s
    }
}

/// Speech Dispatcher's -100..100 rate for a TDSR rate (0-100); 50 is the
/// daemon's normal rate.
pub fn rate_to_spd(rate: u8) -> i32 {
    i32::from(rate.min(100)) * 2 - 100
}

/// Speech Dispatcher's -100..100 volume for a TDSR volume (0-100). The
/// espeak-ng module maps this back onto its 0-100 amplitude, so TDSR's
/// volume means the same thing here as with the espeak backends.
pub fn volume_to_spd(volume: u8) -> i32 {
    i32::from(volume.min(100)) * 2 - 100
}

/// Sort and de-duplicate the daemon's voice list so indices are stable.
pub fn stable_voice_order(mut voices: Vec<SpdVoice>) -> Vec<SpdVoice> {
    voices.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.language.cmp(&b.language))
            .then_with(|| a.variant.cmp(&b.variant))
    });
    voices.dedup();
    voices
}

/// Error for an index the list does not have, phrased to be spoken.
pub fn select_voice(voices: &[SpdVoice], idx: usize) -> Result<&SpdVoice> {
    voices.get(idx).ok_or_else(|| {
        TdsrError::Speech(if voices.is_empty() {
            "Speech Dispatcher reports no voices".to_string()
        } else {
            format!("no voice {}, the last voice is {}", idx, voices.len() - 1)
        })
    })
}

/// The listing `tdsr --list-voices` prints when Speech Dispatcher is the
/// backend.
pub fn render_voices(module: Option<&str>, voices: &[SpdVoice]) -> String {
    let mut out = String::new();
    match module {
        Some(m) => {
            let _ = writeln!(
                out,
                "Speech Dispatcher voices (output module {}; index, language, name):",
                m
            );
        }
        None => {
            let _ = writeln!(out, "Speech Dispatcher voices (index, language, name):");
        }
    }
    if voices.is_empty() {
        let _ = writeln!(
            out,
            "(the current output module lists no voices; its voice is chosen in Speech \
             Dispatcher's own configuration, and a `voice = <name>` in ~/.tdsr.cfg is \
             passed to it as given)"
        );
    }
    let width = voices.len().saturating_sub(1).to_string().len().max(1);
    for (idx, v) in voices.iter().enumerate() {
        let variant = v
            .variant
            .as_deref()
            .filter(|s| !s.is_empty() && *s != "none")
            .map(|s| format!(" ({})", s))
            .unwrap_or_default();
        let _ = writeln!(
            out,
            "{idx:>width$}  {:<15} {}{}",
            v.language, v.name, variant
        );
    }
    let _ = writeln!(
        out,
        "\nThese are the voices of the daemon's current output module; other synthesizers \
         are selected in Speech Dispatcher's own configuration (spd-conf).\n\
         To choose one: in TDSR press Alt+c, then V, type the index and press Enter; \
         or put its name in ~/.tdsr.cfg, e.g. `voice = English (America)`."
    );
    out
}

fn open_connection() -> Result<Connection> {
    // Threaded: the crate turns event notifications on while opening, and
    // libspeechd only allows that on a threaded connection (a single-mode
    // open always fails).
    Connection::open(CLIENT_NAME, "main", CLIENT_NAME, Mode::Threaded).map_err(|e| {
        TdsrError::Speech(format!(
            "Could not connect to Speech Dispatcher: {:?} (is speech-dispatcher installed?)",
            e
        ))
    })
}

fn fetch_voices(conn: &Connection) -> Vec<SpdVoice> {
    match conn.list_synthesis_voices() {
        Ok(list) => stable_voice_order(
            list.into_iter()
                .map(|v| SpdVoice {
                    name: v.name,
                    language: v.language,
                    variant: v.variant,
                })
                .collect(),
        ),
        Err(e) => {
            warn!("Speech Dispatcher would not list voices: {:?}", e);
            Vec::new()
        }
    }
}

/// The current output module's name, if the daemon says.
fn current_module(conn: &Connection) -> Option<String> {
    // SSIP: "GET OUTPUT_MODULE" answers "251-<name>\r\n251 OK ..."
    let reply = conn.send_data("GET OUTPUT_MODULE\r\n", true)?;
    reply
        .lines()
        .find_map(|line| line.strip_prefix("251-"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The daemon's voices with their menu indices (for `tdsr --list-voices`).
/// Fails only if the daemon cannot be reached; a module that lists no
/// voices gets a notice, since TDSR would still speak through it.
pub fn list_voices() -> Result<String> {
    let conn = open_connection()?;
    let voices = fetch_voices(&conn);
    Ok(render_voices(current_module(&conn).as_deref(), &voices))
}

/// Speech through the Speech Dispatcher daemon.
pub struct SpeechDispatcherSynth {
    conn: Connection,
    voices: Vec<SpdVoice>,
}

impl SpeechDispatcherSynth {
    pub fn new() -> Result<Self> {
        debug!("Creating Speech Dispatcher backend");
        let conn = open_connection()?;
        let voices = fetch_voices(&conn);
        info!(
            "Speech Dispatcher backend ready ({} voices{})",
            voices.len(),
            current_module(&conn)
                .map(|m| format!(", module {}", m))
                .unwrap_or_default()
        );
        Ok(Self { conn, voices })
    }

    /// The voices the daemon offers, in `voice_idx` order.
    pub fn voices(&self) -> &[SpdVoice] {
        &self.voices
    }

    fn spd_err(what: &str, e: speech_dispatcher::Error) -> TdsrError {
        TdsrError::Speech(format!("Speech Dispatcher {} failed: {:?}", what, e))
    }
}

impl Synth for SpeechDispatcherSynth {
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
        let spd = rate_to_spd(rate);
        debug!("Setting rate to {} (Speech Dispatcher {})", rate, spd);
        self.conn
            .set_voice_rate(spd)
            .map_err(|e| Self::spd_err("set rate", e))
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        let spd = volume_to_spd(volume);
        debug!("Setting volume to {} (Speech Dispatcher {})", volume, spd);
        self.conn
            .set_volume(spd)
            .map_err(|e| Self::spd_err("set volume", e))
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let name = select_voice(&self.voices, idx)?.name.clone();
        self.set_voice(&name).map(|_| ())
    }

    /// Select by voice name. A name the module's list does not have is
    /// refused, unless the module lists nothing, in which case the daemon
    /// decides.
    fn set_voice(&mut self, id: &str) -> Result<String> {
        let id = id.trim();
        let voice = match self.voices.iter().find(|v| v.name == id) {
            Some(v) => v.clone(),
            None if self.voices.is_empty() => SpdVoice {
                name: id.to_string(),
                language: String::new(),
                variant: None,
            },
            None => {
                return Err(TdsrError::Speech(format!(
                    "Speech Dispatcher has no voice named {}",
                    id
                )))
            }
        };
        debug!("Setting voice to {}", voice.name);
        // Per-client (`SET self SYNTHESIS_VOICE`): TDSR's choice must not
        // change the voice of every other Speech Dispatcher client.
        let spd_voice = speech_dispatcher::Voice {
            name: voice.name.clone(),
            language: voice.language.clone(),
            variant: voice.variant.clone(),
        };
        self.conn
            .set_synthesis_voice(&spd_voice)
            .map_err(|e| Self::spd_err("set voice", e))?;
        Ok(voice.describe())
    }

    fn voice_count(&self) -> Option<usize> {
        Some(self.voices.len())
    }

    fn voice_id(&self, idx: usize) -> Option<String> {
        self.voices.get(idx).map(|v| v.name.clone())
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        debug!("Speaking: {}", text);
        self.conn
            .say(Priority::Text, text)
            .map(|_| ())
            .ok_or_else(|| TdsrError::Speech("Speech Dispatcher rejected the text".to_string()))
    }

    /// A single character is sent as a character, so the daemon announces
    /// capitals and symbols the way it is configured to; a symbol name
    /// (several characters) is spoken as text.
    fn letter(&mut self, text: &str) -> Result<()> {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (None, _) => Ok(()),
            (Some(ch), None) if !ch.is_whitespace() => {
                debug!("Speaking character: {}", ch);
                self.conn
                    .char(Priority::Text, ch.to_string())
                    .map_err(|e| Self::spd_err("speak character", e))
            }
            _ => self.speak(text),
        }
    }

    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        self.conn.cancel().map_err(|e| Self::spd_err("cancel", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(name: &str, lang: &str, variant: Option<&str>) -> SpdVoice {
        SpdVoice {
            name: name.to_string(),
            language: lang.to_string(),
            variant: variant.map(String::from),
        }
    }

    #[test]
    fn rate_and_volume_cover_the_daemon_scale_with_50_as_normal() {
        assert_eq!(rate_to_spd(0), -100);
        assert_eq!(rate_to_spd(50), 0);
        assert_eq!(rate_to_spd(100), 100);
        assert_eq!(rate_to_spd(255), 100);
        assert_eq!(volume_to_spd(0), -100);
        assert_eq!(volume_to_spd(80), 60);
        assert_eq!(volume_to_spd(100), 100);
    }

    #[test]
    fn voices_are_sorted_deduplicated_and_selected_by_index() {
        let voices = stable_voice_order(vec![
            voice("German", "de", None),
            voice("English (America)", "en-US", Some("none")),
            voice("afrikaans", "af", None),
            voice("German", "de", None),
        ]);
        let names: Vec<&str> = voices.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["afrikaans", "English (America)", "German"]);

        assert_eq!(select_voice(&voices, 1).unwrap().language, "en-US");
        let err = select_voice(&voices, 5).unwrap_err().to_string();
        assert!(err.contains("no voice 5"), "{}", err);
        assert!(err.contains("last voice is 2"), "{}", err);
        assert!(select_voice(&[], 0)
            .unwrap_err()
            .to_string()
            .contains("no voices"));

        assert_eq!(voices[1].describe(), "English (America), en-US");
        assert_eq!(voice("en", "en", Some("f2")).describe(), "en, f2");

        let listing = render_voices(Some("espeak-ng"), &voices);
        assert!(listing.contains("output module espeak-ng"));
        assert!(listing.contains("1  en-US           English (America)\n"));
        assert!(render_voices(None, &[]).contains("lists no voices"));
    }

    #[test]
    fn backend_if_daemon_available() {
        match SpeechDispatcherSynth::new() {
            Ok(mut s) => {
                println!("✓ Speech Dispatcher: {} voices", s.voices().len());
                assert!(s.set_rate(50).is_ok());
                assert!(s.set_volume(80).is_ok());
                assert!(s.set_voice_idx(s.voices().len()).is_err());
                assert!(s.cancel().is_ok());
            }
            Err(e) => println!("⚠ Speech Dispatcher not available: {}", e),
        }
    }
}
