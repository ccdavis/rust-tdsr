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

#[test]
fn test_keymap_creation() {
    let keymap = create_default_keymap();

    // Test line navigation keys
    assert_eq!(keymap.get(&b"\x1bu".to_vec()), Some(&KeyAction::PrevLine));
    assert_eq!(keymap.get(&b"\x1bi".to_vec()), Some(&KeyAction::CurrentLine));
    assert_eq!(keymap.get(&b"\x1bo".to_vec()), Some(&KeyAction::NextLine));

    // Test word navigation keys
    assert_eq!(keymap.get(&b"\x1bj".to_vec()), Some(&KeyAction::PrevWord));
    assert_eq!(keymap.get(&b"\x1bk".to_vec()), Some(&KeyAction::CurrentWord));
    assert_eq!(keymap.get(&b"\x1bl".to_vec()), Some(&KeyAction::NextWord));

    // Test char navigation keys
    assert_eq!(keymap.get(&b"\x1bm".to_vec()), Some(&KeyAction::PrevChar));
    assert_eq!(keymap.get(&b"\x1b,".to_vec()), Some(&KeyAction::CurrentChar));
    assert_eq!(keymap.get(&b"\x1b.".to_vec()), Some(&KeyAction::NextChar));

    // Test mode keys
    assert_eq!(keymap.get(&b"\x1bc".to_vec()), Some(&KeyAction::Config));
    assert_eq!(keymap.get(&b"\x1bq".to_vec()), Some(&KeyAction::QuietMode));
    assert_eq!(keymap.get(&b"\x1bv".to_vec()), Some(&KeyAction::CopyMode));

    // Test arrow keys
    assert_eq!(keymap.get(&b"\x1b[A".to_vec()), Some(&KeyAction::ArrowUp));
    assert_eq!(keymap.get(&b"\x1b[B".to_vec()), Some(&KeyAction::ArrowDown));

    // Test double-tap keys
    assert_eq!(keymap.get(&b"\x1bk\x1bk".to_vec()), Some(&KeyAction::SpellWord));
    assert_eq!(keymap.get(&b"\x1b,\x1b,".to_vec()), Some(&KeyAction::SayCharPhonetic));
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
