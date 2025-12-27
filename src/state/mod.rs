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
use crate::Result;
use config::Config;
use log::info;
use phonetics::PHONETICS;
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
}

impl State {
    /// Create a new application state with given terminal dimensions
    ///
    /// Loads configuration from disk and initializes all screen reader state.
    pub fn new(cols: u16, rows: u16) -> Result<Self> {
        info!("Initializing state with {}x{} terminal", cols, rows);

        let config = Config::load()?;
        info!("Configuration loaded from {:?}", config.path());
        info!("  Symbols: {}", config.symbols.len());
        info!("  Plugins: {}", config.plugins.len());
        info!("  Process symbols: {}", config.process_symbols());
        info!("  Key echo: {}", config.key_echo());
        info!("  Cursor tracking: {}", config.cursor_tracking());

        // Create speech synthesizer
        let mut synth = crate::speech::create_synth()?;
        info!("Speech synthesizer created");

        // Apply config settings to synth
        if let Some(rate) = config.rate() {
            synth.set_rate(rate)?;
            info!("Speech rate set to {}", rate);
        }
        if let Some(volume) = config.volume() {
            synth.set_volume(volume)?;
            info!("Speech volume set to {}", volume);
        }
        if let Some(voice_idx) = config.voice_idx() {
            synth.set_voice_idx(voice_idx)?;
            info!("Speech voice index set to {}", voice_idx);
        }

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

        Ok(Self {
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
            last_key: None,
            plugin_manager,
            delayed_functions: Vec::new(),
        })
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
    /// When quiet, screen reader only speaks on explicit navigation commands
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
            let text = self.copy_text_range(screen, start_x, start_y, end_x, end_y);

            // Copy to clipboard
            crate::clipboard::copy_to_clipboard(&text)?;

            // Clear selection
            self.copy_start = None;

            self.speak("copied")?;
        }
        Ok(())
    }

    /// Copy text from a linear region of the screen
    ///
    /// Performs a linear (stream) selection from start to end position,
    /// like selecting text in a word processor. Multi-line selections
    /// include the end of the first line, full middle lines, and the
    /// beginning of the last line.
    fn copy_text_range(
        &self,
        screen: &Screen,
        mut start_x: u16,
        mut start_y: u16,
        mut end_x: u16,
        mut end_y: u16,
    ) -> String {
        // Normalize so start is before end in reading order (row-major)
        // Important: swap both x and y together to maintain the linear selection
        if start_y > end_y || (start_y == end_y && start_x > end_x) {
            std::mem::swap(&mut start_x, &mut end_x);
            std::mem::swap(&mut start_y, &mut end_y);
        }

        let mut text = String::new();
        let (cols, _) = screen.size;

        for y in start_y..=end_y {
            // On first row: start from start_x
            // On subsequent rows: start from column 0
            let line_start = if y == start_y { start_x } else { 0 };

            // On last row: end at end_x
            // On earlier rows: end at last column
            let line_end = if y == end_y { end_x } else { cols - 1 };

            // Get characters from this line
            for x in line_start..=line_end {
                if let Some(ch) = screen.get_char(x, y) {
                    // Skip wide character continuation cells
                    if ch != '\0' {
                        text.push(ch);
                    }
                }
            }

            // Add newline except for last line
            if y < end_y {
                text.push('\n');
            }
        }

        text
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
    pub fn speak(&mut self, text: &str) -> Result<()> {
        if !self.quiet {
            let processed = self.process_symbols_in_text(text);
            self.synth.speak(&processed)?;
        }
        Ok(())
    }

    /// Speak a single character (for key echo)
    ///
    /// Uses the TTS "letter" mode if available, or falls back to
    /// speaking the character's name for special characters.
    pub fn speak_char(&mut self, ch: char) -> Result<()> {
        if self.quiet {
            return Ok(());
        }

        // For special characters, use their symbol name
        if let Some(name) = self.config.symbols.get(&(ch as u32)) {
            self.synth.letter(name)?;
        } else {
            // Use letter mode for regular characters
            self.synth.letter(&ch.to_string())?;
        }
        Ok(())
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
        self.synth.cancel()
    }

    // ========== Review Cursor Navigation ==========
    // These methods implement the screen reader's review cursor for
    // navigating and reading screen content independently of the terminal cursor

    /// Get character at current review cursor position
    fn get_char(&self, screen: &Screen) -> char {
        screen
            .get_char(self.review.pos.0, self.review.pos.1)
            .unwrap_or(' ')
    }

    /// Move review cursor to previous character (handles line wrapping)
    fn move_prevchar(&mut self, screen: &Screen) {
        let (x, y) = self.review.pos;
        if x == 0 {
            if y == 0 {
                return; // Already at top-left
            }
            self.review.pos.1 -= 1;
            self.review.pos.0 = screen.size.0 - 1;
        } else {
            self.review.pos.0 -= 1;
        }
    }

    /// Move review cursor to next character (handles line wrapping)
    fn move_nextchar(&mut self, screen: &Screen) {
        let (x, y) = self.review.pos;
        let (cols, rows) = screen.size;
        if x == cols - 1 {
            if y == rows - 1 {
                return; // Already at bottom-right
            }
            self.review.pos.1 += 1;
            self.review.pos.0 = 0;
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

    /// Say the line at given y position
    pub fn say_line(&mut self, screen: &Screen, y: u16) -> Result<()> {
        let line = screen.get_line_trimmed(y);
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
        if self.review.pos.1 == 0 {
            self.speak("top")?;
        } else {
            self.review.pos.1 -= 1;
        }
        self.say_line(screen, self.review.pos.1)
    }

    /// Speak current line
    pub fn current_line(&mut self, screen: &Screen) -> Result<()> {
        self.say_line(screen, self.review.pos.1)
    }

    /// Move to next line and speak it
    pub fn next_line(&mut self, screen: &Screen) -> Result<()> {
        if self.review.pos.1 >= screen.size.1 - 1 {
            self.speak("bottom")?;
        } else {
            self.review.pos.1 += 1;
        }
        self.say_line(screen, self.review.pos.1)
    }

    /// Say character at given position
    pub fn say_char(&mut self, screen: &Screen, y: u16, x: u16, phonetic: bool) -> Result<()> {
        let ch = screen.get_char(x, y).unwrap_or(' ');
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
        self.synth.letter(&ch.to_string())
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
            && screen.get_char(self.review.pos.0 - 1, self.review.pos.1) != Some(' ')
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
            // Spell the word letter by letter
            for ch in word.chars() {
                self.synth.letter(&ch.to_string())?;
            }
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
            && screen.get_char(self.review.pos.0 - 1, self.review.pos.1) != Some(' ')
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

    /// Jump to top of screen
    pub fn top_of_screen(&mut self, screen: &Screen) -> Result<()> {
        self.review.pos.1 = 0;
        self.say_line(screen, 0)
    }

    /// Jump to bottom of screen
    pub fn bottom_of_screen(&mut self, screen: &Screen) -> Result<()> {
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
    /// Runs the plugin script, collects output, and speaks it to the user
    pub fn execute_plugin(&mut self, key: &str, screen: &Screen) -> Result<()> {
        if let Some(ref pm) = self.plugin_manager {
            match pm.execute_plugin(key, screen, &self.last_command) {
                Ok(lines) => {
                    for line in lines {
                        self.speak(&line)?;
                    }
                }
                Err(e) => {
                    self.speak(&format!("Plugin error: {}", e))?;
                }
            }
        }
        Ok(())
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
        }
    }

    /// Adjust review cursor after screen scrolling
    ///
    /// When the screen scrolls, the review cursor should move to stay with
    /// the same content (or clamp to screen bounds if content scrolled off).
    ///
    /// scroll_offset: positive = scrolled up (move review cursor up to follow content)
    ///                negative = scrolled down (move review cursor down to follow content)
    pub fn adjust_review_cursor_for_scroll(&mut self, scroll_offset: i16, rows: u16) {
        if scroll_offset == 0 {
            return;
        }

        let (x, y) = self.review.pos;
        let new_y = if scroll_offset > 0 {
            // Content scrolled up - review cursor should move up to follow
            y.saturating_sub(scroll_offset as u16)
        } else {
            // Content scrolled down - review cursor should move down to follow
            let offset = (-scroll_offset) as u16;
            (y + offset).min(rows.saturating_sub(1))
        };

        self.review.pos = (x, new_y);
    }
}

#[cfg(test)]
mod tests {
    use crate::terminal::Screen;

    /// Test helper to create a screen with test content
    fn create_test_screen() -> Screen {
        let mut screen = Screen::new(10, 5);
        // Fill with recognizable content:
        // Row 0: "AAAAAAAAAA"
        // Row 1: "BBBBBBBBBB"
        // Row 2: "CCCCCCCCCC"
        // Row 3: "DDDDDDDDDD"
        // Row 4: "EEEEEEEEEE"
        for y in 0..5 {
            let ch = (b'A' + y as u8) as char;
            for x in 0..10 {
                screen.buffer[y][x].data = ch;
            }
        }
        screen
    }

    /// Test helper to extract text from screen using the same logic as copy_text_range
    fn extract_text(screen: &Screen, start_x: u16, start_y: u16, end_x: u16, end_y: u16) -> String {
        let mut start_x = start_x;
        let mut start_y = start_y;
        let mut end_x = end_x;
        let mut end_y = end_y;

        // Same normalization logic as copy_text_range
        if start_y > end_y || (start_y == end_y && start_x > end_x) {
            std::mem::swap(&mut start_x, &mut end_x);
            std::mem::swap(&mut start_y, &mut end_y);
        }

        let mut text = String::new();
        let (cols, _) = screen.size;

        for y in start_y..=end_y {
            let line_start = if y == start_y { start_x } else { 0 };
            let line_end = if y == end_y { end_x } else { cols - 1 };

            for x in line_start..=line_end {
                if let Some(ch) = screen.get_char(x, y) {
                    if ch != '\0' {
                        text.push(ch);
                    }
                }
            }

            if y < end_y {
                text.push('\n');
            }
        }

        text
    }

    #[test]
    fn test_linear_selection_single_line() {
        let screen = create_test_screen();

        // Select part of row 1: columns 2-5
        let text = extract_text(&screen, 2, 1, 5, 1);
        assert_eq!(text, "BBBB");
    }

    #[test]
    fn test_linear_selection_multi_line() {
        let screen = create_test_screen();

        // Select from row 1, col 5 to row 2, col 3
        // Should get: "BBBBB" (5-9 of row 1) + "\n" + "CCCC" (0-3 of row 2)
        let text = extract_text(&screen, 5, 1, 3, 2);
        assert_eq!(text, "BBBBB\nCCCC");
    }

    #[test]
    fn test_linear_selection_backwards() {
        let screen = create_test_screen();

        // Select backwards: from row 2, col 3 to row 1, col 5
        // Should give same result as forward selection
        let text = extract_text(&screen, 3, 2, 5, 1);
        assert_eq!(text, "BBBBB\nCCCC");
    }

    #[test]
    fn test_linear_selection_full_lines() {
        let screen = create_test_screen();

        // Select from row 1, col 0 to row 2, col 9 (full two lines)
        let text = extract_text(&screen, 0, 1, 9, 2);
        assert_eq!(text, "BBBBBBBBBB\nCCCCCCCCCC");
    }

    #[test]
    fn test_linear_selection_three_lines() {
        let screen = create_test_screen();

        // Select from row 1, col 7 to row 3, col 2
        // Row 1: cols 7-9 = "BBB"
        // Row 2: full line = "CCCCCCCCCC"
        // Row 3: cols 0-2 = "DDD"
        let text = extract_text(&screen, 7, 1, 2, 3);
        assert_eq!(text, "BBB\nCCCCCCCCCC\nDDD");
    }

    #[test]
    fn test_linear_selection_single_char() {
        let screen = create_test_screen();

        // Select single character
        let text = extract_text(&screen, 5, 2, 5, 2);
        assert_eq!(text, "C");
    }
}
