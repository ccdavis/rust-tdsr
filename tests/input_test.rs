//! Input system tests
//!
//! Tests key handler stack and key binding system

use tdsr::input::{create_default_keymap, HandlerAction, HandlerStack, KeyAction, KeyHandler};
use tdsr::Result;

struct TestHandler {
    handled: bool,
}

impl KeyHandler for TestHandler {
    fn process(&mut self, key: &[u8]) -> Result<HandlerAction> {
        if key == b"x" {
            self.handled = true;
            Ok(HandlerAction::Remove)
        } else {
            Ok(HandlerAction::Passthrough)
        }
    }
}

#[test]
fn test_handler_stack() {
    let mut stack = HandlerStack::new();
    assert_eq!(stack.len(), 0);

    // Push handler
    stack.push(Box::new(TestHandler { handled: false }));
    assert_eq!(stack.len(), 1);

    // Process key that handler doesn't recognize
    let action = stack.process(b"a").unwrap();
    assert_eq!(action, HandlerAction::Passthrough);
    assert_eq!(stack.len(), 1);

    // Process key that handler handles and removes itself
    let action = stack.process(b"x").unwrap();
    assert_eq!(action, HandlerAction::Remove);
    assert_eq!(stack.len(), 0);
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

#[test]
fn test_handler_stack_multiple() {
    let mut stack = HandlerStack::new();

    // Push two handlers
    stack.push(Box::new(TestHandler { handled: false }));
    stack.push(Box::new(TestHandler { handled: false }));
    assert_eq!(stack.len(), 2);

    // Top handler processes
    let action = stack.process(b"x").unwrap();
    assert_eq!(action, HandlerAction::Remove);
    assert_eq!(stack.len(), 1);

    // Now second handler processes
    let action = stack.process(b"x").unwrap();
    assert_eq!(action, HandlerAction::Remove);
    assert_eq!(stack.len(), 0);
}
