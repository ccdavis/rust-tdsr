//! Plugin system for extending screen reader functionality
//!
//! Plugins are external scripts/programs that analyze terminal output
//! and provide additional speech feedback. They receive screen lines
//! as input and return lines to speak.

use crate::terminal::Screen;
use crate::Result;
use log::{debug, error};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Plugin configuration
#[derive(Debug, Clone)]
pub struct PluginConfig {
    /// Plugin name (maps to script name)
    pub name: String,
    /// Keyboard shortcut that triggers this plugin
    pub key: String,
    /// Optional regex that must match the last command
    pub command_filter: Option<Regex>,
}

/// Input sent to plugin (JSON format)
#[derive(Debug, Serialize)]
struct PluginInput {
    /// Screen lines from bottom to top (up to prompt)
    lines: Vec<String>,
    /// Last command executed (if available)
    last_command: Option<String>,
}

/// Output from plugin (JSON format)
#[derive(Debug, Deserialize)]
struct PluginOutput {
    /// Lines to speak to the user
    speak: Vec<String>,
}

/// Plugin manager for loading and executing plugins
pub struct PluginManager {
    /// Map of keyboard shortcut to plugin config
    plugins: HashMap<String, PluginConfig>,
    /// Base directory for plugin scripts
    plugin_dir: PathBuf,
    /// Prompt regex for finding where to stop collecting lines
    prompt_regex: Regex,
}

impl PluginManager {
    /// Create a new plugin manager
    ///
    /// Loads plugin configurations and sets up the plugin directory
    pub fn new(
        plugins: HashMap<String, String>,
        plugin_commands: HashMap<String, String>,
        plugin_dir: PathBuf,
        prompt_pattern: &str,
    ) -> Result<Self> {
        let prompt_regex = Regex::new(prompt_pattern)
            .unwrap_or_else(|_| {
                // This should never fail as ".*" is always valid
                Regex::new(".*").expect("Failed to compile fallback regex")
            });

        let mut plugin_configs = HashMap::new();

        for (name, key) in plugins {
            let command_filter = plugin_commands.get(&name)
                .and_then(|pattern| Regex::new(pattern).ok());

            plugin_configs.insert(
                key.clone(),
                PluginConfig {
                    name,
                    key,
                    command_filter,
                },
            );
        }

        debug!("Plugin manager initialized with {} plugins", plugin_configs.len());

        Ok(Self {
            plugins: plugin_configs,
            plugin_dir,
            prompt_regex,
        })
    }

    /// Execute a plugin by keyboard shortcut
    ///
    /// Collects screen lines, runs the plugin, and returns text to speak
    pub fn execute_plugin(
        &self,
        key: &str,
        screen: &Screen,
        last_command: &str,
    ) -> Result<Vec<String>> {
        let plugin = self.plugins.get(key)
            .ok_or_else(|| format!("Plugin not found for key: {}", key))?;

        debug!("Executing plugin: {}", plugin.name);

        // Check command filter if configured
        if let Some(ref filter) = plugin.command_filter {
            if !last_command.is_empty() && !filter.is_match(last_command) {
                debug!("Command filter did not match, skipping plugin");
                return Ok(vec![]);
            }
        }

        // Collect screen lines from bottom up until prompt
        let lines = self.collect_screen_lines(screen, last_command);

        // Execute the plugin script
        self.run_plugin_script(&plugin.name, lines, last_command)
    }

    /// Collect screen lines from bottom up until prompt is found
    fn collect_screen_lines(&self, screen: &Screen, last_command: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let (_, rows) = screen.size;

        // Collect lines from bottom to top
        for y in (0..rows).rev() {
            let line = screen.get_line_trimmed(y);
            lines.push(line.clone());

            // Stop if we hit the prompt line
            if self.prompt_regex.is_match(&line) {
                // If there's a command filter, check if this line contains the command
                if !last_command.is_empty() && line.contains(last_command) {
                    break;
                }
            }
        }

        lines
    }

    /// Run a plugin script as a subprocess
    ///
    /// Passes screen lines as JSON input, reads speech output
    fn run_plugin_script(
        &self,
        plugin_name: &str,
        lines: Vec<String>,
        last_command: &str,
    ) -> Result<Vec<String>> {
        // Build plugin script path
        // Support both Python files and executables
        let script_path = if plugin_name.contains('.') {
            // Handle nested modules like "me.my_plugin"
            let path_parts: Vec<&str> = plugin_name.split('.').collect();
            let mut path = self.plugin_dir.clone();

            // Add directory components
            if path_parts.len() > 1 {
                for part in &path_parts[..path_parts.len()-1] {
                    path.push(part);
                }
            }

            // Add the final component as the Python file
            if let Some(&last_part) = path_parts.last() {
                path.push(format!("{}.py", last_part));
            } else {
                // Fallback if somehow we have no parts (shouldn't happen)
                path.push(format!("{}.py", plugin_name));
            }
            path
        } else {
            self.plugin_dir.join(format!("{}.py", plugin_name))
        };

        if !script_path.exists() {
            return Err(format!("Plugin script not found: {}", script_path.display()).into());
        }

        debug!("Running plugin script: {}", script_path.display());

        // Prepare input JSON
        let input = PluginInput {
            lines,
            last_command: if last_command.is_empty() {
                None
            } else {
                Some(last_command.to_string())
            },
        };

        let input_json = serde_json::to_string(&input)?;

        // Execute plugin script
        let mut child = Command::new("python3")
            .arg(&script_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        // Send input JSON to plugin
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(input_json.as_bytes())?;
            stdin.write_all(b"\n")?;
        }

        // Read output from plugin
        let output = child.wait_with_output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Plugin error: {}", stderr);
            return Err(format!("Plugin execution failed: {}", stderr).into());
        }

        // Parse JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: PluginOutput = serde_json::from_str(&stdout)
            .map_err(|e| format!("Failed to parse plugin output: {}", e))?;

        debug!("Plugin returned {} lines to speak", result.speak.len());
        Ok(result.speak)
    }

    /// Check if a key has a plugin bound to it
    pub fn has_plugin(&self, key: &str) -> bool {
        self.plugins.contains_key(key)
    }

    /// Get list of all plugin keys
    pub fn plugin_keys(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }
}
