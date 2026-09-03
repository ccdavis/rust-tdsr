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
//! `CFRunLoopRun()`. The parent side is the generic [`CommandSynth`] in
//! `command.rs`, which talks the line protocol described there (`s`, `l`,
//! `x`, `r`, `v`, `V` lines) over the child's stdin.
//!
//! Flood safety: parent writes are small and the child's stdin reader drains
//! into an unbounded channel, so a terminal dumping a huge amount of text can't
//! block the parent's main thread or the keyboard. A keystroke already sends
//! `x` (cancel) upstream, and the child coalesces its backlog so that cancel
//! takes effect immediately instead of waiting behind queued utterances.
//!
//! When the parent runs with `--debug`, the child is started with `--debug`
//! too and appends its own log lines to the same `tdsr.log`.

use super::command::CommandSynth;
use crate::{Result, TdsrError};
use std::process::{Command, Stdio};

/// CLI flag that puts the binary into speech-server mode.
pub const SPEECH_SERVER_FLAG: &str = "--speech-server";

// ===========================================================================
// Parent side: spawns and talks to the speech-server subprocess.
// ===========================================================================

/// Start our own executable as `tdsr --speech-server` and return a synth
/// that talks to it. Passes `--debug` through when debug logging is on so
/// the child appends to the same `tdsr.log`.
pub fn spawn_server_synth() -> Result<CommandSynth> {
    let exe = std::env::current_exe().map_err(|e| {
        TdsrError::Speech(format!(
            "Cannot locate current executable for speech server: {}",
            e
        ))
    })?;
    let mut args = vec![SPEECH_SERVER_FLAG.to_string()];
    if log::log_enabled!(log::Level::Debug) {
        args.push("--debug".to_string());
    }
    CommandSynth::new(exe.to_string_lossy().into_owned(), args)
}

/// Speak `text` through the system `say` command and wait for it to finish.
///
/// Last-resort path for announcing a fatal startup error when our own speech
/// server could not be started: a blind user cannot read the message on
/// stderr, and `say` is always present on macOS. Failures are ignored.
pub fn say_blocking(text: &str) {
    let _ = Command::new("/usr/bin/say")
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

// ===========================================================================
// Child side: the speech server. Runs on the process main thread.
// ===========================================================================

mod server {
    use crate::speech::SpeechCommand;
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

    // Link the frameworks directly.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}
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
        /// Voice selected by an explicit `voice_idx`, resolved once when the
        /// index is set and retained. `speechVoices()` enumerates every
        /// installed voice and is far too slow to call per utterance.
        selected_voice: Option<Id>,
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
                SpeechCommand::SetVoiceIdx(idx) => self.select_voice(idx),
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
                if let Some(voice) = self.selected_voice.or(self.default_voice) {
                    let _: () = msg_send![utterance, setVoice: voice];
                } else if self.prefers_at {
                    let _: () = msg_send![utterance, setPrefersAssistiveTechnologySettings: YES];
                }

                let _: () = msg_send![self.synth, speakUtterance: utterance];
            }
        }

        /// Resolve `idx` against `speechVoices()` once and keep the voice
        /// retained. An out-of-range index clears the selection so speech
        /// falls back to the default voice instead of going silent.
        fn select_voice(&mut self, idx: usize) {
            // Safety: main thread; the previous voice was retained by us.
            unsafe {
                if let Some(old) = self.selected_voice.take() {
                    let _: () = msg_send![old, release];
                }
                match self.voice_at(idx) {
                    Some(voice) => {
                        let retained: Id = msg_send![voice, retain];
                        self.selected_voice = Some(retained);
                    }
                    None => debug!("speech-server: voice index {} out of range", idx),
                }
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
        std::ffi::CStr::from_ptr(utf8)
            .to_string_lossy()
            .into_owned()
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
        // Safety: enumerating voices is a synchronous class-method call. The
        // autorelease pool owns the voices array and the strings we read.
        unsafe {
            let pool: Id = msg_send![class!(NSAutoreleasePool), new];
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
            println!("{:>4}  {:<30}  {:<8}  quality", "idx", "name", "lang");
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
            let _: () = msg_send![pool, release];
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
            // The voices array is autoreleased, so give it a pool to drain
            // into (the chosen voice is retained separately).
            let pool: Id = msg_send![class!(NSAutoreleasePool), new];
            let default_voice = find_default_voice();
            let _: () = msg_send![pool, release];
            debug!(
                "speech-server default voice resolved: {}",
                default_voice.is_some()
            );

            let (tx, rx) = channel::<SpeechCommand>();
            let worker = Box::new(Worker {
                rx,
                synth,
                rate: 0.5,
                volume: 1.0,
                selected_voice: None,
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
            assert!(
                matches!(parse_line(b"shello\n"), Some(SpeechCommand::Speak(s)) if s == "hello")
            );
            assert!(matches!(parse_line(b"x\n"), Some(SpeechCommand::Cancel)));
            assert!(matches!(
                parse_line(b"r50\n"),
                Some(SpeechCommand::SetRate(50))
            ));
            assert!(matches!(
                parse_line(b"v80\n"),
                Some(SpeechCommand::SetVolume(80))
            ));
            assert!(matches!(
                parse_line(b"V3\n"),
                Some(SpeechCommand::SetVoiceIdx(3))
            ));
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
