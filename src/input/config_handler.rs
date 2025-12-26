//! Configuration menu handler
//!
//! Modal handler for the screen reader's configuration menu (alt+c).
//! Allows user to change speech rate, volume, symbol processing, etc.

use super::{HandlerAction, KeyHandler};
use crate::terminal::Emulator;
use crate::state::State;
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
/// - Enter: exit and save config
pub struct ConfigHandler;

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
                state.handlers.push(Box::new(
                    super::buffer_handler::BufferHandler::new(Box::new(
                        move |input: String, state: &mut State| {
                            Self::set_rate(input, state)
                        },
                    )),
                ));
                Ok(HandlerAction::Handled)
            }

            // Volume setting
            b"v" => {
                debug!("Config: volume");
                state.speak("volume")?;
                state.handlers.push(Box::new(
                    super::buffer_handler::BufferHandler::new(Box::new(
                        move |input: String, state: &mut State| {
                            Self::set_volume(input, state)
                        },
                    )),
                ));
                Ok(HandlerAction::Handled)
            }

            // Voice index setting
            b"V" => {
                debug!("Config: voice index");
                state.speak("voice")?;
                state.handlers.push(Box::new(
                    super::buffer_handler::BufferHandler::new(Box::new(
                        move |input: String, state: &mut State| {
                            Self::set_voice_idx(input, state)
                        },
                    )),
                ));
                Ok(HandlerAction::Handled)
            }

            // Toggle process symbols
            b"p" => {
                debug!("Config: toggle process symbols");
                let current = state.config.process_symbols();
                let new_value = !current;
                state.config.set("speech", "process_symbols", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value { "process symbols on" } else { "process symbols off" })?;
                Ok(HandlerAction::Handled)
            }

            // Set cursor delay
            b"d" => {
                debug!("Config: cursor delay");
                state.speak("delay")?;
                state.handlers.push(Box::new(
                    super::buffer_handler::BufferHandler::new(Box::new(
                        move |input: String, state: &mut State| {
                            Self::set_cursor_delay(input, state)
                        },
                    )),
                ));
                Ok(HandlerAction::Handled)
            }

            // Toggle key echo
            b"e" => {
                debug!("Config: toggle key echo");
                let current = state.config.key_echo();
                let new_value = !current;
                state.config.set("speech", "key_echo", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value { "key echo on" } else { "key echo off" })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle cursor tracking
            b"c" => {
                debug!("Config: toggle cursor tracking");
                let current = state.config.cursor_tracking();
                let new_value = !current;
                state.config.set("speech", "cursor_tracking", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value { "cursor tracking on" } else { "cursor tracking off" })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle line pause
            b"l" => {
                debug!("Config: toggle line pause");
                let current = state.config.line_pause();
                let new_value = !current;
                state.config.set("speech", "line_pause", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value { "line pause on" } else { "line pause off" })?;
                Ok(HandlerAction::Handled)
            }

            // Toggle repeated symbols
            b"s" => {
                debug!("Config: toggle repeated symbols");
                let current = state.config.repeated_symbols();
                let new_value = !current;
                state.config.set("speech", "repeated_symbols", &new_value.to_string());
                state.save_config()?;
                state.speak(if new_value { "repeated symbols on" } else { "repeated symbols off" })?;
                Ok(HandlerAction::Handled)
            }

            // Enter - exit config menu
            b"\r" | b"\n" => {
                debug!("Config: exit");
                Ok(HandlerAction::Remove)
            }

            // Unknown key in config menu
            _ => {
                debug!("Config: unknown key");
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

    /// Set voice index from user input
    fn set_voice_idx(input: String, state: &mut State) -> Result<()> {
        match input.parse::<usize>() {
            Ok(idx) => {
                debug!("Setting voice index to {}", idx);
                state.config.set("speech", "voice_idx", &idx.to_string());
                state.save_config()?;
                state.synth.set_voice_idx(idx)?;
                state.speak("confirmed")?;
            }
            Err(_) => {
                debug!("Invalid voice index value: {}", input);
                state.speak("invalid")?;
            }
        }
        Ok(())
    }

    /// Set cursor delay from user input (in milliseconds)
    fn set_cursor_delay(input: String, state: &mut State) -> Result<()> {
        match input.parse::<f32>() {
            Ok(ms) if ms >= 0.0 => {
                let seconds = ms / 1000.0;
                debug!("Setting cursor delay to {} seconds", seconds);
                state.config.set("speech", "cursor_delay", &seconds.to_string());
                state.save_config()?;
                state.speak("confirmed")?;
            }
            _ => {
                debug!("Invalid cursor delay value: {}", input);
                state.speak("invalid")?;
            }
        }
        Ok(())
    }
}

impl KeyHandler for ConfigHandler {
    fn process(&mut self, _key: &[u8]) -> Result<HandlerAction> {
        // This shouldn't be called directly - use process_with_state instead
        Ok(HandlerAction::Handled)
    }

    fn process_with_context(&mut self, key: &[u8], state: &mut State, _emulator: &mut Emulator) -> Result<HandlerAction> {
        self.process_with_state(key, state)
    }
}
