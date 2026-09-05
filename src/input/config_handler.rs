//! Configuration menu handler
//!
//! Modal handler for the screen reader's configuration menu (alt+c).
//! Allows user to change speech rate, volume, symbol processing, etc.

use super::{HandlerAction, KeyHandler};
use crate::state::State;
use crate::terminal::Emulator;
use crate::Result;
use log::debug;

/// Configuration menu key handler
///
/// When user presses alt+c, this handler intercepts all keys
/// to provide a modal configuration interface:
/// - r: set speech rate
/// - v: set volume
/// - V: set voice index
/// - p: toggle symbol processing
/// - d: set cursor delay
/// - e: toggle key echo
/// - c: toggle cursor tracking
/// - l: toggle line pause
/// - s: toggle repeated symbols
/// - t: cycle TUI mode (auto, apps, on, off)
/// - ?: list the keys
/// - Enter or Escape: leave the menu
///
/// Unknown keys are announced so a user who mistyped is not left in silence.
pub struct ConfigHandler;

/// Spoken on `?` and after an unknown key.
const CONFIG_HELP: &str = "config keys: r rate, v volume, capital V voice, d delay, \
p symbols, e character echo, c cursor tracking, l line pause, s repeated symbols, \
t TUI mode, enter or escape to exit";

/// An error's message without the "Speech synthesis error:" prefix, for
/// speaking.
fn spoken_error(e: &crate::TdsrError) -> String {
    match e {
        crate::TdsrError::Speech(msg) => msg.clone(),
        other => other.to_string(),
    }
}

impl Default for ConfigHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ConfigHandler {
    /// Create a new config handler
    pub fn new() -> Self {
        Self
    }

    /// Process config menu keys
    pub fn process_with_state(&mut self, key: &[u8], state: &mut State) -> Result<HandlerAction> {
        // Process config menu commands
        match key {
            // Rate setting
            b"r" => {
                debug!("Config: rate");
                state.speak("rate")?;
                // Push BufferHandler to collect numeric input
                state
                    .handlers
                    .push(Box::new(super::buffer_handler::BufferHandler::new(
                        Box::new(move |input: String, state: &mut State| {
                            Self::set_rate(input, state)
                        }),
                    )));
                Ok(HandlerAction::Handled)
            }

            // Volume setting
            b"v" => {
                debug!("Config: volume");
                state.speak("volume")?;
                state
                    .handlers
                    .push(Box::new(super::buffer_handler::BufferHandler::new(
                        Box::new(move |input: String, state: &mut State| {
                            Self::set_volume(input, state)
                        }),
                    )));
                Ok(HandlerAction::Handled)
            }

            // Voice index setting
            b"V" => {
                debug!("Config: voice index");
                state.speak("voice index")?;
                state
                    .handlers
                    .push(Box::new(super::buffer_handler::BufferHandler::new(
                        Box::new(move |input: String, state: &mut State| {
                            Self::set_voice_idx(input, state)
                        }),
                    )));
                Ok(HandlerAction::Handled)
            }

            // Toggle process symbols
            b"p" => {
                debug!("Config: toggle process symbols");
                let current = state.config.process_symbols();
                let new_value = !current;
                state
                    .config
                    .set("speech", "process_symbols", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value {
                    "process symbols on"
                } else {
                    "process symbols off"
                })?;
                Ok(HandlerAction::Handled)
            }

            // Set cursor delay
            b"d" => {
                debug!("Config: cursor delay");
                state.speak("cursor delay")?;
                state
                    .handlers
                    .push(Box::new(super::buffer_handler::BufferHandler::new(
                        Box::new(move |input: String, state: &mut State| {
                            Self::set_cursor_delay(input, state)
                        }),
                    )));
                Ok(HandlerAction::Handled)
            }

            // Toggle key echo
            b"e" => {
                debug!("Config: toggle key echo");
                let current = state.config.key_echo();
                let new_value = !current;
                state
                    .config
                    .set("speech", "key_echo", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value {
                    "character echo on"
                } else {
                    "character echo off"
                })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle cursor tracking
            b"c" => {
                debug!("Config: toggle cursor tracking");
                let current = state.config.cursor_tracking();
                let new_value = !current;
                state
                    .config
                    .set("speech", "cursor_tracking", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value {
                    "cursor tracking on"
                } else {
                    "cursor tracking off"
                })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle line pause
            b"l" => {
                debug!("Config: toggle line pause");
                let current = state.config.line_pause();
                let new_value = !current;
                state
                    .config
                    .set("speech", "line_pause", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value {
                    "line pause on"
                } else {
                    "line pause off"
                })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle repeated symbols
            b"s" => {
                debug!("Config: toggle repeated symbols");
                let current = state.config.repeated_symbols();
                let new_value = !current;
                state
                    .config
                    .set("speech", "repeated_symbols", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value {
                    "repeated symbols on"
                } else {
                    "repeated symbols off"
                })?;
                Ok(HandlerAction::Handled)
            }

            // Help
            b"?" => {
                state.speak(CONFIG_HELP)?;
                Ok(HandlerAction::Handled)
            }

            // Enter or Escape - exit config menu
            b"\r" | b"\n" | b"\x1b" => {
                debug!("Config: exit");
                state.speak("exit")?;
                Ok(HandlerAction::Remove)
            }

            // Unknown key in config menu: say so, and stay in the menu
            _ => {
                debug!("Config: unknown key {:?}", key);
                let name = describe_key(key, state);
                state.speak(&format!(
                    "{} is not a config key, press question mark for help",
                    name
                ))?;
                Ok(HandlerAction::Handled)
            }
        }
    }

    /// Set speech rate from user input
    fn set_rate(input: String, state: &mut State) -> Result<()> {
        match input.parse::<u8>() {
            Ok(rate) if rate <= 100 => {
                debug!("Setting rate to {}", rate);
                state.config.set("speech", "rate", &rate.to_string());
                state.save_config()?;
                state.synth.set_rate(rate)?;
                state.speak("confirmed")?;
            }
            _ => {
                debug!("Invalid rate value: {}", input);
                state.speak("invalid")?;
            }
        }
        Ok(())
    }

    /// Set speech volume from user input
    fn set_volume(input: String, state: &mut State) -> Result<()> {
        match input.parse::<u8>() {
            Ok(volume) if volume <= 100 => {
                debug!("Setting volume to {}", volume);
                state.config.set("speech", "volume", &volume.to_string());
                state.save_config()?;
                state.synth.set_volume(volume)?;
                state.speak("confirmed")?;
            }
            _ => {
                debug!("Invalid volume value: {}", input);
                state.speak("invalid")?;
            }
        }
        Ok(())
    }

    /// Set voice from a menu index. A backend that knows its voices maps
    /// the index to a persistent id, which is saved as `voice` only if the
    /// backend accepts it (a rejection is spoken in the backend's words).
    /// A backend that only has indices cannot judge them, so the index is
    /// saved as `voice_idx` whether or not the send succeeded (it may have
    /// failed only because the speech server is down right now).
    fn set_voice_idx(input: String, state: &mut State) -> Result<()> {
        let idx = match input.parse::<usize>() {
            Ok(idx) => idx,
            Err(_) => {
                debug!("Invalid voice index value: {}", input);
                return state.speak("invalid");
            }
        };
        debug!("Setting voice index to {}", idx);
        match (state.synth.voice_count(), state.synth.voice_id(idx)) {
            (Some(_), Some(id)) => match state.synth.set_voice(&id) {
                Ok(name) => {
                    state.config.set("speech", "voice", &id);
                    state.config.remove("speech", "voice_idx");
                    state.save_config()?;
                    state.speak(&format!("confirmed, {}", name))?;
                }
                Err(e) => {
                    debug!("Voice {} (index {}) rejected: {}", id, idx, e);
                    state.speak(&spoken_error(&e))?;
                }
            },
            (Some(0), None) => state.speak("this speech backend lists no voices")?,
            (Some(n), None) => {
                state.speak(&format!("no voice {}, the last voice is {}", idx, n - 1))?
            }
            (None, _) => {
                let result = state.synth.set_voice_idx(idx);
                state.config.set("speech", "voice_idx", &idx.to_string());
                state.config.remove("speech", "voice");
                state.save_config()?;
                match result {
                    Ok(()) => state.speak("confirmed")?,
                    Err(e) => {
                        debug!("Voice index {} not applied: {}", idx, e);
                        state.speak(&spoken_error(&e))?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Set cursor delay from user input (in milliseconds)
    fn set_cursor_delay(input: String, state: &mut State) -> Result<()> {
        match input.parse::<u32>() {
            Ok(ms) => {
                debug!("Setting cursor delay to {} milliseconds", ms);
                // Store as milliseconds directly - getter converts to seconds
                state.config.set("speech", "cursor_delay", &ms.to_string());
                state.save_config()?;
                state.speak("confirmed")?;
            }
            Err(_) => {
                debug!("Invalid cursor delay value: {}", input);
                state.speak("invalid")?;
            }
        }
        Ok(())
    }
}

/// Human-readable name for a key sequence, for error feedback.
fn describe_key(key: &[u8], state: &State) -> String {
    match key {
        [b] if b.is_ascii_graphic() => state
            .config
            .symbols
            .get(&(*b as u32))
            .cloned()
            .unwrap_or_else(|| (*b as char).to_string()),
        [b' '] => "space".to_string(),
        [b'\x7f'] | [b'\x08'] => "backspace".to_string(),
        [b'\t'] => "tab".to_string(),
        [0x1b, b] if b.is_ascii_graphic() => format!("alt {}", *b as char),
        [b] if b.is_ascii_control() => format!("control {}", (b + b'@') as char),
        _ => "that key".to_string(),
    }
}

impl KeyHandler for ConfigHandler {
    fn process_with_context(
        &mut self,
        key: &[u8],
        state: &mut State,
        emulator: &mut Emulator,
    ) -> Result<HandlerAction> {
        // TUI mode needs the screen (switching it on takes a baseline)
        if key == b"t" {
            debug!("Config: TUI mode");
            state.cycle_tui_mode(emulator.screen())?;
            return Ok(HandlerAction::Handled);
        }
        self.process_with_state(key, state)
    }
}
