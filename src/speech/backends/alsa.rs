//! In-process speech engines with direct ALSA playback: the "appliance"
//! backend, selected with `[speech] backend = alsa` (or `TDSR_BACKEND=alsa`).
//!
//! Meant for a machine with no sound server, such as the talking Alpine USB
//! stick: TDSR opens the ALSA PCM itself (`libasound`, loaded with `dlopen`
//! like `libespeak-ng`), keeps at most [`AlsaOptions::buffer_ms`] of audio
//! queued in the device, and a cancel is `snd_pcm_drop` on that buffer. No
//! process, pipe or socket sits between a keystroke and silence.
//!
//! Up to three engines can be loaded, and alt+s steps through them at run
//! time:
//!
//! - espeak-ng, through the [`Espeak`] wrapper of the espeak backend (fast,
//!   good for code and typing);
//! - DECtalk, with the `dectalk` feature: the classic formant engine linked
//!   statically (`DECTALK_LIB_DIR` at build time), 11025 Hz, the natural
//!   cadence for long reading. Its single-threaded build has no stop call and
//!   must not be told to halt mid-utterance (that path hangs the next
//!   utterance), so a cancel makes the callback discard the rest of the
//!   current call. An utterance goes to the engine whole (DECtalk's
//!   intonation spans the sentence); only text longer than
//!   `DECTALK_PIECE` is split, which keeps that discard short;
//! - Piper, with the `piper` feature: neural voices (`.onnx` files in the
//!   `piper_voices` directories) run with rten; espeak-ng phonemises for it.
//!   Each sentence is synthesised whole on a helper thread while the one
//!   before it plays; a cancel stops the sound at once and drops the rest.
//!
//! Everything talks to the engines and the device from one audio thread; the
//! `Synth` methods only push onto a queue.

use crate::speech::backends::espeak::{chunk_callback, Espeak};
#[cfg(feature = "piper")]
use crate::speech::backends::piper;
use crate::speech::backends::pulseaudio::wpm_for_rate;
use crate::speech::voices::VoiceCatalogue;
use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use libloading::Library;
use log::{debug, info, warn};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::ptr;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How long `new` waits for the audio thread to load the libraries and open
/// the device.
const INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a voice or engine change waits for the audio thread's answer.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);

/// How long `drop` waits for the audio thread.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);

const SLEEP_STEP: Duration = Duration::from_millis(10);

/// Options from the config (`[speech]` section).
#[derive(Clone, Debug)]
pub struct AlsaOptions {
    /// ALSA PCM name (`alsa_device`); `default` goes through the system's
    /// asound.conf, `plughw:0` straight to the first card.
    pub device: String,
    /// Engine to start with (`engine`): `espeak`, `dectalk` or `piper`.
    pub engine: String,
    /// espeak-ng's rate, 0-100 (`rate`), if configured.
    pub espeak_rate: Option<u8>,
    /// DECtalk's own rate, 0-100 (`dectalk_rate`).
    pub dectalk_rate: u8,
    /// DECtalk voice (`dectalk_voice`): paul, betty, harry, frank, dennis,
    /// kit, ursula, rita or wendy.
    pub dectalk_voice: String,
    /// Audio queued in the device, in ms (`alsa_buffer`). Bounds how much a
    /// cancel cannot take back.
    pub buffer_ms: u32,
    /// Piper's rate, 0-100 (`piper_rate`).
    pub piper_rate: u8,
    /// Piper voice to start with (`piper_voice`, e.g. `en_US-joe-medium`);
    /// empty for the first one found.
    pub piper_voice: String,
    /// `:`-separated directories holding Piper voices (`piper_voices`);
    /// empty for `~/.local/share/piper-voices:/usr/share/piper-voices`.
    pub piper_voices: String,
}

impl Default for AlsaOptions {
    fn default() -> Self {
        Self {
            device: "default".to_string(),
            engine: "espeak".to_string(),
            espeak_rate: None,
            dectalk_rate: 50,
            dectalk_voice: "paul".to_string(),
            buffer_ms: 50,
            piper_rate: 50,
            piper_voice: String::new(),
            piper_voices: String::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineKind {
    Espeak,
    Dectalk,
    Piper,
}

impl EngineKind {
    fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "espeak" | "espeak-ng" => Some(Self::Espeak),
            "dectalk" => Some(Self::Dectalk),
            "piper" => Some(Self::Piper),
            _ => None,
        }
    }

    /// The announcement when the engine is switched to.
    fn spoken(self) -> &'static str {
        match self {
            Self::Espeak => "e speak",
            Self::Dectalk => "DEC talk",
            Self::Piper => "Piper",
        }
    }

    /// The `[speech]` config key of this engine's rate.
    fn rate_key(self) -> &'static str {
        match self {
            Self::Espeak => "rate",
            Self::Dectalk => "dectalk_rate",
            Self::Piper => "piper_rate",
        }
    }
}

/// The engine after `current` in `engines`, wrapping around (alt+s).
fn next_after(engines: &[EngineKind], current: EngineKind) -> Option<EngineKind> {
    let i = engines.iter().position(|&k| k == current)?;
    let next = engines[(i + 1) % engines.len()];
    (next != current).then_some(next)
}

/// Prefix of the voice ids this backend reports for DECtalk voices.
const DECTALK_VOICE_PREFIX: &str = "dectalk:";

/// Prefix of the voice ids of Piper voices (`piper:en_US-joe-medium`).
const PIPER_VOICE_PREFIX: &str = "piper:";

/// One engine as the audio thread drives it.
trait Engine {
    /// Rate of the PCM `synth` delivers with the current voice.
    fn sample_rate(&self) -> u32;
    fn set_rate(&mut self, rate: u8);
    fn set_volume(&mut self, volume: u8);
    /// Select a voice; returns its spoken name.
    fn set_voice(&mut self, id: &str) -> std::result::Result<String, String>;
    /// Synthesise `text`, handing PCM to `sink` until it is done or `sink`
    /// returns false (a cancel). `sink(&[])` writes nothing and only answers
    /// whether the utterance is still wanted.
    fn synth(&mut self, text: &str, is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool);
}

// ---- espeak-ng -------------------------------------------------------------

struct EspeakEngine {
    /// Shared with the Piper engine, which phonemises with it
    lib: Rc<RefCell<Espeak>>,
    voices: VoiceCatalogue,
}

impl EspeakEngine {
    fn load() -> Result<Self> {
        let mut lib = Espeak::load(chunk_callback)?;
        let voices = lib.voices();
        if !lib.set_voice("en") {
            warn!("espeak-ng: default voice 'en' not available");
        }
        Ok(Self {
            lib: Rc::new(RefCell::new(lib)),
            voices,
        })
    }
}

impl Engine for EspeakEngine {
    fn sample_rate(&self) -> u32 {
        self.lib.borrow().sample_rate()
    }

    fn set_rate(&mut self, rate: u8) {
        self.lib.borrow().set_rate(wpm_for_rate(rate));
    }

    fn set_volume(&mut self, volume: u8) {
        // espeak-ng's default amplitude is 100; above it the output clips.
        self.lib.borrow().set_volume(volume.min(100));
    }

    fn set_voice(&mut self, id: &str) -> std::result::Result<String, String> {
        let (ident, name) = match self.voices.find(id) {
            Some(v) => {
                self.voices.check_usable(v).map_err(|e| e.to_string())?;
                (v.identifier.clone(), v.describe())
            }
            None => (id.trim().to_string(), id.trim().to_string()),
        };
        let mut lib = self.lib.borrow_mut();
        if lib.set_voice(&ident) {
            Ok(name)
        } else {
            // The library's current voice is undefined after a failure.
            lib.set_voice("en");
            Err(format!("no voice {}", id))
        }
    }

    fn synth(&mut self, text: &str, is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool) {
        self.lib.borrow_mut().synth_to(text, !is_letter, sink);
    }
}

// ---- Piper -----------------------------------------------------------------

/// Spoken name of a Piper voice file: `en_US-joe-medium` -> `joe, US English`.
pub fn piper_spoken_name(file: &str) -> String {
    let mut parts = file.split('-');
    let (Some(lang), Some(speaker)) = (parts.next(), parts.next()) else {
        return file.to_string();
    };
    let lang = match lang {
        "en_US" => "US English".to_string(),
        "en_GB" => "British English".to_string(),
        other => other.replace('_', " "),
    };
    format!("{}, {}", speaker.replace('_', " "), lang)
}

/// Samples per write to the device while a Piper sentence plays, so that a
/// cancel is noticed within a few milliseconds.
#[cfg(feature = "piper")]
const PIPER_CHUNK: usize = 512;

/// How often the audio thread checks for a cancel while a sentence is
/// being synthesised.
#[cfg(feature = "piper")]
const PIPER_POLL: Duration = Duration::from_millis(10);

#[cfg(feature = "piper")]
struct PiperEngine {
    /// espeak-ng, for phonemes (shared with the espeak-ng engine)
    espeak: Rc<RefCell<Espeak>>,
    voices: Vec<piper::VoiceFile>,
    current: usize,
    /// The current voice's model, loaded on first use (about 100 MB of RAM);
    /// shared with the synthesis thread, which may outlive a cancel
    loaded: Option<Arc<piper::PiperVoice>>,
    rate: u8,
    volume: u8,
}

#[cfg(feature = "piper")]
impl PiperEngine {
    /// None when there is no voice or espeak-ng cannot phonemise.
    fn new(espeak: Rc<RefCell<Espeak>>, opts: &AlsaOptions) -> Option<Self> {
        if !espeak.borrow().can_phonemise() {
            info!("Piper not loaded: espeak-ng has no phoneme output");
            return None;
        }
        let dirs = if opts.piper_voices.trim().is_empty() {
            piper::default_dirs()
        } else {
            opts.piper_voices.clone()
        };
        let voices = piper::find_voices(&dirs);
        if voices.is_empty() {
            info!("Piper not loaded: no voices in {}", dirs);
            return None;
        }
        let mut engine = Self {
            espeak,
            voices,
            current: 0,
            loaded: None,
            rate: opts.piper_rate,
            volume: 100,
        };
        if !opts.piper_voice.trim().is_empty() {
            if let Err(e) = engine.set_voice(&opts.piper_voice) {
                warn!("{}", e);
            }
        }
        Some(engine)
    }

    fn names(&self) -> Vec<String> {
        self.voices.iter().map(|v| v.name.clone()).collect()
    }

    fn voice(&mut self) -> Option<Arc<piper::PiperVoice>> {
        if self.loaded.is_none() {
            let file = &self.voices[self.current];
            let start = Instant::now();
            match piper::PiperVoice::load(file) {
                Ok(v) => {
                    info!("Piper voice {} loaded in {:?}", file.name, start.elapsed());
                    self.loaded = Some(Arc::new(v));
                }
                Err(e) => warn!("{}", e),
            }
        }
        self.loaded.clone()
    }
}

#[cfg(feature = "piper")]
impl Engine for PiperEngine {
    fn sample_rate(&self) -> u32 {
        self.voices[self.current].config.sample_rate
    }

    fn set_rate(&mut self, rate: u8) {
        self.rate = rate;
    }

    fn set_volume(&mut self, volume: u8) {
        self.volume = volume.min(100);
    }

    fn set_voice(&mut self, id: &str) -> std::result::Result<String, String> {
        let name = id
            .trim()
            .strip_prefix(PIPER_VOICE_PREFIX)
            .unwrap_or(id.trim());
        let i = self
            .voices
            .iter()
            .position(|v| v.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no Piper voice {}", name))?;
        if i != self.current {
            self.current = i;
            self.loaded = None;
        }
        Ok(piper_spoken_name(&self.voices[i].name))
    }

    fn synth(&mut self, text: &str, _is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool) {
        let espeak_voice = self.voices[self.current].config.espeak_voice.clone();
        let Some(clauses) = self.espeak.borrow_mut().phonemes_ipa(&espeak_voice, text) else {
            warn!(
                "Piper: espeak-ng could not phonemise with voice {}",
                espeak_voice
            );
            return;
        };
        let sentences = piper::sentences(&clauses);
        for s in &sentences {
            debug!("Piper phonemes: {}", s.iter().collect::<String>());
        }
        if sentences.is_empty() {
            return;
        }
        let factor = piper::length_factor(self.rate);
        let volume = self.volume;
        let Some(voice) = self.voice() else { return };
        // Sentences are synthesised on a helper thread, the next one while
        // this one plays. The model cannot be interrupted, so after a cancel
        // the helper finishes the sentence it is on and throws it away while
        // this thread is already free for the next utterance.
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel::<Vec<i16>>(1);
        let helper_stop = Arc::clone(&stop);
        let spawned = thread::Builder::new()
            .name("tdsr-piper".to_string())
            .spawn(move || {
                for s in &sentences {
                    if helper_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    match voice.synthesize(s, factor) {
                        Ok(samples) => {
                            if tx.send(piper::to_pcm(&samples, volume)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("{}", e);
                            break;
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            warn!("Piper: cannot start the synthesis thread: {}", e);
            return;
        }
        loop {
            match rx.recv_timeout(PIPER_POLL) {
                Ok(pcm) => {
                    for chunk in pcm.chunks(PIPER_CHUNK) {
                        if !sink(chunk) {
                            stop.store(true, Ordering::SeqCst);
                            return;
                        }
                    }
                }
                // Still synthesising: an empty write asks whether the
                // utterance is still wanted.
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if !sink(&[]) {
                        stop.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

// ---- DECtalk ---------------------------------------------------------------

/// DECtalk voices: config name, inline command, spoken name.
pub const DECTALK_VOICES: &[(&str, &str, &str)] = &[
    ("paul", "np", "Paul"),
    ("betty", "nb", "Betty"),
    ("harry", "nh", "Harry"),
    ("frank", "nf", "Frank"),
    ("dennis", "nd", "Dennis"),
    ("kit", "nk", "Kit"),
    ("ursula", "nu", "Ursula"),
    ("rita", "nr", "Rita"),
    ("wendy", "nw", "Wendy"),
];

/// DECtalk words per minute for a TDSR rate: 90 at 0, 240 at 50, 390 at
/// 100 (the engine accepts 75-600; its default is 200).
pub fn dectalk_wpm(rate: u8) -> u16 {
    90 + (rate.min(100) as u16) * 3
}

/// Longest piece of text handed to DECtalk in one call. A cancel discards
/// the rest of the current piece (synthesised at thousands of times real
/// time), so this bounds the wasted work; a screen line is far shorter.
const DECTALK_PIECE: usize = 400;

/// Split text for DECtalk. Each call ends with a forced sentence end, and
/// DECtalk shapes the pitch of a whole sentence (rise on the first stressed
/// word, fall on the last) and handles sentence and clause punctuation inside
/// one call itself, so text is only split when it is longer than
/// [`DECTALK_PIECE`]: after the last sentence end that fits, else after a
/// clause mark, else at a space.
pub fn dectalk_pieces(text: &str) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut rest = text.trim();
    while rest.len() > DECTALK_PIECE {
        let mut cut = DECTALK_PIECE;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let head = &rest[..cut];
        // Byte offset just past a punctuation mark followed by a space.
        let after = |marks: &[char]| {
            head.char_indices()
                .zip(head.chars().skip(1))
                .filter(|((_, c), next)| marks.contains(c) && next.is_whitespace())
                .map(|((i, c), _)| i + c.len_utf8())
                .last()
        };
        let split = after(&['.', '?', '!'])
            .or_else(|| after(&[',', ';', ':']))
            .or_else(|| head.rfind(char::is_whitespace))
            .filter(|&i| i > 0)
            .unwrap_or(cut);
        pieces.push(rest[..split].trim().to_string());
        rest = rest[split..].trim_start();
    }
    if !rest.is_empty() {
        pieces.push(rest.to_string());
    }
    pieces
}

/// What DECtalk gets: ASCII only (the engine's text front end predates
/// UTF-8), no control characters, and no `[:` so the text cannot carry
/// inline commands.
pub fn dectalk_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev = ' ';
    for ch in text.chars() {
        let c = match ch {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            '\u{2013}' | '\u{2014}' => '-',
            c if c.is_ascii_graphic() || c == ' ' => c,
            _ => ' ',
        };
        if c == ':' && prev == '[' {
            out.push(' ');
        }
        out.push(c);
        prev = c;
    }
    out
}

#[cfg(feature = "dectalk")]
mod dectalk {
    use super::*;
    use crate::speech::backends::espeak::ChunkSink;
    use std::cell::Cell;

    type Callback = unsafe extern "C" fn(*mut i16, c_long) -> *mut i16;

    extern "C" {
        fn TextToSpeechInit(callback: Option<Callback>, user_dict: *mut c_void) -> c_int;
        fn TextToSpeechStart(input: *mut c_char, buffer: *mut i16, output_format: c_int) -> c_int;
    }

    /// 11025 Hz, 71 samples per callback (`WAVE_FORMAT_1M16` in epsonapi.h).
    const FORMAT_11K: c_int = 1;
    const BLOCK: usize = 71;
    const SAMPLE_RATE: u32 = 11025;
    /// Callback code for an index mark (no samples).
    const CODE_INDEX: c_long = 3;

    thread_local! {
        static SINK: Cell<Option<ChunkSink>> = const { Cell::new(None) };
        /// Set once the sink asked to stop: the rest of the piece is dropped
        /// (the engine must never be told to halt).
        static DISCARD: Cell<bool> = const { Cell::new(false) };
    }

    unsafe extern "C" fn callback(buffer: *mut i16, code: c_long) -> *mut i16 {
        if code == CODE_INDEX || DISCARD.with(|d| d.get()) {
            return buffer;
        }
        if let Some(sink) = SINK.with(|s| s.get()) {
            // SAFETY: the engine hands a block of BLOCK samples; the sink
            // pointer is installed by the thread making the synchronous call.
            let samples = unsafe { std::slice::from_raw_parts(buffer, BLOCK) };
            if !unsafe { &mut *sink }(samples) {
                DISCARD.with(|d| d.set(true));
            }
        }
        buffer
    }

    /// The engine keeps its state in statics: one instance per process.
    static INITIALISED: AtomicBool = AtomicBool::new(false);

    pub struct DectalkEngine {
        buffer: Vec<i16>,
        rate: u8,
        volume: u8,
        /// Inline command of the voice (`np`)
        voice_cmd: &'static str,
    }

    impl DectalkEngine {
        pub fn new(voice: &str) -> Result<Self> {
            if INITIALISED.swap(true, Ordering::SeqCst) {
                return Err(TdsrError::Speech(
                    "DECtalk is already initialised in this process".to_string(),
                ));
            }
            // SAFETY: the engine's documented initialisation; the callback
            // outlives the process.
            if unsafe { TextToSpeechInit(Some(callback), ptr::null_mut()) } != 0 {
                return Err(TdsrError::Speech(
                    "DECtalk failed to initialise".to_string(),
                ));
            }
            let mut engine = Self {
                buffer: vec![0; BLOCK + 16],
                rate: 50,
                volume: 100,
                voice_cmd: "np",
            };
            if let Err(e) = engine.set_voice(voice) {
                warn!("{}", e);
            }
            // Run the pipeline once with the output discarded so lazily
            // built tables exist before the first real utterance.
            DISCARD.with(|d| d.set(true));
            engine.start("Warm up: 1,234.5 dollars; the 3rd e-mail from Mr. O'Neil at 10:30 AM (test) & 50% done? Yes!");
            DISCARD.with(|d| d.set(false));
            Ok(engine)
        }

        fn start(&mut self, text: &str) {
            let Ok(c) = CString::new(text) else { return };
            let mut bytes = c.into_bytes_with_nul();
            // SAFETY: NUL-terminated text and a block buffer that outlive
            // the synchronous call.
            unsafe {
                TextToSpeechStart(
                    bytes.as_mut_ptr() as *mut c_char,
                    self.buffer.as_mut_ptr(),
                    FORMAT_11K,
                );
            }
        }
    }

    impl Engine for DectalkEngine {
        fn sample_rate(&self) -> u32 {
            SAMPLE_RATE
        }

        fn set_rate(&mut self, rate: u8) {
            self.rate = rate;
        }

        fn set_volume(&mut self, volume: u8) {
            self.volume = volume.min(100);
        }

        fn set_voice(&mut self, id: &str) -> std::result::Result<String, String> {
            let name = id
                .trim()
                .strip_prefix(DECTALK_VOICE_PREFIX)
                .unwrap_or(id.trim());
            match DECTALK_VOICES
                .iter()
                .find(|(n, _, _)| n.eq_ignore_ascii_case(name))
            {
                Some((_, cmd, spoken)) => {
                    self.voice_cmd = cmd;
                    Ok(spoken.to_string())
                }
                None => Err(format!("no DECtalk voice {}", name)),
            }
        }

        fn synth(&mut self, text: &str, is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool) {
            let gain = self.volume as i32;
            let mut scaled: Vec<i16> = Vec::with_capacity(BLOCK);
            let mut sink_gain = |samples: &[i16]| -> bool {
                if gain >= 100 {
                    return sink(samples);
                }
                scaled.clear();
                scaled.extend(samples.iter().map(|&s| (s as i32 * gain / 100) as i16));
                sink(&scaled)
            };
            let prefix = format!("[:{}][:ra {}]", self.voice_cmd, dectalk_wpm(self.rate));
            let clean = dectalk_text(text);
            // Key echo: a typed character is spelled (`[:say letter]`); a
            // symbol name for it ("space", "dollar") is a word and is spoken
            // as one.
            let single = clean.trim().chars().count() == 1;
            let pieces = if is_letter && single {
                vec![format!("[:say letter]{}[:say clause]", clean.trim())]
            } else {
                dectalk_pieces(&clean)
            };
            let sink_ptr: *mut (dyn FnMut(&[i16]) -> bool + '_) = &mut sink_gain;
            // SAFETY: the pointer is only dereferenced during the
            // synchronous `start` calls below, while `sink_gain` is alive;
            // the transmute only erases the lifetime.
            let sink_ptr: ChunkSink = unsafe { std::mem::transmute(sink_ptr) };
            SINK.with(|s| s.set(Some(sink_ptr)));
            DISCARD.with(|d| d.set(false));
            for piece in pieces {
                self.start(&format!("{} {}", prefix, piece));
                if DISCARD.with(|d| d.get()) {
                    break;
                }
            }
            SINK.with(|s| s.set(None));
            DISCARD.with(|d| d.set(false));
        }
    }
}

// ---- libasound -------------------------------------------------------------

const SND_PCM_STREAM_PLAYBACK: c_int = 0;
const SND_PCM_FORMAT_S16_LE: c_int = 2;
const SND_PCM_ACCESS_RW_INTERLEAVED: c_int = 3;

type SndPcmOpen = unsafe extern "C" fn(*mut *mut c_void, *const c_char, c_int, c_int) -> c_int;
type SndPcmSetParams =
    unsafe extern "C" fn(*mut c_void, c_int, c_int, c_uint, c_uint, c_int, c_uint) -> c_int;
type SndPcmWritei = unsafe extern "C" fn(*mut c_void, *const c_void, c_ulong) -> c_long;
type SndPcmRecover = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type SndPcmOp = unsafe extern "C" fn(*mut c_void) -> c_int;
type SndStrerror = unsafe extern "C" fn(c_int) -> *const c_char;
type SndLibErrorSetHandler = unsafe extern "C" fn(*const c_void) -> c_int;

/// alsa-lib's error handler type is variadic; a plain function with the
/// leading fixed parameters is called correctly on every supported ABI, and
/// this one ignores everything anyway. Without it the library prints
/// configuration complaints to stderr, on top of the terminal TDSR reads.
unsafe extern "C" fn quiet_error_handler(
    _file: *const c_char,
    _line: c_int,
    _function: *const c_char,
    _err: c_int,
    _fmt: *const c_char,
) {
}

struct Alsa {
    _lib: Library,
    open: SndPcmOpen,
    set_params: SndPcmSetParams,
    writei: SndPcmWritei,
    recover: SndPcmRecover,
    drop: SndPcmOp,
    prepare: SndPcmOp,
    drain: SndPcmOp,
    close: SndPcmOp,
    strerror: SndStrerror,
}

impl Alsa {
    fn load() -> Result<Self> {
        let err = |e: libloading::Error| TdsrError::Speech(format!("libasound: {}", e));
        // SAFETY: loading a well-known shared library and its documented
        // entry points; the signatures match alsa/pcm.h.
        unsafe {
            let lib = Library::new("libasound.so.2")
                .map_err(|e| TdsrError::Speech(format!("libasound not available: {}", e)))?;
            let open = *lib.get::<SndPcmOpen>(b"snd_pcm_open\0").map_err(err)?;
            let set_params = *lib
                .get::<SndPcmSetParams>(b"snd_pcm_set_params\0")
                .map_err(err)?;
            let writei = *lib.get::<SndPcmWritei>(b"snd_pcm_writei\0").map_err(err)?;
            let recover = *lib
                .get::<SndPcmRecover>(b"snd_pcm_recover\0")
                .map_err(err)?;
            let drop = *lib.get::<SndPcmOp>(b"snd_pcm_drop\0").map_err(err)?;
            let prepare = *lib.get::<SndPcmOp>(b"snd_pcm_prepare\0").map_err(err)?;
            let drain = *lib.get::<SndPcmOp>(b"snd_pcm_drain\0").map_err(err)?;
            let close = *lib.get::<SndPcmOp>(b"snd_pcm_close\0").map_err(err)?;
            let strerror = *lib.get::<SndStrerror>(b"snd_strerror\0").map_err(err)?;
            if let Ok(set_handler) =
                lib.get::<SndLibErrorSetHandler>(b"snd_lib_error_set_handler\0")
            {
                set_handler(quiet_error_handler as *const c_void);
            }
            Ok(Self {
                _lib: lib,
                open,
                set_params,
                writei,
                recover,
                drop,
                prepare,
                drain,
                close,
                strerror,
            })
        }
    }

    fn error(&self, what: &str, code: c_int) -> TdsrError {
        // SAFETY: snd_strerror returns a static string.
        let msg = unsafe { CStr::from_ptr((self.strerror)(code)) };
        TdsrError::Speech(format!("{}: {}", what, msg.to_string_lossy()))
    }
}

/// An open playback stream at one sample rate.
struct Pcm {
    alsa: Arc<Alsa>,
    handle: *mut c_void,
    rate: u32,
}

impl Pcm {
    fn open(alsa: &Arc<Alsa>, device: &str, rate: u32, latency_ms: u32) -> Result<Self> {
        let name = CString::new(device)
            .map_err(|_| TdsrError::Speech("invalid ALSA device name".to_string()))?;
        let mut handle: *mut c_void = ptr::null_mut();
        // SAFETY: documented calls with valid arguments.
        unsafe {
            let r = (alsa.open)(&mut handle, name.as_ptr(), SND_PCM_STREAM_PLAYBACK, 0);
            if r < 0 {
                return Err(alsa.error(&format!("cannot open ALSA device {}", device), r));
            }
            let pcm = Self {
                alsa: Arc::clone(alsa),
                handle,
                rate,
            };
            let r = (alsa.set_params)(
                handle,
                SND_PCM_FORMAT_S16_LE,
                SND_PCM_ACCESS_RW_INTERLEAVED,
                1,
                rate,
                1,
                latency_ms.max(10) * 1000,
            );
            if r < 0 {
                return Err(alsa.error(&format!("cannot set {} Hz mono on {}", rate, device), r));
            }
            Ok(pcm)
        }
    }

    /// Write every sample, waiting for room in the device buffer. Underruns
    /// (the device ran dry between utterances) are recovered silently.
    fn write(&self, samples: &[i16]) -> Result<()> {
        let mut rest = samples;
        let mut retries = 0;
        while !rest.is_empty() {
            // SAFETY: valid buffer of `rest.len()` frames (mono).
            let n = unsafe {
                (self.alsa.writei)(
                    self.handle,
                    rest.as_ptr() as *const c_void,
                    rest.len() as c_ulong,
                )
            };
            if n < 0 {
                retries += 1;
                // SAFETY: documented recovery for EPIPE/ESTRPIPE.
                let r = unsafe { (self.alsa.recover)(self.handle, n as c_int, 1) };
                if r < 0 || retries > 5 {
                    return Err(self.alsa.error("ALSA write failed", n as c_int));
                }
                continue;
            }
            rest = &rest[(n as usize).min(rest.len())..];
        }
        Ok(())
    }

    /// Discard what is queued and get ready for the next write.
    fn drop_queued(&self) {
        // SAFETY: documented calls on an open handle.
        unsafe {
            (self.alsa.drop)(self.handle);
            (self.alsa.prepare)(self.handle);
        }
    }

    /// Play what is queued to the end (at most the buffer length), then get
    /// ready for the next write. Needed for an utterance shorter than the
    /// device buffer, which would otherwise wait for more.
    fn drain(&self) {
        // SAFETY: as above.
        unsafe {
            (self.alsa.drain)(self.handle);
            (self.alsa.prepare)(self.handle);
        }
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        // SAFETY: open handle, not used afterwards.
        unsafe {
            (self.alsa.drop)(self.handle);
            (self.alsa.close)(self.handle);
        }
    }
}

// SAFETY: the handle is only used by the audio thread that created it.
unsafe impl Send for Pcm {}

// ---- queue -----------------------------------------------------------------

#[derive(Clone, Debug)]
struct Utterance {
    text: String,
    is_letter: bool,
}

type Reply = mpsc::Sender<std::result::Result<String, String>>;

enum Control {
    Rate(u8),
    Volume(u8),
    Voice(String, Reply),
    /// Switch to this engine, or to the other one
    Engine(Option<EngineKind>, Reply),
}

struct Queue {
    items: VecDeque<Utterance>,
    controls: VecDeque<Control>,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Bumped by every cancel; an utterance stops when it no longer matches
    /// the value seen when it was dequeued.
    epoch: AtomicU64,
    shutdown: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the constructor learns from the audio thread.
struct Ready {
    espeak_voices: Option<VoiceCatalogue>,
    /// Piper voice file names
    piper_voices: Vec<String>,
    engines: Vec<EngineKind>,
    current: EngineKind,
}

enum Job {
    Control(Control),
    Speak(Utterance, u64),
    Drain,
    Wait,
}

/// The audio thread's state.
struct Worker {
    shared: Arc<Shared>,
    alsa: Arc<Alsa>,
    device: String,
    buffer_ms: u32,
    espeak: Option<EspeakEngine>,
    #[cfg(feature = "dectalk")]
    dectalk: Option<dectalk::DectalkEngine>,
    #[cfg(feature = "piper")]
    piper: Option<PiperEngine>,
    current: EngineKind,
    pcm: Option<Pcm>,
    /// Audio has been written since the last drain
    undrained: bool,
    volume: u8,
}

impl Worker {
    fn engine(&mut self, kind: EngineKind) -> Option<&mut dyn Engine> {
        match kind {
            EngineKind::Espeak => self.espeak.as_mut().map(|e| e as &mut dyn Engine),
            #[cfg(feature = "dectalk")]
            EngineKind::Dectalk => self.dectalk.as_mut().map(|e| e as &mut dyn Engine),
            #[cfg(not(feature = "dectalk"))]
            EngineKind::Dectalk => None,
            #[cfg(feature = "piper")]
            EngineKind::Piper => self.piper.as_mut().map(|e| e as &mut dyn Engine),
            #[cfg(not(feature = "piper"))]
            EngineKind::Piper => None,
        }
    }

    fn engines(&self) -> Vec<EngineKind> {
        let mut v = Vec::new();
        if self.espeak.is_some() {
            v.push(EngineKind::Espeak);
        }
        #[cfg(feature = "dectalk")]
        if self.dectalk.is_some() {
            v.push(EngineKind::Dectalk);
        }
        #[cfg(feature = "piper")]
        if self.piper.is_some() {
            v.push(EngineKind::Piper);
        }
        v
    }

    fn other_engine(&self) -> Option<EngineKind> {
        next_after(&self.engines(), self.current)
    }

    /// The stream for the current engine's rate, (re)opened as needed.
    fn pcm(&mut self) -> Result<&Pcm> {
        let rate = self
            .engine(self.current)
            .map(|e| e.sample_rate())
            .unwrap_or(22050);
        if self.pcm.as_ref().map(|p| p.rate) != Some(rate) {
            self.pcm = None;
            self.pcm = Some(Pcm::open(&self.alsa, &self.device, rate, self.buffer_ms)?);
            self.undrained = false;
        }
        Ok(self.pcm.as_ref().expect("opened above"))
    }

    fn next_job(&self) -> Job {
        let mut q = self.shared.lock();
        loop {
            if self.shared.shutdown.load(Ordering::SeqCst) {
                return Job::Wait;
            }
            if let Some(c) = q.controls.pop_front() {
                return Job::Control(c);
            }
            if let Some(u) = q.items.pop_front() {
                return Job::Speak(u, self.shared.epoch.load(Ordering::SeqCst));
            }
            if self.undrained {
                return Job::Drain;
            }
            q = self.shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn run(&mut self) {
        while !self.shared.shutdown.load(Ordering::SeqCst) {
            match self.next_job() {
                Job::Wait => {}
                Job::Drain => {
                    if let Some(p) = &self.pcm {
                        p.drain();
                    }
                    self.undrained = false;
                }
                Job::Control(c) => self.control(c),
                Job::Speak(u, epoch) => self.speak(u, epoch),
            }
        }
    }

    fn control(&mut self, c: Control) {
        match c {
            Control::Rate(r) => {
                if let Some(e) = self.engine(self.current) {
                    e.set_rate(r);
                }
            }
            Control::Volume(v) => {
                self.volume = v;
                for k in self.engines() {
                    if let Some(e) = self.engine(k) {
                        e.set_volume(v);
                    }
                }
            }
            Control::Voice(id, reply) => {
                let kind = if id.starts_with(DECTALK_VOICE_PREFIX) {
                    EngineKind::Dectalk
                } else if id.starts_with(PIPER_VOICE_PREFIX) {
                    EngineKind::Piper
                } else {
                    EngineKind::Espeak
                };
                let result = match self.engine(kind) {
                    Some(e) => e.set_voice(&id),
                    None => Err(format!("{} is not loaded", kind.spoken())),
                };
                let _ = reply.send(result);
            }
            Control::Engine(kind, reply) => {
                let target = kind.or_else(|| self.other_engine());
                let result = match target {
                    Some(k) if self.engine(k).is_some() => {
                        if k != self.current {
                            self.current = k;
                            // Interrupt what is playing: the announcement
                            // that follows comes from the new engine.
                            if let Some(p) = &self.pcm {
                                p.drop_queued();
                            }
                            self.undrained = false;
                        }
                        Ok(k.spoken().to_string())
                    }
                    Some(k) => Err(format!("{} is not loaded", k.spoken())),
                    None => Err("only one speech engine is loaded".to_string()),
                };
                let _ = reply.send(result);
            }
        }
    }

    fn speak(&mut self, u: Utterance, epoch: u64) {
        let shared = Arc::clone(&self.shared);
        let pcm = match self.pcm() {
            Ok(p) => p as *const Pcm,
            Err(e) => {
                warn!("{}", e);
                return;
            }
        };
        // SAFETY: `pcm` points into `self.pcm`, which is not touched while
        // the engine runs below (the engine borrow is disjoint in practice
        // but not to the borrow checker, hence the raw pointer).
        let pcm = unsafe { &*pcm };
        let mut failed = false;
        let mut sink = |samples: &[i16]| -> bool {
            if shared.epoch.load(Ordering::SeqCst) != epoch
                || shared.shutdown.load(Ordering::SeqCst)
            {
                return false;
            }
            if let Err(e) = pcm.write(samples) {
                if !failed {
                    warn!("{}", e);
                    failed = true;
                }
                return false;
            }
            true
        };
        let current = self.current;
        let Some(engine) = self.engine(current) else {
            return;
        };
        engine.synth(&u.text, u.is_letter, &mut sink);
        let cancelled = shared.epoch.load(Ordering::SeqCst) != epoch;
        if cancelled {
            pcm.drop_queued();
            self.undrained = false;
        } else {
            self.undrained = true;
        }
    }
}

fn audio_thread(shared: Arc<Shared>, opts: AlsaOptions, ready: mpsc::Sender<Result<Ready>>) {
    let alsa = match Alsa::load() {
        Ok(a) => Arc::new(a),
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let espeak = match EspeakEngine::load() {
        Ok(mut e) => {
            if let Some(rate) = opts.espeak_rate {
                e.set_rate(rate);
            }
            Some(e)
        }
        Err(e) => {
            info!("espeak-ng not loaded: {}", e);
            None
        }
    };
    #[cfg(feature = "dectalk")]
    let dectalk = match dectalk::DectalkEngine::new(&opts.dectalk_voice) {
        Ok(mut e) => {
            e.set_rate(opts.dectalk_rate);
            Some(e)
        }
        Err(e) => {
            info!("DECtalk not loaded: {}", e);
            None
        }
    };
    #[cfg(feature = "piper")]
    let piper = espeak
        .as_ref()
        .and_then(|e| PiperEngine::new(Rc::clone(&e.lib), &opts));
    #[cfg(feature = "piper")]
    let piper_voices = piper.as_ref().map(|p| p.names()).unwrap_or_default();
    #[cfg(not(feature = "piper"))]
    let piper_voices = Vec::new();
    let mut worker = Worker {
        shared,
        alsa,
        device: opts.device.clone(),
        buffer_ms: opts.buffer_ms,
        espeak,
        #[cfg(feature = "dectalk")]
        dectalk,
        #[cfg(feature = "piper")]
        piper,
        current: EngineKind::Espeak,
        pcm: None,
        undrained: false,
        volume: 100,
    };
    let engines = worker.engines();
    if engines.is_empty() {
        let _ = ready.send(Err(TdsrError::Speech(
            "no speech engine available (libespeak-ng not found, DECtalk not built in)".to_string(),
        )));
        return;
    }
    worker.current = EngineKind::parse(&opts.engine)
        .filter(|k| engines.contains(k))
        .unwrap_or(engines[0]);
    // Open the device now so a missing sound card is reported at start-up.
    if let Err(e) = worker.pcm() {
        let _ = ready.send(Err(e));
        return;
    }
    let espeak_voices = worker.espeak.as_ref().map(|e| e.voices.clone());
    let _ = ready.send(Ok(Ready {
        espeak_voices,
        piper_voices,
        engines,
        current: worker.current,
    }));
    worker.run();
}

// ---- the backend ------------------------------------------------------------

pub struct AlsaSynth {
    shared: Arc<Shared>,
    espeak_voices: Option<VoiceCatalogue>,
    piper_voices: Vec<String>,
    /// Loaded engines, in the audio thread's order
    engines: Vec<EngineKind>,
    /// The engine speaking now (kept in step with the audio thread's)
    current: EngineKind,
    audio_thread: Option<thread::JoinHandle<()>>,
}

impl AlsaSynth {
    pub fn new(opts: AlsaOptions) -> Result<Self> {
        debug!("Creating ALSA backend ({:?})", opts);
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                controls: VecDeque::new(),
            }),
            wake: Condvar::new(),
            epoch: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let worker_shared = Arc::clone(&shared);
        let audio_thread = thread::Builder::new()
            .name("tdsr-audio".to_string())
            .spawn(move || audio_thread(worker_shared, opts, tx))
            .map_err(|e| TdsrError::Speech(format!("Failed to start audio thread: {}", e)))?;
        let ready = match rx.recv_timeout(INIT_TIMEOUT) {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                shared.shutdown.store(true, Ordering::SeqCst);
                return Err(TdsrError::Speech(
                    "the ALSA device did not answer".to_string(),
                ));
            }
        };
        info!(
            "ALSA backend ready, engines {:?}, {} espeak-ng voices, Piper voices {:?}",
            ready.engines,
            ready.espeak_voices.as_ref().map(|v| v.len()).unwrap_or(0),
            ready.piper_voices
        );
        Ok(Self {
            shared,
            espeak_voices: ready.espeak_voices,
            piper_voices: ready.piper_voices,
            engines: ready.engines,
            current: ready.current,
            audio_thread: Some(audio_thread),
        })
    }

    fn enqueue(&self, text: &str, is_letter: bool) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let mut q = self.shared.lock();
        q.items.push_back(Utterance {
            text: text.to_string(),
            is_letter,
        });
        drop(q);
        self.shared.wake.notify_one();
    }

    fn control(&self, c: Control) {
        self.shared.lock().controls.push_back(c);
        self.shared.wake.notify_one();
    }

    /// Send a control that answers, and wait for the answer.
    fn ask(&self, make: impl FnOnce(Reply) -> Control) -> Result<String> {
        let (tx, rx) = mpsc::channel();
        self.control(make(tx));
        match rx.recv_timeout(CONTROL_TIMEOUT) {
            Ok(Ok(s)) => Ok(s),
            Ok(Err(msg)) => Err(TdsrError::Speech(msg)),
            Err(_) => Err(TdsrError::Speech(
                "the speech thread did not answer".to_string(),
            )),
        }
    }

    fn espeak_count(&self) -> usize {
        self.espeak_voices.as_ref().map(|v| v.len()).unwrap_or(0)
    }
}

impl Synth for AlsaSynth {
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
        self.control(Control::Rate(rate));
        Ok(())
    }

    fn set_volume(&mut self, volume: u8) -> Result<()> {
        self.control(Control::Volume(volume));
        Ok(())
    }

    fn set_voice_idx(&mut self, idx: usize) -> Result<()> {
        let id = self.voice_id(idx).ok_or_else(|| {
            TdsrError::Speech(format!(
                "no voice {}, the last voice is {}",
                idx,
                self.voice_count().unwrap_or(1) - 1
            ))
        })?;
        self.set_voice(&id).map(|_| ())
    }

    /// An espeak-ng identifier or name (`gmw/en-US`, `en-us`), a DECtalk
    /// voice as `dectalk:paul`, or a Piper voice as `piper:en_US-joe-medium`.
    /// Applies to that engine whether or not it is the current one.
    fn set_voice(&mut self, id: &str) -> Result<String> {
        let id = id.trim().to_string();
        self.ask(|reply| Control::Voice(id, reply))
    }

    fn voice_count(&self) -> Option<usize> {
        Some(self.espeak_count() + DECTALK_VOICES.len() + self.piper_voices.len())
    }

    /// espeak-ng's voices, then DECtalk's, then Piper's.
    fn voice_id(&self, idx: usize) -> Option<String> {
        let n = self.espeak_count();
        let d = n + DECTALK_VOICES.len();
        if idx < n {
            self.espeak_voices
                .as_ref()
                .and_then(|v| v.get(idx))
                .map(|v| v.identifier.clone())
        } else if idx < d {
            DECTALK_VOICES
                .get(idx - n)
                .map(|(name, _, _)| format!("{}{}", DECTALK_VOICE_PREFIX, name))
        } else {
            self.piper_voices
                .get(idx - d)
                .map(|name| format!("{}{}", PIPER_VOICE_PREFIX, name))
        }
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

    /// Drop the queue, stop the current utterance at its next block and
    /// discard what the device still holds.
    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        let mut q = self.shared.lock();
        q.items.clear();
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        drop(q);
        self.shared.wake.notify_one();
        Ok(())
    }

    /// Each engine keeps its own rate (`rate` is espeak-ng's, `dectalk_rate`
    /// DECtalk's, `piper_rate` Piper's), so a rate set in the config menu
    /// goes to the one speaking.
    fn rate_key(&self) -> &'static str {
        self.current.rate_key()
    }

    fn next_engine(&mut self) -> Result<String> {
        self.cancel()?;
        let name = self.ask(|reply| Control::Engine(None, reply))?;
        // The audio thread switched to the next loaded engine.
        if let Some(k) = next_after(&self.engines, self.current) {
            self.current = k;
        }
        Ok(name)
    }
}

impl Drop for AlsaSynth {
    fn drop(&mut self) {
        debug!("Shutting down ALSA backend");
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.epoch.fetch_add(1, Ordering::SeqCst);
        self.shared.lock().items.clear();
        self.shared.wake.notify_all();
        if let Some(handle) = self.audio_thread.take() {
            let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            while !handle.is_finished() && Instant::now() < deadline {
                thread::sleep(SLEEP_STEP);
            }
            if handle.is_finished() {
                let _ = handle.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpm_range() {
        assert_eq!(dectalk_wpm(0), 90);
        assert_eq!(dectalk_wpm(50), 240);
        assert_eq!(dectalk_wpm(100), 390);
        assert_eq!(dectalk_wpm(200), 390);
    }

    #[test]
    fn pieces_keep_a_line_whole() {
        // DECtalk handles the sentence and clause marks itself.
        let line = "One two. Three four? Five; six: Mr. Smith has 3.5 files.txt";
        assert_eq!(dectalk_pieces(line), vec![line]);
        assert_eq!(dectalk_pieces("  padded  "), vec!["padded"]);
        assert!(dectalk_pieces("   ").is_empty());
    }

    #[test]
    fn pieces_split_long_text_at_sentence_ends() {
        let sentence = "This sentence is about forty characters. ";
        let text = sentence.repeat(20);
        let p = dectalk_pieces(&text);
        assert!(p.len() > 1);
        assert!(p
            .iter()
            .all(|s| s.len() <= DECTALK_PIECE && s.ends_with('.')));
        assert_eq!(p.join(" "), text.trim());
    }

    #[test]
    fn pieces_split_long_sentence_at_clauses_then_words() {
        let text = "one, two three four five ".repeat(30);
        let p = dectalk_pieces(&text);
        assert!(p.len() > 1);
        assert!(p[..p.len() - 1].iter().all(|s| s.ends_with(',')));
        let text = "word ".repeat(100);
        let p = dectalk_pieces(&text);
        assert!(p.len() > 1 && p.iter().all(|s| s.len() <= DECTALK_PIECE));
        assert_eq!(p.join(" ").split_whitespace().count(), 100);
        let p = dectalk_pieces(&"\u{e9}".repeat(300));
        assert_eq!(p.concat().chars().count(), 300);
    }

    #[test]
    fn text_is_ascii_and_command_free() {
        assert_eq!(
            dectalk_text("caf\u{e9} [:np] \u{201c}hi\u{201d}"),
            "caf  [ :np] \"hi\""
        );
        assert!(dectalk_text("tab\there").is_ascii());
    }

    #[test]
    fn engine_names() {
        assert_eq!(EngineKind::parse("DECtalk"), Some(EngineKind::Dectalk));
        assert_eq!(EngineKind::parse("espeak-ng"), Some(EngineKind::Espeak));
        assert_eq!(EngineKind::parse("sam"), None);
    }
}
