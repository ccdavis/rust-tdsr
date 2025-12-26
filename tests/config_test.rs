//! Configuration loading tests
//!
//! Tests that screen reader configuration loads correctly
//! and provides expected default values

use tdsr::state::config::Config;

#[test]
fn test_config_loads_successfully() {
    // Load or create config
    let config = Config::load().expect("Failed to load config");

    // Test default symbol mappings exist (these should always be present)
    assert!(config.symbols.contains_key(&33)); // ! -> bang
    assert!(config.symbols.contains_key(&36)); // $ -> dollar
    assert!(config.symbols.contains_key(&64)); // @ -> at

    // Test that boolean settings are accessible (actual values depend on user config)
    // Just verify they don't panic
    let _ = config.process_symbols();
    let _ = config.key_echo();
    let _ = config.cursor_tracking();
    let _ = config.line_pause();
}

#[test]
fn test_config_methods() {
    let config = Config::load().expect("Failed to load config");

    // Test that config path is available
    assert!(config.path().to_str().unwrap().contains(".tdsr.cfg"));

    // Test symbol access
    let symbols = &config.symbols;
    assert!(!symbols.is_empty());

    // Test repeated symbols config
    let repeated = config.repeated_symbols_values();
    assert!(repeated.contains('-') || repeated.contains('='));
}

#[test]
fn test_cursor_delay() {
    let config = Config::load().expect("Failed to load config");

    // Default cursor delay should be 0.02 seconds (20ms)
    let delay = config.cursor_delay();
    assert!(delay > 0.0 && delay < 1.0);
}
