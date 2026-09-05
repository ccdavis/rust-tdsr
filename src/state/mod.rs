//! Application state management
//!
//! The State struct is the central data structure for the screen reader,
//! holding configuration, review cursor position, speech buffer, and UI state.

pub mod config;
pub mod phonetics;

use crate::input::HandlerStack;
use crate::plugins::PluginManager;
use crate::review::ReviewCursor;
use crate::speech::{SpeechBuffer, Synth};
use crate::terminal::Screen;
use crate::{Result, TdsrError};
use config::Config;
use log::{info, warn};
use phonetics::PHONETICS;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

/// Type for delayed functions (used for cursor tracking)
/// Stores a function to call and when it should be called
type DelayedFunction = (
    Instant,
    Box<dyn FnOnce(&mut State, &Screen) -> Result<()> + Send>,
);

/// Main application state for the screen reader
///
/// This is the central state that persists across the event loop,
/// tracking everything the screen reader needs to provide speech feedback.
pub struct State {
    /// Configuration loaded from ~/.tdsr.cfg
    pub config: Config,

    /// Review cursor for navigating screen content
    /// User can move this independently of the terminal cursor
    /// to read any part of the screen
    pub review: ReviewCursor,

    /// Speech synthesizer for text-to-speech output
    /// This is how the screen reader speaks to the user
    pub synth: Box<dyn Synth>,

    /// Last position where text was drawn to screen
    /// Used to track what's new for automatic speech
    pub last_drawn: (u16, u16),

    /// Quiet mode - when true, suppress automatic speech
    /// User toggles this with alt+q to silence all output
    pub quiet: bool,

    /// Temporary silence during cursor tracking delay
    /// Prevents speaking while waiting for cursor to settle
    pub temp_silence: bool,

    /// Speech buffer accumulating text to be spoken
    /// Text is added as it's drawn, then flushed to TTS
    pub speech_buffer: SpeechBuffer,

    /// Key handler stack for modal input
    /// Allows config menu, copy mode, etc. to intercept keys
    pub handlers: HandlerStack,

    /// Copy/selection start position if user is selecting text
    /// Used with alt+r to mark selection start
    pub copy_start: Option<(u16, u16)>,

    /// Flag indicating delayed speech is pending
    /// Used for cursor tracking - speech happens after a short delay
    pub delaying_output: bool,

    /// Last command executed (for plugin filtering)
    /// Some plugins only trigger after specific commands
    pub last_command: String,

    /// Input line accumulator for tracking what the user types
    /// When Enter is pressed, this becomes last_command
    pub input_line: String,

    /// Last key typed by user (for key echo)
    /// When terminal echoes this character back and key_echo is enabled,
    /// we speak the character
    pub last_key: Option<char>,

    /// Plugin manager for executing external plugins
    /// Allows custom output parsing and speech generation
    pub plugin_manager: Option<PluginManager>,

    /// Delayed functions for cursor tracking
    /// Functions scheduled to run after a delay (e.g., speak character after arrow key)
    delayed_functions: Vec<DelayedFunction>,

    /// Results from plugins running on worker threads; drained by
    /// `poll_plugin_results` from the event loop.
    plugin_results: Receiver<Result<Vec<String>>>,
    plugin_sender: Sender<Result<Vec<String>>>,

    /// Number of plugins currently running
    plugins_running: usize,
}

impl State {
    /// Create a new application state with given terminal dimensions
    ///
    /// Loads configuration from disk and initializes all screen reader state.
    ///
    /// `speech_command` overrides the config's `speech_command` (from the
    /// `--speech-command` command-line option).
    pub fn new(cols: u16, rows: u16, speech_command: Option<String>) -> Result<Self> {
        info!("Initializing state with {}x{} terminal", cols, rows);

        let config = Config::load()?;
        info!("Configuration loaded from {:?}", config.path());
        info!("  Symbols: {}", config.symbols.len());
        info!("  Plugins: {}", config.plugins.len());
        info!("  Process symbols: {}", config.process_symbols());
        info!("  Key echo: {}", config.key_echo());
        info!("  Cursor tracking: {}", config.cursor_tracking());

        // Create speech synthesizer
        let speech_command = speech_command.or_else(|| config.speech_command());
        let synth = crate::speech::create_synth(speech_command.as_deref())?;
        info!("Speech synthesizer created");

        Self::from_parts(config, synth, cols, rows)
    }

    /// Build state from an already-loaded config and synthesizer.
    ///
    /// `new()` uses this with the platform synth; tests use it with a mock
    /// synth and a config loaded from a temporary path.
    pub fn from_parts(
        config: Config,
        mut synth: Box<dyn Synth>,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        // Apply config settings to synth
        if let Some(rate) = config.rate() {
            synth.set_rate(rate)?;
            info!("Speech rate set to {}", rate);
        }
        if let Some(volume) = config.volume() {
            synth.set_volume(volume)?;
            info!("Speech volume set to {}", volume);
        }
        let mut config = config;
        let startup_warning = Self::apply_configured_voice(&mut config, synth.as_mut());

        // Initialize plugin manager if plugins are configured
        let plugin_manager = if !config.plugins.is_empty() {
            let plugin_dir = dirs::home_dir()
                .ok_or("Could not find home directory")?
                .join(".tdsr")
                .join("plugins");

            let prompt_pattern = config.prompt_pattern();

            match PluginManager::new(
                config.plugins.clone(),
                config.plugin_commands.clone(),
                plugin_dir,
                &prompt_pattern,
            ) {
                Ok(pm) => {
                    info!(
                        "Plugin manager initialized with {} plugins",
                        config.plugins.len()
                    );
                    Some(pm)
                }
                Err(e) => {
                    info!("Failed to initialize plugin manager: {}", e);
                    None
                }
            }
        } else {
            info!("No plugins configured");
            None
        };

        let (tx, rx) = mpsc::channel();
        let mut state = Self {
            config,
            review: ReviewCursor::new(cols, rows),
            synth,
            last_drawn: (0, 0),
            quiet: false,
            temp_silence: false,
            speech_buffer: SpeechBuffer::new(),
            handlers: HandlerStack::new(),
            copy_start: None,
            delaying_output: false,
            last_command: String::new(),
            input_line: String::new(),
            last_key: None,
            plugin_manager,
            delayed_functions: Vec::new(),
            plugin_results: rx,
            plugin_sender: tx,
            plugins_running: 0,
        };
        if let Some(msg) = startup_warning {
            state.speak(&msg)?;
        }
        Ok(state)
    }

    /// Apply the configured voice. `voice` (a persistent id) wins; a bare
    /// `voice_idx` is either a legacy index on a backend whose numbering
    /// changed, which is migrated to `voice`, or the only handle an
    /// index-only backend has. A voice that cannot be used is not fatal:
    /// the default voice is kept and the reason returned to be spoken.
    fn apply_configured_voice(config: &mut Config, synth: &mut dyn Synth) -> Option<String> {
        let reason = |e: &TdsrError| match e {
            TdsrError::Speech(msg) => msg.clone(),
            other => other.to_string(),
        };
        if let Some(id) = config.voice() {
            return match synth.set_voice(&id) {
                Ok(name) => {
                    info!("Speech voice set to {} ({})", id, name);
                    None
                }
                Err(e) => {
                    warn!("voice {} from config not applied: {}", id, e);
                    Some(format!("Configured voice {} not used: {}", id, reason(&e)))
                }
            };
        }
        let idx = config.voice_idx()?;
        match synth.legacy_voice_id(idx) {
            Some(id) => match synth.set_voice(&id) {
                Ok(name) => {
                    info!("voice_idx {} migrated to voice {} ({})", idx, id, name);
                    config.set("speech", "voice", &id);
                    config.remove("speech", "voice_idx");
                    if let Err(e) = config.save() {
                        warn!("Could not save migrated voice setting: {}", e);
                    }
                    Some(format!("voice setting updated to {}", name))
                }
                Err(e) => {
                    warn!("legacy voice_idx {} ({}) not applied: {}", idx, id, e);
                    Some(format!("Configured voice {} not used: {}", idx, reason(&e)))
                }
            },
            None => match synth.set_voice_idx(idx) {
                Ok(()) => {
                    info!("Speech voice index set to {}", idx);
                    None
                }
                Err(e) => {
                    warn!("voice_idx {} from config not applied: {}", idx, e);
                    Some(format!("Configured voice {} not used: {}", idx, reason(&e)))
                }
            },
        }
    }

    /// Save configuration to disk
    ///
    /// Called when user changes settings in config menu
    pub fn save_config(&self) -> Result<()> {
        self.config.save()
    }

    /// Update terminal dimensions
    ///
    /// Called when terminal is resized (SIGWINCH)
    /// Updates review cursor bounds to match new size
    pub fn resize(&mut self, cols: u16, rows: u16) {
        info!("State resize to {}x{}", cols, rows);
        self.review.resize(cols, rows);
    }

    /// Toggle quiet mode
    ///
    /// When quiet, terminal output is not read automatically; explicit
    /// navigation commands and announcements still speak.
    pub fn toggle_quiet(&mut self) -> bool {
        self.quiet = !self.quiet;
        self.quiet
    }

    /// Clear speech buffer
    ///
    /// Discards any pending speech output
    pub fn clear_speech_buffer(&mut self) {
        self.speech_buffer.flush();
    }

    /// Start text selection
    ///
    /// Marks current review cursor position as selection start
    pub fn start_selection(&mut self) {
        self.copy_start = Some(self.review.pos);
    }

    /// End text selection and copy to clipboard
    ///
    /// Copies the selected region from copy_start to current review cursor position
    pub fn copy_selection(&mut self, screen: &Screen) -> Result<()> {
        if let Some((start_x, start_y)) = self.copy_start {
            let (end_x, end_y) = self.review.pos;

            // Copy text from selection
            let text = screen.text_range((start_x, start_y), (end_x, end_y));

            // Copy to clipboard
            crate::clipboard::copy_to_clipboard(&text)?;

            // Clear selection
            self.copy_start = None;

            self.speak("copied")?;
        }
        Ok(())
    }

    /// End text selection without copying
    pub fn end_selection(&mut self) {
        self.copy_start = None;
    }

    /// Check if selection is active
    pub fn has_selection(&self) -> bool {
        self.copy_start.is_some()
    }

    /// Speak text to the user
    ///
    /// Central method for all screen reader speech output
    /// Processes symbols if enabled (e.g., "!" becomes "bang")
    ///
    /// Quiet mode is deliberately *not* checked here: it only suppresses
    /// automatic reading of terminal output (see `handle_pty_output`), while
    /// explicit review commands and announcements must always be heard.
    ///
    /// Speech backend failures are logged, not returned: a synth that has
    /// died must never take the screen reader session down with it.
    pub fn speak(&mut self, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let processed = self.process_symbols_in_text(text);
        self.synth_op("speak", |s| s.speak(&processed))
    }

    /// Speak automatically-read terminal output: like `speak`, but with
    /// repeated-symbol condensing applied when that option is on.
    pub fn speak_output(&mut self, text: &str) -> Result<()> {
        let condensed = self.replace_duplicate_characters(text);
        self.speak(&condensed)
    }

    /// Run a synth command, turning failures into a log line.
    fn synth_op(
        &mut self,
        what: &str,
        op: impl FnOnce(&mut dyn Synth) -> Result<()>,
    ) -> Result<()> {
        if let Err(e) = op(self.synth.as_mut()) {
            warn!("Speech {} failed: {}", what, e);
        }
        Ok(())
    }

    /// Flush the speech buffer to the synthesizer (pending lines first).
    /// Called from the scheduled flush after terminal output settles.
    pub fn flush_speech(&mut self) -> Result<()> {
        for line in self.speech_buffer.drain_lines() {
            self.speak_output(&line)?;
        }
        if !self.speech_buffer.is_empty() {
            let text = self.speech_buffer.flush();
            self.speak_output(&text)?;
        }
        Ok(())
    }

    /// Speak the buffered output shortly, once no more output arrives.
    ///
    /// Output often comes in several writes a few milliseconds apart (a
    /// prompt, then a command's echo, then its output). Deferring the flush
    /// by a few milliseconds turns those into one utterance instead of a
    /// stutter of fragments, as the original TDSR did.
    pub fn schedule_speech_flush(&mut self) {
        if self.delaying_output {
            return;
        }
        self.delaying_output = true;
        self.schedule(
            Duration::from_millis(5),
            |state, _screen| {
                state.delaying_output = false;
                state.flush_speech()
            },
            false,
        );
    }

    /// Speak a single character (for key echo)
    ///
    /// Uses the TTS "letter" mode if available, or falls back to
    /// speaking the character's name for special characters.
    pub fn speak_char(&mut self, ch: char) -> Result<()> {
        // For special characters, use their symbol name
        let text = match self.config.symbols.get(&(ch as u32)) {
            Some(name) => name.clone(),
            None => ch.to_string(),
        };
        self.synth_op("letter", |s| s.letter(&text))
    }

    /// Process symbols in text if enabled
    ///
    /// Converts special characters to their word equivalents
    /// (e.g., "!" → "bang", "$" → "dollar")
    ///
    /// Uses pre-compiled regex from Config for efficiency in the hot path.
    fn process_symbols_in_text(&self, text: &str) -> String {
        if !self.config.process_symbols() {
            return text.to_string();
        }

        // Use the pre-compiled regex from config
        if let Some(re) = self.config.symbols_regex() {
            return re
                .replace_all(text, |caps: &regex::Captures| {
                    // Safe unwrap: get(0) always exists in a capture, and we matched at least one char
                    if let Some(matched) = caps.get(0) {
                        if let Some(ch) = matched.as_str().chars().next() {
                            let code = ch as u32;
                            if let Some(name) = self.config.symbols.get(&code) {
                                return format!(" {} ", name);
                            }
                            return ch.to_string();
                        }
                    }
                    // Fallback: return the original match
                    caps.get(0)
                        .map_or(String::new(), |m| m.as_str().to_string())
                })
                .to_string();
        }

        text.to_string()
    }

    /// Cancel any pending speech
    pub fn cancel_speech(&mut self) -> Result<()> {
        self.synth_op("cancel", |s| s.cancel())
    }

    // ========== Review Cursor Navigation ==========
    // These methods implement the screen reader's review cursor for
    // navigating and reading screen content independently of the terminal cursor

    /// Character at column `x` of the review cursor's row (screen row or
    /// scrolled-off line, see `ReviewCursor::above`)
    fn char_at(&self, screen: &Screen, x: u16) -> Option<char> {
        screen.get_char_at(self.review.above, x, self.review.pos.1)
    }

    /// Get character at current review cursor position
    fn get_char(&self, screen: &Screen) -> char {
        self.char_at(screen, self.review.pos.0).unwrap_or(' ')
    }

    /// Text of the review cursor's row, trailing spaces removed
    pub fn review_line(&self, screen: &Screen) -> String {
        screen.get_line_trimmed_at(self.review.above, self.review.pos.1)
    }

    /// Move the review cursor up one row: onto the previous screen row, or
    /// from the top row into the scrolled-off history. False at the oldest
    /// line there is.
    fn move_up_row(&mut self, screen: &Screen) -> bool {
        if self.review.pos.1 > 0 {
            self.review.pos.1 -= 1;
            true
        } else if self.review.above < screen.history_len() {
            self.review.above += 1;
            true
        } else {
            false
        }
    }

    /// Move the review cursor down one row, back out of the history first.
    /// False on the bottom row of the screen.
    fn move_down_row(&mut self, screen: &Screen) -> bool {
        if self.review.above > 0 {
            self.review.above -= 1;
            self.review.pos.1 = 0;
            true
        } else if self.review.pos.1 + 1 < screen.size.1 {
            self.review.pos.1 += 1;
            true
        } else {
            false
        }
    }

    /// Move review cursor to previous character (handles line wrapping)
    fn move_prevchar(&mut self, screen: &Screen) {
        if self.review.pos.0 == 0 {
            if self.move_up_row(screen) {
                self.review.pos.0 = screen.size.0 - 1;
            }
        } else {
            self.review.pos.0 -= 1;
        }
    }

    /// Move review cursor to next character (handles line wrapping)
    fn move_nextchar(&mut self, screen: &Screen) {
        if self.review.pos.0 == screen.size.0 - 1 {
            if self.move_down_row(screen) {
                self.review.pos.0 = 0;
            }
        } else {
            self.review.pos.0 += 1;
        }
    }

    /// Skip backwards over wide character continuation cells
    /// Screen reader needs to skip these to land on actual characters
    fn skip_to_previous_char(&mut self, screen: &Screen) {
        while self.get_char(screen) == '\0' && self.review.pos.0 > 0 {
            self.review.pos.0 -= 1;
        }
    }

    /// Say the line at given y position (a scrolled-off line when the review
    /// cursor is in the history)
    pub fn say_line(&mut self, screen: &Screen, y: u16) -> Result<()> {
        let line = screen.get_line_trimmed_at(self.review.above, y);
        let text = if line.is_empty() {
            "blank".to_string()
        } else {
            // Replace duplicate characters with count if enabled
            self.replace_duplicate_characters(&line)
        };
        self.speak(&text)
    }

    /// Replace duplicate characters with count (e.g., "====" -> "4 equals")
    /// Used to condense repeated symbols for clearer speech
    fn replace_duplicate_characters(&self, line: &str) -> String {
        if !self.config.repeated_symbols() {
            return line.to_string();
        }

        let chars_to_condense = self.config.repeated_symbols_values();
        crate::symbols::condense_repeated_chars(line, &chars_to_condense, &self.config.symbols)
    }

    /// Move to previous line and speak it
    pub fn prev_line(&mut self, screen: &Screen) -> Result<()> {
        if !self.move_up_row(screen) {
            self.speak("top")?;
        }
        self.say_line(screen, self.review.pos.1)
    }

    /// Speak current line
    pub fn current_line(&mut self, screen: &Screen) -> Result<()> {
        self.say_line(screen, self.review.pos.1)
    }

    /// Move to next line and speak it
    pub fn next_line(&mut self, screen: &Screen) -> Result<()> {
        if !self.move_down_row(screen) {
            self.speak("bottom")?;
        }
        self.say_line(screen, self.review.pos.1)
    }

    /// Say character at given position
    pub fn say_char(&mut self, screen: &Screen, y: u16, x: u16, phonetic: bool) -> Result<()> {
        let ch = screen.get_char_at(self.review.above, x, y).unwrap_or(' ');
        if phonetic {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if let Some(phonetic_word) = PHONETICS.get(&lower) {
                return self.speak(phonetic_word);
            }
        }

        // Check if character has a symbol name (always for characters, not just when process_symbols is on)
        let code = ch as u32;
        if let Some(name) = self.config.symbols.get(&code).cloned() {
            return self.speak(&name);
        }

        // Use letter speech command for single characters
        let text = ch.to_string();
        self.synth_op("letter", |s| s.letter(&text))
    }

    /// Move to previous character and speak it
    pub fn prev_char(&mut self, screen: &Screen) -> Result<()> {
        if self.review.pos.0 == 0 {
            self.speak("left")?;
        } else {
            self.review.pos.0 -= 1;
            self.skip_to_previous_char(screen);
        }
        self.say_char(screen, self.review.pos.1, self.review.pos.0, false)
    }

    /// Speak current character
    pub fn current_char(&mut self, screen: &Screen, phonetic: bool) -> Result<()> {
        self.say_char(screen, self.review.pos.1, self.review.pos.0, phonetic)
    }

    /// Move to next character and speak it
    pub fn next_char(&mut self, screen: &Screen) -> Result<()> {
        let ch = self.get_char(screen);
        let width = ch.width().unwrap_or(1) as u16;
        self.review.pos.0 += width;

        if self.review.pos.0 > screen.size.0 - 1 {
            self.speak("right")?;
            self.review.pos.0 = screen.size.0 - 1;
            self.skip_to_previous_char(screen);
        }
        self.say_char(screen, self.review.pos.1, self.review.pos.0, false)
    }

    /// Get word at current position and move cursor to word start
    /// Returns the word and saves the original cursor position
    fn get_word_at_cursor(&mut self, screen: &Screen) -> (String, (u16, u16)) {
        let orig_pos = self.review.pos;
        let (cols, _) = screen.size;

        // Move to beginning of word
        while self.review.pos.0 > 0
            && self.get_char(screen) != ' '
            && self.char_at(screen, self.review.pos.0 - 1) != Some(' ')
        {
            self.move_prevchar(screen);
        }

        // At start of line with space? That's just "space"
        if self.review.pos.0 == 0 && self.get_char(screen) == ' ' {
            return ("".to_string(), orig_pos);
        }

        // Collect the word
        let mut word = String::new();
        word.push(self.get_char(screen));

        while self.review.pos.0 < cols - 1 {
            self.move_nextchar(screen);
            let ch = self.get_char(screen);
            if ch == ' ' {
                break;
            }
            word.push(ch);
        }

        (word, orig_pos)
    }

    /// Say word at current position (with optional spelling)
    pub fn say_word(&mut self, screen: &Screen, spell: bool) -> Result<()> {
        let (word, orig_pos) = self.get_word_at_cursor(screen);

        if word.is_empty() {
            self.speak("space")?;
        } else if spell {
            // Spell the word as one utterance ("h e l l o"), naming symbols
            // even when symbol processing is off. One utterance is much
            // smoother than a separate letter command per character.
            let spelled = word
                .chars()
                .map(|ch| match self.config.symbols.get(&(ch as u32)) {
                    Some(name) => name.clone(),
                    None => ch.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            self.synth_op("speak", |s| s.speak(&spelled))?;
        } else {
            self.speak(&word)?;
        }

        // Restore original position
        self.review.pos = orig_pos;
        Ok(())
    }

    /// Move to previous word and speak it
    pub fn prev_word(&mut self, screen: &Screen) -> Result<()> {
        if self.review.pos.0 == 0 {
            self.speak("left")?;
            return self.say_word(screen, false);
        }

        // Move over any existing word we're in
        while self.review.pos.0 > 0 && self.get_char(screen) != ' ' {
            self.move_prevchar(screen);
        }

        // Skip whitespace
        while self.review.pos.0 > 0 && self.get_char(screen) == ' ' {
            self.move_prevchar(screen);
        }

        // Move to beginning of the word we're now on
        while self.review.pos.0 > 0
            && self.get_char(screen) != ' '
            && self.char_at(screen, self.review.pos.0 - 1) != Some(' ')
        {
            self.move_prevchar(screen);
        }

        self.say_word(screen, false)
    }

    /// Move to next word and speak it
    pub fn next_word(&mut self, screen: &Screen) -> Result<()> {
        let (cols, _) = screen.size;
        let orig_pos = self.review.pos;

        // Move over current word
        while self.review.pos.0 < cols - 1 && self.get_char(screen) != ' ' {
            self.move_nextchar(screen);
        }

        // Skip whitespace
        while self.review.pos.0 < cols - 1 && self.get_char(screen) == ' ' {
            self.move_nextchar(screen);
        }

        // Hit right edge on whitespace?
        if self.review.pos.0 == cols - 1 && self.get_char(screen) == ' ' {
            self.speak("right")?;
            self.review.pos = orig_pos;
            return self.say_word(screen, false);
        }

        self.say_word(screen, false)
    }

    /// Jump to top of screen; when already there (or in the history), jump
    /// to the oldest scrolled-off line instead.
    pub fn top_of_screen(&mut self, screen: &Screen) -> Result<()> {
        let history = screen.history_len();
        if (self.review.pos.1 == 0 || self.review.above > 0) && self.review.above < history {
            self.review.above = history;
        }
        self.review.pos.1 = 0;
        self.say_line(screen, 0)
    }

    /// Jump to bottom of screen (leaving the history if the cursor was there)
    pub fn bottom_of_screen(&mut self, screen: &Screen) -> Result<()> {
        self.review.above = 0;
        self.review.pos.1 = screen.size.1 - 1;
        self.say_line(screen, self.review.pos.1)
    }

    /// Jump to start of line
    pub fn start_of_line(&mut self, screen: &Screen) -> Result<()> {
        self.review.pos.0 = 0;
        self.say_char(screen, self.review.pos.1, 0, false)
    }

    /// Jump to end of line
    pub fn end_of_line(&mut self, screen: &Screen) -> Result<()> {
        self.review.pos.0 = screen.size.0 - 1;
        self.say_char(screen, self.review.pos.1, self.review.pos.0, false)
    }

    /// Execute a plugin by keyboard shortcut
    ///
    /// Starts the plugin on a worker thread; its output is spoken when
    /// `poll_plugin_results` picks it up, so keys and terminal output keep
    /// flowing while it runs.
    pub fn execute_plugin(&mut self, key: &str, screen: &Screen) -> Result<()> {
        let Some(pm) = self.plugin_manager.as_ref() else {
            return Ok(());
        };
        match pm.execute_plugin_async(key, screen, &self.last_command, self.plugin_sender.clone()) {
            Ok(true) => self.plugins_running += 1,
            Ok(false) => {}
            Err(e) => self.speak(&format!("Plugin error: {}", e))?,
        }
        Ok(())
    }

    /// Speak any plugin results that have arrived. Called from the event
    /// loop on every iteration (it wakes at least every 100 ms).
    pub fn poll_plugin_results(&mut self) -> Result<()> {
        while let Ok(result) = self.plugin_results.try_recv() {
            self.plugins_running = self.plugins_running.saturating_sub(1);
            match result {
                Ok(lines) => {
                    for line in lines {
                        self.speak(&line)?;
                    }
                }
                Err(e) => self.speak(&format!("Plugin error: {}", e))?,
            }
        }
        Ok(())
    }

    /// Whether any plugin is still running on a worker thread
    pub fn plugins_running(&self) -> bool {
        self.plugins_running > 0
    }

    /// Check if a key has a plugin bound to it
    pub fn has_plugin(&self, key: &str) -> bool {
        self.plugin_manager
            .as_ref()
            .is_some_and(|pm| pm.has_plugin(key))
    }

    // ========== Cursor Tracking / Delayed Functions ==========

    /// Schedule a function to run after a delay
    ///
    /// Used for cursor tracking - when arrow keys are pressed, we schedule
    /// speech after a delay to let the cursor settle
    pub fn schedule<F>(&mut self, delay: Duration, func: F, set_temp_silence: bool)
    where
        F: FnOnce(&mut State, &Screen) -> Result<()> + Send + 'static,
    {
        let when = Instant::now() + delay;
        self.delayed_functions.push((when, Box::new(func)));
        if set_temp_silence {
            self.temp_silence = true;
        }
    }

    /// Clear all scheduled delayed functions
    ///
    /// Called when user presses a key, canceling any pending cursor tracking speech
    pub fn clear_delayed_functions(&mut self) {
        self.delayed_functions.clear();
        self.temp_silence = false;
        // A keypress silences speech; drop output that was waiting to be
        // spoken as well, rather than reading it out stale later.
        self.delaying_output = false;
        self.speech_buffer.drain_lines();
        self.speech_buffer.flush();
    }

    /// Run any delayed functions that are ready
    ///
    /// Returns true if any functions were executed
    pub fn run_scheduled(&mut self, screen: &Screen) -> Result<bool> {
        let now = Instant::now();

        // Extract ready functions from the list
        let mut to_run = Vec::new();
        let mut i = 0;
        while i < self.delayed_functions.len() {
            if now >= self.delayed_functions[i].0 {
                to_run.push(self.delayed_functions.remove(i));
            } else {
                i += 1;
            }
        }

        // Clear temp_silence if we ran any functions
        if !to_run.is_empty() {
            self.temp_silence = false;
        }

        // Execute the ready functions
        let executed = !to_run.is_empty();
        for (_when, func) in to_run {
            func(self, screen)?;
        }

        Ok(executed)
    }

    /// Get time until next scheduled function
    ///
    /// Returns None if no functions are scheduled, otherwise duration until next function
    /// Used to set timeout for select/poll
    pub fn time_until_next_scheduled(&self) -> Option<Duration> {
        if self.delayed_functions.is_empty() {
            return None;
        }

        let now = Instant::now();
        let next = self.delayed_functions.iter().map(|(when, _)| *when).min()?;

        Some(next.saturating_duration_since(now))
    }

    /// Update review cursor to match terminal cursor
    ///
    /// Called when cursor tracking is enabled and terminal cursor moves
    pub fn update_review_cursor_from_terminal(&mut self, cursor: (u16, u16)) {
        if self.config.cursor_tracking() {
            self.review.pos = cursor;
            self.review.above = 0;
        }
    }

    /// Adjust review cursor after screen scrolling
    ///
    /// When the screen scrolls, the review cursor should move to stay with
    /// the same content (or clamp to screen bounds if content scrolled off).
    ///
    /// scroll_offset: positive = scrolled up (move review cursor up to follow content)
    ///                negative = scrolled down (move review cursor down to follow content)
    ///
    /// Content that scrolls off the top is followed into the history (see
    /// `ReviewCursor::above`), as far as `history_len` lines are kept.
    pub fn adjust_review_cursor_for_scroll(
        &mut self,
        scroll_offset: i16,
        rows: u16,
        history_len: usize,
    ) {
        if scroll_offset == 0 {
            return;
        }

        let (x, y) = self.review.pos;
        if scroll_offset > 0 {
            // Content scrolled up - review cursor should move up to follow
            let offset = scroll_offset as u16;
            if self.review.above > 0 {
                self.review.above = (self.review.above + offset as usize).min(history_len);
            } else if y >= offset {
                self.review.pos = (x, y - offset);
            } else {
                self.review.above = ((offset - y) as usize).min(history_len);
                self.review.pos = (x, 0);
            }
        } else if self.review.above == 0 {
            // Content scrolled down - review cursor should move down to follow
            let offset = (-scroll_offset) as u16;
            self.review.pos = (x, (y + offset).min(rows.saturating_sub(1)));
        }
    }
}
