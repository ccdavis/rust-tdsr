//! Default key bindings for TDSR

use std::collections::HashMap;

/// Key sequence type
pub type KeySequence = Vec<u8>;

/// Action identifier for key bindings
///
/// Each variant represents a screen reader command that can be triggered by a key
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    // Line navigation
    PrevLine,
    CurrentLine,
    NextLine,

    // Word navigation
    PrevWord,
    CurrentWord,
    NextWord,
    SpellWord,

    // Character navigation
    PrevChar,
    CurrentChar,
    NextChar,
    SayCharPhonetic,

    // Screen navigation
    TopOfScreen,
    BottomOfScreen,
    StartOfLine,
    EndOfLine,

    // Arrow keys with delay
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,

    // Special keys
    Backspace,
    Delete,

    // Modes
    Config,
    QuietMode,
    SelectionStart,
    CopyMode,
    Silence,
}

/// Create the default keymap
pub fn create_default_keymap() -> HashMap<KeySequence, KeyAction> {
    let mut map = HashMap::new();

    // Line navigation (alt+u/i/o)
    map.insert(b"\x1bu".to_vec(), KeyAction::PrevLine);
    map.insert(b"\x1bi".to_vec(), KeyAction::CurrentLine);
    map.insert(b"\x1bo".to_vec(), KeyAction::NextLine);

    // Word navigation (alt+j/k/l)
    map.insert(b"\x1bj".to_vec(), KeyAction::PrevWord);
    map.insert(b"\x1bk".to_vec(), KeyAction::CurrentWord);
    map.insert(b"\x1bl".to_vec(), KeyAction::NextWord);

    // Character navigation (alt+m/comma/dot)
    map.insert(b"\x1bm".to_vec(), KeyAction::PrevChar);
    map.insert(b"\x1b,".to_vec(), KeyAction::CurrentChar);
    map.insert(b"\x1b.".to_vec(), KeyAction::NextChar);

    // Screen edges (alt+U/O/M/>)
    map.insert(b"\x1bU".to_vec(), KeyAction::TopOfScreen);
    map.insert(b"\x1bO".to_vec(), KeyAction::BottomOfScreen);
    map.insert(b"\x1bM".to_vec(), KeyAction::StartOfLine);
    map.insert(b"\x1b>".to_vec(), KeyAction::EndOfLine);
    map.insert(b"\x1b:".to_vec(), KeyAction::EndOfLine); // Hungarian keyboard

    // Arrow keys
    map.insert(b"\x1b[A".to_vec(), KeyAction::ArrowUp);
    map.insert(b"\x1b[B".to_vec(), KeyAction::ArrowDown);
    map.insert(b"\x1b[C".to_vec(), KeyAction::ArrowRight);
    map.insert(b"\x1b[D".to_vec(), KeyAction::ArrowLeft);
    map.insert(b"\x1bOA".to_vec(), KeyAction::ArrowUp);
    map.insert(b"\x1bOB".to_vec(), KeyAction::ArrowDown);
    map.insert(b"\x1bOC".to_vec(), KeyAction::ArrowRight);
    map.insert(b"\x1bOD".to_vec(), KeyAction::ArrowLeft);

    // Special keys
    map.insert(b"\x08".to_vec(), KeyAction::Backspace);
    map.insert(b"\x7f".to_vec(), KeyAction::Backspace);
    map.insert(b"\x1b[3~".to_vec(), KeyAction::Delete);

    // Modes
    map.insert(b"\x1bc".to_vec(), KeyAction::Config);
    map.insert(b"\x1bq".to_vec(), KeyAction::QuietMode);
    map.insert(b"\x1br".to_vec(), KeyAction::SelectionStart);
    map.insert(b"\x1bv".to_vec(), KeyAction::CopyMode);
    map.insert(b"\x1bx".to_vec(), KeyAction::Silence);

    // Double-tap keys (press twice within timeout)
    // alt+k twice = spell word
    map.insert(b"\x1bk\x1bk".to_vec(), KeyAction::SpellWord);
    // alt+comma twice = phonetic character
    map.insert(b"\x1b,\x1b,".to_vec(), KeyAction::SayCharPhonetic);

    map
}
