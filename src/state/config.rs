//! Configuration management

use crate::{Result, TdsrError};
use ini::Ini;
use log::{debug, info};
use regex;
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

    /// Cached symbols regex for efficient symbol replacement
    /// Built from symbols dictionary for repeated characters
    symbols_regex: Option<String>,

    /// Plugin bindings (plugin_name -> key)
    /// Maps plugin modules to keyboard shortcuts
    pub plugins: HashMap<String, String>,

    /// Plugin command matchers (plugin_name -> regex)
    /// Filters when plugins are triggered based on last command
    pub plugin_commands: HashMap<String, String>,
}

impl Config {
    /// Load configuration from disk or create default
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
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
            .set("prompt", ".*");

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
                self.plugin_commands.insert(plugin.to_string(), cmd.to_string());
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

    /// Get a float value from config
    pub fn get_float(&self, section: &str, key: &str, default: f32) -> f32 {
        self.ini
            .get_from(Some(section), key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    /// Build regex pattern for symbol replacement
    /// Used by speech output to pronounce symbols
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
            self.symbols_regex = Some(pattern);
        }
    }

    /// Get symbols regex pattern for replacement
    pub fn symbols_regex(&self) -> Option<&str> {
        self.symbols_regex.as_deref()
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
        self.get_int("speech", "voice_idx", -1)
            .try_into()
            .ok()
    }

    /// Prompt pattern for plugin line collection
    /// Default matches any line (plugins collect until they find prompt)
    pub fn prompt_pattern(&self) -> String {
        self.get_string("speech", "prompt", ".*")
    }

    /// Cursor tracking delay in seconds
    /// How long to wait after cursor movement before speaking
    pub fn cursor_delay(&self) -> f32 {
        self.get_float("speech", "cursor_delay", 0.02)
    }
}
