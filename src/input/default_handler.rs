//! Default key handler for the screen reader
//!
//! Processes Alt+key combinations for screen reader navigation commands
//! and passes unrecognized keys through to the shell.

use super::{HandlerAction, KeyAction, KeyHandler};
use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;
use log::{debug, trace};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Default key handler for screen reader commands
///
/// This is the base handler that processes all screen reader key bindings.
/// Alt+key combinations trigger navigation (alt+u = previous line, etc.)
/// while regular keys pass through to the shell.
pub struct DefaultKeyHandler {
    /// Key bindings map
    keymap: HashMap<Vec<u8>, KeyAction>,

    /// Last key pressed (for detecting double-tap)
    last_key: Option<Vec<u8>>,

    /// Time of last keypress
    last_key_time: Instant,

    /// Timeout for detecting double-tap (500ms)
    repeat_timeout: Duration,
}

impl DefaultKeyHandler {
    /// Create a new default key handler
    ///
    /// Initializes the keymap with all screen reader navigation commands
    pub fn new(keymap: HashMap<Vec<u8>, KeyAction>) -> Self {
        debug!(
            "Creating default key handler with {} bindings",
            keymap.len()
        );
        Self {
            keymap,
            last_key: None,
            last_key_time: Instant::now(),
            repeat_timeout: Duration::from_millis(500),
        }
    }

    /// Check if this is a double-tap of the same key
    ///
    /// Some commands require double-tap (e.g., alt+k twice to spell word)
    fn is_repeat(&self, key: &[u8]) -> bool {
        if let Some(ref last) = self.last_key {
            if last == key {
                let elapsed = self.last_key_time.elapsed();
                return elapsed < self.repeat_timeout;
            }
        }
        false
    }

    /// Process a key with the screen reader's key bindings
    ///
    /// Checks for double-tap variants first (e.g., alt+k+k for spell),
    /// then single key bindings, then passes through to shell.
    pub fn process_key(
        &mut self,
        key: &[u8],
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        let now = Instant::now();

        // Check for double-tap binding (e.g., alt+k twice)
        if self.is_repeat(key) {
            let mut double_key = key.to_vec();
            double_key.extend_from_slice(key);

            if let Some(action) = self.keymap.get(&double_key).cloned() {
                debug!("Double-tap key detected: {:?}", action);
                self.last_key = Some(key.to_vec());
                self.last_key_time = now;
                return self.execute_action(&action, state, emulator);
            }
        }

        // Check for single key binding
        if let Some(action) = self.keymap.get(key).cloned() {
            trace!("Key action: {:?}", action);
            self.last_key = Some(key.to_vec());
            self.last_key_time = now;
            return self.execute_action(&action, state, emulator);
        }

        // No binding found - check if it's a plugin key
        self.last_key = Some(key.to_vec());
        self.last_key_time = now;

        // Convert key bytes to string for plugin lookup
        if let Ok(key_str) = String::from_utf8(key.to_vec()) {
            if state.has_plugin(&key_str) {
                debug!("Executing plugin for key: {}", key_str);
                let screen = emulator.screen();
                state.execute_plugin(&key_str, screen)?;
                return Ok(HandlerAction::Handled);
            }
        }

        Ok(HandlerAction::Passthrough)
    }

    /// Execute a screen reader action
    ///
    /// Each action performs navigation or mode switching.
    fn execute_action(
        &mut self,
        action: &KeyAction,
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        use KeyAction::*;

        match action {
            // Config mode - push ConfigHandler onto stack
            Config => {
                debug!("Entering config mode");
                state.speak("config")?;
                state
                    .handlers
                    .push(Box::new(super::config_handler::ConfigHandler::new()));
                Ok(HandlerAction::Handled)
            }

            // Quiet mode toggle - suppress automatic speech
            QuietMode => {
                let quiet = state.toggle_quiet();
                debug!("Quiet mode: {}", quiet);
                let msg = if quiet { "quiet on" } else { "quiet off" };
                state.speak(msg)?;
                Ok(HandlerAction::Handled)
            }

            // Copy mode - push CopyHandler onto stack
            CopyMode => {
                debug!("Entering copy mode");
                state.speak("copy")?;
                state
                    .handlers
                    .push(Box::new(super::copy_handler::CopyHandler::new()));
                Ok(HandlerAction::Handled)
            }

            // Selection start/end - mark position for copying
            SelectionStart => {
                if state.has_selection() {
                    debug!("Ending selection and copying");
                    let screen = emulator.screen();
                    state.copy_selection(screen)?;
                } else {
                    debug!("Starting selection");
                    state.start_selection();
                    state.speak("select")?;
                }
                Ok(HandlerAction::Handled)
            }

            // Silence - cancel any pending speech
            Silence => {
                debug!("Silence requested");
                state.clear_speech_buffer();
                state.cancel_speech()?;
                Ok(HandlerAction::Handled)
            }

            // Navigation commands - review cursor movement and speech
            PrevLine => {
                debug!("Previous line");
                let screen = emulator.screen();
                state.prev_line(screen)?;
                Ok(HandlerAction::Handled)
            }
            CurrentLine => {
                debug!("Current line");
                let screen = emulator.screen();
                state.current_line(screen)?;
                Ok(HandlerAction::Handled)
            }
            NextLine => {
                debug!("Next line");
                let screen = emulator.screen();
                state.next_line(screen)?;
                Ok(HandlerAction::Handled)
            }
            PrevWord => {
                debug!("Previous word");
                let screen = emulator.screen();
                state.prev_word(screen)?;
                Ok(HandlerAction::Handled)
            }
            CurrentWord => {
                debug!("Current word");
                let screen = emulator.screen();
                state.say_word(screen, false)?;
                Ok(HandlerAction::Handled)
            }
            SpellWord => {
                debug!("Spell word");
                let screen = emulator.screen();
                state.say_word(screen, true)?;
                Ok(HandlerAction::Handled)
            }
            NextWord => {
                debug!("Next word");
                let screen = emulator.screen();
                state.next_word(screen)?;
                Ok(HandlerAction::Handled)
            }
            PrevChar => {
                debug!("Previous character");
                let screen = emulator.screen();
                state.prev_char(screen)?;
                Ok(HandlerAction::Handled)
            }
            CurrentChar => {
                debug!("Current character");
                let screen = emulator.screen();
                state.current_char(screen, false)?;
                Ok(HandlerAction::Handled)
            }
            SayCharPhonetic => {
                debug!("Say character phonetically");
                let screen = emulator.screen();
                state.current_char(screen, true)?;
                Ok(HandlerAction::Handled)
            }
            NextChar => {
                debug!("Next character");
                let screen = emulator.screen();
                state.next_char(screen)?;
                Ok(HandlerAction::Handled)
            }
            TopOfScreen => {
                debug!("Top of screen");
                let screen = emulator.screen();
                state.top_of_screen(screen)?;
                Ok(HandlerAction::Handled)
            }
            BottomOfScreen => {
                debug!("Bottom of screen");
                let screen = emulator.screen();
                state.bottom_of_screen(screen)?;
                Ok(HandlerAction::Handled)
            }
            StartOfLine => {
                debug!("Start of line");
                let screen = emulator.screen();
                state.start_of_line(screen)?;
                Ok(HandlerAction::Handled)
            }
            EndOfLine => {
                debug!("End of line");
                let screen = emulator.screen();
                state.end_of_line(screen)?;
                Ok(HandlerAction::Handled)
            }

            // Arrow keys - pass through but schedule delayed speech
            ArrowUp => {
                debug!("Arrow up");
                if state.config.cursor_tracking() {
                    let delay = Duration::from_secs_f32(state.config.cursor_delay());
                    state.schedule(
                        delay,
                        |state, screen| {
                            let cursor = state.review.pos;
                            state.say_line(screen, cursor.1)
                        },
                        true,
                    );
                }
                Ok(HandlerAction::Passthrough)
            }

            ArrowDown => {
                debug!("Arrow down");
                if state.config.cursor_tracking() {
                    let delay = Duration::from_secs_f32(state.config.cursor_delay());
                    state.schedule(
                        delay,
                        |state, screen| {
                            let cursor = state.review.pos;
                            state.say_line(screen, cursor.1)
                        },
                        true,
                    );
                }
                Ok(HandlerAction::Passthrough)
            }

            ArrowLeft => {
                debug!("Arrow left");
                if state.config.cursor_tracking() {
                    let delay = Duration::from_secs_f32(state.config.cursor_delay());
                    state.schedule(
                        delay,
                        |state, screen| {
                            let cursor = state.review.pos;
                            state.say_char(screen, cursor.1, cursor.0, false)
                        },
                        true,
                    );
                }
                Ok(HandlerAction::Passthrough)
            }

            ArrowRight => {
                debug!("Arrow right");
                if state.config.cursor_tracking() {
                    let delay = Duration::from_secs_f32(state.config.cursor_delay());
                    state.schedule(
                        delay,
                        |state, screen| {
                            let cursor = state.review.pos;
                            state.say_char(screen, cursor.1, cursor.0, false)
                        },
                        true,
                    );
                }
                Ok(HandlerAction::Passthrough)
            }

            // Backspace/Delete - speak character being deleted
            Backspace => {
                debug!("Backspace");
                let screen = emulator.screen();
                let cursor = emulator.cursor();

                // Speak the character to the left of cursor (what will be deleted)
                if cursor.0 > 0 && screen.get_char(cursor.0 - 1, cursor.1).is_some() {
                    state.say_char(screen, cursor.1, cursor.0 - 1, false)?;
                }
                Ok(HandlerAction::Passthrough)
            }

            Delete => {
                debug!("Delete");
                let screen = emulator.screen();
                let cursor = emulator.cursor();

                // Speak the character at cursor (what will be deleted)
                if screen.get_char(cursor.0, cursor.1).is_some() {
                    state.say_char(screen, cursor.1, cursor.0, false)?;
                }
                Ok(HandlerAction::Passthrough)
            }
        }
    }
}

impl KeyHandler for DefaultKeyHandler {
    fn process(&mut self, _key: &[u8]) -> Result<HandlerAction> {
        // This shouldn't be called directly - use process_key instead
        // which needs state and emulator access
        trace!("DefaultKeyHandler::process called (passthrough)");
        Ok(HandlerAction::Passthrough)
    }
}
