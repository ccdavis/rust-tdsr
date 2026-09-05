//! PulseAudio backend using espeak-ng
//!
//! This backend is designed for WSL with WSLG support, where PulseAudio
//! is available through /mnt/wslg/PulseServer. It uses espeak-ng for
//! text-to-speech synthesis. It is also the fallback on native Linux when
//! Speech Dispatcher is unavailable.
//!
//! # Process model
//!
//! Each batch of queued text is spoken by its own short-lived `espeak-ng`
//! process. A worker thread writes the batch to the process's stdin, closes
//! the pipe (so espeak-ng exits once it has spoken everything) and waits for
//! it; text queued meanwhile is spoken by the next process, in order.
//! `cancel` kills the current process and drops the queue. Nothing is ever
//! written to the synthesizer from the event-loop thread, so a stalled audio
//! server cannot block the keyboard.
//!
//! One process per utterance is deliberate, and matters on WSL. WSLg's
//! PulseAudio sink (`module-rdp-sink`) keeps pushing audio to Windows for as
//! long as any playback stream is open, and the amount in flight ratchets up
//! with every late wakeup of its thread (measured at roughly 50 ms per
//! second, saturating near 1.3 s). Only an idle sink, one with no streams at
//! all, resets it. Letting espeak-ng exit after each utterance gives the sink
//! that idle moment, so a key echo typed a moment later is not queued behind
//! a second of buffered silence.
//!
//! The same sink also fails to reset its pacing clock when it resumes from
//! suspend (5 s idle), so the first playback stream after a pause makes it
//! burst a backlog to the client and every following utterance is about a
//! second late. Resuming it through its monitor source instead (a brief
//! `parec`) lets it catch up silently; [`wake_sink`] does that on WSL before
//! starting speech after a long enough silence.
//!
//! Dependencies:
//! - espeak-ng (install with: sudo apt install espeak-ng)
//! - PulseAudio client libraries (usually pre-installed with WSLG)
//! - parec from pulseaudio-utils, for the WSL sink wake-up (optional)

use crate::platform::is_wsl;
use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, info, warn};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Silence after which the WSLg sink is woken through its monitor before
/// speaking. PulseAudio's module-suspend-on-idle suspends a sink 5 s after
/// its last stream closes; waking a sink that is merely idle is harmless.
const SINK_SUSPEND_GUARD: Duration = Duration::from_secs(4);

/// How long the monitor recording is held open during a wake-up. The sink
/// resumes as soon as the recording stream is created; this leaves margin
/// for parec to start and connect.
const SINK_WAKE_HOLD: Duration = Duration::from_millis(50);

/// espeak-ng settings applied to each spawned process.
#[derive(Clone, Debug)]
struct Settings {
    /// Path to espeak-ng
    espeak_path: String,
    /// TDSR rate (0-100)
    rate: u8,
    /// TDSR volume (0-100)
    volume: u8,
    /// espeak-ng voice name
    voice: String,
}

/// One queued line of speech.
#[derive(Clone, Debug, PartialEq)]
struct Utterance {
    text: String,
    is_letter: bool,
}

impl Utterance {
    /// The line written to espeak-ng. Letters are padded with spaces so
    /// espeak-ng reads them as letter names; embedded line breaks in text
    /// are flattened so one utterance stays one line.
    fn line(&self) -> String {
        if self.is_letter {
            format!(" {} ", self.text)
        } else {
            self.text.replace(['\n', '\r'], " ")
        }
    }
}

/// State shared between the synth and its worker thread.
struct Inner {
    /// Lines waiting for the next espeak-ng process
    queue: Vec<Utterance>,
    /// PID of the espeak-ng process currently speaking, if any
    current_pid: Option<u32>,
    settings: Settings,
    /// Set by `Drop`; the worker exits when it sees it
    shutdown: bool,
    /// When the last espeak-ng process exited, i.e. when its playback
    /// stream closed and the sink became idle
    last_stream_end: Option<Instant>,
    /// Wake the sink through its monitor before speaking after a pause
    /// (WSL with parec available)
    wake_sink: bool,
}

struct Shared {
    inner: Mutex<Inner>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// PulseAudio backend: one espeak-ng process per utterance, driven by a
/// worker thread
pub struct PulseAudioSynth {
    shared: Arc<Shared>,
}

/// Setup PulseAudio server environment
///
/// Auto-detects WSLG PulseAudio server and sets PULSE_SERVER if needed.
/// Returns error with helpful message if PulseAudio is not available.
/// Shared with the in-process espeak-ng backend.
pub(crate) fn setup_pulseaudio() -> Result<()> {
    const WSLG_PULSE_PATH: &str = "/mnt/wslg/PulseServer";

    // Check if PULSE_SERVER is already set
    if std::env::var("PULSE_SERVER").is_ok() {
        debug!("PULSE_SERVER already set via environment");
        return Ok(());
    }

    // Try to auto-detect WSLG PulseAudio server
    if std::path::Path::new(WSLG_PULSE_PATH).exists() {
        info!(
            "Auto-detected WSLG PulseAudio server at {}",
            WSLG_PULSE_PATH
        );
        std::env::set_var("PULSE_SERVER", WSLG_PULSE_PATH);
        return Ok(());
    }

    // PulseAudio not found - provide helpful error message only on WSL
    if is_wsl() {
        warn!("WSLG PulseAudio server not found at {}", WSLG_PULSE_PATH);
        warn!("Make sure WSLg is installed and running");
        warn!("You can also set the PULSE_SERVER environment variable:");
        warn!("  export PULSE_SERVER=/path/to/pulseaudio");
        return Err(TdsrError::Speech(
            "PulseAudio server not found. Install WSLg or set PULSE_SERVER environment variable."
                .to_string(),
        ));
    }

    // On native Linux, PulseAudio might be available via default socket
    // Let espeak-ng try to connect - it will fail if not available
    debug!("Running on native Linux - PulseAudio will use default configuration");
    Ok(())
}

/// Convert TDSR rate (0-100) to espeak speed (80-450 wpm)
pub(crate) fn wpm_for_rate(tdsr_rate: u8) -> u16 {
    // TDSR 0 = 80 wpm (very slow)
    // TDSR 50 = 265 wpm
    // TDSR 100 = 450 wpm (very fast)
    80 + ((tdsr_rate as u16) * 370 / 100)
}

/// espeak-ng voice name for a TDSR voice index
///
/// Indices 0-9: espeak-ng built-in voices (always available)
/// Indices 10+: MBROLA voices (require mbrola + voice data packages)
pub(crate) fn espeak_voice_name(idx: usize) -> &'static str {
    const VOICES: &[&str] = &[
        "en",     // 0: Default English
        "en-us",  // 1: US English
        "en-gb",  // 2: British English
        "en-sc",  // 3: Scottish English
        "es",     // 4: Spanish
        "fr",     // 5: French
        "de",     // 6: German
        "it",     // 7: Italian
        "pt",     // 8: Portuguese
        "ru",     // 9: Russian
        "mb-us1", // 10: MBROLA US English Female (apt: mbrola mbrola-us1)
        "mb-us2", // 11: MBROLA US English Male (apt: mbrola mbrola-us2)
        "mb-us3", // 12: MBROLA US English Male 2 (apt: mbrola mbrola-us3)
        "mb-en1", // 13: MBROLA British English Male (apt: mbrola mbrola-en1)
    ];

    VOICES.get(idx).unwrap_or(&"en")
}

impl PulseAudioSynth {
    /// Create a new PulseAudio synthesizer
    ///
    /// Verifies espeak-ng and PulseAudio are available and starts the
    /// worker thread.
    pub fn new() -> Result<Self> {
        debug!("Creating PulseAudio backend");

        // Setup PulseAudio environment
        setup_pulseaudio()?;

        // Find espeak-ng
        let espeak_path = Self::find_espeak()?;
        debug!("Found espeak-ng at: {}", espeak_path);

        // The sink wake-up only matters for WSLg's RDP sink, and needs parec.
        let wake_sink = is_wsl() && Self::have_program("parec");
        if is_wsl() && !wake_sink {
            info!("parec not found (pulseaudio-utils); speech after a pause may lag on WSLg");
        }

        let shared = Arc::new(Shared {
            inner: Mutex::new(Inner {
                queue: Vec::new(),
                current_pid: None,
                settings: Settings {
                    espeak_path,
                    rate: 50,                // Default rate
                    volume: 80,              // Default volume
                    voice: "en".to_string(), // Default English voice
                },
                shutdown: false,
                last_stream_end: None,
                wake_sink,
            }),
            wake: Condvar::new(),
        });

        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("espeak-ng".to_string())
            .spawn(move || worker(worker_shared))
            .map_err(|e| TdsrError::Speech(format!("Failed to start speech worker: {}", e)))?;

        Ok(Self { shared })
    }

    /// Find espeak-ng executable
    fn find_espeak() -> Result<String> {
        let paths = vec!["espeak-ng", "/usr/bin/espeak-ng"];

        for path in paths {
            if Self::have_program(path) {
                return Ok(path.to_string());
            }
        }

        Err(TdsrError::Speech(
            "espeak-ng not found. Install with: sudo apt install espeak-ng".to_string(),
        ))
    }

    /// Whether `program --version` runs successfully.
    fn have_program(program: &str) -> bool {
        Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Convert TDSR rate (0-100) to espeak speed (80-450 wpm)
    fn rate_to_espeak_speed(tdsr_rate: u8) -> u16 {
        wpm_for_rate(tdsr_rate)
    }

    /// Convert TDSR volume (0-100) to espeak amplitude (0-100)
    fn volume_to_espeak_amplitude(tdsr_volume: u8) -> u8 {
        // Direct mapping: TDSR 0-100 → espeak 0-100
        // espeak-ng's default amplitude is 100; values above 100 cause
        // clipping and distortion (scratchy/static audio)
        tdsr_volume
    }

    /// Get voice name by index (see `espeak_voice_name`)
    fn get_voice_by_idx(idx: usize) -> &'static str {
        espeak_voice_name(idx)
    }

    /// Command-line arguments for one espeak-ng process.
    ///
    /// A batch of nothing but letters (key echo) gets `-z`, which drops the
    /// 120 ms end-of-text pause espeak-ng otherwise appends: the letter is
    /// heard just the same, the process exits sooner and the sink goes idle
    /// sooner. Text keeps the pause so consecutive lines do not run together.
    fn espeak_args(settings: &Settings, letters_only: bool) -> Vec<String> {
        let mut args = vec![
            "-v".to_string(),
            settings.voice.clone(),
            "-s".to_string(),
            Self::rate_to_espeak_speed(settings.rate).to_string(),
            "-a".to_string(),
            Self::volume_to_espeak_amplitude(settings.volume).to_string(),
        ];
        if letters_only {
            args.push("-z".to_string());
        }
        args
    }

    /// Queue one line and wake the worker.
    fn enqueue(&mut self, text: &str, is_letter: bool) {
        if text.is_empty() {
            return;
        }
        let mut inner = self.shared.lock();
        inner.queue.push(Utterance {
            text: text.to_string(),
            is_letter,
        });
        drop(inner);
        self.shared.wake.notify_one();
    }

    /// Change a setting; it applies to the next espeak-ng process.
    fn update_settings(&mut self, f: impl FnOnce(&mut Settings)) {
        let mut inner = self.shared.lock();
        f(&mut inner.settings);
    }
}

/// Start espeak-ng for one batch. Returns the child and its stdin, which the
/// caller writes to outside the lock.
fn spawn_espeak(settings: &Settings, letters_only: bool) -> Result<Child> {
    let mut cmd = Command::new(&settings.espeak_path);
    cmd.args(PulseAudioSynth::espeak_args(settings, letters_only))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Increase PulseAudio buffer to reduce crackling/static artifacts,
    // especially on WSLG where the audio transport adds latency
    cmd.env("PULSE_LATENCY_MSEC", "60");

    cmd.spawn()
        .map_err(|e| TdsrError::Speech(format!("Failed to start espeak-ng: {}", e)))
}

/// Resume the sink through its monitor source so it catches up without
/// sending anything to the client (see the module docs). Returns false if
/// parec could not be started.
fn wake_sink() -> bool {
    let started = Instant::now();
    let child = Command::new("parec")
        .args(["-d", "@DEFAULT_MONITOR@", "--raw"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            warn!("Could not start parec to wake the audio sink: {}", e);
            return false;
        }
    };
    thread::sleep(SINK_WAKE_HOLD);
    let _ = child.kill();
    let _ = child.wait();
    debug!("Woke audio sink in {:?}", started.elapsed());
    true
}

/// Worker thread: speaks queued batches one espeak-ng process at a time.
fn worker(shared: Arc<Shared>) {
    loop {
        // Wait for work, and decide whether the sink needs waking first.
        let needs_wake = {
            let mut inner = shared.lock();
            while inner.queue.is_empty() && !inner.shutdown {
                inner = shared.wake.wait(inner).unwrap_or_else(|e| e.into_inner());
            }
            if inner.shutdown {
                return;
            }
            inner.wake_sink
                && inner
                    .last_stream_end
                    .map_or(true, |t| t.elapsed() >= SINK_SUSPEND_GUARD)
        };

        // Done outside the lock: it takes tens of milliseconds.
        if needs_wake && !wake_sink() {
            shared.lock().wake_sink = false;
        }

        // Take the batch and start its process. The PID is published under
        // the lock so cancel() can kill it as soon as it exists.
        let (mut child, batch) = {
            let mut inner = shared.lock();
            if inner.queue.is_empty() {
                // Cancelled while the sink was being woken
                continue;
            }
            let batch = std::mem::take(&mut inner.queue);
            let letters_only = batch.iter().all(|u| u.is_letter);
            match spawn_espeak(&inner.settings, letters_only) {
                Ok(child) => {
                    inner.current_pid = Some(child.id());
                    (child, batch)
                }
                Err(e) => {
                    warn!("{}", e);
                    continue;
                }
            }
        };

        // Write the batch and close the pipe so espeak-ng exits when done.
        // A write error means the process is gone (killed by cancel, or
        // failed to start); the wait below reaps it either way.
        if let Some(mut stdin) = child.stdin.take() {
            for utterance in &batch {
                if writeln!(stdin, "{}", utterance.line()).is_err() {
                    debug!("espeak-ng exited before its batch was written");
                    break;
                }
            }
        }

        match child.wait() {
            Ok(status) if !status.success() => {
                debug!("espeak-ng exited with {}", status);
            }
            Err(e) => warn!("Waiting for espeak-ng failed: {}", e),
            _ => {}
        }

        let mut inner = shared.lock();
        inner.current_pid = None;
        inner.last_stream_end = Some(Instant::now());
    }
}

impl Synth for PulseAudioSynth {
    fn send(&mut self, cmd: SpeechCommand) -> Result<()> {
        match cmd {
            SpeechCommand::Speak(text) => self.speak(&text),
            SpeechCommand::Letter(ch) => self.letter(&ch.to_string()),
            SpeechCommand::Cancel => self.cancel(),
            SpeechCommand::SetRate(rate) => self.set_rate(rate),
            SpeechCommand::SetVolume(vol) => self.set_volume(vol),
            SpeechCommand::SetVoiceIdx(idx) => self.set_voice_idx(idx),
        }
    }

    fn set_rate(&mut self, rate: u8) -> Result<()> {
        debug!("Setting rate to {}", rate);
        self.update_settings(|s| s.rate = rate);
        Ok(())
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        debug!("Setting volume to {}", volume);
        self.update_settings(|s| s.volume = volume);
        Ok(())
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let voice = Self::get_voice_by_idx(idx);
        debug!("Setting voice to {} (index {})", voice, idx);
        self.update_settings(|s| s.voice = voice.to_string());
        Ok(())
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        debug!("Speaking: {}", text);
        self.enqueue(text, false);
        Ok(())
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        debug!("Speaking letter: {}", text);
        self.enqueue(text, true);
        Ok(())
    }

    /// Drop queued text and kill the process speaking now. The worker reaps
    /// it; the next utterance gets a fresh process.
    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        let mut inner = self.shared.lock();
        inner.queue.clear();
        if let Some(pid) = inner.current_pid {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
        Ok(())
    }
}

impl Drop for PulseAudioSynth {
    fn drop(&mut self) {
        debug!("Shutting down PulseAudio backend");
        let mut inner = self.shared.lock();
        inner.shutdown = true;
        inner.queue.clear();
        if let Some(pid) = inner.current_pid {
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
        }
        drop(inner);
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            espeak_path: "espeak-ng".to_string(),
            rate: 50,
            volume: 80,
            voice: "en-us".to_string(),
        }
    }

    #[test]
    fn test_rate_conversion() {
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(0), 80); // Slowest
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(50), 265); // Normal
        assert_eq!(PulseAudioSynth::rate_to_espeak_speed(100), 450); // Fastest
    }

    #[test]
    fn test_volume_conversion() {
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(0), 0);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(50), 50);
        assert_eq!(PulseAudioSynth::volume_to_espeak_amplitude(100), 100);
    }

    #[test]
    fn test_voice_selection() {
        assert_eq!(PulseAudioSynth::get_voice_by_idx(0), "en");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(1), "en-us");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(2), "en-gb");
        assert_eq!(PulseAudioSynth::get_voice_by_idx(999), "en"); // Out of range defaults to en
    }

    #[test]
    fn test_espeak_args_for_text() {
        let args = PulseAudioSynth::espeak_args(&settings(), false);
        assert_eq!(args, ["-v", "en-us", "-s", "265", "-a", "80"]);
    }

    #[test]
    fn test_letter_batches_skip_final_pause() {
        let args = PulseAudioSynth::espeak_args(&settings(), true);
        assert_eq!(args, ["-v", "en-us", "-s", "265", "-a", "80", "-z"]);
    }

    #[test]
    fn test_utterance_lines() {
        let letter = Utterance {
            text: "a".to_string(),
            is_letter: true,
        };
        assert_eq!(letter.line(), " a ");
        let text = Utterance {
            text: "one\ntwo\r\nthree".to_string(),
            is_letter: false,
        };
        assert_eq!(text.line(), "one two  three");
    }

    #[test]
    fn test_create_pulseaudio_synth() {
        match PulseAudioSynth::new() {
            Ok(_) => println!("✓ PulseAudio backend available"),
            Err(e) => println!("⚠ PulseAudio backend not available: {}", e),
        }
    }
}
