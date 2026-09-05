//! Configuration tests
//!
//! Every test loads from a temporary directory so nothing here touches the
//! developer's real `~/.tdsr.cfg`.

use std::path::PathBuf;
use tdsr::state::config::Config;

fn temp_config() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tdsr.cfg");
    (dir, path)
}

#[test]
fn test_missing_config_is_created_with_defaults() {
    let (_dir, path) = temp_config();
    let config = Config::load_from(path.clone()).expect("Failed to load config");
    assert!(path.is_file(), "default config should be written");

    // Default symbol mappings
    assert_eq!(config.symbols.get(&33).map(String::as_str), Some("bang"));
    assert_eq!(config.symbols.get(&36).map(String::as_str), Some("dollar"));
    assert_eq!(config.symbols.get(&64).map(String::as_str), Some("at"));

    // Default settings
    assert!(!config.process_symbols());
    assert!(config.key_echo());
    assert!(config.cursor_tracking());
    assert!(config.line_pause());
    assert_eq!(config.rate(), None);
    assert_eq!(config.voice_idx(), None);
    assert!(config.path().ends_with("tdsr.cfg"));
    assert!(config.repeated_symbols_values().contains('-'));
}

#[test]
fn test_cursor_delay_is_converted_from_milliseconds() {
    let (_dir, path) = temp_config();
    std::fs::write(&path, "[speech]\ncursor_delay = 300\n").unwrap();
    let config = Config::load_from(path).unwrap();
    assert!((config.cursor_delay() - 0.3).abs() < 1e-6);

    let (_dir, path) = temp_config();
    let config = Config::load_from(path).unwrap();
    assert!(
        (config.cursor_delay() - 0.02).abs() < 1e-6,
        "default is 20 ms"
    );
}

#[test]
fn test_set_save_and_reload_round_trip() {
    let (_dir, path) = temp_config();
    let mut config = Config::load_from(path.clone()).unwrap();
    config.set("speech", "rate", "72");
    config.set("speech", "key_echo", "false");
    config.set("speech", "voice_idx", "-1");
    config.save().unwrap();

    let reloaded = Config::load_from(path).unwrap();
    assert_eq!(reloaded.rate(), Some(72));
    assert!(!reloaded.key_echo());
    assert_eq!(reloaded.voice_idx(), None, "-1 means unset");
}

#[test]
fn test_out_of_range_values_are_ignored() {
    let (_dir, path) = temp_config();
    std::fs::write(&path, "[speech]\nrate = 150\nvolume = -5\n").unwrap();
    let config = Config::load_from(path).unwrap();
    assert_eq!(config.rate(), None);
    assert_eq!(config.volume(), None);
}

#[test]
fn test_tui_settings_defaults_and_parsing() {
    use tdsr::tui::TuiMode;

    let (_dir, path) = temp_config();
    let config = Config::load_from(path).unwrap();
    assert_eq!(config.tui_mode(), TuiMode::Auto);
    assert_eq!(config.tui_apps(), vec!["fp".to_string(), "mc".to_string()]);
    assert_eq!(config.tui_settle_ms(), 30);
    assert!(config.tui_announce());

    let (_dir, path) = temp_config();
    std::fs::write(
        &path,
        "[speech]\ntui_mode = ON\ntui_apps = mc, dialog ,\ntui_settle = 5000\ntui_announce = false\n",
    )
    .unwrap();
    let config = Config::load_from(path).unwrap();
    assert_eq!(config.tui_mode(), TuiMode::On);
    assert_eq!(
        config.tui_apps(),
        vec!["mc".to_string(), "dialog".to_string()]
    );
    assert_eq!(config.tui_settle_ms(), 1000, "settle time is capped");
    assert!(!config.tui_announce());

    let (_dir, path) = temp_config();
    std::fs::write(&path, "[speech]\ntui_mode = sometimes\n").unwrap();
    let config = Config::load_from(path).unwrap();
    assert_eq!(config.tui_mode(), TuiMode::Auto, "unknown values mean auto");
}
