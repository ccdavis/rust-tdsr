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
    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        self.send(SpeechCommand::SetVoiceIdx(idx))
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
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load_from(dir.path().join("tdsr.cfg")).unwrap();
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
