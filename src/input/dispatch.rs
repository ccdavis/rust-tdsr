//! Route one key sequence to the active handler.
//!
//! Lives in the library (not `main.rs`) so the modal-handler stack behaviour
//! can be tested without a terminal.

use super::{DefaultKeyHandler, HandlerAction};
use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;

/// Dispatch a single key sequence.
///
/// If a modal handler (config menu, copy mode, numeric entry) is active it
/// gets the key; otherwise the default screen-reader bindings do. Returns
/// `true` when the key was not consumed and should be forwarded to the PTY.
pub fn dispatch_key(
    key: &[u8],
    state: &mut State,
    emulator: &mut Emulator,
    default_handler: &mut DefaultKeyHandler,
) -> Result<bool> {
    // The handler needs `&mut State` while it is itself owned by `state`, so
    // take it off the stack for the duration of the call.
    if let Some(mut handler) = state.handlers.pop() {
        // Anything the handler pushes during the call (e.g. the config menu
        // pushing a numeric-entry handler) must end up *above* it, so remember
        // where it came from and reinsert it there rather than at the top.
        let depth = state.handlers.len();
        let action = match handler.process_with_context(key, state, emulator) {
            Ok(action) => action,
            Err(e) => {
                // Keep the modal handler active on a transient error (e.g. a
                // speech backend hiccup) instead of silently dropping the user
                // out of the mode.
                state.handlers.insert(depth, handler);
                return Err(e);
            }
        };
        return Ok(match action {
            HandlerAction::Passthrough => {
                state.handlers.insert(depth, handler);
                true
            }
            HandlerAction::Handled => {
                state.handlers.insert(depth, handler);
                false
            }
            HandlerAction::Remove => false,
        });
    }

    let action = default_handler.process_key(key, state, emulator)?;
    Ok(action == HandlerAction::Passthrough)
}
