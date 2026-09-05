//! Integration tests for speech synthesis
//!
//! These exercise the real platform backend (on macOS that means spawning the
//! speech-server subprocess, which can be audible). They are opt-in: set
//! `TDSR_AUDIO_TESTS=1` to run them; otherwise each test returns immediately.

use std::sync::{Mutex, MutexGuard};
use tdsr::speech::synth::create_synth;
use tdsr::speech::Synth;
use tdsr::Result;

/// One backend at a time: the in-process espeak-ng backend loads a library
/// that is process-global and not thread-safe, and terminating it while
/// another test initialises it can hang. The guard is dropped with the synth.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A real synth and the lock that keeps other tests from creating one while
/// it lives.
struct RealSynth {
    synth: Box<dyn Synth>,
    _guard: MutexGuard<'static, ()>,
}

impl std::ops::Deref for RealSynth {
    type Target = Box<dyn Synth>;
    fn deref(&self) -> &Self::Target {
        &self.synth
    }
}

impl std::ops::DerefMut for RealSynth {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.synth
    }
}

/// Create the real synth, or `None` when audio tests are not opted in or no
/// backend is available (CI, headless).
fn real_synth() -> Option<RealSynth> {
    if std::env::var_os("TDSR_AUDIO_TESTS").is_none() {
        println!("⚠ Skipping: set TDSR_AUDIO_TESTS=1 to run real speech backend tests");
        return None;
    }
    let guard = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let result: Result<Box<dyn Synth>> = create_synth(None);
    match result {
        Ok(mut synth) => {
            // Keep the machine quiet while testing
            let _ = synth.set_volume(0);
            Some(RealSynth {
                synth,
                _guard: guard,
            })
        }
        Err(e) => {
            println!("⚠ TTS creation failed (may be expected): {}", e);
            None
        }
    }
}

#[test]
fn test_create_native_synth() {
    if let Some(synth) = real_synth() {
        println!("✓ Successfully created platform speech backend");
        drop(synth);
    }
}

#[test]
fn test_speech_configuration() {
    // Test that we can configure speech parameters
    if let Some(mut synth) = real_synth() {
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
    if let Some(mut synth) = real_synth() {
        // These operations should not error, even if speech doesn't actually play
        // (which may happen in CI or headless environments)

        // Test speaking text
        assert!(
            synth.speak("Integration test").is_ok(),
            "Should speak text without error"
        );

        // Test speaking empty string (should be no-op)
        assert!(synth.speak("").is_ok(), "Should handle empty string");

        // Test speaking letter
        assert!(
            synth.letter("a").is_ok(),
            "Should speak letter without error"
        );

        // Test cancel
        assert!(synth.cancel().is_ok(), "Should cancel without error");

        println!("✓ Speech operation tests passed");
    } else {
        println!("⚠ Skipping operation tests (TTS not available)");
    }
}

#[test]
fn test_speech_unicode() {
    // Test handling of Unicode characters
    if let Some(mut synth) = real_synth() {
        // Test various Unicode strings
        assert!(
            synth.speak("Hello 世界").is_ok(),
            "Should handle CJK characters"
        );

        assert!(synth.speak("Emoji: 🎤").is_ok(), "Should handle emoji");

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
    if let Some(mut synth) = real_synth() {
        for rate in [25, 50, 75, 100] {
            assert!(synth.set_rate(rate).is_ok(), "Should set rate to {}", rate);
        }

        println!("✓ Rate sequence test passed");
    } else {
        println!("⚠ Skipping rate sequence test (TTS not available)");
    }
}
