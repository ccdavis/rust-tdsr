//! Integration tests for speech synthesis
//!
//! These tests verify that the native TTS backend works correctly
//! across different operations and configurations.

use tdsr::speech::synth::create_synth;

#[test]
fn test_create_native_synth() {
    // Test that we can create a native TTS synthesizer
    let result = create_synth();

    match result {
        Ok(synth) => {
            println!("✓ Successfully created native TTS backend");
            drop(synth);
        }
        Err(e) => {
            // This may fail in CI or environments without speech-dispatcher
            println!("⚠ TTS creation failed (may be expected): {}", e);
            // Don't panic - this is acceptable in headless environments
        }
    }
}

#[test]
fn test_speech_configuration() {
    // Test that we can configure speech parameters
    let result = create_synth();

    if let Ok(mut synth) = result {
        // Test rate setting
        assert!(synth.set_rate(50).is_ok(), "Should set rate to 50");
        assert!(synth.set_rate(0).is_ok(), "Should set rate to 0");
        assert!(synth.set_rate(100).is_ok(), "Should set rate to 100");

        // Test volume setting
        assert!(synth.set_volume(50).is_ok(), "Should set volume to 50");
        assert!(synth.set_volume(0).is_ok(), "Should set volume to 0");
        assert!(synth.set_volume(100).is_ok(), "Should set volume to 100");

        // Test voice index (may not work on all platforms)
        let voice_result = synth.set_voice_idx(0);
        println!("Voice index setting result: {:?}", voice_result);

        println!("✓ Speech configuration tests passed");
    } else {
        println!("⚠ Skipping configuration tests (TTS not available)");
    }
}

#[test]
fn test_speech_operations() {
    // Test that we can perform basic speech operations
    let result = create_synth();

    if let Ok(mut synth) = result {
        // These operations should not error, even if speech doesn't actually play
        // (which may happen in CI or headless environments)

        // Test speaking text
        assert!(
            synth.speak("Integration test").is_ok(),
            "Should speak text without error"
        );

        // Test speaking empty string (should be no-op)
        assert!(
            synth.speak("").is_ok(),
            "Should handle empty string"
        );

        // Test speaking letter
        assert!(
            synth.letter("a").is_ok(),
            "Should speak letter without error"
        );

        // Test cancel
        assert!(
            synth.cancel().is_ok(),
            "Should cancel without error"
        );

        println!("✓ Speech operation tests passed");
    } else {
        println!("⚠ Skipping operation tests (TTS not available)");
    }
}

#[test]
fn test_speech_unicode() {
    // Test handling of Unicode characters
    let result = create_synth();

    if let Ok(mut synth) = result {
        // Test various Unicode strings
        assert!(
            synth.speak("Hello 世界").is_ok(),
            "Should handle CJK characters"
        );

        assert!(
            synth.speak("Emoji: 🎤").is_ok(),
            "Should handle emoji"
        );

        assert!(
            synth.speak("Accents: café naïve").is_ok(),
            "Should handle accented characters"
        );

        println!("✓ Unicode speech tests passed");
    } else {
        println!("⚠ Skipping Unicode tests (TTS not available)");
    }
}

#[test]
fn test_speech_rate_sequence() {
    // Test changing rate multiple times
    let result = create_synth();

    if let Ok(mut synth) = result {
        for rate in [25, 50, 75, 100] {
            assert!(
                synth.set_rate(rate).is_ok(),
                "Should set rate to {}",
                rate
            );
        }

        println!("✓ Rate sequence test passed");
    } else {
        println!("⚠ Skipping rate sequence test (TTS not available)");
    }
}
