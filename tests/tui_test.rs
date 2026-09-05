//! TUI mode against real programs: the byte streams in `tests/fixtures/`
//! were recorded from the Free Pascal IDE, whiptail, ls and less with
//! `tools/capture_tui.py` (one file per keystroke). Each step feeds the
//! key through the dispatcher and the recorded output through the
//! emulator the way `main` does, then runs the screen comparison, and
//! checks what was spoken.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tdsr::input::{create_default_keymap, dispatch_key, DefaultKeyHandler};
use tdsr::speech::{SpeechCommand, Synth};
use tdsr::state::config::Config;
use tdsr::state::State;
use tdsr::terminal::Emulator;
use tdsr::tui::{Foreground, KeyKind, TuiMode};
use tdsr::Result;

const COLS: u16 = 80;
const ROWS: u16 = 25;

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
    fn set_voice(&mut self, id: &str) -> Result<String> {
        Ok(id.to_string())
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
    foreground: Foreground,
    _dir: tempfile::TempDir,
}

impl Harness {
    /// A harness with the given config file contents, and a foreground
    /// process of that name (not the shell)
    fn new(config: &str, program: &str) -> Self {
        // RUST_LOG=trace shows every candidate span's score
        let _ = env_logger::builder().is_test(true).try_init();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tdsr.cfg");
        std::fs::write(&path, config).unwrap();
        let config = Config::load_from(path).unwrap();
        let spoken = Arc::new(Mutex::new(Vec::new()));
        let synth = Box::new(RecordingSynth(spoken.clone()));
        let state = State::from_parts(config, synth, COLS, ROWS).unwrap();
        Self {
            state,
            emulator: Emulator::new(COLS, ROWS),
            handler: DefaultKeyHandler::new(create_default_keymap()),
            spoken,
            foreground: Foreground {
                is_shell: Some(false),
                name: Some(program.to_string()),
            },
            _dir: dir,
        }
    }

    fn shell_is_foreground(&mut self) {
        self.foreground = Foreground {
            is_shell: Some(true),
            name: Some("bash".to_string()),
        };
    }

    /// One keystroke and the output it produced (as `main` handles them),
    /// with the screen comparison run at once instead of after the settle
    /// delay. Returns what was spoken.
    fn step(&mut self, key: Option<&[u8]>, output: &[u8]) -> Vec<String> {
        self.spoken.lock().unwrap().clear();
        if let Some(key) = key {
            self.state.cancel_speech().unwrap();
            self.state.clear_delayed_functions();
            self.state.last_key_kind = Some(KeyKind::of(key));
            let passthrough =
                dispatch_key(key, &mut self.state, &mut self.emulator, &mut self.handler).unwrap();
            if passthrough {
                self.state.last_key = match key {
                    [c] if c.is_ascii_graphic() || *c == b' ' => Some(*c as char),
                    _ => None,
                };
            }
        }
        let old_cursor = self.emulator.cursor();
        let line_pause = self.state.config.line_pause();
        let echoed = self
            .emulator
            .process_with_speech(
                output,
                &mut self.state.speech_buffer,
                &mut self.state.last_drawn,
                line_pause,
                &mut self.state.last_key,
            )
            .unwrap();
        if let Some(ch) = echoed {
            if self.state.config.key_echo() {
                self.state.speak_char(ch).unwrap();
            }
        }
        let foreground = self.foreground.clone();
        self.state
            .after_output(
                self.emulator.screen(),
                old_cursor,
                echoed,
                Some(&foreground),
            )
            .unwrap();
        let scroll = self.emulator.screen_mut().take_scroll_offset();
        if scroll != 0 {
            let screen = self.emulator.screen();
            self.state
                .adjust_review_cursor_for_scroll(scroll, screen.size.1, screen.history_len());
        }
        let new_cursor = self.emulator.cursor();
        if old_cursor != new_cursor
            && !(self.state.tui.active() && !self.emulator.screen().cursor_visible)
        {
            self.state.update_review_cursor_from_terminal(new_cursor);
        }
        // What the event loop would do once the timers fire
        self.state.flush_tui(self.emulator.screen()).unwrap();
        self.state.flush_speech().unwrap();
        self.spoken_text()
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
}

/// The recorded steps of one program: (name, keys sent, output)
fn fixture(program: &str) -> Vec<(String, Option<Vec<u8>>, Vec<u8>)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(program);
    let index = std::fs::read_to_string(dir.join("steps.txt")).unwrap();
    let mut steps = Vec::new();
    for line in index
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let (file, keys) = line.split_once('\t').unwrap();
        let output = std::fs::read(dir.join(file)).unwrap();
        let keys = parse_keys(keys);
        steps.push((file.trim_end_matches(".bin").to_string(), keys, output));
    }
    steps
}

/// The Python bytes literal `steps.txt` records (`b'\x1b[B'`, `None`, or a
/// list of chunks whose bytes are concatenated)
fn parse_keys(literal: &str) -> Option<Vec<u8>> {
    if literal == "None" {
        return None;
    }
    let mut out = Vec::new();
    let mut rest = literal;
    while let Some(start) = rest.find("b'") {
        let body = &rest[start + 2..];
        let end = body.find('\'').unwrap();
        let mut chars = body[..end].chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next().unwrap() {
                    'x' => {
                        let hex: String = chars.by_ref().take(2).collect();
                        out.push(u8::from_str_radix(&hex, 16).unwrap());
                    }
                    'r' => out.push(b'\r'),
                    'n' => out.push(b'\n'),
                    't' => out.push(b'\t'),
                    '\\' => out.push(b'\\'),
                    '\'' => out.push(b'\''),
                    other => panic!("unexpected escape {}", other),
                }
            } else {
                out.push(c as u8);
            }
        }
        rest = &body[end + 1..];
    }
    Some(out)
}

/// Run every step of a fixture and collect what each one spoke
fn run(h: &mut Harness, program: &str) -> Vec<(String, Vec<String>)> {
    fixture(program)
        .into_iter()
        .map(|(name, keys, output)| {
            let spoken = h.step(keys.as_deref(), &output);
            eprintln!("{}: {:?}", name, spoken);
            (name, spoken)
        })
        .collect()
}

fn spoken_at<'a>(log: &'a [(String, Vec<String>)], step: &str) -> &'a [String] {
    &log.iter().find(|(name, _)| name == step).unwrap().1
}

const DEFAULT_CONFIG: &str = "[speech]\nline_pause = false\n";

#[test]
fn free_pascal_ide_menus_dialogs_and_editor() {
    let mut h = Harness::new(DEFAULT_CONFIG, "fp");
    let log = run(&mut h, "fp");

    assert_eq!(spoken_at(&log, "00_start"), ["TUI mode on"]);
    assert_eq!(spoken_at(&log, "01_f10_menu"), ["File"]);
    assert_eq!(spoken_at(&log, "02_right_edit"), ["Edit"]);
    assert_eq!(
        spoken_at(&log, "03_down_open_edit_menu"),
        ["Show clipboard"]
    );
    assert!(spoken_at(&log, "04_down_no_move").is_empty());
    assert!(spoken_at(&log, "05_escape_close_menu").is_empty());
    assert_eq!(spoken_at(&log, "06_alt_f_file_menu"), ["File", "New"]);
    assert_eq!(
        spoken_at(&log, "07_down_new_from_template"),
        ["New from template..."]
    );
    assert_eq!(spoken_at(&log, "08_up_new"), ["New"]);
    assert_eq!(spoken_at(&log, "09_enter_new_editor"), ["noname01.pas"]);
    assert_eq!(spoken_at(&log, "10_type_a"), ["a"]);
    assert_eq!(spoken_at(&log, "11_type_b"), ["b"]);
    assert_eq!(spoken_at(&log, "12_type_c"), ["c"]);
    assert_eq!(spoken_at(&log, "13_left"), ["c"]);
    assert_eq!(spoken_at(&log, "14_left_again"), ["b"]);
    assert_eq!(spoken_at(&log, "15_enter_split_line"), ["bc"]);
    assert_eq!(spoken_at(&log, "16_up_line"), ["a"]);
    assert_eq!(spoken_at(&log, "17_down_line"), ["bc"]);
    assert_eq!(spoken_at(&log, "18_f10_menu_again"), ["File"]);
    assert_eq!(spoken_at(&log, "19_left_help"), ["Help"]);
    assert_eq!(spoken_at(&log, "20_left_window"), ["Window"]);
    assert_eq!(spoken_at(&log, "21_left_options"), ["Options"]);
    assert_eq!(
        spoken_at(&log, "22_down_open_options_menu"),
        ["Mode... Normal"]
    );
    assert_eq!(
        spoken_at(&log, "23_enter_mode_dialog"),
        [
            "SwitchesMode",
            "Switches Mode",
            "(*) Normal",
            "( ) Debug",
            "( ) Release",
            "OK Cancel",
            "(*) Normal"
        ]
    );
    assert_eq!(spoken_at(&log, "24_tab_ok"), ["OK"]);
    assert_eq!(spoken_at(&log, "25_tab_cancel"), ["Cancel"]);
    assert_eq!(spoken_at(&log, "26_tab_back_to_radio"), ["(*) Normal"]);
    assert_eq!(spoken_at(&log, "27_down_debug_radio"), ["(*) Debug"]);
    assert!(spoken_at(&log, "28_escape_close_dialog").is_empty());
    assert_eq!(spoken_at(&log, "29_alt_f_file_menu_again"), ["File", "New"]);
    assert_eq!(spoken_at(&log, "30_up_wrap_to_exit"), ["Exit Alt+X"]);
    assert_eq!(
        spoken_at(&log, "31_enter_exit_prompt"),
        ["Information", "Save untitled file?", "Yes No Cancel", "Yes"]
    );
    assert!(spoken_at(&log, "32_n_dont_save").contains(&"TUI mode off".to_string()));
    assert!(!h.state.tui.active());
}

#[test]
fn whiptail_menu_is_detected_from_evidence() {
    // Not in tui_apps: the alternate screen, hidden cursor and colours decide
    let mut h = Harness::new(DEFAULT_CONFIG, "whiptail");
    let log = run(&mut h, "whiptail");
    assert_eq!(
        spoken_at(&log, "00_start"),
        [
            "TUI mode on",
            "Pick one",
            "Choose a thing to do",
            "open Open a file",
            "save Save the file",
            "quit Leave now",
            "<Ok> <Cancel>",
            "open Open a file"
        ]
    );
    assert_eq!(spoken_at(&log, "01_down_save"), ["save Save the file"]);
    assert_eq!(spoken_at(&log, "02_down_quit"), ["quit Leave now"]);
    assert_eq!(spoken_at(&log, "03_tab_ok"), ["<Ok>"]);
    assert_eq!(spoken_at(&log, "04_tab_cancel"), ["<Cancel>"]);
    assert!(spoken_at(&log, "05_enter").contains(&"TUI mode off".to_string()));
}

#[test]
fn coloured_listing_and_pager_stay_in_ordinary_mode() {
    let mut h = Harness::new(DEFAULT_CONFIG, "ls");
    h.shell_is_foreground();
    let log = run(&mut h, "ls");
    let spoken = spoken_at(&log, "00_start");
    assert!(!h.state.tui.active());
    assert!(!spoken.iter().any(|s| s.contains("TUI mode")));
    assert!(spoken.iter().any(|s| s.contains("passwd")), "{:?}", spoken);

    let mut h = Harness::new(DEFAULT_CONFIG, "less");
    let log = run(&mut h, "less");
    assert!(!h.state.tui.active());
    for (_, spoken) in &log {
        assert!(!spoken.iter().any(|s| s.contains("TUI mode")));
    }
    // The pager's text is still read the ordinary way
    assert!(spoken_at(&log, "00_start")
        .iter()
        .any(|s| s.contains("tcpmux")));
}

#[test]
fn manual_mode_and_keys() {
    let mut h = Harness::new("[speech]\ntui_mode = off\nline_pause = false\n", "fp");
    let steps = fixture("fp");
    let (_, _, start) = &steps[0];
    let spoken = h.step(None, start);
    assert!(!h.state.tui.active());
    assert!(!spoken.iter().any(|s| s.contains("TUI mode")));
    // Alt+t: off -> auto (detection then kicks in on the next output)
    // (fp is in the foreground and in tui_apps, so it switches on at once)
    let spoken = h.step(Some(b"\x1bt"), b"");
    assert_eq!(spoken, ["TUI mode auto", "TUI mode on"]);
    assert_eq!(h.state.config.tui_mode(), TuiMode::Auto);
    let (_, keys, f10) = &steps[1];
    let spoken = h.step(keys.as_deref(), f10);
    assert_eq!(spoken, ["File"]);
    // Alt+h repeats the highlight, Alt+w reads the screen (no frame yet)
    assert_eq!(h.step(Some(b"\x1bh"), b""), ["File"]);
    let window = h.step(Some(b"\x1bw"), b"");
    assert!(
        window.iter().any(|s| s.starts_with("File Edit Search")),
        "{:?}",
        window
    );
    // Alt+t: apps (fp is listed, so it comes straight back on), on, off
    assert_eq!(
        h.step(Some(b"\x1bt"), b""),
        ["TUI mode apps", "TUI mode on"]
    );
    assert_eq!(h.step(Some(b"\x1bt"), b""), ["TUI mode on"]);
    assert_eq!(h.step(Some(b"\x1bt"), b""), ["TUI mode off"]);
    assert!(!h.state.tui.active());
    assert_eq!(h.step(Some(b"\x1bh"), b""), ["no highlight"]);
}

#[test]
fn midnight_commander_panels_menus_and_viewer() {
    let mut h = Harness::new(DEFAULT_CONFIG, "mc");
    let log = run(&mut h, "mc");
    // The active panel is read once; the selected row is the focus
    let start = spoken_at(&log, "00_start");
    assert_eq!(start[0], "TUI mode on");
    assert!(
        start.contains(&"notes.txt 43 Sep 5 16:20".to_string()),
        "{:?}",
        start
    );
    assert_eq!(start.last().unwrap(), "/.. UP--DIR Sep 5 16:20");
    assert_eq!(spoken_at(&log, "01_down"), ["/.cache 4096 Sep 5 16:20"]);
    assert_eq!(
        spoken_at(&log, "02_down_again"),
        ["/.config 4096 Sep 5 16:20"]
    );
    // Tab: the other panel's selected row, not its title
    assert_eq!(
        spoken_at(&log, "03_tab_other_panel"),
        ["/.. UP--DIR Sep 5 16:20"]
    );
    assert_eq!(
        spoken_at(&log, "04_down_other_panel"),
        ["/.cache 4096 Sep 5 16:20"]
    );
    assert_eq!(
        spoken_at(&log, "05_tab_back"),
        ["/.config 4096 Sep 5 16:20"]
    );
    // Menu bar and a drop-down taller than the screen
    assert_eq!(spoken_at(&log, "06_f9_menu"), ["Left"]);
    assert_eq!(spoken_at(&log, "07_right_menu"), ["File"]);
    assert_eq!(spoken_at(&log, "08_down_open_menu"), ["View F3"]);
    assert_eq!(spoken_at(&log, "09_down_next_item"), ["View file..."]);
    // Closing it uncovers the panels: only the selected row is spoken
    assert_eq!(
        spoken_at(&log, "10_escape_close_menu"),
        ["/.config 4096 Sep 5 16:20"]
    );
    assert_eq!(
        spoken_at(&log, "11_end_last_file"),
        ["notes.txt 43 Sep 5 16:20"]
    );
    // The viewer is a full page of text
    let view = spoken_at(&log, "12_f3_view");
    for line in ["first line of notes", "second line", "third line"] {
        assert!(view.contains(&line.to_string()), "{:?}", view);
    }
    assert_eq!(
        spoken_at(&log, "13_f3_close_view"),
        ["notes.txt 43 Sep 5 16:20"]
    );
    assert_eq!(spoken_at(&log, "14_f10_quit"), ["TUI mode off"]);
    assert!(!h.state.tui.active());
}

#[test]
fn nano_stays_in_ordinary_mode() {
    let mut h = Harness::new(DEFAULT_CONFIG, "nano");
    let log = run(&mut h, "nano");
    assert!(!h.state.tui.active());
    for (step, spoken) in &log {
        assert!(
            !spoken.iter().any(|s| s.contains("TUI mode")),
            "{}: {:?}",
            step,
            spoken
        );
    }
    // Ordinary reading: echo, help text, prompts
    assert!(spoken_at(&log, "01_type_h").iter().any(|s| s.contains('h')));
    assert!(spoken_at(&log, "07_ctrl_g_help")
        .iter()
        .any(|s| s.contains("nano editor")));
    assert!(spoken_at(&log, "09_ctrl_x_exit")
        .iter()
        .any(|s| s.contains("Save modified buffer?")));
}
