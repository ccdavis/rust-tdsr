//! Input system tests
//!
//! Tests the handler stack container and the default key bindings. Key
//! *dispatch* through the stack (with a real State) is covered by
//! `modal_input_test.rs`.

use tdsr::input::{create_default_keymap, HandlerAction, HandlerStack, KeyAction, KeyHandler};
use tdsr::state::State;
use tdsr::terminal::Emulator;
use tdsr::Result;

struct Tag;

impl KeyHandler for Tag {
    fn process_with_context(
        &mut self,
        _key: &[u8],
        _state: &mut State,
        _emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        Ok(HandlerAction::Handled)
    }
}

#[test]
fn test_handler_stack_push_pop_insert() {
    let mut stack = HandlerStack::new();
    assert!(stack.is_empty());

    stack.push(Box::new(Tag));
    stack.push(Box::new(Tag));
    assert_eq!(stack.len(), 2);

    // Popping returns the top; inserting at 0 puts a handler underneath.
    let top = stack.pop().unwrap();
    assert_eq!(stack.len(), 1);
    stack.insert(0, top);
    assert_eq!(stack.len(), 2);

    // An out-of-range index clamps to the top.
    stack.insert(99, Box::new(Tag));
    assert_eq!(stack.len(), 3);

    stack.pop();
    stack.pop();
    stack.pop();
    assert!(stack.pop().is_none());
}

/// Helper to look up a key sequence in the keymap without triggering clippy
fn lookup(keymap: &std::collections::HashMap<Vec<u8>, KeyAction>, key: &[u8]) -> Option<KeyAction> {
    keymap.get(key).cloned()
}

#[test]
fn test_keymap_creation() {
    let keymap = create_default_keymap();

    // Test line navigation keys
    assert_eq!(lookup(&keymap, b"\x1bu"), Some(KeyAction::PrevLine));
    assert_eq!(lookup(&keymap, b"\x1bi"), Some(KeyAction::CurrentLine));
    assert_eq!(lookup(&keymap, b"\x1bo"), Some(KeyAction::NextLine));

    // Test word navigation keys
    assert_eq!(lookup(&keymap, b"\x1bj"), Some(KeyAction::PrevWord));
    assert_eq!(lookup(&keymap, b"\x1bk"), Some(KeyAction::CurrentWord));
    assert_eq!(lookup(&keymap, b"\x1bl"), Some(KeyAction::NextWord));

    // Test char navigation keys
    assert_eq!(lookup(&keymap, b"\x1bm"), Some(KeyAction::PrevChar));
    assert_eq!(lookup(&keymap, b"\x1b,"), Some(KeyAction::CurrentChar));
    assert_eq!(lookup(&keymap, b"\x1b."), Some(KeyAction::NextChar));

    // Test mode keys
    assert_eq!(lookup(&keymap, b"\x1bc"), Some(KeyAction::Config));
    assert_eq!(lookup(&keymap, b"\x1bq"), Some(KeyAction::QuietMode));
    assert_eq!(lookup(&keymap, b"\x1bv"), Some(KeyAction::CopyMode));

    // Test arrow keys
    assert_eq!(lookup(&keymap, b"\x1b[A"), Some(KeyAction::ArrowUp));
    assert_eq!(lookup(&keymap, b"\x1b[B"), Some(KeyAction::ArrowDown));

    // Test double-tap keys
    assert_eq!(lookup(&keymap, b"\x1bk\x1bk"), Some(KeyAction::SpellWord));
    assert_eq!(
        lookup(&keymap, b"\x1b,\x1b,"),
        Some(KeyAction::SayCharPhonetic)
    );
}
