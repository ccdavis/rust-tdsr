//! Plugin system for extending screen reader functionality
//!
//! Plugins are external scripts/programs that analyze terminal output
//! and provide additional speech feedback. They receive screen lines
//! as input and return lines to speak.

use crate::terminal::Screen;
use crate::{Result, TdsrError};
use log::{debug, error, warn};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

/// How long a plugin may run before it is killed. Plugins run synchronously
/// on the event loop (keyboard and speech are blocked meanwhile), so a hung
/// plugin must not be allowed to hang the screen reader.
pub const PLUGIN_TIMEOUT: Duration = Duration::from_secs(10);

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
        let prompt_regex = Regex::new(prompt_pattern).unwrap_or_else(|_| {
            // This should never fail as ".*" is always valid
            Regex::new(".*").expect("Failed to compile fallback regex")
        });

        let mut plugin_configs = HashMap::new();

        for (name, key) in plugins {
            let command_filter = plugin_commands
                .get(&name)
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

        debug!(
            "Plugin manager initialized with {} plugins",
            plugin_configs.len()
        );

        Ok(Self {
            plugins: plugin_configs,
            plugin_dir,
            prompt_regex,
        })
    }

    /// Execute a plugin by keyboard shortcut, synchronously.
    ///
    /// Collects screen lines, runs the plugin, and returns text to speak.
    /// Blocks for up to [`PLUGIN_TIMEOUT`]; the event loop uses
    /// [`Self::execute_plugin_async`] instead.
    pub fn execute_plugin(
        &self,
        key: &str,
        screen: &Screen,
        last_command: &str,
    ) -> Result<Vec<String>> {
        let Some(job) = self.prepare(key, screen, last_command)? else {
            return Ok(vec![]);
        };
        job.run()
    }

    /// Execute a plugin by keyboard shortcut on a worker thread.
    ///
    /// The result (lines to speak, or an error) is sent to `results` when
    /// the plugin finishes or times out, so the event loop keeps handling
    /// keys and output meanwhile. Returns `Ok(false)` if the plugin's
    /// command filter rejected the last command (nothing was started).
    pub fn execute_plugin_async(
        &self,
        key: &str,
        screen: &Screen,
        last_command: &str,
        results: Sender<Result<Vec<String>>>,
    ) -> Result<bool> {
        let Some(job) = self.prepare(key, screen, last_command)? else {
            return Ok(false);
        };
        std::thread::Builder::new()
            .name(format!("tdsr-plugin-{}", job.name))
            .spawn(move || {
                let _ = results.send(job.run());
            })
            .map_err(|e| TdsrError::Plugin(format!("Failed to start plugin thread: {}", e)))?;
        Ok(true)
    }

    /// Everything a plugin run needs, resolved on the main thread so the
    /// worker doesn't touch the screen or the manager.
    fn prepare(&self, key: &str, screen: &Screen, last_command: &str) -> Result<Option<PluginJob>> {
        let plugin = self
            .plugins
            .get(key)
            .ok_or_else(|| TdsrError::Plugin(format!("Plugin not found for key: {}", key)))?;

        debug!("Executing plugin: {}", plugin.name);

        // Check command filter if configured
        if let Some(ref filter) = plugin.command_filter {
            if !last_command.is_empty() && !filter.is_match(last_command) {
                debug!("Command filter did not match, skipping plugin");
                return Ok(None);
            }
        }

        let command = self.resolve_plugin(&plugin.name)?;
        let lines = self.collect_screen_lines(screen, last_command);
        Ok(Some(PluginJob {
            name: plugin.name.clone(),
            command,
            input: PluginInput {
                lines,
                last_command: (!last_command.is_empty()).then(|| last_command.to_string()),
            },
        }))
    }

    /// Collect screen lines from bottom up until prompt is found
    fn collect_screen_lines(&self, screen: &Screen, last_command: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let (_, rows) = screen.size;

        // Collect lines from bottom to top
        for y in (0..rows).rev() {
            let line = screen.get_line_trimmed(y);

            // Stop if we hit the prompt line
            let at_prompt = self.prompt_regex.is_match(&line)
                && !last_command.is_empty()
                && line.contains(last_command);
            lines.push(line);
            if at_prompt {
                break;
            }
        }

        lines
    }

    /// Locate a plugin and decide how to run it.
    ///
    /// `name` may use dots for subdirectories ("me.my_plugin" ->
    /// `me/my_plugin`). The first match wins:
    /// 1. `<dir>/<name>` — an executable of any kind (shell, Rust, ...),
    ///    run directly via its shebang or binary format;
    /// 2. `<dir>/<name>.py` — run with `python3`.
    fn resolve_plugin(&self, name: &str) -> Result<Command> {
        let mut base = self.plugin_dir.clone();
        for part in name.split('.') {
            base.push(part);
        }

        if is_executable(&base) {
            debug!("Running plugin executable: {}", base.display());
            return Ok(Command::new(&base));
        }

        let script = base.with_extension("py");
        if script.is_file() {
            debug!("Running plugin script: python3 {}", script.display());
            let mut cmd = Command::new("python3");
            cmd.arg(&script);
            return Ok(cmd);
        }

        Err(TdsrError::Plugin(format!(
            "Plugin not found: {} (looked for an executable or {})",
            base.display(),
            script.display()
        )))
    }

    /// Check if a key has a plugin bound to it
    pub fn has_plugin(&self, key: &str) -> bool {
        self.plugins.contains_key(key)
    }

    /// Run a plugin by name (test hook; `execute_plugin` is the keyed entry)
    #[doc(hidden)]
    pub fn run_by_name(&self, name: &str, lines: Vec<String>) -> Result<Vec<String>> {
        PluginJob {
            name: name.to_string(),
            command: self.resolve_plugin(name)?,
            input: PluginInput {
                lines,
                last_command: None,
            },
        }
        .run()
    }

    /// Get list of all plugin keys
    pub fn plugin_keys(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }
}

/// A resolved plugin invocation, safe to run on any thread.
struct PluginJob {
    name: String,
    command: Command,
    input: PluginInput,
}

impl PluginJob {
    /// Run the plugin as a subprocess.
    ///
    /// Passes screen lines as JSON input, reads speech output. The plugin is
    /// killed if it runs longer than [`PLUGIN_TIMEOUT`].
    fn run(self) -> Result<Vec<String>> {
        let PluginJob {
            name: plugin_name,
            mut command,
            input,
        } = self;
        let plugin_name = plugin_name.as_str();

        let input_json = serde_json::to_string(&input)?;

        // Execute plugin
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| TdsrError::Plugin(format!("Failed to start plugin: {}", e)))?;
        let pid = child.id();

        // Send input JSON to plugin, then close its stdin so it sees EOF
        if let Some(mut stdin) = child.stdin.take() {
            // A plugin that exits without reading is not an error here.
            let _ = stdin.write_all(input_json.as_bytes());
            let _ = stdin.write_all(b"\n");
        }

        // Collect output on a helper thread so a plugin that floods stdout
        // can't deadlock us, and so we can enforce the timeout from here.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });

        let output = match rx.recv_timeout(PLUGIN_TIMEOUT) {
            Ok(result) => result?,
            Err(_) => {
                warn!(
                    "Plugin {} exceeded {:?}; killing it",
                    plugin_name, PLUGIN_TIMEOUT
                );
                // Safety: plain kill(2) on a pid we spawned; if it already
                // exited the call simply fails.
                unsafe {
                    nix::libc::kill(pid as nix::libc::pid_t, nix::libc::SIGKILL);
                }
                // The helper thread finishes reaping it.
                let _ = rx.recv();
                return Err(TdsrError::Plugin(format!(
                    "Plugin {} timed out after {} seconds",
                    plugin_name,
                    PLUGIN_TIMEOUT.as_secs()
                )));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            error!("Plugin error: {}", stderr);
            return Err(TdsrError::Plugin(format!(
                "Plugin execution failed: {}",
                stderr.trim()
            )));
        }

        // Parse JSON output
        let stdout = String::from_utf8_lossy(&output.stdout);
        let result: PluginOutput = serde_json::from_str(&stdout)
            .map_err(|e| TdsrError::Plugin(format!("Failed to parse plugin output: {}", e)))?;

        debug!("Plugin returned {} lines to speak", result.speak.len());
        Ok(result.speak)
    }
}

/// Whether `path` is a regular file with any execute bit set.
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn manager(dir: &Path) -> PluginManager {
        let mut plugins = HashMap::new();
        plugins.insert("echo".to_string(), "e".to_string());
        plugins.insert("pyecho".to_string(), "p".to_string());
        plugins.insert("hang".to_string(), "h".to_string());
        PluginManager::new(plugins, HashMap::new(), dir.to_path_buf(), ".*").unwrap()
    }

    fn write_exec(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn runs_shell_executable_directly() {
        let dir = tempfile::tempdir().unwrap();
        write_exec(
            &dir.path().join("echo"),
            "#!/bin/sh\nread -r line\nprintf '{\"speak\": [\"from shell\"]}'\n",
        );
        let out = manager(dir.path())
            .run_by_name("echo", vec!["x".into()])
            .unwrap();
        assert_eq!(out, vec!["from shell".to_string()]);
    }

    #[test]
    fn runs_python_script_when_no_executable() {
        if Command::new("python3").arg("--version").output().is_err() {
            return; // no python3 on this machine
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pyecho.py"),
            "import json,sys\nd=json.load(sys.stdin)\nprint(json.dumps({'speak':[str(len(d['lines']))]}))\n",
        )
        .unwrap();
        let out = manager(dir.path())
            .run_by_name("pyecho", vec!["a".into(), "b".into()])
            .unwrap();
        assert_eq!(out, vec!["2".to_string()]);
    }

    #[test]
    fn async_execution_delivers_result_on_channel() {
        let dir = tempfile::tempdir().unwrap();
        write_exec(
            &dir.path().join("echo"),
            "#!/bin/sh\nread -r line\nsleep 0.2\nprintf '{\"speak\": [\"later\"]}'\n",
        );
        let pm = manager(dir.path());
        let screen = Screen::new(10, 2);
        let (tx, rx) = mpsc::channel();
        let started = std::time::Instant::now();
        assert!(pm.execute_plugin_async("e", &screen, "", tx).unwrap());
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "must not block"
        );
        let result = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
        assert_eq!(result, vec!["later".to_string()]);
    }

    #[test]
    fn missing_plugin_is_a_plugin_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = manager(dir.path()).run_by_name("nope", vec![]).unwrap_err();
        assert!(matches!(err, TdsrError::Plugin(_)), "{:?}", err);
    }

    #[test]
    fn failing_plugin_reports_stderr() {
        let dir = tempfile::tempdir().unwrap();
        write_exec(
            &dir.path().join("echo"),
            "#!/bin/sh\necho boom >&2\nexit 3\n",
        );
        let err = manager(dir.path()).run_by_name("echo", vec![]).unwrap_err();
        assert!(err.to_string().contains("boom"), "{}", err);
    }
}
