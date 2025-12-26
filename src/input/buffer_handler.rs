//! Buffer handler for collecting text input
//!
//! Used when screen reader needs to collect user input
//! (e.g., entering a rate value in config menu)

use super::{HandlerAction, KeyHandler};
use crate::terminal::Emulator;
use crate::state::State;
use crate::Result;
use log::debug;

/// Callback function type for when input is complete
type OnAcceptFn = Box<dyn FnOnce(String, &mut State) -> Result<()> + Send>;

/// Handler that collects text input until Enter is pressed
///
/// Used for numeric input in config menu and other text entry scenarios.
/// When user presses Enter, calls the provided callback with the collected text.
pub struct BufferHandler {
    /// Accumulated input buffer
    buffer: String,

    /// Callback to execute when Enter is pressed
    on_accept: Option<OnAcceptFn>,
}

impl BufferHandler {
    /// Create a new buffer handler
    ///
    /// The callback will be invoked with the collected text when user presses Enter
    pub fn new(on_accept: OnAcceptFn) -> Self {
        Self {
            buffer: String::new(),
            on_accept: Some(on_accept),
        }
    }

    /// Process input with state access
    pub fn process_with_state(&mut self, key: &[u8], state: &mut State) -> Result<HandlerAction> {
        match key {
            // Enter - accept input and invoke callback
            b"\r" | b"\n" => {
                debug!("BufferHandler: accepting input '{}'", self.buffer);

                if let Some(callback) = self.on_accept.take() {
                    callback(self.buffer.clone(), state)?;
                }

                // Remove this handler from stack
                Ok(HandlerAction::Remove)
            }

            // Backspace - remove last character
            b"\x08" | b"\x7f" => {
                if !self.buffer.is_empty() {
                    self.buffer.pop();
                    debug!("BufferHandler: backspace, buffer now '{}'", self.buffer);
                    // TODO: Phase 5 - Echo the backspace
                }
                Ok(HandlerAction::Handled)
            }

            // Regular character - add to buffer
            _ => {
                if let Ok(s) = std::str::from_utf8(key) {
                    self.buffer.push_str(s);
                    debug!("BufferHandler: added '{}', buffer now '{}'", s, self.buffer);
                    // TODO: Phase 5 - Echo the character if key_echo is on
                }
                Ok(HandlerAction::Handled)
            }
        }
    }
}

impl KeyHandler for BufferHandler {
    fn process(&mut self, _key: &[u8]) -> Result<HandlerAction> {
        // This shouldn't be called directly - use process_with_state instead
        Ok(HandlerAction::Handled)
    }

    fn process_with_context(&mut self, key: &[u8], state: &mut State, _emulator: &mut Emulator) -> Result<HandlerAction> {
        self.process_with_state(key, state)
    }
}
