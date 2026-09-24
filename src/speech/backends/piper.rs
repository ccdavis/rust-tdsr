//! Piper neural voices (the VITS models of github.com/rhasspy/piper-voices)
//! for the ALSA backend, run in-process with rten, a pure-Rust ONNX runtime,
//! so the same code works on x86_64 and 32-bit x86 with no ONNX Runtime
//! library.
//!
//! The pipeline is Piper's own (OHF-Voice/piper1-gpl): espeak-ng turns the
//! text into IPA phonemes clause by clause
//! (`espeak_TextToPhonemesWithTerminator`); language-switch flags `(en)` are
//! dropped, each clause gets its punctuation back, clauses are grouped into
//! sentences, the phonemes are NFD-decomposed and mapped to the model's ids
//! with `^`, `_` between phonemes and `$`; the model runs once per sentence
//! and its float output is peak-normalised to 16-bit PCM.
//!
//! A voice is a `NAME.onnx` file with its `NAME.onnx.json` beside it.

use crate::speech::backends::espeak::PhonemeClause;
use crate::{Result, TdsrError};
use rten::Model;
use rten_tensor::prelude::*;
use rten_tensor::NdTensor;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

/// Longest sentence (in phoneme characters) given to the model in one run:
/// a longer one is ended at a clause boundary, or at a space inside a very
/// long clause. Text without full stops (a listing, code) would otherwise be
/// one run that takes seconds and a lot of memory, and cannot be cancelled.
pub const MAX_SENTENCE: usize = 400;

/// Peak level of the normalised output, below full scale so that Piper is
/// about as loud as espeak-ng and DECtalk.
const PEAK: f32 = 0.8;

// espeak-ng clause terminators (translate.h), after masking with 0xFFFFF
const CLAUSE_TYPE_SENTENCE: i32 = 0x0008_0000;
const CLAUSE_PERIOD: i32 = 40 | CLAUSE_TYPE_SENTENCE;
const CLAUSE_QUESTION: i32 = 40 | 0x2000 | CLAUSE_TYPE_SENTENCE;
const CLAUSE_EXCLAMATION: i32 = 45 | 0x3000 | CLAUSE_TYPE_SENTENCE;
const CLAUSE_COMMA: i32 = 20 | 0x1000 | 0x0004_0000;
const CLAUSE_COLON: i32 = 30 | 0x0004_0000;
const CLAUSE_SEMICOLON: i32 = 30 | 0x1000 | 0x0004_0000;

/// A voice found on disk.
#[derive(Clone, Debug)]
pub struct VoiceFile {
    /// File name without `.onnx`, e.g. `en_US-joe-medium`
    pub name: String,
    pub model: PathBuf,
    pub config: VoiceConfig,
}

/// Every voice in the `:`-separated directory list `dirs`, sorted by name;
/// a name found twice is taken from the first directory.
pub fn find_voices(dirs: &str) -> Vec<VoiceFile> {
    let mut voices: Vec<VoiceFile> = Vec::new();
    for dir in dirs.split(':').filter(|d| !d.is_empty()) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
            .collect();
        found.sort();
        for model in found {
            let Some(name) = model.file_stem().map(|s| s.to_string_lossy().into_owned()) else {
                continue;
            };
            if voices.iter().any(|v| v.name == name) {
                continue;
            }
            let json = model.with_extension("onnx.json");
            match std::fs::read_to_string(&json)
                .map_err(|e| e.to_string())
                .and_then(|s| VoiceConfig::parse(&s))
            {
                Ok(config) => voices.push(VoiceFile {
                    name,
                    model,
                    config,
                }),
                Err(e) => log::warn!("Piper voice {}: {}: {}", name, json.display(), e),
            }
        }
    }
    voices.sort_by(|a, b| a.name.cmp(&b.name));
    voices
}

/// The parts of a voice's `.onnx.json` that synthesis needs.
#[derive(Clone, Debug)]
pub struct VoiceConfig {
    pub sample_rate: u32,
    /// espeak-ng voice for phonemising (`espeak.voice`)
    pub espeak_voice: String,
    ids: HashMap<char, Vec<i64>>,
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
    /// Speaker id for multi-speaker models
    speaker: Option<i64>,
}

impl VoiceConfig {
    pub fn parse(json: &str) -> std::result::Result<Self, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let phoneme_type = v["phoneme_type"].as_str().unwrap_or("espeak");
        if phoneme_type != "espeak" {
            return Err(format!("phoneme_type {} is not supported", phoneme_type));
        }
        let mut ids = HashMap::new();
        for (k, list) in v["phoneme_id_map"].as_object().ok_or("no phoneme_id_map")? {
            let mut chars = k.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                continue;
            };
            let list: Vec<i64> = list
                .as_array()
                .ok_or("bad phoneme_id_map")?
                .iter()
                .filter_map(|n| n.as_i64())
                .collect();
            ids.insert(c, list);
        }
        for c in ['^', '_', '$'] {
            if !ids.contains_key(&c) {
                return Err(format!("phoneme_id_map has no '{}'", c));
            }
        }
        let f =
            |key: &str, default: f32| v["inference"][key].as_f64().map_or(default, |x| x as f32);
        let speakers = v["num_speakers"].as_i64().unwrap_or(1);
        Ok(Self {
            sample_rate: v["audio"]["sample_rate"]
                .as_u64()
                .ok_or("no audio.sample_rate")? as u32,
            espeak_voice: v["espeak"]["voice"].as_str().unwrap_or("en-us").to_string(),
            ids,
            noise_scale: f("noise_scale", 0.667),
            length_scale: f("length_scale", 1.0),
            noise_w: f("noise_w", 0.8),
            speaker: (speakers > 1).then(|| v["default_speaker_id"].as_i64().unwrap_or(0)),
        })
    }

    /// Model input for one sentence: `^ _ p1 _ p2 _ … $`; phonemes the model
    /// does not know are left out.
    pub fn phoneme_ids(&self, phonemes: &[char]) -> Vec<i64> {
        let mut ids = self.ids[&'^'].clone();
        ids.extend(&self.ids[&'_']);
        for p in phonemes {
            if let Some(i) = self.ids.get(p) {
                ids.extend(i);
                ids.extend(&self.ids[&'_']);
            }
        }
        ids.extend(&self.ids[&'$']);
        ids
    }
}

/// Group espeak-ng's clauses into sentences of phonemes, as Piper does:
/// language flags removed, the clause's punctuation appended (with a space
/// after `,` `:` `;`), NFD decomposition, a new sentence after `.` `?` `!`.
/// A clause ending without a known mark (an espeak-ng without
/// `espeak_TextToPhonemesWithTerminator` reports none) is followed by a
/// space, so that its last word does not run into the next clause.
/// Sentences are kept to [`MAX_SENTENCE`] phonemes.
pub(crate) fn sentences(clauses: &[PhonemeClause]) -> Vec<Vec<char>> {
    let mut out = Vec::new();
    let mut cur: Vec<char> = Vec::new();
    for clause in clauses {
        let mut text = strip_flags(&clause.phonemes);
        let term = clause.terminator & 0x000F_FFFF;
        match term {
            CLAUSE_PERIOD => text.push('.'),
            CLAUSE_QUESTION => text.push('?'),
            CLAUSE_EXCLAMATION => text.push('!'),
            CLAUSE_COMMA => text.push_str(", "),
            CLAUSE_COLON => text.push_str(": "),
            CLAUSE_SEMICOLON => text.push_str("; "),
            _ if term & CLAUSE_TYPE_SENTENCE == 0 => text.push(' '),
            _ => {}
        }
        cur.extend(text.nfd());
        if term & CLAUSE_TYPE_SENTENCE != 0 || cur.len() >= MAX_SENTENCE {
            push_sentence(&mut out, std::mem::take(&mut cur));
        }
    }
    push_sentence(&mut out, cur);
    out
}

/// Keep a sentence that has something to say, without trailing spaces,
/// cut at spaces into pieces of at most [`MAX_SENTENCE`].
fn push_sentence(out: &mut Vec<Vec<char>>, mut sentence: Vec<char>) {
    while sentence.len() > MAX_SENTENCE {
        let cut = sentence[..MAX_SENTENCE]
            .iter()
            .rposition(|c| c.is_whitespace())
            .filter(|&i| i > 0)
            .unwrap_or(MAX_SENTENCE);
        let rest = sentence.split_off(cut);
        push_sentence(out, sentence);
        sentence = rest.into_iter().skip_while(|c| c.is_whitespace()).collect();
    }
    while sentence.last().is_some_and(|c| c.is_whitespace()) {
        sentence.pop();
    }
    if has_phonemes(&sentence) {
        out.push(sentence);
    }
}

/// Anything to say besides spaces and punctuation?
fn has_phonemes(s: &[char]) -> bool {
    s.iter()
        .any(|c| !c.is_whitespace() && !matches!(c, '.' | ',' | '?' | '!' | ':' | ';'))
}

/// Remove espeak-ng's language-switch flags: `(en)wˈɜːd(fr)` -> `wˈɜːd`.
fn strip_flags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Piper's `length_scale` factor for a TDSR rate (0-100, 50 normal): twice
/// as fast at 100, half as fast at 0.
pub fn length_factor(rate: u8) -> f32 {
    2f32.powf((50.0 - rate.min(100) as f32) / 50.0)
}

/// The model's float output as 16-bit PCM: peak-normalised to [`PEAK`],
/// then scaled by `volume` (0-100).
pub fn to_pcm(samples: &[f32], volume: u8) -> Vec<i16> {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak < 1e-8 {
        return vec![0; samples.len()];
    }
    let gain = PEAK / peak * volume.min(100) as f32 / 100.0;
    samples
        .iter()
        .map(|s| ((s * gain).clamp(-1.0, 1.0) * 32767.0) as i16)
        .collect()
}

/// A loaded voice.
pub struct PiperVoice {
    model: Model,
    pub config: VoiceConfig,
}

impl PiperVoice {
    pub fn load(file: &VoiceFile) -> Result<Self> {
        let model = Model::load_file(&file.model).map_err(|e| {
            TdsrError::Speech(format!("Piper voice {}: {}", file.model.display(), e))
        })?;
        Ok(Self {
            model,
            config: file.config.clone(),
        })
    }

    /// Run the model on one sentence of phonemes; `length_factor` scales the
    /// voice's own `length_scale` (the speaking rate).
    pub fn synthesize(&self, phonemes: &[char], length_factor: f32) -> Result<Vec<f32>> {
        let err = |e: &dyn std::fmt::Display| TdsrError::Speech(format!("Piper: {}", e));
        let c = &self.config;
        let ids: Vec<i32> = c.phoneme_ids(phonemes).iter().map(|&i| i as i32).collect();
        let n = ids.len();
        let m = &self.model;
        let mut inputs = vec![
            (
                m.node_id("input").map_err(|e| err(&e))?,
                NdTensor::from_data([1, n], ids).into(),
            ),
            (
                m.node_id("input_lengths").map_err(|e| err(&e))?,
                NdTensor::from([n as i32]).into(),
            ),
            (
                m.node_id("scales").map_err(|e| err(&e))?,
                NdTensor::from([c.noise_scale, c.length_scale * length_factor, c.noise_w]).into(),
            ),
        ];
        // A multi-speaker model whose config lacks num_speakers still needs
        // a speaker: the default one.
        let speaker = c.speaker.or_else(|| m.find_node("sid").map(|_| 0));
        if let Some(sid) = speaker {
            inputs.push((
                m.node_id("sid").map_err(|e| err(&e))?,
                NdTensor::from([sid as i32]).into(),
            ));
        }
        let [out] = m
            .run_n(inputs, [m.node_id("output").map_err(|e| err(&e))?], None)
            .map_err(|e| err(&e))?;
        let out: rten_tensor::Tensor<f32> = out.try_into().map_err(|e| err(&e))?;
        Ok(out.iter().copied().collect())
    }
}

/// Where to look for voices when the config does not say.
pub fn default_dirs() -> String {
    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(
            Path::new(&home)
                .join(".local/share/piper-voices")
                .display()
                .to_string(),
        );
    }
    dirs.push("/usr/share/piper-voices".to_string());
    dirs.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &str = r#"{
        "audio": {"sample_rate": 22050},
        "espeak": {"voice": "en-us"},
        "inference": {"noise_scale": 0.5, "length_scale": 1.2, "noise_w": 0.7},
        "num_speakers": 1,
        "phoneme_id_map": {"_": [0], "^": [1], "$": [2], " ": [3], ",": [8],
                           ".": [10], "a": [14], "b": [15], "́": [50]}
    }"#;

    fn clause(p: &str, t: i32) -> PhonemeClause {
        PhonemeClause {
            phonemes: p.to_string(),
            terminator: t,
        }
    }

    #[test]
    fn config_and_ids() {
        let c = VoiceConfig::parse(JSON).unwrap();
        assert_eq!(c.sample_rate, 22050);
        assert_eq!(c.espeak_voice, "en-us");
        assert!(c.speaker.is_none());
        // unknown 'z' dropped; pad after BOS and every phoneme, none after EOS
        assert_eq!(c.phoneme_ids(&['a', 'z', 'b']), vec![1, 0, 14, 0, 15, 0, 2]);
        assert!(VoiceConfig::parse(r#"{"phoneme_type": "text"}"#).is_err());
    }

    #[test]
    fn clauses_become_sentences() {
        let s = sentences(&[
            clause("(en)ab(fr)", CLAUSE_COMMA | 0x100000),
            clause("ba", CLAUSE_PERIOD),
            clause("a", CLAUSE_QUESTION),
            clause("b", 0x0001_0000),
        ]);
        let s: Vec<String> = s.iter().map(|v| v.iter().collect()).collect();
        assert_eq!(s, vec!["ab, ba.", "a?", "b"]);
        // no terminators reported: clauses stay words apart, one sentence
        let s = sentences(&[clause("ab", 0), clause("ba", 0)]);
        assert_eq!(s, vec!["ab ba".chars().collect::<Vec<_>>()]);
        // NFD: a precomposed vowel splits into base + combining mark
        let s = sentences(&[clause("\u{e1}", CLAUSE_PERIOD)]);
        assert_eq!(s, vec![vec!['a', '\u{301}', '.']]);
        assert!(sentences(&[clause(" ", CLAUSE_PERIOD)]).is_empty());
        // No full stop in a long text: cut at clause boundaries...
        let comma = clause(&"ab ".repeat(100), CLAUSE_COMMA);
        let s = sentences(&[comma.clone(), comma.clone(), comma]);
        assert!(s.len() >= 2 && s.iter().all(|v| v.len() <= MAX_SENTENCE));
        // ...and inside one very long clause, at spaces
        let s = sentences(&[clause(&"abab ".repeat(300), 0)]);
        assert!(s.len() >= 3 && s.iter().all(|v| v.len() <= MAX_SENTENCE));
        assert!(s.iter().all(|v| !v[0].is_whitespace()));
        let total: usize = s
            .iter()
            .map(|v| v.iter().filter(|c| **c == 'a').count())
            .sum();
        assert_eq!(total, 600);
    }

    #[test]
    fn rate_and_pcm() {
        assert_eq!(length_factor(50), 1.0);
        assert!((length_factor(100) - 0.5).abs() < 1e-6);
        assert!((length_factor(0) - 2.0).abs() < 1e-6);
        let pcm = to_pcm(&[0.0, 0.5, -0.25], 100);
        assert_eq!(pcm[1], (PEAK * 32767.0) as i16);
        assert_eq!(pcm[2], (-PEAK / 2.0 * 32767.0) as i16);
        assert_eq!(to_pcm(&[0.0; 3], 100), vec![0; 3]);
    }
}
