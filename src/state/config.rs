//! Configuration management

use crate::{Result, TdsrError};
use ini::Ini;
use log::{debug, info};
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

/// Application configuration for the screen reader
///
/// Manages all persistent settings including speech parameters,
/// symbol pronunciation, key bindings, and plugin configuration.
pub struct Config {
    /// INI configuration storage
    ini: Ini,

    /// Config file path (~/.tdsr.cfg)
    path: PathBuf,

    /// Symbols dictionary (char code -> name) for pronunciation
    /// e.g., 33 -> "bang" so '!' is spoken as "bang" not just "exclamation"
    pub symbols: HashMap<u32, String>,

    /// Cached compiled regex for efficient symbol replacement
    /// Built from symbols dictionary, compiled once on config load
    symbols_regex: Option<Regex>,

    /// Plugin bindings (plugin_name -> key)
    /// Maps plugin modules to keyboard shortcuts
    pub plugins: HashMap<String, String>,

    /// Plugin command matchers (plugin_name -> regex)
    /// Filters when plugins are triggered based on last command
    pub plugin_commands: HashMap<String, String>,
}

impl Config {
    /// Load configuration from `~/.tdsr.cfg`, creating it with defaults if absent
    pub fn load() -> Result<Self> {
        Self::load_from(Self::config_path())
    }

    /// Load configuration from an explicit path, creating it with defaults if
    /// absent. Later `save()` calls write back to the same path.
    pub fn load_from(path: PathBuf) -> Result<Self> {
        debug!("Loading config from {:?}", path);

        let ini = if path.exists() {
            Ini::load_from_file(&path)
                .map_err(|e| TdsrError::IniParse(format!("Failed to load config: {}", e)))?
        } else {
            info!("Config file not found, creating default");
            let default = Self::default_config();
            default
                .write_to_file(&path)
                .map_err(|e| TdsrError::IniParse(format!("Failed to write config: {}", e)))?;
            default
        };

        let mut config = Self {
            ini,
            path,
            symbols: HashMap::new(),
            symbols_regex: None,
            plugins: HashMap::new(),
            plugin_commands: HashMap::new(),
        };

        config.parse_symbols();
        config.parse_plugins();
        config.build_symbols_regex();

        Ok(config)
    }

    /// Save configuration to disk
    pub fn save(&self) -> Result<()> {
        debug!("Saving config to {:?}", self.path);
        self.ini
            .write_to_file(&self.path)
            .map_err(|e| TdsrError::Config(format!("Failed to save config: {}", e)))
    }

    /// Get config file path (~/.tdsr.cfg)
    ///
    /// This is where screen reader settings persist between sessions
    fn config_path() -> PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(home).join(".tdsr.cfg")
    }

    /// Expose the config file path for display
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Create default configuration
    fn default_config() -> Ini {
        let mut ini = Ini::new();

        ini.with_section(Some("speech"))
            .set("process_symbols", "false")
            .set("key_echo", "true")
            .set("cursor_tracking", "true")
            .set("line_pause", "true")
            .set("repeated_symbols", "false")
            .set("repeated_symbols_values", "-=!#")
            .set("prompt", ".*")
            .set("tui_mode", "auto")
            .set("tui_apps", "fp,mc")
            .set("tui_settle", "30");

        ini.with_section(Some("symbols"))
            .set("32", "space")
            .set("33", "bang")
            .set("34", "quote")
            .set("35", "number")
            .set("36", "dollar")
            .set("37", "percent")
            .set("38", "and")
            .set("39", "tick")
            .set("40", "left paren")
            .set("41", "right paren")
            .set("42", "star")
            .set("43", "plus")
            .set("44", "comma")
            .set("45", "dash")
            .set("46", "dot")
            .set("47", "slash")
            .set("58", "colon")
            .set("59", "semi")
            .set("60", "less")
            .set("61", "equals")
            .set("62", "greater")
            .set("63", "question")
            .set("64", "at")
            .set("91", "left bracket")
            .set("92", "backslash")
            .set("93", "right bracket")
            .set("94", "caret")
            .set("95", "line")
            .set("96", "grav")
            .set("123", "left brace")
            .set("124", "bar")
            .set("125", "right brace")
            .set("126", "tilda");

        ini.with_section(Some("commands"));
        ini.with_section(Some("plugins"));

        ini
    }

    /// Parse symbols from config
    fn parse_symbols(&mut self) {
        if let Some(section) = self.ini.section(Some("symbols")) {
            for (key, value) in section.iter() {
                if let Ok(code) = key.parse::<u32>() {
                    self.symbols.insert(code, value.to_string());
                }
            }
        }
        debug!("Loaded {} symbols", self.symbols.len());
    }

    /// Parse plugins from config
    fn parse_plugins(&mut self) {
        if let Some(section) = self.ini.section(Some("plugins")) {
            for (plugin, key) in section.iter() {
                self.plugins.insert(plugin.to_string(), key.to_string());
            }
        }

        if let Some(section) = self.ini.section(Some("commands")) {
            for (plugin, cmd) in section.iter() {
                self.plugin_commands
                    .insert(plugin.to_string(), cmd.to_string());
            }
        }

        debug!("Loaded {} plugins", self.plugins.len());
    }

    /// Get a boolean value from config
    pub fn get_bool(&self, section: &str, key: &str, default: bool) -> bool {
        self.ini
            .get_from(Some(section), key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Get a string value from config
    pub fn get_string(&self, section: &str, key: &str, default: &str) -> String {
        self.ini
            .get_from(Some(section), key)
            .unwrap_or(default)
            .to_string()
    }

    /// Get an integer value from config
    pub fn get_int(&self, section: &str, key: &str, default: i32) -> i32 {
        self.ini
            .get_from(Some(section), key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Set a value in config
    pub fn set(&mut self, section: &str, key: &str, value: &str) {
        self.ini.with_section(Some(section)).set(key, value);
    }

    /// Remove a key (no-op if absent).
    pub fn remove(&mut self, section: &str, key: &str) {
        self.ini.delete_from(Some(section), key);
    }

    /// Get a float value from config
    pub fn get_float(&self, section: &str, key: &str, default: f32) -> f32 {
        self.ini
            .get_from(Some(section), key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Build and compile regex for symbol replacement
    /// Compiled once during config load for efficiency in the hot path
    fn build_symbols_regex(&mut self) {
        if self.symbols.is_empty() {
            return;
        }

        // Build pattern from all symbol characters
        let pattern: String = self
            .symbols
            .keys()
            .filter(|&&code| code != 32) // Skip space
            .filter_map(|&code| {
                // Only include valid unicode characters
                char::from_u32(code).map(|ch| regex::escape(&ch.to_string()))
            })
            .collect::<Vec<_>>()
            .join("|");

        if !pattern.is_empty() {
            // Compile the regex once and cache it
            match Regex::new(&pattern) {
                Ok(re) => {
                    debug!(
                        "Compiled symbols regex with {} patterns",
                        self.symbols.len()
                    );
                    self.symbols_regex = Some(re);
                }
                Err(e) => {
                    debug!("Failed to compile symbols regex: {}", e);
                }
            }
        }
    }

    /// Get the compiled symbols regex for replacement
    pub fn symbols_regex(&self) -> Option<&Regex> {
        self.symbols_regex.as_ref()
    }

    // Screen reader-specific configuration getters

    /// Should the screen reader process symbols into words?
    /// When true, "!" becomes "bang", "$" becomes "dollar", etc.
    pub fn process_symbols(&self) -> bool {
        self.get_bool("speech", "process_symbols", false)
    }

    /// Should individual keystrokes be echoed?
    /// When true, typing "a" speaks "a"
    pub fn key_echo(&self) -> bool {
        self.get_bool("speech", "key_echo", true)
    }

    /// Should cursor position be tracked and spoken?
    /// When true, arrow keys trigger delayed speech of new position
    pub fn cursor_tracking(&self) -> bool {
        self.get_bool("speech", "cursor_tracking", true)
    }

    /// Should speech pause at newlines?
    /// When true, each line is spoken separately as it arrives
    pub fn line_pause(&self) -> bool {
        self.get_bool("speech", "line_pause", true)
    }

    /// Should repeated symbols be condensed?
    /// When true, "====" becomes "4 equals" instead of "equals equals equals equals"
    pub fn repeated_symbols(&self) -> bool {
        self.get_bool("speech", "repeated_symbols", false)
    }

    /// Which symbols should be condensed when repeated?
    pub fn repeated_symbols_values(&self) -> String {
        self.get_string("speech", "repeated_symbols_values", "-=!#")
    }

    /// Speech rate (0-100)
    pub fn rate(&self) -> Option<u8> {
        self.get_int("speech", "rate", -1)
            .try_into()
            .ok()
            .filter(|&r| r <= 100)
    }

    /// Speech volume (0-100)
    pub fn volume(&self) -> Option<u8> {
        self.get_int("speech", "volume", -1)
            .try_into()
            .ok()
            .filter(|&v| v <= 100)
    }

    /// Voice index for TTS engine
    pub fn voice_idx(&self) -> Option<usize> {
        self.get_int("speech", "voice_idx", -1).try_into().ok()
    }

    /// Voice by persistent id (`[speech] voice`): an espeak-ng voice file
    /// such as `gmw/en-US` or `mb/mb-us1`, or a Speech Dispatcher voice
    /// name. Takes precedence over `voice_idx`.
    pub fn voice(&self) -> Option<String> {
        let v = self.get_string("speech", "voice", "");
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    }

    /// External speech server command (`[speech] speech_command`), if set.
    ///
    /// The command is started once and driven over its stdin with the
    /// original TDSR line protocol (`s<text>`, `l<char>`, `x`, `r<0-100>`,
    /// `v<0-100>`, `V<idx>`), so any program that speaks it can be used.
    pub fn speech_command(&self) -> Option<String> {
        let cmd = self.get_string("speech", "speech_command", "");
        let cmd = cmd.trim();
        (!cmd.is_empty()).then(|| cmd.to_string())
    }

    /// Prompt pattern for plugin line collection
    /// Default matches any line (plugins collect until they find prompt)
    pub fn prompt_pattern(&self) -> String {
        self.get_string("speech", "prompt", ".*")
    }

    /// Cursor tracking delay in seconds
    /// How long to wait after cursor movement before speaking
    /// Config file stores value in milliseconds for user convenience
    pub fn cursor_delay(&self) -> f32 {
        // Config stores milliseconds, convert to seconds for internal use
        let ms = self.get_float("speech", "cursor_delay", 20.0);
        ms / 1000.0
    }

    /// How TUI mode (screen diffing for full-screen programs) is engaged:
    /// `auto` (detected), `apps` (listed names only), `on` or `off`.
    /// Unknown values mean auto.
    pub fn tui_mode(&self) -> crate::tui::TuiMode {
        self.get_string("speech", "tui_mode", "auto")
            .parse()
            .unwrap_or_default()
    }

    /// Programs (by process name, comma separated) that always get TUI
    /// mode when they are in the foreground
    pub fn tui_apps(&self) -> Vec<String> {
        self.get_string("speech", "tui_apps", "fp,mc")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    }

    /// Milliseconds of quiet after output before the screen is compared
    /// with its previous state in TUI mode (full-screen programs paint in
    /// several writes)
    pub fn tui_settle_ms(&self) -> u64 {
        self.get_int("speech", "tui_settle", 30).clamp(0, 1000) as u64
    }

    /// Whether switching TUI mode on or off is announced
    pub fn tui_announce(&self) -> bool {
        self.get_bool("speech", "tui_announce", true)
    }
}
