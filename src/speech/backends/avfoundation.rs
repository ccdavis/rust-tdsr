//! macOS AVFoundation speech backend, run as a subprocess.
//!
//! `AVSpeechSynthesizer` only advances its utterance queue and renders audio
//! reliably while a CFRunLoop is serviced on the process's **main thread**.
//! TDSR's main thread parks in mio's kqueue `poll()` and never runs a run loop,
//! so driving the synth there (directly or from a worker thread) plays only the
//! first utterance and then goes silent.
//!
//! The original Python TDSR solved this by running the macOS synth in a
//! separate process whose entire job was `AppHelper.runConsoleEventLoop()` — a
//! live Cocoa run loop on that process's main thread. This backend reproduces
//! that design: it re-execs our own binary as `tdsr --speech-server`, and that
//! child runs `AVSpeechSynthesizer` on its main thread with a real
//! `CFRunLoopRun()`. The parent talks to it over a simple line protocol on the
//! child's stdin:
//!
//! ```text
//!   s<text>\n   speak text
//!   l<text>\n   speak a letter (same handling as speak)
//!   x\n         cancel/silence immediately
//!   r<0-100>\n  set rate
//!   v<0-100>\n  set volume
//!   V<idx>\n    set voice index
//! ```
//!
//! Flood safety: parent writes are small and the child's stdin reader drains
//! into an unbounded channel, so a terminal dumping a huge amount of text can't
//! block the parent's main thread or the keyboard. A keystroke already sends
//! `x` (cancel) upstream, and the child coalesces its backlog so that cancel
//! takes effect immediately instead of waiting behind queued utterances.

use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use log::{debug, info};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// CLI flag that puts the binary into speech-server mode.
pub const SPEECH_SERVER_FLAG: &str = "--speech-server";

// ===========================================================================
// Parent side: spawns and talks to the speech-server subprocess.
// ===========================================================================

/// Speech synthesizer that drives a `tdsr --speech-server` child process.
pub struct AvServerSynth {
    /// Path to our own executable, re-execed in speech-server mode.
    exe: PathBuf,
    child: Option<Child>,
    // Cached settings so they can be re-applied if the child is respawned.
    rate: Option<u8>,
    volume: Option<u8>,
    voice_idx: Option<usize>,
}

impl AvServerSynth {
    pub fn new() -> Result<Self> {
        let exe = std::env::current_exe().map_err(|e| {
            TdsrError::Speech(format!("Cannot locate current executable for speech server: {}", e))
        })?;
        let mut synth = Self {
            exe,
            child: None,
            rate: None,
            volume: None,
            voice_idx: None,
        };
        synth.spawn()?;
        info!("AVFoundation speech server started: {:?}", synth.exe);
        Ok(synth)
    }

    fn spawn(&mut self) -> Result<()> {
        let child = Command::new(&self.exe)
            .arg(SPEECH_SERVER_FLAG)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| TdsrError::Speech(format!("Failed to spawn speech server: {}", e)))?;
        self.child = Some(child);

        // Re-apply cached settings (best effort; a fresh child has defaults).
        if let Some(r) = self.rate {
            let _ = self.try_write(&format!("r{}", r));
        }
        if let Some(v) = self.volume {
            let _ = self.try_write(&format!("v{}", v));
        }
        if let Some(i) = self.voice_idx {
            let _ = self.try_write(&format!("V{}", i));
        }
        Ok(())
    }

    /// Write one protocol line, respawning the child once if the pipe broke.
    fn write_line(&mut self, line: &str) -> Result<()> {
        if self.try_write(line).is_err() {
            debug!("Speech server pipe broken; respawning");
            self.spawn()?;
            self.try_write(line)
                .map_err(|e| TdsrError::Speech(format!("Speech server write failed: {}", e)))?;
        }
        Ok(())
    }

    fn try_write(&mut self, line: &str) -> io::Result<()> {
        let child = self
            .child
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "no speech server"))?;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "speech server stdin closed"))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()
    }
}

impl Drop for AvServerSynth {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Strip framing-breaking newlines; the child handles the rest of the cleanup.
fn sanitize(text: &str) -> String {
    text.replace(['\n', '\r'], " ")
}

impl Synth for AvServerSynth {
    fn send(&mut self, cmd: SpeechCommand) -> Result<()> {
        match cmd {
            SpeechCommand::Speak(t) => self.speak(&t),
            SpeechCommand::Letter(c) => self.letter(&c.to_string()),
            SpeechCommand::Cancel => self.cancel(),
            SpeechCommand::SetRate(r) => self.set_rate(r),
            SpeechCommand::SetVolume(v) => self.set_volume(v),
            SpeechCommand::SetVoiceIdx(i) => self.set_voice_idx(i),
        }
    }

    fn set_rate(&mut self, rate: u8) -> Result<()> {
        self.rate = Some(rate);
        self.write_line(&format!("r{}", rate))
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        self.volume = Some(volume);
        self.write_line(&format!("v{}", volume))
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        self.voice_idx = Some(idx);
        self.write_line(&format!("V{}", idx))
    }

    fn speak(&mut self, text: &str) -> Result<()> {
        let text = sanitize(text);
        if text.is_empty() {
            return Ok(());
        }
        self.write_line(&format!("s{}", text))
    }

    fn letter(&mut self, text: &str) -> Result<()> {
        let text = sanitize(text);
        if text.is_empty() {
            return Ok(());
        }
        self.write_line(&format!("l{}", text))
    }

    fn cancel(&mut self) -> Result<()> {
        self.write_line("x")
    }
}

// ===========================================================================
// Child side: the speech server. Runs on the process main thread.
// ===========================================================================

mod server {
    use super::SpeechCommand;
    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::runloop::{
        kCFRunLoopDefaultMode, CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRun,
        CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceRef, CFRunLoopSourceSignal,
        CFRunLoopWakeUp,
    };
    use log::debug;
    use objc::runtime::{Object, BOOL, NO, YES};
    use objc::{class, msg_send, sel, sel_impl};
    use std::ffi::c_void;
    use std::io::{self, BufRead};
    use std::ptr;
    use std::sync::mpsc::{channel, Receiver, Sender};

    // Link the frameworks directly so they're present regardless of `tts`.
    #[link(name = "AVFoundation", kind = "framework")]
    #[link(name = "Foundation", kind = "framework")]
    extern "C" {}

    type Id = *mut Object;

    /// `AVSpeechBoundaryImmediate` — stop without finishing the current word.
    const AV_SPEECH_BOUNDARY_IMMEDIATE: isize = 0;

    /// Wake handle shared with the stdin-reader thread. The two CFRunLoop
    /// signaling functions used with it are documented thread-safe.
    #[derive(Clone, Copy)]
    struct RunLoopHandle {
        run_loop: CFRunLoopRef,
        source: CFRunLoopSourceRef,
    }
    // Safety: only the thread-safe CFRunLoop signaling functions are called
    // through these pointers from the reader thread.
    unsafe impl Send for RunLoopHandle {}

    /// State owned by the main thread; holds Objective-C objects that must
    /// never leave it.
    struct Worker {
        rx: Receiver<SpeechCommand>,
        synth: Id,
        /// 0.0..=1.0 (AVFoundation scale; 0.5 is default).
        rate: f32,
        /// 0.0..=1.0.
        volume: f32,
        voice_idx: Option<usize>,
        /// Whether `setPrefersAssistiveTechnologySettings:` exists (macOS 14+).
        prefers_at: bool,
        /// Voice used when no explicit `voice_idx` is set — Eloquence if it is
        /// installed (retained for the process lifetime), else `None` to follow
        /// the VoiceOver voice.
        default_voice: Option<Id>,
    }

    impl Worker {
        /// Drain queued commands. If a Cancel is present, speech queued before
        /// it is dropped so a keystroke silences a flood immediately.
        fn drain(&mut self) {
            let mut batch: Vec<SpeechCommand> = Vec::new();
            while let Ok(cmd) = self.rx.try_recv() {
                batch.push(cmd);
            }

            if let Some(last_cancel) = batch
                .iter()
                .rposition(|c| matches!(c, SpeechCommand::Cancel))
            {
                let mut kept = Vec::with_capacity(batch.len());
                for (i, cmd) in batch.into_iter().enumerate() {
                    match cmd {
                        SpeechCommand::Speak(_) | SpeechCommand::Letter(_) if i < last_cancel => {}
                        other => kept.push(other),
                    }
                }
                batch = kept;
            }

            for cmd in batch {
                self.handle(cmd);
            }
        }

        fn handle(&mut self, cmd: SpeechCommand) {
            match cmd {
                SpeechCommand::Speak(text) => self.speak(&text),
                SpeechCommand::Letter(ch) => self.speak(&ch.to_string()),
                SpeechCommand::Cancel => self.cancel(),
                SpeechCommand::SetRate(r) => self.rate = (r as f32 / 100.0).clamp(0.0, 1.0),
                SpeechCommand::SetVolume(v) => self.volume = (v as f32 / 100.0).clamp(0.0, 1.0),
                SpeechCommand::SetVoiceIdx(idx) => self.voice_idx = Some(idx),
            }
        }

        fn speak(&mut self, text: &str) {
            let cleaned = clean_text(text);
            if cleaned.is_empty() {
                return;
            }
            debug!("speech-server speak: {}", cleaned);

            // Safety: all messaging runs on the main thread (run-loop perform).
            unsafe {
                let cf = CFString::new(&cleaned);
                // CFStringRef is toll-free bridged to NSString*.
                let ns_string = cf.as_concrete_TypeRef() as Id;
                let utterance: Id =
                    msg_send![class!(AVSpeechUtterance), speechUtteranceWithString: ns_string];
                if utterance.is_null() {
                    return;
                }

                let _: () = msg_send![utterance, setRate: self.rate];
                let _: () = msg_send![utterance, setVolume: self.volume];

                // Voice priority: an explicit voice_idx wins; otherwise the
                // default voice (Eloquence if installed); otherwise follow the
                // system assistive-technology (VoiceOver) voice, which is the
                // only way to reach VoiceOver's Siri voices.
                let mut voice_set = false;
                if let Some(idx) = self.voice_idx {
                    if let Some(voice) = self.voice_at(idx) {
                        let _: () = msg_send![utterance, setVoice: voice];
                        voice_set = true;
                    }
                }
                if !voice_set {
                    if let Some(voice) = self.default_voice {
                        let _: () = msg_send![utterance, setVoice: voice];
                        voice_set = true;
                    }
                }
                if !voice_set && self.prefers_at {
                    let _: () = msg_send![utterance, setPrefersAssistiveTechnologySettings: YES];
                }

                let _: () = msg_send![self.synth, speakUtterance: utterance];
            }
        }

        unsafe fn voice_at(&self, idx: usize) -> Option<Id> {
            let voices: Id = msg_send![class!(AVSpeechSynthesisVoice), speechVoices];
            if voices.is_null() {
                return None;
            }
            let count: usize = msg_send![voices, count];
            if idx >= count {
                return None;
            }
            let voice: Id = msg_send![voices, objectAtIndex: idx];
            if voice.is_null() {
                None
            } else {
                Some(voice)
            }
        }

        fn cancel(&mut self) {
            // Safety: messaging the synth on its owning (main) thread.
            unsafe {
                let _: BOOL =
                    msg_send![self.synth, stopSpeakingAtBoundary: AV_SPEECH_BOUNDARY_IMMEDIATE];
            }
        }
    }

    /// Convert an `NSString*` to a Rust `String` via `-UTF8String`.
    unsafe fn nsstring_to_string(s: Id) -> String {
        if s.is_null() {
            return String::new();
        }
        let utf8: *const std::os::raw::c_char = msg_send![s, UTF8String];
        if utf8.is_null() {
            return String::new();
        }
        std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
    }

    /// Whether a voice's identifier marks it as an Eloquence voice. Apple's
    /// Eloquence voices use identifiers like `com.apple.eloquence.en-US.Reed`;
    /// their display names overlap with the classic voices, so the identifier
    /// is the reliable signal.
    unsafe fn is_eloquence(identifier: Id) -> bool {
        nsstring_to_string(identifier)
            .to_lowercase()
            .contains("eloquence")
    }

    /// Pick the default voice when no explicit `voice_idx` is configured:
    /// prefer Eloquence (favored by screen-reader users), specifically the Reed
    /// variant, matching the current language when possible. Returns a retained
    /// voice (kept for the process lifetime) or `None` to fall back to the
    /// VoiceOver voice.
    unsafe fn find_default_voice() -> Option<Id> {
        let voices: Id = msg_send![class!(AVSpeechSynthesisVoice), speechVoices];
        if voices.is_null() {
            return None;
        }
        let count: usize = msg_send![voices, count];

        let cur_lang_id: Id = msg_send![class!(AVSpeechSynthesisVoice), currentLanguageCode];
        let cur_lang = nsstring_to_string(cur_lang_id);
        let cur_prefix = cur_lang.split('-').next().unwrap_or("").to_lowercase();

        // Track the best Eloquence Reed voice and the best Eloquence voice of
        // any variant, each ranked by language closeness: 2 = exact tag
        // (e.g. en-US), 1 = same primary subtag (en), 0 = any language.
        let mut best_reed: Option<(u8, Id)> = None;
        let mut best_any: Option<(u8, Id)> = None;

        for i in 0..count {
            let v: Id = msg_send![voices, objectAtIndex: i];
            let ident_id: Id = msg_send![v, identifier];
            let ident = nsstring_to_string(ident_id).to_lowercase();
            if !ident.contains("eloquence") {
                continue;
            }

            let lang_id: Id = msg_send![v, language];
            let lang = nsstring_to_string(lang_id);
            let rank = if lang.eq_ignore_ascii_case(&cur_lang) {
                2
            } else if !cur_prefix.is_empty()
                && lang.split('-').next().unwrap_or("").to_lowercase() == cur_prefix
            {
                1
            } else {
                0
            };

            // The variant is the last identifier component, e.g.
            // `com.apple.eloquence.en-US.Reed` -> "reed". Use the identifier
            // (not the display name) so the classic "Reed" voice is excluded.
            let is_reed = ident.rsplit('.').next() == Some("reed");

            let better = |slot: &Option<(u8, Id)>| slot.map_or(true, |(r, _)| rank > r);
            if better(&best_any) {
                best_any = Some((rank, v));
            }
            if is_reed && better(&best_reed) {
                best_reed = Some((rank, v));
            }
        }

        let chosen = best_reed.or(best_any).map(|(_, v)| v)?;
        // Retain it: voices from speechVoices() are autoreleased, but this one
        // is used for every utterance for the life of the process.
        let retained: Id = msg_send![chosen, retain];
        Some(retained)
    }

    /// Print the installed AVFoundation voices with their indices, then exit.
    /// Runs synchronously — no run loop required.
    pub fn list_voices() -> ! {
        // Safety: enumerating voices is a synchronous class-method call.
        unsafe {
            let voices: Id = msg_send![class!(AVSpeechSynthesisVoice), speechVoices];
            let count: usize = if voices.is_null() {
                0
            } else {
                msg_send![voices, count]
            };

            println!("{} AVFoundation voice(s) available to TDSR:\n", count);
            println!("Set voice_idx in ~/.tdsr.cfg (or Alt+c then V) to one of these indices.");
            println!(
                "Leave voice_idx unset (or -1) for the default: Eloquence if installed,\n\
                 otherwise the VoiceOver voice."
            );
            println!(
                "Note: VoiceOver's Siri voices are not listed here — they are only\n      \
                 reachable via the default (VoiceOver-follow) setting.\n"
            );
            println!("{:>4}  {:<30}  {:<8}  {}", "idx", "name", "lang", "quality");
            for i in 0..count {
                let v: Id = msg_send![voices, objectAtIndex: i];
                let name_id: Id = msg_send![v, name];
                let lang_id: Id = msg_send![v, language];
                let ident_id: Id = msg_send![v, identifier];
                let name = nsstring_to_string(name_id);
                let lang = nsstring_to_string(lang_id);
                // AVSpeechSynthesisVoiceQuality: 1 default, 2 enhanced, 3 premium.
                let quality: isize = msg_send![v, quality];
                let q = match quality {
                    2 => "enhanced",
                    3 => "premium",
                    _ => "default",
                };
                let tag = if is_eloquence(ident_id) {
                    "  <- eloquence"
                } else {
                    ""
                };
                println!("{:>4}  {:<30}  {:<8}  {}{}", i, name, lang, q, tag);
            }
        }
        std::process::exit(0);
    }

    /// Run-loop source callback: drain the channel under an autorelease pool.
    extern "C" fn perform(info: *const c_void) {
        // Safety: `info` is the `Box<Worker>` pointer installed in the context;
        // only ever dereferenced here, on the main thread.
        unsafe {
            let worker = &mut *(info as *mut Worker);
            let pool: Id = msg_send![class!(NSAutoreleasePool), new];
            worker.drain();
            let _: () = msg_send![pool, release];
        }
    }

    /// Speech-server entry point. MUST be called on the process main thread.
    /// Runs the AVFoundation run loop until stdin closes, then exits the process.
    pub fn run() -> ! {
        // Safety: Objective-C objects created here stay on the main thread.
        unsafe {
            let synth: Id = msg_send![class!(AVSpeechSynthesizer), new];
            if synth.is_null() {
                eprintln!("tdsr speech-server: failed to create AVSpeechSynthesizer");
                std::process::exit(1);
            }

            // Probe for the macOS 14+ assistive-technology settings selector.
            let probe: Id = msg_send![class!(AVSpeechUtterance), new];
            let responds: BOOL =
                msg_send![probe, respondsToSelector: sel!(setPrefersAssistiveTechnologySettings:)];
            let prefers_at = responds != NO;
            let _: () = msg_send![probe, release];

            // Default to Eloquence when installed; falls back to VoiceOver.
            let default_voice = find_default_voice();
            debug!("speech-server default voice resolved: {}", default_voice.is_some());

            let (tx, rx) = channel::<SpeechCommand>();
            let worker = Box::new(Worker {
                rx,
                synth,
                rate: 0.5,
                volume: 1.0,
                voice_idx: None,
                prefers_at,
                default_voice,
            });
            let worker_ptr = Box::into_raw(worker);

            let mut context = CFRunLoopSourceContext {
                version: 0,
                info: worker_ptr as *mut c_void,
                retain: None,
                release: None,
                copyDescription: None,
                equal: None,
                hash: None,
                schedule: None,
                cancel: None,
                perform,
            };
            let source = CFRunLoopSourceCreate(ptr::null(), 0, &mut context);
            if source.is_null() {
                eprintln!("tdsr speech-server: failed to create run-loop source");
                std::process::exit(1);
            }
            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopDefaultMode);

            // Read commands from stdin on a helper thread; it signals the source
            // so the main thread's run loop drains and speaks.
            let handle = RunLoopHandle { run_loop, source };
            std::thread::Builder::new()
                .name("tdsr-speech-stdin".into())
                .spawn(move || stdin_reader(tx, handle))
                .expect("spawn speech-server stdin reader");

            CFRunLoopRun();
            // The run loop only returns if all sources are removed; treat as exit.
            std::process::exit(0);
        }
    }

    /// Read the line protocol from stdin, posting parsed commands and waking the
    /// run loop. Exits the process on EOF (parent closed the pipe).
    fn stdin_reader(tx: Sender<SpeechCommand>, handle: RunLoopHandle) -> ! {
        let stdin = io::stdin();
        let mut reader = stdin.lock();
        let mut line: Vec<u8> = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    if let Some(cmd) = parse_line(&line) {
                        if tx.send(cmd).is_err() {
                            break;
                        }
                        // Safety: thread-safe CFRunLoop signaling functions.
                        unsafe {
                            CFRunLoopSourceSignal(handle.source);
                            CFRunLoopWakeUp(handle.run_loop);
                        }
                    }
                }
                Err(_) => break,
            }
        }
        std::process::exit(0);
    }

    /// Parse one protocol line into a command (trailing CR/LF stripped).
    fn parse_line(raw: &[u8]) -> Option<SpeechCommand> {
        let mut bytes = raw;
        while matches!(bytes.last(), Some(b'\n') | Some(b'\r')) {
            bytes = &bytes[..bytes.len() - 1];
        }
        let (&kind, rest) = bytes.split_first()?;
        let text = String::from_utf8_lossy(rest);
        match kind {
            b's' | b'l' => Some(SpeechCommand::Speak(text.into_owned())),
            b'x' => Some(SpeechCommand::Cancel),
            b'r' => text.trim().parse().ok().map(SpeechCommand::SetRate),
            b'v' => text.trim().parse().ok().map(SpeechCommand::SetVolume),
            b'V' => text.trim().parse().ok().map(SpeechCommand::SetVoiceIdx),
            _ => None,
        }
    }

    /// Normalize text the way the original Python macOS backend did:
    /// - `[[` is the AVSpeech command-escape; neutralize it.
    /// - U+23CE (return symbol) becomes a space.
    /// - Escape `&`, `<`, `>` because macOS 26 drops text inside `<...>`.
    fn clean_text(text: &str) -> String {
        text.replace("[[", " ")
            .replace('\u{23ce}', " ")
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn clean_text_neutralizes_and_escapes() {
            assert_eq!(clean_text("a[[b"), "a b");
            assert_eq!(clean_text("a\u{23ce}b"), "a b");
            assert_eq!(clean_text("a<b>c&d"), "a&lt;b&gt;c&amp;d");
        }

        #[test]
        fn parse_line_handles_protocol() {
            assert!(matches!(parse_line(b"shello\n"), Some(SpeechCommand::Speak(s)) if s == "hello"));
            assert!(matches!(parse_line(b"x\n"), Some(SpeechCommand::Cancel)));
            assert!(matches!(parse_line(b"r50\n"), Some(SpeechCommand::SetRate(50))));
            assert!(matches!(parse_line(b"v80\n"), Some(SpeechCommand::SetVolume(80))));
            assert!(matches!(parse_line(b"V3\n"), Some(SpeechCommand::SetVoiceIdx(3))));
            assert!(parse_line(b"\n").is_none());
            assert!(parse_line(b"zbogus\n").is_none());
        }
    }
}

/// Speech-server entry point — re-exported for `main`. Call only on the process
/// main thread when invoked with [`SPEECH_SERVER_FLAG`].
pub fn run_speech_server() -> ! {
    server::run()
}

/// CLI flag that lists the installed macOS voices and exits.
pub const LIST_VOICES_FLAG: &str = "--list-voices";

/// Print available macOS voices and exit. Invoked for [`LIST_VOICES_FLAG`].
pub fn list_voices() -> ! {
    server::list_voices()
}
