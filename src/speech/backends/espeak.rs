//! espeak-ng in-process, with TDSR's own PulseAudio playback (Linux and WSL).
//!
//! `libespeak-ng` synthesises on a dedicated audio thread and hands us 20 ms
//! chunks of PCM, which the same thread writes into a small PulseAudio
//! playback buffer (`libpulse-simple`). Both libraries are loaded at runtime
//! with `dlopen`, so they are optional: if either is missing this backend
//! reports an error and the platform fallback chain continues.
//!
//! Owning the audio path is what makes speech responsive:
//!
//! - The first samples of an utterance reach the server a couple of
//!   milliseconds after the request (no process to spawn, no pipe).
//! - `cancel` stops synthesis at the next chunk and flushes the ~40 ms the
//!   server holds for us, so nothing queued behind the current chunk plays.
//! - Nothing here ever blocks the event loop: the `Synth` methods only push
//!   onto a queue.
//!
//! # WSL
//!
//! WSLg's PulseAudio sink (`module-rdp-sink`) forwards audio to Windows in
//! 5 ms blocks and lets up to 256 of them (1.3 s) be outstanding. Measured
//! here, the Windows side plays those blocks several percent slower than the
//! sink sends them, so during continuous audio the backlog grows by tens of
//! milliseconds per second, and it grows with transmitted silence too: an
//! open stream that carries nothing still makes the sink send silence. Audio
//! already forwarded cannot be cancelled, and once the backlog saturates the
//! server's threads block and it stops answering clients altogether.
//!
//! The backlog only shrinks while the sink has no stream at all, so this
//! backend transmits sound and nothing else:
//!
//! - espeak-ng's own pauses (between clauses, at the end of a line) are not
//!   sent as silent samples. The stream is closed for their duration and
//!   reopened for the next sound, so the cadence is unchanged but every
//!   pause is drain time for the client.
//! - While sound is streaming, the stream's reported latency (which on WSL
//!   is the client's measured playback delay) is checked every 250 ms; if it
//!   exceeds [`BACKLOG_LIMIT_MS`] a short silent gap is inserted to bring it
//!   back down. Ordinary prose never triggers this; pause-free audio does.
//! - The stream is closed whenever there is nothing to play.
//!
//! One more WSL quirk: that sink never resets its pacing clock when it
//! resumes from suspend (PulseAudio suspends it 5 s after the last stream
//! closes), so the first stream after a pause bursts a backlog. The sink
//! stays awake, without sending anything, while a recording of its monitor
//! source is open, so on WSL a keep-alive thread holds one for the life of
//! the backend.
//!
//! Audio is delivered at 44100 Hz (see [`crate::speech::resample`]) so
//! WSLg's low-quality resampler is not involved.

use crate::platform::is_wsl;
use crate::speech::backends::pulseaudio::{espeak_voice_name, setup_pulseaudio, wpm_for_rate};
use crate::speech::resample::Upsampler;
use crate::speech::{SpeechCommand, Synth};
use crate::{Result, TdsrError};
use libloading::Library;
use log::{debug, info, warn};
use std::cell::Cell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// Sample rate delivered to PulseAudio (espeak-ng produces 22050 Hz).
const OUTPUT_RATE: u32 = 44100;

/// Length of the PCM chunks espeak-ng hands to the callback.
const CHUNK_MS: c_int = 20;

/// Playback buffer the server keeps for us. Bounds how much audio a cancel
/// cannot take back, and how long a write can block.
const TARGET_BUFFER_MS: u32 = 40;

/// Pauses at least this long become closed-stream gaps; shorter ones are
/// sent as silent samples.
const GAP_MIN_MS: u32 = 60;

/// Peak amplitude at or below which a chunk counts as silence.
const SILENCE_PEAK: u16 = 50;

/// How much sound to send between backlog checks.
const BACKLOG_CHECK_MS: u32 = 250;

/// Reported stream latency above which a drain gap is inserted, and the
/// level it is brought back to.
const BACKLOG_LIMIT_MS: u64 = 200;
const BACKLOG_TARGET_MS: u64 = 100;

/// Granularity of cancellable waits.
const SLEEP_STEP: Duration = Duration::from_millis(10);

/// How long `new` waits for the audio thread to bring the libraries up and
/// reach the server.
const INIT_TIMEOUT: Duration = Duration::from_secs(3);

/// Minimum interval between repeated warnings about a failing server.
const WARN_INTERVAL: Duration = Duration::from_secs(30);

// ---- libespeak-ng ----------------------------------------------------------

const AUDIO_OUTPUT_SYNCHRONOUS: c_int = 2;
const ESPEAK_INITIALIZE_DONT_EXIT: c_int = 0x8000;
const ESPEAK_CHARS_UTF8: c_uint = 1;
const ESPEAK_ENDPAUSE: c_uint = 0x1000;
const ESPEAK_POS_CHARACTER: c_int = 1;
const ESPEAK_RATE: c_int = 1;
const ESPEAK_VOLUME: c_int = 2;

type SynthCallback = unsafe extern "C" fn(*mut i16, c_int, *mut c_void) -> c_int;
type EspeakInitialize = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int) -> c_int;
type EspeakSetSynthCallback = unsafe extern "C" fn(Option<SynthCallback>);
type EspeakSynthFn = unsafe extern "C" fn(
    *const c_void,
    usize,
    c_uint,
    c_int,
    c_uint,
    c_uint,
    *mut c_uint,
    *mut c_void,
) -> c_int;
type EspeakSetParameter = unsafe extern "C" fn(c_int, c_int, c_int) -> c_int;
type EspeakSetVoiceByName = unsafe extern "C" fn(*const c_char) -> c_int;
type EspeakTerminate = unsafe extern "C" fn() -> c_int;

/// The espeak-ng library. Not thread-safe: used only by the audio thread.
struct Espeak {
    _lib: Library,
    synth: EspeakSynthFn,
    set_parameter: EspeakSetParameter,
    set_voice_by_name: EspeakSetVoiceByName,
    terminate: EspeakTerminate,
    /// Rate of the PCM it produces
    sample_rate: u32,
}

impl Espeak {
    fn load(callback: SynthCallback) -> Result<Self> {
        let err = |e: libloading::Error| TdsrError::Speech(format!("libespeak-ng: {}", e));
        // SAFETY: loading a well-known shared library and looking up its
        // documented entry points; the signatures match espeak_lib.h.
        unsafe {
            let lib = Library::new("libespeak-ng.so.1")
                .map_err(|e| TdsrError::Speech(format!("libespeak-ng not available: {}", e)))?;
            let initialize = *lib
                .get::<EspeakInitialize>(b"espeak_Initialize\0")
                .map_err(err)?;
            let set_synth_callback = *lib
                .get::<EspeakSetSynthCallback>(b"espeak_SetSynthCallback\0")
                .map_err(err)?;
            let synth = *lib.get::<EspeakSynthFn>(b"espeak_Synth\0").map_err(err)?;
            let set_parameter = *lib
                .get::<EspeakSetParameter>(b"espeak_SetParameter\0")
                .map_err(err)?;
            let set_voice_by_name = *lib
                .get::<EspeakSetVoiceByName>(b"espeak_SetVoiceByName\0")
                .map_err(err)?;
            let terminate = *lib
                .get::<EspeakTerminate>(b"espeak_Terminate\0")
                .map_err(err)?;

            let rate = initialize(
                AUDIO_OUTPUT_SYNCHRONOUS,
                CHUNK_MS,
                ptr::null(),
                ESPEAK_INITIALIZE_DONT_EXIT,
            );
            if rate <= 0 {
                return Err(TdsrError::Speech(
                    "espeak_Initialize failed (is espeak-ng-data installed?)".to_string(),
                ));
            }
            set_synth_callback(Some(callback));
            Ok(Self {
                _lib: lib,
                synth,
                set_parameter,
                set_voice_by_name,
                terminate,
                sample_rate: rate as u32,
            })
        }
    }

    fn set_voice(&self, name: &str) {
        if let Ok(c) = CString::new(name) {
            // SAFETY: valid NUL-terminated string, library initialised.
            if unsafe { (self.set_voice_by_name)(c.as_ptr()) } != 0 {
                warn!("espeak-ng voice '{}' not available", name);
            }
        }
    }

    fn set_rate(&self, wpm: u16) {
        // SAFETY: library initialised.
        unsafe { (self.set_parameter)(ESPEAK_RATE, wpm as c_int, 0) };
    }

    fn set_volume(&self, amplitude: u8) {
        // SAFETY: library initialised.
        unsafe { (self.set_parameter)(ESPEAK_VOLUME, amplitude as c_int, 0) };
    }

    /// Synthesise `text`, delivering PCM to the callback until it is done or
    /// the callback asks to stop.
    fn synth(&self, text: &str, end_pause: bool) {
        let Ok(c) = CString::new(text) else { return };
        let flags = ESPEAK_CHARS_UTF8 | if end_pause { ESPEAK_ENDPAUSE } else { 0 };
        // SAFETY: `c` outlives the (synchronous) call; size includes the NUL.
        unsafe {
            (self.synth)(
                c.as_ptr() as *const c_void,
                c.as_bytes_with_nul().len(),
                0,
                ESPEAK_POS_CHARACTER,
                0,
                flags,
                ptr::null_mut(),
                ptr::null_mut(),
            );
        }
    }
}

impl Drop for Espeak {
    fn drop(&mut self) {
        // SAFETY: library initialised; nothing else uses it afterwards.
        unsafe { (self.terminate)() };
    }
}

// ---- libpulse-simple -------------------------------------------------------

#[repr(C)]
struct PaSampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}

#[repr(C)]
struct PaBufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}

const PA_SAMPLE_S16LE: c_int = 3;
const PA_STREAM_PLAYBACK: c_int = 1;
const PA_STREAM_RECORD: c_int = 2;
const PA_INVALID: u32 = u32::MAX;

type PaSimpleNew = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    c_int,
    *const c_char,
    *const c_char,
    *const PaSampleSpec,
    *const c_void,
    *const PaBufferAttr,
    *mut c_int,
) -> *mut c_void;
type PaSimpleWrite = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int;
type PaSimpleRead = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type PaSimpleOp = unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int;
type PaSimpleFree = unsafe extern "C" fn(*mut c_void);
type PaSimpleGetLatency = unsafe extern "C" fn(*mut c_void, *mut c_int) -> u64;
type PaStrerror = unsafe extern "C" fn(c_int) -> *const c_char;

/// The PulseAudio simple API. Shared (read-only) by the audio and keep-alive
/// threads; each stream is used by one thread only.
struct Pulse {
    _simple: Library,
    _core: Library,
    new: PaSimpleNew,
    write: PaSimpleWrite,
    read: PaSimpleRead,
    flush: PaSimpleOp,
    free: PaSimpleFree,
    get_latency: PaSimpleGetLatency,
    strerror: PaStrerror,
}

impl Pulse {
    fn load() -> Result<Arc<Self>> {
        let err = |e: libloading::Error| TdsrError::Speech(format!("libpulse: {}", e));
        // SAFETY: as for Espeak::load; signatures match pulse/simple.h.
        unsafe {
            let simple = Library::new("libpulse-simple.so.0")
                .map_err(|e| TdsrError::Speech(format!("libpulse-simple not available: {}", e)))?;
            let core = Library::new("libpulse.so.0")
                .map_err(|e| TdsrError::Speech(format!("libpulse not available: {}", e)))?;
            Ok(Arc::new(Self {
                new: *simple.get::<PaSimpleNew>(b"pa_simple_new\0").map_err(err)?,
                write: *simple
                    .get::<PaSimpleWrite>(b"pa_simple_write\0")
                    .map_err(err)?,
                read: *simple
                    .get::<PaSimpleRead>(b"pa_simple_read\0")
                    .map_err(err)?,
                flush: *simple
                    .get::<PaSimpleOp>(b"pa_simple_flush\0")
                    .map_err(err)?,
                free: *simple
                    .get::<PaSimpleFree>(b"pa_simple_free\0")
                    .map_err(err)?,
                get_latency: *simple
                    .get::<PaSimpleGetLatency>(b"pa_simple_get_latency\0")
                    .map_err(err)?,
                strerror: *core.get::<PaStrerror>(b"pa_strerror\0").map_err(err)?,
                _simple: simple,
                _core: core,
            }))
        }
    }

    fn error(&self, code: c_int) -> String {
        // SAFETY: pa_strerror returns a static string for any code.
        unsafe {
            let p = (self.strerror)(code);
            if p.is_null() {
                format!("error {}", code)
            } else {
                CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        }
    }

    fn open(
        self: &Arc<Self>,
        direction: c_int,
        device: Option<&str>,
        name: &str,
        rate: u32,
        attr: &PaBufferAttr,
    ) -> Result<Stream> {
        let spec = PaSampleSpec {
            format: PA_SAMPLE_S16LE,
            rate,
            channels: 1,
        };
        let app = CString::new("tdsr").unwrap_or_default();
        let name = CString::new(name).unwrap_or_default();
        let device = device.and_then(|d| CString::new(d).ok());
        let mut err: c_int = 0;
        // SAFETY: all pointers are valid for the duration of the call.
        let handle = unsafe {
            (self.new)(
                ptr::null(),
                app.as_ptr(),
                direction,
                device.as_ref().map_or(ptr::null(), |d| d.as_ptr()),
                name.as_ptr(),
                &spec,
                ptr::null(),
                attr,
                &mut err,
            )
        };
        if handle.is_null() {
            return Err(TdsrError::Speech(format!(
                "PulseAudio stream failed: {}",
                self.error(err)
            )));
        }
        Ok(Stream {
            pulse: Arc::clone(self),
            handle,
        })
    }

    /// A playback stream with a small server-side buffer that starts playing
    /// as soon as data arrives.
    fn open_playback(self: &Arc<Self>, rate: u32) -> Result<Stream> {
        let attr = PaBufferAttr {
            maxlength: PA_INVALID,
            tlength: rate * 2 * TARGET_BUFFER_MS / 1000,
            prebuf: 0,
            minreq: PA_INVALID,
            fragsize: PA_INVALID,
        };
        self.open(PA_STREAM_PLAYBACK, None, "speech", rate, &attr)
    }

    /// A recording of the default sink's monitor (the keep-alive).
    fn open_monitor(self: &Arc<Self>, rate: u32) -> Result<Stream> {
        let attr = PaBufferAttr {
            maxlength: PA_INVALID,
            tlength: PA_INVALID,
            prebuf: PA_INVALID,
            minreq: PA_INVALID,
            fragsize: rate * 2 / 10,
        };
        self.open(
            PA_STREAM_RECORD,
            Some("@DEFAULT_MONITOR@"),
            "keep-alive",
            rate,
            &attr,
        )
    }
}

/// One PulseAudio stream; freed on drop.
struct Stream {
    pulse: Arc<Pulse>,
    handle: *mut c_void,
}

impl Stream {
    fn write(&self, samples: &[i16]) -> Result<()> {
        let mut err: c_int = 0;
        // SAFETY: valid handle and buffer.
        let rc = unsafe {
            (self.pulse.write)(
                self.handle,
                samples.as_ptr() as *const c_void,
                samples.len() * 2,
                &mut err,
            )
        };
        if rc < 0 {
            return Err(TdsrError::Speech(format!(
                "PulseAudio write failed: {}",
                self.pulse.error(err)
            )));
        }
        Ok(())
    }

    fn read(&self, buf: &mut [u8]) -> Result<()> {
        let mut err: c_int = 0;
        // SAFETY: valid handle and buffer.
        let rc = unsafe {
            (self.pulse.read)(
                self.handle,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut err,
            )
        };
        if rc < 0 {
            return Err(TdsrError::Speech(format!(
                "PulseAudio read failed: {}",
                self.pulse.error(err)
            )));
        }
        Ok(())
    }

    /// Discard everything the server still holds for this stream.
    fn flush(&self) {
        let mut err: c_int = 0;
        // SAFETY: valid handle.
        unsafe { (self.pulse.flush)(self.handle, &mut err) };
    }

    /// Time until the last sample written is played, per the server
    /// (buffered data plus sink latency). On WSL the sink part is the
    /// client's measured playback delay.
    fn latency_ms(&self) -> Option<u64> {
        let mut err: c_int = 0;
        // SAFETY: valid handle.
        let usec = unsafe { (self.pulse.get_latency)(self.handle, &mut err) };
        if usec == u64::MAX {
            None
        } else {
            Some(usec / 1000)
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // SAFETY: valid handle, freed exactly once.
        unsafe { (self.pulse.free)(self.handle) };
    }
}

// ---- queue shared with the event loop -------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Utterance {
    text: String,
    is_letter: bool,
}

#[derive(Clone, Debug)]
struct Settings {
    rate: u8,
    volume: u8,
    voice: String,
}

struct Queue {
    items: VecDeque<Utterance>,
    settings: Settings,
    settings_changed: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    /// Abort the current utterance and drop what is buffered
    cancel: AtomicBool,
    shutdown: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }
}

// ---- audio thread ---------------------------------------------------------

thread_local! {
    /// The player the espeak-ng callback delivers to (set around each
    /// `espeak_Synth` call on the audio thread).
    static PLAYER: Cell<*mut Player> = const { Cell::new(ptr::null_mut()) };
}

unsafe extern "C" fn synth_callback(wav: *mut i16, count: c_int, _events: *mut c_void) -> c_int {
    let player = PLAYER.with(|p| p.get());
    if player.is_null() {
        return 1;
    }
    if wav.is_null() {
        // End of synthesis; the trailing pause is handled after the call.
        return 0;
    }
    // SAFETY: espeak hands us `count` valid samples; the player pointer is
    // set by the thread that owns the player, for the duration of the call.
    let samples = if count > 0 {
        unsafe { std::slice::from_raw_parts(wav, count as usize) }
    } else {
        &[]
    };
    let player = unsafe { &mut *player };
    if player.on_chunk(samples) {
        0
    } else {
        1
    }
}

/// Plays the PCM of one utterance at a time.
struct Player {
    pulse: Arc<Pulse>,
    shared: Arc<Shared>,
    stream: Option<Stream>,
    upsampler: Upsampler,
    /// Rate of the samples espeak-ng delivers
    in_rate: u32,
    /// Silence (in input samples) seen since the last sound
    pending_silence: usize,
    /// Sound sent since the last backlog check
    sent_since_check_ms: u32,
    last_warning: Option<Instant>,
    wsl: bool,
}

impl Player {
    fn cancelled(&self) -> bool {
        self.shared.cancel.load(Ordering::SeqCst) || self.shared.shutdown.load(Ordering::SeqCst)
    }

    /// Sleep up to `ms`, stopping early on cancel. Returns false if cancelled.
    fn wait(&self, ms: u64) -> bool {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if self.cancelled() {
                return false;
            }
            let left = end.saturating_duration_since(Instant::now());
            thread::sleep(left.min(SLEEP_STEP));
        }
        !self.cancelled()
    }

    fn warn(&mut self, msg: &str) {
        if self
            .last_warning
            .map_or(true, |t| t.elapsed() >= WARN_INTERVAL)
        {
            warn!("{}", msg);
            self.last_warning = Some(Instant::now());
        } else {
            debug!("{}", msg);
        }
    }

    /// One chunk from espeak-ng. Returns false to stop synthesis.
    fn on_chunk(&mut self, samples: &[i16]) -> bool {
        if self.cancelled() {
            return false;
        }
        if samples.is_empty() {
            return true;
        }
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        if peak <= SILENCE_PEAK {
            self.pending_silence += samples.len();
            return true;
        }
        self.flush_silence(false) && self.check_backlog() && self.write(samples)
    }

    /// Deliver the silence seen so far: as a closed-stream gap if it is a
    /// real pause (or the end of the utterance), as samples if it is short.
    fn flush_silence(&mut self, end: bool) -> bool {
        let n = std::mem::take(&mut self.pending_silence);
        if n == 0 {
            return true;
        }
        let ms = (n as u64 * 1000 / self.in_rate as u64) as u32;
        if end || ms >= GAP_MIN_MS {
            self.gap(ms as u64)
        } else {
            let zeros = vec![0i16; n];
            self.write(&zeros)
        }
    }

    fn write(&mut self, samples: &[i16]) -> bool {
        if self.stream.is_none() {
            match self.pulse.open_playback(OUTPUT_RATE) {
                Ok(s) => {
                    self.stream = Some(s);
                    self.upsampler.reset();
                }
                Err(e) => {
                    self.warn(&format!("Speech output unavailable: {}", e));
                    return false;
                }
            }
        }
        let out = self.upsampler.process(samples);
        if let Err(e) = self.stream.as_ref().map_or(Ok(()), |s| s.write(&out)) {
            self.warn(&e.to_string());
            self.stream = None;
            return false;
        }
        self.sent_since_check_ms += (samples.len() as u64 * 1000 / self.in_rate as u64) as u32;
        true
    }

    /// On WSL, insert a gap when the client's backlog has grown too far.
    fn check_backlog(&mut self) -> bool {
        if !self.wsl || self.sent_since_check_ms < BACKLOG_CHECK_MS {
            return true;
        }
        self.sent_since_check_ms = 0;
        let Some(latency) = self.stream.as_ref().and_then(|s| s.latency_ms()) else {
            return true;
        };
        if latency > BACKLOG_LIMIT_MS {
            debug!("Audio backlog {} ms; draining", latency);
            return self.gap(latency - BACKLOG_TARGET_MS);
        }
        true
    }

    /// How long after the last write the stream may be closed without
    /// losing sound. On WSL, forwarded audio is already at the client, so
    /// only our server-side buffer matters; elsewhere the sink's buffer is
    /// rewound when a stream goes away, so wait for the whole latency.
    fn close_delay_ms(&self) -> u64 {
        let latency = self
            .stream
            .as_ref()
            .and_then(|s| s.latency_ms())
            .unwrap_or(TARGET_BUFFER_MS as u64);
        if self.wsl {
            latency.min(TARGET_BUFFER_MS as u64 + 10)
        } else {
            latency + 10
        }
    }

    /// A pause of `ms`: let the buffered sound finish, close the stream, and
    /// wait out the remainder. Returns false if cancelled meanwhile.
    fn gap(&mut self, ms: u64) -> bool {
        let start = Instant::now();
        if self.stream.is_some() {
            let delay = self.close_delay_ms();
            if !self.wait(delay) {
                return false;
            }
            self.stream = None;
            self.sent_since_check_ms = 0;
        }
        let elapsed = start.elapsed().as_millis() as u64;
        ms <= elapsed || self.wait(ms - elapsed)
    }

    /// After an utterance completed: play out its trailing pause (as a gap)
    /// and close the stream.
    fn finish(&mut self) {
        if !self.flush_silence(true) {
            self.abort();
            return;
        }
        if self.stream.is_some() {
            // Ended in sound (a letter): let it finish, then close.
            let delay = self.close_delay_ms();
            if !self.wait(delay) {
                self.abort();
                return;
            }
            self.stream = None;
        }
        self.sent_since_check_ms = 0;
    }

    /// Drop everything not yet played.
    fn abort(&mut self) {
        self.pending_silence = 0;
        self.sent_since_check_ms = 0;
        if let Some(s) = self.stream.take() {
            s.flush();
        }
    }
}

/// What the audio thread reports back to `new`.
type InitResult = std::result::Result<Arc<Pulse>, TdsrError>;

fn audio_thread(shared: Arc<Shared>, ready: mpsc::Sender<InitResult>) {
    let espeak = match Espeak::load(synth_callback) {
        Ok(e) => e,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let pulse = match Pulse::load() {
        Ok(p) => p,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    // Prove the server is reachable before reporting success
    if let Err(e) = pulse.open_playback(OUTPUT_RATE) {
        let _ = ready.send(Err(e));
        return;
    }
    let _ = ready.send(Ok(Arc::clone(&pulse)));

    let mut player = Player {
        pulse,
        shared: Arc::clone(&shared),
        stream: None,
        upsampler: Upsampler::new(),
        in_rate: espeak.sample_rate,
        pending_silence: 0,
        sent_since_check_ms: 0,
        last_warning: None,
        wsl: is_wsl(),
    };

    loop {
        let utterance = {
            let mut q = shared.lock();
            while q.items.is_empty() && !shared.shutdown.load(Ordering::SeqCst) {
                q = shared.wake.wait(q).unwrap_or_else(|e| e.into_inner());
            }
            if shared.shutdown.load(Ordering::SeqCst) {
                return;
            }
            if q.settings_changed {
                espeak.set_voice(&q.settings.voice);
                espeak.set_rate(wpm_for_rate(q.settings.rate));
                espeak.set_volume(q.settings.volume);
                q.settings_changed = false;
            }
            // Everything a cancel referred to has been removed from the
            // queue by now, so it is safe to arm the next utterance.
            shared.cancel.store(false, Ordering::SeqCst);
            q.items.pop_front()
        };
        let Some(utterance) = utterance else { continue };

        PLAYER.with(|p| p.set(&mut player as *mut Player));
        espeak.synth(&utterance.text, !utterance.is_letter);
        PLAYER.with(|p| p.set(ptr::null_mut()));

        if player.cancelled() {
            player.abort();
        } else {
            player.finish();
        }
    }
}

/// Holds a recording of the sink's monitor so WSLg's sink never suspends.
fn keepalive_thread(pulse: Arc<Pulse>, shared: Arc<Shared>) {
    let stream = match pulse.open_monitor(OUTPUT_RATE) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "Could not open the sink monitor for keep-alive ({}); speech after a pause may lag",
                e
            );
            return;
        }
    };
    debug!("Sink keep-alive recording open");
    let mut buf = vec![0u8; (OUTPUT_RATE * 2 / 10) as usize];
    while !shared.shutdown.load(Ordering::SeqCst) {
        if let Err(e) = stream.read(&mut buf) {
            warn!("Sink keep-alive stopped: {}", e);
            return;
        }
    }
}

// ---- the Synth ------------------------------------------------------------

/// espeak-ng speech with TDSR-managed PulseAudio playback.
pub struct EspeakSynth {
    shared: Arc<Shared>,
}

impl EspeakSynth {
    pub fn new() -> Result<Self> {
        debug!("Creating espeak-ng (in-process) backend");
        setup_pulseaudio()?;

        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue {
                items: VecDeque::new(),
                settings: Settings {
                    rate: 50,
                    volume: 80,
                    voice: "en".to_string(),
                },
                settings_changed: true,
            }),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
        });

        let (tx, rx) = mpsc::channel();
        let worker_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("tdsr-audio".to_string())
            .spawn(move || audio_thread(worker_shared, tx))
            .map_err(|e| TdsrError::Speech(format!("Failed to start audio thread: {}", e)))?;

        let pulse = match rx.recv_timeout(INIT_TIMEOUT) {
            Ok(Ok(pulse)) => pulse,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                shared.shutdown.store(true, Ordering::SeqCst);
                return Err(TdsrError::Speech(
                    "PulseAudio server not responding".to_string(),
                ));
            }
        };

        if is_wsl() {
            let ka_shared = Arc::clone(&shared);
            if let Err(e) = thread::Builder::new()
                .name("tdsr-keepalive".to_string())
                .spawn(move || keepalive_thread(pulse, ka_shared))
            {
                warn!("Could not start sink keep-alive thread: {}", e);
            }
        }

        info!("espeak-ng in-process backend ready");
        Ok(Self { shared })
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

    fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut q = self.shared.lock();
        f(&mut q.settings);
        q.settings_changed = true;
    }
}

impl Synth for EspeakSynth {
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
        let voice = espeak_voice_name(idx);
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

    /// Stop the current utterance at its next chunk, discard what the
    /// server still holds, and drop the queue.
    fn cancel(&mut self) -> Result<()> {
        debug!("Canceling speech");
        let mut q = self.shared.lock();
        q.items.clear();
        self.shared.cancel.store(true, Ordering::SeqCst);
        drop(q);
        self.shared.wake.notify_one();
        Ok(())
    }
}

impl Drop for EspeakSynth {
    fn drop(&mut self) {
        debug!("Shutting down espeak-ng backend");
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.cancel.store(true, Ordering::SeqCst);
        self.shared.lock().items.clear();
        self.shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_buffer_attr_is_small_and_starts_immediately() {
        let attr = PaBufferAttr {
            maxlength: PA_INVALID,
            tlength: OUTPUT_RATE * 2 * TARGET_BUFFER_MS / 1000,
            prebuf: 0,
            minreq: PA_INVALID,
            fragsize: PA_INVALID,
        };
        assert_eq!(attr.tlength, 3528); // 40 ms of s16 mono at 44100 Hz
        assert_eq!(attr.prebuf, 0);
    }

    #[test]
    fn create_backend_if_available() {
        match EspeakSynth::new() {
            Ok(_) => println!("✓ espeak-ng in-process backend available"),
            Err(e) => println!("⚠ espeak-ng in-process backend not available: {}", e),
        }
    }
}
