//! Key handler system with modal input support

use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;

/// Action to take after processing a key
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerAction {
    /// Pass the key through to the PTY
    Passthrough,
    /// Remove this handler from the stack
    Remove,
    /// Key was handled, do nothing more
    Handled,
}

/// A modal key handler (config menu, numeric entry, copy mode).
///
/// Handlers live on the [`HandlerStack`]; the top one receives every key
/// until it returns [`HandlerAction::Remove`]. Dispatch (popping the handler
/// for the duration of the call and putting it back) is done by
/// `input::dispatch_key`, not by the stack itself.
pub trait KeyHandler {
    /// Process one key sequence with access to state and the emulator
    fn process_with_context(
        &mut self,
        key: &[u8],
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction>;
}

/// Stack of key handlers (last one processes input first)
#[derive(Default)]
pub struct HandlerStack {
    handlers: Vec<Box<dyn KeyHandler>>,
}

impl HandlerStack {
    /// Create a new handler stack
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a handler onto the stack
    pub fn push(&mut self, handler: Box<dyn KeyHandler>) {
        self.handlers.push(handler);
    }

    /// Pop the top handler from the stack
    pub fn pop(&mut self) -> Option<Box<dyn KeyHandler>> {
        self.handlers.pop()
    }

    /// Insert a handler at `index` (0 = bottom). Used to put a temporarily
    /// popped handler back beneath anything it pushed while it was running.
    pub fn insert(&mut self, index: usize, handler: Box<dyn KeyHandler>) {
        let index = index.min(self.handlers.len());
        self.handlers.insert(index, handler);
    }

    /// Get the number of handlers in the stack
    pub fn len(&self) -> usize {
        self.handlers.len()
    }

    /// Check if the stack is empty
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }
}
