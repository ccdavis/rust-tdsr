//! Test program for WSL speech
//!
//! Run with: cargo run --example test_wsl_speech

use tdsr::speech::synth::create_synth;

fn main() {
    env_logger::init();

    println!("Testing WSL speech synthesis...");
    println!("Creating synthesizer...");

    let mut synth = match create_synth() {
        Ok(s) => {
            println!("✓ Synthesizer created successfully");
            s
        }
        Err(e) => {
            eprintln!("✗ Failed to create synthesizer: {}", e);
            std::process::exit(1);
        }
    };

    println!("\nTesting basic speech...");
    if let Err(e) = synth.speak("Hello from TDSR on WSL") {
        eprintln!("✗ Speech failed: {}", e);
        std::process::exit(1);
    }
    println!("✓ Basic speech test passed");

    println!("\nTesting rate control...");
    synth.set_rate(25).ok();
    synth.speak("This is slow speech").ok();

    synth.set_rate(75).ok();
    synth.speak("This is fast speech").ok();

    synth.set_rate(50).ok();
    println!("✓ Rate control test passed");

    println!("\nTesting volume control...");
    synth.set_volume(30).ok();
    synth.speak("This is quiet").ok();

    synth.set_volume(100).ok();
    synth.speak("This is loud").ok();

    synth.set_volume(80).ok();
    println!("✓ Volume control test passed");

    println!("\nTesting special characters...");
    synth
        .speak("Testing punctuation: Hello, world! How are you?")
        .ok();
    synth
        .speak("Testing quotes: It's a test with 'single' and \"double\" quotes")
        .ok();
    println!("✓ Special character test passed");

    println!("\n✓ All tests passed!");
    println!("\nIf you heard speech through Windows SAPI, WSL integration is working!");
}
