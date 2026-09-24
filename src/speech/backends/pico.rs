//! SVOX Pico for the ALSA backend: the small Android-era synthesiser
//! (`libttspico`, loaded with `dlopen` like `libespeak-ng`), 16 kHz, one voice
//! per language. A voice is a pair of lingware files in the language
//! directory (`/usr/share/pico/lang`): `en-US_ta.bin` (text analysis) and
//! `en-US_lh0_sg.bin` (signal generation).
//!
//! Text goes in with `pico_putTextUtf8` and speech comes out in small steps of
//! `pico_getData`, so a cancel is noticed between steps and
//! `pico_resetEngine` drops the rest. The rate is Pico's own `<speed>` markup.

use super::alsa::Engine;
use crate::{Result, TdsrError};
use libloading::Library;
use log::{info, warn};
use std::ffi::{c_char, c_int, c_short, c_uint, c_void, CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

/// Voice id prefix in the ALSA backend's voice list (`pico:en-US`).
pub const VOICE_PREFIX: &str = "pico:";

const SAMPLE_RATE: u32 = 16000;
/// Working memory Pico is given (what pico2wave uses).
const MEMORY: usize = 2_500_000;
const PICO_OK: c_int = 0;
const PICO_STEP_IDLE: c_int = 200;
const PICO_STEP_BUSY: c_int = 201;
const PICO_RESET_FULL: c_int = 0;
const PICO_RESET_SOFT: c_int = 0x10;
/// Name under which the loaded resources are combined into a voice.
const VOICE_DEF: &[u8] = b"tdsr\0";
/// Samples per `pico_getData` step.
const STEP_SAMPLES: usize = 512;
/// Samples gathered before they go to the device (Pico hands out a few
/// dozen per step).
const WRITE_SAMPLES: usize = 1024;
/// Rounds in which Pico takes no text and gives no audio, and busy steps in
/// a row without audio, after which the utterance is given up on: guards
/// against a loop that never ends, far beyond any real analysis time.
const IDLE_ROUNDS: u32 = 1000;
const BUSY_STEPS: u32 = 200_000;

type Sys = *mut c_void;
type Res = *mut c_void;
type Eng = *mut c_void;
type PInitialize = unsafe extern "C" fn(*mut c_void, c_uint, *mut Sys) -> c_int;
type PTerminate = unsafe extern "C" fn(*mut Sys) -> c_int;
type PLoadResource = unsafe extern "C" fn(Sys, *const u8, *mut Res) -> c_int;
type PUnloadResource = unsafe extern "C" fn(Sys, *mut Res) -> c_int;
type PGetResourceName = unsafe extern "C" fn(Sys, Res, *mut c_char) -> c_int;
type PVoiceDef = unsafe extern "C" fn(Sys, *const u8) -> c_int;
type PAddResource = unsafe extern "C" fn(Sys, *const u8, *const u8) -> c_int;
type PNewEngine = unsafe extern "C" fn(Sys, *const u8, *mut Eng) -> c_int;
type PDisposeEngine = unsafe extern "C" fn(Sys, *mut Eng) -> c_int;
type PPutText = unsafe extern "C" fn(Eng, *const u8, c_short, *mut c_short) -> c_int;
type PGetData =
    unsafe extern "C" fn(Eng, *mut c_void, c_short, *mut c_short, *mut c_short) -> c_int;
type PResetEngine = unsafe extern "C" fn(Eng, c_int) -> c_int;

struct Api {
    _lib: Library,
    initialize: PInitialize,
    terminate: PTerminate,
    load_resource: PLoadResource,
    unload_resource: PUnloadResource,
    get_resource_name: PGetResourceName,
    create_voice: PVoiceDef,
    add_resource: PAddResource,
    release_voice: PVoiceDef,
    new_engine: PNewEngine,
    dispose_engine: PDisposeEngine,
    put_text: PPutText,
    get_data: PGetData,
    reset_engine: PResetEngine,
}

impl Api {
    fn load() -> Result<Self> {
        let err = |e: libloading::Error| TdsrError::Speech(format!("libttspico: {}", e));
        // SAFETY: loading a known library and its documented entry points;
        // the signatures match picoapi.h (pico_Status is int, pico_Int16
        // short, pico_Uint32 unsigned int, handles are pointers).
        unsafe {
            let lib = Library::new("libttspico.so.0")
                .map_err(|e| TdsrError::Speech(format!("libttspico not available: {}", e)))?;
            macro_rules! f {
                ($t:ty, $n:literal) => {
                    *lib.get::<$t>($n).map_err(err)?
                };
            }
            Ok(Self {
                initialize: f!(PInitialize, b"pico_initialize\0"),
                terminate: f!(PTerminate, b"pico_terminate\0"),
                load_resource: f!(PLoadResource, b"pico_loadResource\0"),
                unload_resource: f!(PUnloadResource, b"pico_unloadResource\0"),
                get_resource_name: f!(PGetResourceName, b"pico_getResourceName\0"),
                create_voice: f!(PVoiceDef, b"pico_createVoiceDefinition\0"),
                add_resource: f!(PAddResource, b"pico_addResourceToVoiceDefinition\0"),
                release_voice: f!(PVoiceDef, b"pico_releaseVoiceDefinition\0"),
                new_engine: f!(PNewEngine, b"pico_newEngine\0"),
                dispose_engine: f!(PDisposeEngine, b"pico_disposeEngine\0"),
                put_text: f!(PPutText, b"pico_putTextUtf8\0"),
                get_data: f!(PGetData, b"pico_getData\0"),
                reset_engine: f!(PResetEngine, b"pico_resetEngine\0"),
                _lib: lib,
            })
        }
    }
}

/// A language's two lingware files.
#[derive(Clone, Debug)]
pub struct PicoVoice {
    /// Language tag, e.g. `en-US`
    pub name: String,
    ta: PathBuf,
    sg: PathBuf,
}

/// The voices in `dir`: every `LANG_ta.bin` with a `LANG_*_sg.bin` beside it.
pub fn find_voices(dir: &Path) -> Vec<PicoVoice> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<String> = entries
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    let mut voices: Vec<PicoVoice> = files
        .iter()
        .filter_map(|f| {
            let lang = f.strip_suffix("_ta.bin")?;
            let sg = files
                .iter()
                .find(|s| s.starts_with(&format!("{}_", lang)) && s.ends_with("_sg.bin"))?;
            Some(PicoVoice {
                name: lang.to_string(),
                ta: dir.join(f),
                sg: dir.join(sg),
            })
        })
        .collect();
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    voices
}

/// Pico's `<speed>` level (percent) for a TDSR rate: 100 at 50, three
/// times as fast at 100, a third at 0.
pub fn speed_level(rate: u8) -> u32 {
    (100.0 * 3f32.powf((rate.min(100) as f32 - 50.0) / 50.0)).round() as u32
}

/// English letter names for key echo: Pico reads a lone "a" as the article.
pub fn letter_name(c: char, british: bool) -> Option<&'static str> {
    const NAMES: [&str; 26] = [
        "ay",
        "bee",
        "see",
        "dee",
        "ee",
        "eff",
        "gee",
        "aitch",
        "eye",
        "jay",
        "kay",
        "el",
        "em",
        "en",
        "oh",
        "pee",
        "cue",
        "ar",
        "ess",
        "tee",
        "you",
        "vee",
        "double you",
        "ex",
        "why",
        "zee",
    ];
    let c = c.to_ascii_lowercase();
    if !c.is_ascii_lowercase() {
        return None;
    }
    if c == 'z' && british {
        return Some("zed");
    }
    Some(NAMES[(c as u8 - b'a') as usize])
}

/// The text as Pico input: markup of our own around it, and every `<` in
/// the text followed by a space so that the text cannot open a tag (Pico
/// crashes on some, such as `<spell>`).
pub fn markup(text: &str, level: u32) -> String {
    format!(
        "<speed level=\"{}\">{}</speed>",
        level,
        text.replace('<', "< ")
    )
}

/// The loaded voice: its resources and the engine built on them.
struct Loaded {
    ta: Res,
    sg: Res,
    engine: Eng,
}

pub struct PicoEngine {
    api: Api,
    /// Pico's working memory; must outlive the system
    _memory: Vec<u8>,
    system: Sys,
    voices: Vec<PicoVoice>,
    current: usize,
    loaded: Option<Loaded>,
    rate: u8,
    volume: u8,
}

impl PicoEngine {
    /// None when the library or the lingware is missing.
    pub fn new(lang_dir: &str, voice: &str, rate: u8) -> Option<Self> {
        let voices = find_voices(Path::new(lang_dir));
        if voices.is_empty() {
            info!("Pico not loaded: no lingware in {}", lang_dir);
            return None;
        }
        let api = match Api::load() {
            Ok(a) => a,
            Err(e) => {
                info!("Pico not loaded: {}", e);
                return None;
            }
        };
        let mut memory = vec![0u8; MEMORY];
        let mut system: Sys = ptr::null_mut();
        // SAFETY: the memory block outlives the system (kept in the struct).
        let r = unsafe {
            (api.initialize)(
                memory.as_mut_ptr() as *mut c_void,
                MEMORY as c_uint,
                &mut system,
            )
        };
        if r != PICO_OK {
            warn!("Pico not loaded: pico_initialize returned {}", r);
            return None;
        }
        let mut engine = Self {
            api,
            _memory: memory,
            system,
            current: voices.iter().position(|v| v.name == "en-US").unwrap_or(0),
            voices,
            loaded: None,
            rate,
            volume: 100,
        };
        if !voice.trim().is_empty() {
            if let Err(e) = engine.set_voice(voice) {
                warn!("{}", e);
            }
        }
        Some(engine)
    }

    pub fn names(&self) -> Vec<String> {
        self.voices.iter().map(|v| v.name.clone()).collect()
    }

    fn unload(&mut self) {
        if let Some(mut l) = self.loaded.take() {
            // SAFETY: handles created by this system and not used afterwards.
            unsafe {
                (self.api.dispose_engine)(self.system, &mut l.engine);
                (self.api.release_voice)(self.system, VOICE_DEF.as_ptr());
                (self.api.unload_resource)(self.system, &mut l.sg);
                (self.api.unload_resource)(self.system, &mut l.ta);
            }
        }
    }

    /// The engine for the current voice, loading its lingware on first use.
    fn engine(&mut self) -> Option<Eng> {
        if self.loaded.is_none() {
            match self.load(self.current) {
                Ok(l) => self.loaded = Some(l),
                Err(e) => warn!("{}", e),
            }
        }
        self.loaded.as_ref().map(|l| l.engine)
    }

    fn load(&self, i: usize) -> std::result::Result<Loaded, String> {
        let v = &self.voices[i];
        let api = &self.api;
        let path = |p: &Path| CString::new(p.as_os_str().as_bytes()).map_err(|e| e.to_string());
        let (ta_path, sg_path) = (path(&v.ta)?, path(&v.sg)?);
        let fail = |what: &str, r: c_int| format!("Pico voice {}: {} returned {}", v.name, what, r);
        let mut ta: Res = ptr::null_mut();
        let mut sg: Res = ptr::null_mut();
        let mut engine: Eng = ptr::null_mut();
        let mut ta_name = [0 as c_char; 200];
        let mut sg_name = [0 as c_char; 200];
        // SAFETY: documented call sequence (pico2wave's); names are
        // NUL-terminated buffers of PICO_RETSTRINGSIZE.
        unsafe {
            let r = (api.load_resource)(self.system, ta_path.as_ptr() as *const u8, &mut ta);
            if r != PICO_OK {
                return Err(fail("loading the text analysis file", r));
            }
            let r = (api.load_resource)(self.system, sg_path.as_ptr() as *const u8, &mut sg);
            if r != PICO_OK {
                (api.unload_resource)(self.system, &mut ta);
                return Err(fail("loading the signal generation file", r));
            }
            (api.get_resource_name)(self.system, ta, ta_name.as_mut_ptr());
            (api.get_resource_name)(self.system, sg, sg_name.as_mut_ptr());
            let mut r = (api.create_voice)(self.system, VOICE_DEF.as_ptr());
            let mut what = "pico_createVoiceDefinition";
            if r == PICO_OK {
                r = (api.add_resource)(
                    self.system,
                    VOICE_DEF.as_ptr(),
                    ta_name.as_ptr() as *const u8,
                );
                what = "adding the text analysis resource";
            }
            if r == PICO_OK {
                r = (api.add_resource)(
                    self.system,
                    VOICE_DEF.as_ptr(),
                    sg_name.as_ptr() as *const u8,
                );
                what = "adding the signal generation resource";
            }
            if r == PICO_OK {
                r = (api.new_engine)(self.system, VOICE_DEF.as_ptr(), &mut engine);
                what = "pico_newEngine";
            }
            if r != PICO_OK {
                (api.release_voice)(self.system, VOICE_DEF.as_ptr());
                (api.unload_resource)(self.system, &mut sg);
                (api.unload_resource)(self.system, &mut ta);
                return Err(fail(what, r));
            }
        }
        info!(
            "Pico voice {} loaded ({})",
            v.name,
            // SAFETY: NUL-terminated by Pico.
            unsafe { CStr::from_ptr(ta_name.as_ptr()) }.to_string_lossy()
        );
        Ok(Loaded { ta, sg, engine })
    }
}

impl Drop for PicoEngine {
    fn drop(&mut self) {
        self.unload();
        // SAFETY: no engine left; the memory block is still alive.
        unsafe { (self.api.terminate)(&mut self.system) };
    }
}

impl Engine for PicoEngine {
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
        let name = id.trim().strip_prefix(VOICE_PREFIX).unwrap_or(id.trim());
        let i = self
            .voices
            .iter()
            .position(|v| v.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no Pico voice {}", name))?;
        if i != self.current {
            self.unload();
            self.current = i;
        }
        Ok(format!("Pico {}", self.voices[i].name))
    }

    fn synth(&mut self, text: &str, is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool) {
        let british = self.voices[self.current].name == "en-GB";
        let english = self.voices[self.current].name.starts_with("en");
        let mut chars = text.trim().chars();
        let text = match (chars.next(), chars.next()) {
            (Some(c), None) if is_letter && english => letter_name(c, british).unwrap_or(text),
            _ => text,
        };
        let input = CString::new(markup(&text.replace('\0', " "), speed_level(self.rate)))
            .expect("NULs replaced");
        let gain = self.volume as i32;
        let Some(engine) = self.engine() else { return };
        let api = &self.api;
        let reset = |mode: c_int| {
            // SAFETY: an engine of this system; a soft reset drops the rest
            // of the text, a full one recovers from an engine error.
            unsafe { (api.reset_engine)(engine, mode) };
        };
        // The text with its terminating NUL, which makes Pico finish it.
        let mut rest = input.as_bytes_with_nul();
        let mut buf = [0i16; STEP_SAMPLES];
        let mut out: Vec<i16> = Vec::with_capacity(WRITE_SAMPLES + STEP_SAMPLES);
        let mut stalled = 0u32;
        loop {
            if !rest.is_empty() {
                let mut put: c_short = 0;
                let n = rest.len().min(c_short::MAX as usize) as c_short;
                // SAFETY: `rest` is valid for `n` bytes during the call.
                let r = unsafe { (api.put_text)(engine, rest.as_ptr(), n, &mut put) };
                if r != PICO_OK {
                    warn!("Pico: pico_putTextUtf8 returned {}", r);
                    reset(PICO_RESET_FULL);
                    return;
                }
                rest = &rest[(put.max(0) as usize).min(rest.len())..];
            }
            // Speech for what went in, until Pico wants more text or is done
            let mut quiet = 0u32;
            let status = loop {
                let (mut got, mut kind): (c_short, c_short) = (0, 0);
                // SAFETY: `buf` holds STEP_SAMPLES samples (the size is in
                // bytes).
                let r = unsafe {
                    (api.get_data)(
                        engine,
                        buf.as_mut_ptr() as *mut c_void,
                        (STEP_SAMPLES * 2) as c_short,
                        &mut got,
                        &mut kind,
                    )
                };
                if r != PICO_STEP_BUSY && r != PICO_STEP_IDLE {
                    break r;
                }
                let samples = &buf[..(got.max(0) as usize / 2).min(STEP_SAMPLES)];
                if samples.is_empty() {
                    quiet += 1;
                } else {
                    quiet = 0;
                    stalled = 0;
                    out.extend(samples.iter().map(|&s| (s as i32 * gain / 100) as i16));
                }
                // Hand the audio over in larger pieces, or ask whether it is
                // still wanted while Pico is busy without output.
                let flush = out.len() >= WRITE_SAMPLES || r == PICO_STEP_IDLE;
                let wanted = if flush && !out.is_empty() {
                    let ok = sink(&out);
                    out.clear();
                    ok
                } else if quiet > 0 && quiet % 64 == 0 {
                    sink(&[])
                } else {
                    true
                };
                if !wanted {
                    reset(PICO_RESET_SOFT);
                    return;
                }
                if r == PICO_STEP_IDLE {
                    break r;
                }
                if quiet >= BUSY_STEPS {
                    warn!("Pico is busy without output; giving up on this utterance");
                    reset(PICO_RESET_SOFT);
                    return;
                }
            };
            if status != PICO_STEP_BUSY && status != PICO_STEP_IDLE {
                warn!(
                    "Pico: pico_getData returned {}; resetting the engine",
                    status
                );
                reset(PICO_RESET_FULL);
                return;
            }
            if rest.is_empty() {
                if !out.is_empty() {
                    sink(&out);
                }
                return;
            }
            stalled += 1;
            if stalled >= IDLE_ROUNDS {
                warn!("Pico takes no more text; giving up on this utterance");
                reset(PICO_RESET_SOFT);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_and_markup() {
        assert_eq!(speed_level(50), 100);
        assert_eq!(speed_level(100), 300);
        assert_eq!(speed_level(0), 33);
        assert_eq!(
            markup("a <b> c", 100),
            "<speed level=\"100\">a < b> c</speed>"
        );
    }

    #[test]
    fn letters() {
        assert_eq!(letter_name('A', false), Some("ay"));
        assert_eq!(letter_name('z', false), Some("zee"));
        assert_eq!(letter_name('z', true), Some("zed"));
        assert_eq!(letter_name('5', false), None);
    }

    #[test]
    fn voices_are_pairs_of_files() {
        let dir = std::env::temp_dir().join(format!("tdsr-pico-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in [
            "en-US_ta.bin",
            "en-US_lh0_sg.bin",
            "en-GB_ta.bin",
            "de-DE_gl0_sg.bin",
        ] {
            std::fs::write(dir.join(f), b"").unwrap();
        }
        let v = find_voices(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "en-US");
        assert!(v[0].sg.ends_with("en-US_lh0_sg.bin"));
    }
}
