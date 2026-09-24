//! RHVoice for the ALSA backend: a statistical parametric synthesiser made
//! for screen readers (`libRHVoice`, loaded with `dlopen`). Its data
//! directory holds `languages/English` and `voices/<name>`; the library finds
//! the voices itself, and each is offered as `rhvoice:<name>`.
//!
//! Speech arrives through the `play_speech` callback; returning 0 from it
//! stops synthesis, which is how a cancel ends an utterance at once. The
//! sample rate is only reported through a callback when speaking starts, so
//! each voice is measured with a short warm-up when it is selected.

use super::alsa::Engine;
use crate::{Result, TdsrError};
use libloading::Library;
use log::{info, warn};
use std::ffi::{c_char, c_double, c_int, c_uint, c_void, CStr, CString};
use std::ptr;

/// Voice id prefix in the ALSA backend's voice list (`rhvoice:slt`).
pub const VOICE_PREFIX: &str = "rhvoice:";

const MESSAGE_TEXT: c_int = 0;
const MESSAGE_CHARACTERS: c_int = 2;

type SetSampleRate = unsafe extern "C" fn(c_int, *mut c_void) -> c_int;
type PlaySpeech = unsafe extern "C" fn(*const i16, c_uint, *mut c_void) -> c_int;

/// `RHVoice_callbacks` (RHVoice.h): the two required ones, the rest unset.
#[repr(C)]
struct Callbacks {
    set_sample_rate: Option<SetSampleRate>,
    play_speech: Option<PlaySpeech>,
    process_mark: *const c_void,
    word_starts: *const c_void,
    word_ends: *const c_void,
    sentence_starts: *const c_void,
    sentence_ends: *const c_void,
    play_audio: *const c_void,
    done: *const c_void,
}

/// `RHVoice_init_params`
#[repr(C)]
struct InitParams {
    data_path: *const c_char,
    config_path: *const c_char,
    resource_paths: *const *const c_char,
    callbacks: Callbacks,
    options: c_uint,
}

/// `RHVoice_voice_info`
#[repr(C)]
struct VoiceInfo {
    language: *const c_char,
    name: *const c_char,
    gender: c_int,
    country: *const c_char,
}

/// `RHVoice_synth_params`
#[repr(C)]
struct SynthParams {
    voice_profile: *const c_char,
    absolute_rate: c_double,
    absolute_pitch: c_double,
    absolute_volume: c_double,
    relative_rate: c_double,
    relative_pitch: c_double,
    relative_volume: c_double,
    punctuation_mode: c_int,
    punctuation_list: *const c_char,
    capitals_mode: c_int,
    flags: c_int,
}

type NewEngine = unsafe extern "C" fn(*const InitParams) -> *mut c_void;
type DeleteEngine = unsafe extern "C" fn(*mut c_void);
type NumberOfVoices = unsafe extern "C" fn(*mut c_void) -> c_uint;
type GetVoices = unsafe extern "C" fn(*mut c_void) -> *const VoiceInfo;
type NewMessage = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    c_uint,
    c_int,
    *const SynthParams,
    *mut c_void,
) -> *mut c_void;
type DeleteMessage = unsafe extern "C" fn(*mut c_void);
type Speak = unsafe extern "C" fn(*mut c_void) -> c_int;

/// What the callbacks of one message work with.
struct Call<'a> {
    sink: &'a mut dyn FnMut(&[i16]) -> bool,
    rate: u32,
    gain: i32,
    scaled: Vec<i16>,
}

unsafe extern "C" fn set_sample_rate(rate: c_int, user: *mut c_void) -> c_int {
    // SAFETY: `user` is the `Call` of the synchronous RHVoice_speak.
    let call = unsafe { &mut *(user as *mut Call) };
    call.rate = rate.max(0) as u32;
    1
}

unsafe extern "C" fn play_speech(samples: *const i16, count: c_uint, user: *mut c_void) -> c_int {
    // SAFETY: as above; RHVoice hands `count` valid samples.
    let call = unsafe { &mut *(user as *mut Call) };
    let samples = if samples.is_null() || count == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(samples, count as usize) }
    };
    let ok = if call.gain >= 100 {
        (call.sink)(samples)
    } else {
        call.scaled.clear();
        call.scaled
            .extend(samples.iter().map(|&s| (s as i32 * call.gain / 100) as i16));
        (call.sink)(&call.scaled)
    };
    ok as c_int
}

/// RHVoice's `absolute_rate` (-1 to 1, 0 the voice's normal speed) for a
/// TDSR rate (0-100, 50 normal).
pub fn absolute_rate(rate: u8) -> f64 {
    (rate.min(100) as f64 - 50.0) / 50.0
}

pub struct RhvoiceEngine {
    _lib: Library,
    delete_engine: DeleteEngine,
    new_message: NewMessage,
    delete_message: DeleteMessage,
    speak: Speak,
    engine: *mut c_void,
    /// Voice names as RHVoice knows them (`SLT`), their languages (`en`, or
    /// `English` in some versions) and their CStrings
    voices: Vec<String>,
    languages: Vec<String>,
    profiles: Vec<CString>,
    current: usize,
    /// PCM rate of the current voice, measured when it was selected
    sample_rate: u32,
    rate: u8,
    volume: u8,
}

impl RhvoiceEngine {
    /// None when the library or the data is missing. `data` is a
    /// `:`-separated list; the first directory with `voices/` is used.
    pub fn new(data: &str, voice: &str, rate: u8) -> Option<Self> {
        let Some(dir) = data
            .split(':')
            .find(|d| !d.is_empty() && std::path::Path::new(d).join("voices").is_dir())
        else {
            info!("RHVoice not loaded: no voices in {}", data);
            return None;
        };
        match Self::load(dir) {
            Ok(mut e) => {
                e.rate = rate;
                let wanted = if voice.trim().is_empty() {
                    "slt"
                } else {
                    voice
                };
                if e.set_voice(wanted).is_err() {
                    // The first English voice, else the first one
                    e.current = e
                        .languages
                        .iter()
                        .position(|l| l.to_ascii_lowercase().starts_with("en"))
                        .unwrap_or(0);
                    e.measure();
                }
                Some(e)
            }
            Err(e) => {
                info!("RHVoice not loaded: {}", e);
                None
            }
        }
    }

    fn load(dir: &str) -> Result<Self> {
        let err = |e: libloading::Error| TdsrError::Speech(format!("libRHVoice: {}", e));
        // SAFETY: a known library and its documented entry points; the
        // structures above follow RHVoice.h and RHVoice_common.h.
        unsafe {
            let lib = Library::new("libRHVoice.so.1")
                .map_err(|e| TdsrError::Speech(format!("libRHVoice not available: {}", e)))?;
            let new_engine = *lib
                .get::<NewEngine>(b"RHVoice_new_tts_engine\0")
                .map_err(err)?;
            let delete_engine = *lib
                .get::<DeleteEngine>(b"RHVoice_delete_tts_engine\0")
                .map_err(err)?;
            let number = *lib
                .get::<NumberOfVoices>(b"RHVoice_get_number_of_voices\0")
                .map_err(err)?;
            let get_voices = *lib.get::<GetVoices>(b"RHVoice_get_voices\0").map_err(err)?;
            let new_message = *lib
                .get::<NewMessage>(b"RHVoice_new_message\0")
                .map_err(err)?;
            let delete_message = *lib
                .get::<DeleteMessage>(b"RHVoice_delete_message\0")
                .map_err(err)?;
            let speak = *lib.get::<Speak>(b"RHVoice_speak\0").map_err(err)?;
            let data_path = CString::new(dir).map_err(|e| TdsrError::Speech(e.to_string()))?;
            let config_path = CString::new("/etc/RHVoice").expect("no NUL");
            let params = InitParams {
                data_path: data_path.as_ptr(),
                config_path: config_path.as_ptr(),
                resource_paths: ptr::null(),
                callbacks: Callbacks {
                    set_sample_rate: Some(set_sample_rate),
                    play_speech: Some(play_speech),
                    process_mark: ptr::null(),
                    word_starts: ptr::null(),
                    word_ends: ptr::null(),
                    sentence_starts: ptr::null(),
                    sentence_ends: ptr::null(),
                    play_audio: ptr::null(),
                    done: ptr::null(),
                },
                options: 0,
            };
            let engine = new_engine(&params);
            if engine.is_null() {
                return Err(TdsrError::Speech(format!(
                    "RHVoice found no usable data in {}",
                    dir
                )));
            }
            let n = number(engine) as usize;
            let list = get_voices(engine);
            let mut found: Vec<(String, String)> = Vec::new();
            for i in 0..n {
                let v = &*list.add(i);
                if !v.name.is_null() {
                    let language = if v.language.is_null() {
                        String::new()
                    } else {
                        CStr::from_ptr(v.language).to_string_lossy().into_owned()
                    };
                    found.push((
                        CStr::from_ptr(v.name).to_string_lossy().into_owned(),
                        language,
                    ));
                }
            }
            found.sort_by_key(|(v, _)| v.to_ascii_lowercase());
            let (voices, languages): (Vec<String>, Vec<String>) = found.into_iter().unzip();
            if voices.is_empty() {
                delete_engine(engine);
                return Err(TdsrError::Speech(format!("no RHVoice voices in {}", dir)));
            }
            let profiles = voices
                .iter()
                .map(|v| CString::new(v.as_str()).unwrap_or_default())
                .collect();
            info!("RHVoice voices in {}: {:?}", dir, voices);
            Ok(Self {
                _lib: lib,
                delete_engine,
                new_message,
                delete_message,
                speak,
                engine,
                voices,
                languages,
                profiles,
                current: 0,
                sample_rate: 24000,
                rate: 50,
                volume: 100,
            })
        }
    }

    /// Voice ids without the prefix, lower case (`slt`).
    pub fn names(&self) -> Vec<String> {
        self.voices.iter().map(|v| v.to_ascii_lowercase()).collect()
    }

    /// Speak `text` into `sink`; returns the sample rate RHVoice reported.
    fn run(&mut self, text: &str, kind: c_int, sink: &mut dyn FnMut(&[i16]) -> bool) -> u32 {
        // RHVoice rejects an empty message
        if text.trim().is_empty() {
            return self.sample_rate;
        }
        let c = CString::new(text.replace('\0', " ")).expect("NULs replaced");
        let params = SynthParams {
            voice_profile: self.profiles[self.current].as_ptr(),
            absolute_rate: absolute_rate(self.rate),
            absolute_pitch: 0.0,
            absolute_volume: 0.0,
            relative_rate: 1.0,
            relative_pitch: 1.0,
            relative_volume: 1.0,
            punctuation_mode: 0,
            punctuation_list: ptr::null(),
            capitals_mode: 0,
            flags: 0,
        };
        let mut call = Call {
            sink,
            rate: self.sample_rate,
            gain: self.volume as i32,
            scaled: Vec::new(),
        };
        // SAFETY: text, params and `call` outlive the synchronous speak;
        // the message is deleted afterwards.
        unsafe {
            let msg = (self.new_message)(
                self.engine,
                c.as_ptr(),
                c.as_bytes().len() as c_uint,
                kind,
                &params,
                &mut call as *mut Call as *mut c_void,
            );
            if msg.is_null() {
                warn!("RHVoice could not take the text");
                return self.sample_rate;
            }
            (self.speak)(msg);
            (self.delete_message)(msg);
        }
        call.rate
    }

    /// Find the current voice's sample rate with a short warm-up (which
    /// also loads the voice before it is first needed).
    fn measure(&mut self) {
        let rate = self.run("ready", MESSAGE_TEXT, &mut |_| true);
        if rate > 0 {
            self.sample_rate = rate;
        }
    }
}

impl Drop for RhvoiceEngine {
    fn drop(&mut self) {
        // SAFETY: engine created by `load`, not used afterwards.
        unsafe { (self.delete_engine)(self.engine) };
    }
}

impl Engine for RhvoiceEngine {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
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
            .position(|v| v.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no RHVoice voice {}", name))?;
        self.current = i;
        self.measure();
        Ok(format!("R H voice {}", self.voices[i]))
    }

    fn synth(&mut self, text: &str, is_letter: bool, sink: &mut dyn FnMut(&[i16]) -> bool) {
        let single = text.trim().chars().count() == 1;
        let kind = if is_letter && single {
            MESSAGE_CHARACTERS
        } else {
            MESSAGE_TEXT
        };
        let rate = self.run(text.trim(), kind, sink);
        if rate != self.sample_rate {
            warn!(
                "RHVoice switched to {} Hz while the device was open at {} Hz",
                rate, self.sample_rate
            );
            self.sample_rate = rate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_mapping() {
        assert_eq!(absolute_rate(50), 0.0);
        assert_eq!(absolute_rate(100), 1.0);
        assert_eq!(absolute_rate(0), -1.0);
    }
}
