//! Copy mode handler
//!
//! Modal handler for copying screen content (alt+v)
//! Allows copying the current line or entire screen.

use super::{HandlerAction, KeyHandler};
use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;
use log::debug;

/// Copy mode key handler
///
/// When user presses alt+v, this handler intercepts keys:
/// - l: copy current line (where review cursor is)
/// - s: copy entire screen
/// - Other: exit copy mode
pub struct CopyHandler;

impl Default for CopyHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl CopyHandler {
    /// Create a new copy handler
    pub fn new() -> Self {
        Self
    }

    /// Process copy mode keys
    pub fn process_with_state(
        &mut self,
        key: &[u8],
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        match key {
            // Copy line where review cursor is positioned
            b"l" => {
                debug!("Copy mode: copy line");
                let y = state.review.pos.1;
                let line = emulator.screen().get_line_trimmed(y);

                // Copy to clipboard
                if let Err(e) = crate::clipboard::copy_to_clipboard(&line) {
                    debug!("Failed to copy line: {}", e);
                    state.speak("failed")?;
                } else {
                    debug!("Copied line to clipboard: '{}'", line);
                    state.speak("line")?;
                }

                Ok(HandlerAction::Remove)
            }

            // Copy entire screen
            b"s" => {
                debug!("Copy mode: copy screen");
                let mut text = String::new();

                // Collect all lines from screen
                for y in 0..emulator.screen().size.1 {
                    let line = emulator.screen().get_line_trimmed(y);
                    if !line.is_empty() {
                        text.push_str(&line);
                        text.push('\n');
                    }
                }

                // Copy to clipboard
                if let Err(e) = crate::clipboard::copy_to_clipboard(&text) {
                    debug!("Failed to copy screen: {}", e);
                    state.speak("failed")?;
                } else {
                    debug!("Copied screen to clipboard: {} lines", emulator.screen().size.1);
                    state.speak("screen")?;
                }

                Ok(HandlerAction::Remove)
            }

            // Any other key - exit copy mode
            _ => {
                debug!("Copy mode: unknown key, exiting");
                state.speak("unknown key")?;
                Ok(HandlerAction::Remove)
            }
        }
    }
}

impl KeyHandler for CopyHandler {
    fn process(&mut self, _key: &[u8]) -> Result<HandlerAction> {
        // This shouldn't be called directly - use process_with_context instead
        Ok(HandlerAction::Handled)
    }

    fn process_with_context(&mut self, key: &[u8], state: &mut State, emulator: &mut Emulator) -> Result<HandlerAction> {
        self.process_with_state(key, state, emulator)
    }
}
