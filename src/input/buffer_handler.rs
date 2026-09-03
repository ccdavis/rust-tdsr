//! Buffer handler for collecting numeric input
//!
//! Used when the screen reader needs a number from the user (rate, volume,
//! voice index, cursor delay in the config menu).

use super::{HandlerAction, KeyHandler};
use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;
use log::debug;

/// Callback function type for when input is complete
type OnAcceptFn = Box<dyn FnOnce(String, &mut State) -> Result<()> + Send>;

/// Handler that collects digits until Enter is pressed
///
/// Every accepted digit is spoken, backspace speaks the digit it removed,
/// non-digits are rejected with a spoken message, and Escape (or Enter on an
/// empty buffer) cancels without calling the callback.
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
            // Enter - accept input and invoke callback (empty input cancels)
            b"\r" | b"\n" => {
                if self.buffer.is_empty() {
                    debug!("BufferHandler: empty input, cancelled");
                    state.speak("cancelled")?;
                    return Ok(HandlerAction::Remove);
                }
                debug!("BufferHandler: accepting input '{}'", self.buffer);
                if let Some(callback) = self.on_accept.take() {
                    callback(std::mem::take(&mut self.buffer), state)?;
                }
                Ok(HandlerAction::Remove)
            }

            // Escape - cancel
            b"\x1b" => {
                debug!("BufferHandler: cancelled");
                state.speak("cancelled")?;
                Ok(HandlerAction::Remove)
            }

            // Backspace - remove last digit and say which one
            b"\x08" | b"\x7f" => {
                match self.buffer.pop() {
                    Some(removed) => {
                        debug!("BufferHandler: backspace, buffer now '{}'", self.buffer);
                        state.speak(&format!("deleted {}", removed))?;
                    }
                    None => state.speak("empty")?,
                }
                Ok(HandlerAction::Handled)
            }

            // A digit - add to buffer and echo it
            [d] if d.is_ascii_digit() => {
                self.buffer.push(*d as char);
                debug!("BufferHandler: buffer now '{}'", self.buffer);
                state.speak_char(*d as char)?;
                Ok(HandlerAction::Handled)
            }

            // Anything else is rejected, with feedback
            _ => {
                debug!("BufferHandler: rejected {:?}", key);
                state.speak("digits only")?;
                Ok(HandlerAction::Handled)
            }
        }
    }
}

impl KeyHandler for BufferHandler {
    fn process_with_context(
        &mut self,
        key: &[u8],
        state: &mut State,
        _emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        self.process_with_state(key, state)
    }
}
