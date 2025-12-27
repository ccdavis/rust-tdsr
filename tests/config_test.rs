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

    // Cursor delay should be returned in seconds
    // Config stores milliseconds, getter converts to seconds
    let delay = config.cursor_delay();

    // Default is 20ms = 0.02 seconds
    // Should be a reasonable value (less than 1 second)
    assert!(
        delay > 0.0 && delay < 1.0,
        "cursor_delay should be in seconds (0 < {} < 1)",
        delay
    );

    // Verify it's not the raw milliseconds (would be >= 1.0 for any practical value)
    // If config has cursor_delay=300, it should return 0.3, not 300.0
    assert!(
        delay < 0.5,
        "cursor_delay {} seems too high - may not be converting ms to seconds",
        delay
    );
}
