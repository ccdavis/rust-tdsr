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

/// A key handler processes keyboard input
pub trait KeyHandler {
    /// Process a key sequence (basic version)
    fn process(&mut self, key: &[u8]) -> Result<HandlerAction>;

    /// Process a key with access to state and emulator
    /// Modal handlers can override this for full access
    fn process_with_context(
        &mut self,
        key: &[u8],
        _state: &mut State,
        _emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        self.process(key)
    }

    /// Handle an unknown key (default: passthrough)
    fn handle_unknown(&mut self, _key: &[u8]) -> Result<HandlerAction> {
        Ok(HandlerAction::Passthrough)
    }
}

/// Stack of key handlers (last one processes input first)
pub struct HandlerStack {
    handlers: Vec<Box<dyn KeyHandler>>,
}

impl HandlerStack {
    /// Create a new handler stack
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }

    /// Push a handler onto the stack
    pub fn push(&mut self, handler: Box<dyn KeyHandler>) {
        self.handlers.push(handler);
    }

    /// Pop the top handler from the stack
    pub fn pop(&mut self) -> Option<Box<dyn KeyHandler>> {
        self.handlers.pop()
    }

    /// Process a key with the top handler (with state and emulator access)
    pub fn process_with_context(
        &mut self,
        key: &[u8],
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        if let Some(handler) = self.handlers.last_mut() {
            let action = handler.process_with_context(key, state, emulator)?;
            if action == HandlerAction::Remove {
                self.pop();
            }
            Ok(action)
        } else {
            Ok(HandlerAction::Passthrough)
        }
    }

    /// Process a key with the top handler (legacy method)
    pub fn process(&mut self, key: &[u8]) -> Result<HandlerAction> {
        if let Some(handler) = self.handlers.last_mut() {
            let action = handler.process(key)?;
            if action == HandlerAction::Remove {
                self.pop();
            }
            Ok(action)
        } else {
            Ok(HandlerAction::Passthrough)
        }
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

impl Default for HandlerStack {
    fn default() -> Self {
        Self::new()
    }
}
