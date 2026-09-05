//! End-to-end key dispatch tests: modal handler stack, key tokenization and
//! quiet mode, driven through the same `dispatch_key` path `main` uses, with a
//! recording synth and a config in a temporary directory.

use std::sync::{Arc, Mutex};
use tdsr::input::{create_default_keymap, dispatch_key, split_keys, DefaultKeyHandler};
use tdsr::speech::{SpeechCommand, Synth};
use tdsr::state::config::Config;
use tdsr::state::State;
use tdsr::terminal::Emulator;
use tdsr::Result;

/// The voices the recording synth pretends to have: (persistent id, name).
const MOCK_VOICES: &[(&str, &str)] = &[("gmw/af", "Afrikaans"), ("gmw/en-US", "English (America)")];

/// Synth that records every command instead of speaking.
struct RecordingSynth(Arc<Mutex<Vec<SpeechCommand>>>);

impl Synth for RecordingSynth {
    fn send(&mut self, cmd: SpeechCommand) -> Result<()> {
        self.0.lock().unwrap().push(cmd);
        Ok(())
    }
    fn set_rate(&mut self, rate: u8) -> Result<()> {
        self.send(SpeechCommand::SetRate(rate))
    }
    fn set_volume(&mut self, volume: u8) -> Result<()> {
        self.send(SpeechCommand::SetVolume(volume))
    }
    /// Pretends to know two voices, like the espeak backends do; a voice
    /// change is recorded as `SetVoiceIdx` of the matching index.
    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let id = self
            .voice_id(idx)
            .ok_or_else(|| tdsr::TdsrError::Speech(format!("no voice {}", idx)))?;
        self.set_voice(&id).map(|_| ())
    }
    fn set_voice(&mut self, id: &str) -> Result<String> {
        let idx = MOCK_VOICES
            .iter()
            .position(|(voice_id, _)| *voice_id == id)
            .ok_or_else(|| tdsr::TdsrError::Speech(format!("no voice named {}", id)))?;
        self.send(SpeechCommand::SetVoiceIdx(idx))?;
        Ok(MOCK_VOICES[idx].1.to_string())
    }
    fn voice_count(&self) -> Option<usize> {
        Some(MOCK_VOICES.len())
    }
    fn voice_id(&self, idx: usize) -> Option<String> {
        MOCK_VOICES.get(idx).map(|(id, _)| id.to_string())
    }
    /// Old numbering: index 1 used to mean US English.
    fn legacy_voice_id(&self, idx: usize) -> Option<String> {
        (idx == 1).then(|| "gmw/en-US".to_string())
    }
    fn speak(&mut self, text: &str) -> Result<()> {
        self.send(SpeechCommand::Speak(text.to_string()))
    }
    fn letter(&mut self, text: &str) -> Result<()> {
        self.send(SpeechCommand::Speak(text.to_string()))
    }
    fn cancel(&mut self) -> Result<()> {
        self.send(SpeechCommand::Cancel)
    }
}

struct Harness {
    state: State,
    emulator: Emulator,
    handler: DefaultKeyHandler,
    spoken: Arc<Mutex<Vec<SpeechCommand>>>,
    _dir: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with_config("")
    }

    /// A harness whose config file starts with `contents`.
    fn with_config(contents: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tdsr.cfg");
        if !contents.is_empty() {
            std::fs::write(&path, contents).unwrap();
        }
        let config = Config::load_from(path).unwrap();
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let synth = Box::new(RecordingSynth(spoken.clone()));
        let state = State::from_parts(config, synth, 20, 5).unwrap();
        Self {
            state,
            emulator: Emulator::new(20, 5),
            handler: DefaultKeyHandler::new(create_default_keymap()),
            spoken,
            _dir: dir,
        }
    }

    /// Feed one stdin read's worth of bytes; returns the keys forwarded to the PTY.
    fn feed(&mut self, input: &[u8]) -> Vec<Vec<u8>> {
        let mut forwarded = Vec::new();
        for key in split_keys(input) {
            if dispatch_key(key, &mut self.state, &mut self.emulator, &mut self.handler).unwrap() {
                forwarded.push(key.to_vec());
            }
        }
        forwarded
    }

    fn spoken_text(&self) -> Vec<String> {
        self.spoken
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                SpeechCommand::Speak(s) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    fn commands(&self) -> Vec<SpeechCommand> {
        self.spoken.lock().unwrap().clone()
    }

    fn clear_spoken(&self) {
        self.spoken.lock().unwrap().clear();
    }
}

#[test]
fn config_menu_numeric_entry_sets_rate() {
    let mut h = Harness::new();

    // Alt+c opens the config menu, r asks for a rate, "50" Enter sets it.
    assert!(h.feed(b"\x1bc").is_empty());
    assert_eq!(h.state.handlers.len(), 1);
    assert!(h.feed(b"r").is_empty());
    assert_eq!(
        h.state.handlers.len(),
        2,
        "buffer handler stacked on config"
    );

    assert!(h.feed(b"5").is_empty());
    assert!(h.feed(b"0").is_empty());
    assert!(h.feed(b"\r").is_empty());

    assert!(
        h.commands()
            .iter()
            .any(|c| matches!(c, SpeechCommand::SetRate(50))),
        "rate 50 should reach the synth, got {:?}",
        h.commands()
    );
    assert_eq!(h.state.config.rate(), Some(50));
    assert_eq!(
        h.spoken_text().last().map(String::as_str),
        Some("confirmed")
    );
    assert_eq!(h.state.handlers.len(), 1, "back in the config menu");

    // Enter leaves the config menu; the next key goes to the shell.
    assert!(h.feed(b"\r").is_empty());
    assert_eq!(h.state.handlers.len(), 0);
    assert_eq!(h.feed(b"a"), vec![b"a".to_vec()]);
}

#[test]
fn config_menu_voice_entry_saves_the_id_and_rejects_bad_indices() {
    let mut h = Harness::new();

    // A voice the backend knows: its persistent id is saved and the name
    // announced.
    assert!(h.feed(b"\x1bcV1\r").is_empty());
    assert!(h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVoiceIdx(1))));
    assert_eq!(h.state.config.voice().as_deref(), Some("gmw/en-US"));
    assert_eq!(h.state.config.voice_idx(), None);
    assert_eq!(
        h.spoken_text().last().map(String::as_str),
        Some("confirmed, English (America)")
    );

    // An index past the list: refused, nothing saved or sent.
    h.clear_spoken();
    assert!(h.feed(b"V7\r").is_empty());
    assert!(!h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVoiceIdx(7))));
    assert_eq!(h.state.config.voice().as_deref(), Some("gmw/en-US"));
    assert_eq!(
        h.spoken_text().last().map(String::as_str),
        Some("no voice 7, the last voice is 1")
    );
    assert_eq!(h.state.handlers.len(), 1, "back in the config menu");
    assert!(h.feed(b"\r").is_empty());
}

#[test]
fn configured_voice_is_applied_and_a_legacy_index_is_migrated() {
    // `voice` wins and is applied by id.
    let h = Harness::with_config("[speech]\nvoice = gmw/af\n");
    assert!(h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVoiceIdx(0))));
    assert!(h.spoken_text().is_empty(), "{:?}", h.spoken_text());

    // A bare voice_idx from an older config is read with the old meaning,
    // rewritten as `voice`, and the change announced.
    let h = Harness::with_config("[speech]\nvoice_idx = 1\n");
    assert!(h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVoiceIdx(1))));
    assert_eq!(h.state.config.voice().as_deref(), Some("gmw/en-US"));
    assert_eq!(h.state.config.voice_idx(), None);
    assert_eq!(
        h.spoken_text().first().map(String::as_str),
        Some("voice setting updated to English (America)")
    );
    let saved = std::fs::read_to_string(h._dir.path().join("tdsr.cfg")).unwrap();
    assert!(
        saved.contains("voice=gmw/en-US") || saved.contains("voice = gmw/en-US"),
        "{}",
        saved
    );
    assert!(!saved.contains("voice_idx"), "{}", saved);

    // A voice that does not exist is announced and the default kept.
    let h = Harness::with_config("[speech]\nvoice = klingon\n");
    assert!(!h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVoiceIdx(_))));
    let first = h.spoken_text().first().cloned().unwrap_or_default();
    assert!(
        first.starts_with("Configured voice klingon not used"),
        "{}",
        first
    );
}

#[test]
fn config_menu_toggle_and_second_numeric_entry() {
    let mut h = Harness::new();
    h.feed(b"\x1bc");
    h.feed(b"e"); // toggle key echo (default true -> false)
    assert!(!h.state.config.key_echo());
    h.feed(b"v");
    h.feed(b"7");
    h.feed(b"\r");
    assert!(h
        .commands()
        .iter()
        .any(|c| matches!(c, SpeechCommand::SetVolume(7))));
    h.feed(b"\r");
    assert_eq!(h.state.handlers.len(), 0);
}

#[test]
fn coalesced_alt_keys_are_each_dispatched() {
    let mut h = Harness::new();
    h.clear_spoken();

    // Key auto-repeat delivered three Alt+o in one read: three "next line"
    // commands, nothing forwarded to the shell.
    let forwarded = h.feed(b"\x1bo\x1bo\x1bo");
    assert!(forwarded.is_empty(), "nothing should leak to the PTY");
    assert_eq!(h.state.review.pos.1, 3);
    assert_eq!(h.spoken_text().len(), 3);

    // A typed character immediately followed by Alt+u in the same read.
    let forwarded = h.feed(b"z\x1bu");
    assert_eq!(forwarded, vec![b"z".to_vec()]);
    assert_eq!(h.state.review.pos.1, 2);
}

#[test]
fn quiet_mode_still_speaks_review_commands() {
    let mut h = Harness::new();
    h.feed(b"\x1bq");
    assert!(h.state.quiet);
    assert_eq!(h.spoken_text().last().map(String::as_str), Some("quiet on"));

    h.clear_spoken();
    h.feed(b"\x1bi"); // current line
    assert_eq!(h.spoken_text(), vec!["blank".to_string()]);

    h.feed(b"\x1bq");
    assert!(!h.state.quiet);
    assert_eq!(
        h.spoken_text().last().map(String::as_str),
        Some("quiet off")
    );
}

#[test]
fn config_menu_escape_exits_and_unknown_keys_get_feedback() {
    let mut h = Harness::new();
    h.feed(b"\x1bc");
    h.clear_spoken();

    h.feed(b"z");
    let said = h.spoken_text().join(" ");
    assert!(said.contains("z is not a config key"), "{}", said);
    assert_eq!(h.state.handlers.len(), 1, "still in the menu");

    h.clear_spoken();
    h.feed(b"?");
    assert!(h.spoken_text()[0].starts_with("config keys:"));

    h.clear_spoken();
    h.feed(b"\x1b");
    assert_eq!(h.spoken_text(), vec!["exit".to_string()]);
    assert_eq!(h.state.handlers.len(), 0);
}

#[test]
fn numeric_entry_echoes_digits_rejects_others_and_cancels() {
    let mut h = Harness::new();
    h.feed(b"\x1bc");
    h.feed(b"r");
    h.clear_spoken();

    h.feed(b"4");
    h.feed(b"x");
    h.feed(b"2");
    h.feed(b"\x7f");
    assert_eq!(
        h.spoken_text(),
        vec!["4", "digits only", "2", "deleted 2"]
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    );

    // Escape cancels: nothing set, back in the config menu
    h.clear_spoken();
    h.feed(b"\x1b");
    assert_eq!(h.spoken_text(), vec!["cancelled".to_string()]);
    assert_eq!(h.state.config.rate(), None);
    assert_eq!(h.state.handlers.len(), 1);

    // Enter on an empty buffer also cancels rather than reporting "invalid"
    h.feed(b"v");
    h.clear_spoken();
    h.feed(b"\r");
    assert_eq!(h.spoken_text(), vec!["cancelled".to_string()]);
    assert_eq!(h.state.handlers.len(), 1);

    // Backspace on an empty buffer says so
    h.feed(b"d");
    h.clear_spoken();
    h.feed(b"\x7f");
    assert_eq!(h.spoken_text(), vec!["empty".to_string()]);
    h.feed(b"\x1b");
    h.feed(b"\r");
    assert_eq!(h.state.handlers.len(), 0);
}

#[test]
fn spell_word_is_one_utterance_with_symbol_names() {
    let mut h = Harness::new();
    // Put "a-b" on the screen at the review cursor
    h.emulator.process(b"a-b").unwrap();
    h.clear_spoken();
    h.feed(b"\x1bk");
    h.feed(b"\x1bk"); // double tap within the repeat window
    let said = h.spoken_text();
    assert_eq!(
        said.last().map(String::as_str),
        Some("a dash b"),
        "{:?}",
        said
    );
}

#[test]
fn speak_trims_and_skips_whitespace() {
    let mut h = Harness::new();
    h.clear_spoken();
    h.state.speak("   ").unwrap();
    h.state.speak("  hi  ").unwrap();
    assert_eq!(h.spoken_text(), vec!["hi".to_string()]);
}

#[test]
fn review_cursor_reads_lines_that_scrolled_off_the_top() {
    let mut h = Harness::new();
    // Eight lines on a five-row screen: l1..l4 scroll off, l5..l8 stay,
    // the cursor ends on the blank bottom row, where tracking puts the
    // review cursor.
    h.emulator
        .process(b"l1\r\nl2\r\nl3\r\nl4\r\nl5\r\nl6\r\nl7\r\nl8\r\n")
        .unwrap();
    h.emulator.screen_mut().take_scroll_offset();
    h.state
        .update_review_cursor_from_terminal(h.emulator.cursor());
    assert_eq!(h.state.review.pos, (0, 4));
    h.clear_spoken();

    // Alt+u walks up the screen, then keeps going into the history
    for _ in 0..8 {
        h.feed(b"\x1bu");
    }
    assert_eq!(
        h.spoken_text(),
        vec!["l8", "l7", "l6", "l5", "l4", "l3", "l2", "l1"]
    );
    assert_eq!(h.state.review.above, 4);
    h.clear_spoken();

    // Oldest line: "top", and it stays put
    h.feed(b"\x1bu");
    assert_eq!(h.spoken_text(), vec!["top", "l1"]);
    h.clear_spoken();

    // Word and character review work on a history line
    h.feed(b"\x1bk");
    h.feed(b"\x1b.");
    assert_eq!(h.spoken_text(), vec!["l1", "1"]);
    h.clear_spoken();

    // Alt+o comes back down; Alt+O jumps to the bottom of the screen
    h.feed(b"\x1bo");
    h.feed(b"\x1bO");
    assert_eq!(h.spoken_text(), vec!["l2", "blank"]);
    assert_eq!(h.state.review.above, 0);
    h.clear_spoken();

    // Alt+U: top of the screen first, the oldest history line on a repeat
    h.feed(b"\x1bU");
    h.feed(b"\x1bU");
    h.feed(b"\x1bU");
    assert_eq!(h.spoken_text(), vec!["l5", "l1", "l1"]);
    h.clear_spoken();

    // New output while reading the history: the cursor follows its line
    h.feed(b"\x1bo");
    h.feed(b"\x1bo");
    assert_eq!(h.spoken_text(), vec!["l2", "l3"]);
    h.clear_spoken();
    h.emulator.process(b"l9\r\nl10\r\n").unwrap();
    let scrolled = h.emulator.screen_mut().take_scroll_offset();
    let (rows, history) = (
        h.emulator.screen().size.1,
        h.emulator.screen().history_len(),
    );
    h.state
        .adjust_review_cursor_for_scroll(scrolled, rows, history);
    h.feed(b"\x1bi");
    assert_eq!(h.spoken_text(), vec!["l3"]);
    h.clear_spoken();

    // ...and so does a cursor on a screen row that scrolls off
    h.state.review.above = 0;
    h.state.review.pos = (0, 0);
    h.emulator.process(b"l11\r\n").unwrap();
    let scrolled = h.emulator.screen_mut().take_scroll_offset();
    let (rows, history) = (
        h.emulator.screen().size.1,
        h.emulator.screen().history_len(),
    );
    h.state
        .adjust_review_cursor_for_scroll(scrolled, rows, history);
    h.feed(b"\x1bi");
    assert_eq!(h.state.review.above, 1);
    assert_eq!(h.spoken_text(), vec!["l7"]);
}
