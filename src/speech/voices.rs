//! The espeak-ng voice catalogue shared by the Linux/WSL backends.
//!
//! espeak-ng reports two kinds of voices: its own (formant synthesis, always
//! usable once `espeak-ng-data` is installed) and MBROLA voices, which are
//! only definitions unless the `mbrola` program and that voice's diphone
//! database are installed as well. The catalogue lists espeak-ng's own voices
//! first, in the order espeak-ng reports them, then every MBROLA voice
//! definition in the same order. The index is what the config menu and
//! `tdsr --list-voices` number; what the config stores is the voice's
//! identifier (its voice file, `gmw/en-US`, `mb/mb-us1`), which
//! `espeak_SetVoiceByName` accepts and which does not depend on the list.
//!
//! Whether an MBROLA database is installed is probed the way espeak-ng
//! itself looks for it (`<data>/mbrola/<name>`, then `/usr/share/mbrola/`),
//! so the listing can hide definitions that cannot work and a selection can
//! be refused with a useful message before espeak-ng (and mbrola) print
//! their own complaints to the terminal. The in-process backend still lets
//! espeak-ng's actual answer decide whether a selection stands.
//!
//! The in-process backend fills the catalogue from `espeak_ListVoices`; the
//! subprocess backend parses `espeak-ng --voices` (and `--voices=mb`), which
//! print the same lists in the same order.

use crate::{Result, TdsrError};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// The gender espeak-ng attributes to a voice (0 = unspecified).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gender {
    Unknown,
    Male,
    Female,
}

impl Gender {
    /// From espeak-ng's `espeak_VOICE.gender` byte.
    pub fn from_code(code: u8) -> Self {
        match code {
            1 => Gender::Male,
            2 => Gender::Female,
            _ => Gender::Unknown,
        }
    }

    /// From the `Age/Gender` column of `espeak-ng --voices` (`--/M`, `--/F`).
    fn from_listing(col: &str) -> Self {
        match col.rsplit('/').next() {
            Some("M") => Gender::Male,
            Some("F") => Gender::Female,
            _ => Gender::Unknown,
        }
    }
}

/// One voice espeak-ng knows about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EspeakVoice {
    /// Display name, e.g. `English (America)` or `us-mbrola-1`.
    pub name: String,
    /// Voice file relative to espeak-ng's voices directory, e.g. `gmw/en-US`
    /// or `mb/mb-us1`. This is what the backends select the voice by and
    /// what the config stores.
    pub identifier: String,
    /// Primary language tag, e.g. `en-us`.
    pub language: String,
    pub gender: Gender,
    /// For MBROLA voices, whether `mbrola` and the voice's database are
    /// installed. Always true for espeak-ng's own voices.
    pub installed: bool,
}

impl EspeakVoice {
    /// Whether this is an MBROLA voice definition.
    pub fn is_mbrola(&self) -> bool {
        self.identifier.starts_with("mb/")
    }

    /// The MBROLA database this voice needs (`us1` for `mb/mb-us1-en`).
    pub fn mbrola_database(&self) -> Option<&str> {
        let rest = self.identifier.strip_prefix("mb/mb-")?;
        Some(rest.split('-').next().unwrap_or(rest))
    }

    /// The last component of the identifier (`en-US` for `gmw/en-US`),
    /// which espeak-ng also accepts as the voice's name.
    fn basename(&self) -> &str {
        self.identifier
            .rsplit('/')
            .next()
            .unwrap_or(&self.identifier)
    }

    /// Short spoken description.
    pub fn describe(&self) -> String {
        let mut s = self.name.clone();
        if self.is_mbrola() {
            s.push_str(", MBROLA");
        }
        s
    }
}

/// Whether `program` is on `PATH`.
pub(crate) fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
        .unwrap_or(false)
}

/// Whether the MBROLA database `name` is where espeak-ng looks for it: its
/// own data directory first, then the `/usr/share/mbrola` layouts. espeak-ng
/// requires a non-empty file.
pub(crate) fn mbrola_database_installed(name: &str, espeak_data: Option<&str>) -> bool {
    let mut candidates = Vec::with_capacity(4);
    if let Some(data) = espeak_data {
        candidates.push(format!("{data}/mbrola/{name}"));
    }
    candidates.push(format!("/usr/share/mbrola/{name}"));
    candidates.push(format!("/usr/share/mbrola/{name}/{name}"));
    candidates.push(format!("/usr/share/mbrola/voices/{name}"));
    candidates
        .iter()
        .any(|p| std::fs::metadata(Path::new(p)).is_ok_and(|m| m.is_file() && m.len() > 0))
}

/// The voice names TDSR versions before the catalogue mapped `voice_idx`
/// to. A config that still carries a bare `voice_idx` on an espeak backend
/// is read with this table and migrated to `voice = <identifier>`.
pub fn legacy_voice_name(idx: usize) -> Option<&'static str> {
    const LEGACY: &[&str] = &[
        "en", "en-us", "en-gb", "en-sc", "es", "fr", "de", "it", "pt", "ru", "mb-us1", "mb-us2",
        "mb-us3", "mb-en1",
    ];
    LEGACY.get(idx).copied()
}

/// Every voice espeak-ng reports, indexed the way the config menu and
/// `--list-voices` number them.
#[derive(Clone, Debug, Default)]
pub struct VoiceCatalogue {
    voices: Vec<EspeakVoice>,
}

impl VoiceCatalogue {
    /// Build from espeak-ng's own voices and its MBROLA voice definitions,
    /// each in the order espeak-ng reported them, marking each MBROLA voice
    /// as installed or not. `espeak_data` is espeak-ng's data directory
    /// (`espeak_Info`, or `espeak-ng --version`), the first place it looks
    /// for MBROLA databases.
    pub fn new(
        native: Vec<EspeakVoice>,
        mbrola: Vec<EspeakVoice>,
        espeak_data: Option<&str>,
    ) -> Self {
        let have_mbrola = on_path("mbrola");
        Self::assemble(native, mbrola, |db| {
            have_mbrola && mbrola_database_installed(db, espeak_data)
        })
    }

    /// `new` with the installation check supplied (for tests).
    fn assemble(
        native: Vec<EspeakVoice>,
        mbrola: Vec<EspeakVoice>,
        installed: impl Fn(&str) -> bool,
    ) -> Self {
        let mut voices: Vec<EspeakVoice> = native
            .into_iter()
            .filter(|v| !v.is_mbrola())
            .map(|mut v| {
                v.installed = true;
                v
            })
            .collect();
        voices.extend(mbrola.into_iter().filter(|v| v.is_mbrola()).map(|mut v| {
            v.installed = v.mbrola_database().is_some_and(&installed);
            v
        }));
        Self { voices }
    }

    /// Fill the catalogue by running the espeak-ng command-line program.
    pub fn from_command(espeak: &str) -> Result<Self> {
        let run = |arg: &str| -> Result<String> {
            let out = Command::new(espeak).arg(arg).output().map_err(|e| {
                TdsrError::Speech(format!("Could not run {} {}: {}", espeak, arg, e))
            })?;
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let native = Self::parse_listing(&run("--voices")?);
        let mbrola = Self::parse_listing(&run("--voices=mb")?);
        let data = run("--version")
            .ok()
            .and_then(|v| Self::parse_data_path(&v));
        Ok(Self::new(native, mbrola, data.as_deref()))
    }

    /// espeak-ng's data directory from its `--version` line
    /// (`... Data at: /usr/lib/x86_64-linux-gnu/espeak-ng-data`).
    pub fn parse_data_path(version_output: &str) -> Option<String> {
        version_output
            .split("Data at:")
            .nth(1)
            .map(|s| s.trim().lines().next().unwrap_or("").trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Parse the table `espeak-ng --voices` prints. Columns are
    /// `Pty Language Age/Gender VoiceName File [Other Languages]`; spaces in
    /// names are printed as underscores.
    pub fn parse_listing(text: &str) -> Vec<EspeakVoice> {
        text.lines()
            .filter_map(|line| {
                let cols: Vec<&str> = line.split_whitespace().collect();
                if cols.len() < 5 || cols[0].parse::<u32>().is_err() {
                    return None;
                }
                Some(EspeakVoice {
                    name: cols[3].replace('_', " "),
                    identifier: cols[4].to_string(),
                    language: cols[1].to_string(),
                    gender: Gender::from_listing(cols[2]),
                    installed: false,
                })
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.voices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.voices.is_empty()
    }

    pub fn get(&self, idx: usize) -> Option<&EspeakVoice> {
        self.voices.get(idx)
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &EspeakVoice)> {
        self.voices.iter().enumerate()
    }

    /// The voice with this identifier, or whose file name or language
    /// equals `name` (case-insensitively): `gmw/en-US`, `en-US`, `en-us`
    /// and `mb-us1` all resolve. `None` for a name the catalogue does not
    /// have; espeak-ng may still accept it (aliases, variants).
    pub fn find(&self, name: &str) -> Option<&EspeakVoice> {
        let name = name.trim();
        self.voices
            .iter()
            .find(|v| v.identifier == name)
            .or_else(|| {
                self.voices
                    .iter()
                    .find(|v| v.basename().eq_ignore_ascii_case(name))
            })
            .or_else(|| {
                self.voices
                    .iter()
                    .find(|v| v.language.eq_ignore_ascii_case(name))
            })
    }

    /// Refuse a voice the catalogue knows cannot work: an MBROLA definition
    /// whose program or database is missing. The message is meant to be
    /// spoken.
    pub fn check_usable(&self, voice: &EspeakVoice) -> Result<()> {
        if voice.installed {
            return Ok(());
        }
        let db = voice.mbrola_database().unwrap_or("?");
        Err(TdsrError::Speech(format!(
            "{} is not installed, it needs the mbrola and mbrola-{} packages",
            voice.name, db
        )))
    }

    /// The voice at `idx` if it can be used, else an error whose message is
    /// meant to be spoken.
    pub fn select(&self, idx: usize) -> Result<&EspeakVoice> {
        let Some(voice) = self.get(idx) else {
            return Err(TdsrError::Speech(if self.voices.is_empty() {
                "no voices available".to_string()
            } else {
                format!(
                    "no voice {}, the last voice is {}",
                    idx,
                    self.voices.len() - 1
                )
            }));
        };
        self.check_usable(voice)?;
        Ok(voice)
    }

    /// The listing `tdsr --list-voices` prints: every usable voice with its
    /// index, and a note about MBROLA voices that are not installed.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let width = self.voices.len().saturating_sub(1).to_string().len().max(1);
        let mut missing = 0usize;
        let mut mbrola_header = false;
        let _ = writeln!(out, "espeak-ng voices (index, language, name, voice file):");
        for (idx, v) in self.iter() {
            if v.is_mbrola() && !mbrola_header {
                mbrola_header = true;
                let _ = writeln!(out, "\nMBROLA voices (installed):");
            }
            if !v.installed {
                missing += 1;
                continue;
            }
            let gender = match v.gender {
                Gender::Male => "male",
                Gender::Female => "female",
                Gender::Unknown => "",
            };
            let _ = writeln!(
                out,
                "{idx:>width$}  {:<15} {} {}  [{}]",
                v.language, v.name, gender, v.identifier
            );
        }
        if missing > 0 {
            let _ = writeln!(
                out,
                "\n{} MBROLA voice definition{} not installed and hidden. \
                 `espeak-ng --voices=mb` lists them; install `mbrola` and the voice's \
                 `mbrola-<name>` package, then run this again to see its index.",
                missing,
                if missing == 1 { " is" } else { "s are" }
            );
        }
        let _ = writeln!(
            out,
            "\nTo choose one: in TDSR press Alt+c, then V, type the index and press Enter; \
             or put its voice file in ~/.tdsr.cfg, e.g. `voice = gmw/en-US`."
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn voice(name: &str, id: &str, lang: &str) -> EspeakVoice {
        EspeakVoice {
            name: name.to_string(),
            identifier: id.to_string(),
            language: lang.to_string(),
            gender: Gender::Male,
            installed: false,
        }
    }

    fn sample() -> VoiceCatalogue {
        let native = vec![
            voice("Afrikaans", "gmw/af", "af"),
            voice("English (Great Britain)", "gmw/en", "en-gb"),
            voice("English (America)", "gmw/en-US", "en-us"),
        ];
        let mbrola = vec![
            voice("afrikaans-mbrola-1", "mb/mb-af1", "af"),
            voice("us-mbrola-1", "mb/mb-us1", "en-us"),
        ];
        VoiceCatalogue::assemble(native, mbrola, |db| db == "us1")
    }

    #[test]
    fn parses_espeak_listing_columns() {
        let text = "Pty Language       Age/Gender VoiceName          File                 Other Languages\n\
                    \x20 2  en-us           --/M      English_(America)  gmw/en-US            (en 3)\n\
                    \x20 5  en-us-nyc       --/M      English_(America,_New_York_City) gmw/en-US-nyc\n\
                    \x20 7  af              --/F      afrikaans-mbrola-1 mb/mb-af1\n";
        let voices = VoiceCatalogue::parse_listing(text);
        assert_eq!(voices.len(), 3);
        assert_eq!(voices[0].name, "English (America)");
        assert_eq!(voices[0].identifier, "gmw/en-US");
        assert_eq!(voices[0].language, "en-us");
        assert_eq!(voices[1].name, "English (America, New York City)");
        assert_eq!(voices[2].gender, Gender::Female);
        assert!(voices[2].is_mbrola());
    }

    #[test]
    fn parses_data_path_from_version() {
        let v =
            "eSpeak NG text-to-speech: 1.51  Data at: /usr/lib/x86_64-linux-gnu/espeak-ng-data\n";
        assert_eq!(
            VoiceCatalogue::parse_data_path(v).as_deref(),
            Some("/usr/lib/x86_64-linux-gnu/espeak-ng-data")
        );
        assert_eq!(VoiceCatalogue::parse_data_path("eSpeak NG 1.50"), None);
    }

    #[test]
    fn mbrola_database_is_derived_from_identifier() {
        assert_eq!(voice("", "mb/mb-us1", "").mbrola_database(), Some("us1"));
        assert_eq!(voice("", "mb/mb-af1-en", "").mbrola_database(), Some("af1"));
        assert_eq!(voice("", "gmw/en-US", "").mbrola_database(), None);
    }

    #[test]
    fn native_voices_come_first_and_mbrola_installation_is_checked() {
        let cat = sample();
        assert_eq!(cat.len(), 5);
        assert!(cat.get(0).unwrap().installed);
        assert_eq!(cat.get(2).unwrap().identifier, "gmw/en-US");
        assert!(!cat.get(3).unwrap().installed);
        assert!(cat.get(4).unwrap().installed);

        assert_eq!(cat.select(4).unwrap().identifier, "mb/mb-us1");
        let err = cat.select(3).unwrap_err().to_string();
        assert!(err.contains("mbrola-af1"), "{}", err);
        let err = cat.select(9).unwrap_err().to_string();
        assert!(err.contains("no voice 9"), "{}", err);
        assert!(err.contains("last voice is 4"), "{}", err);

        let listing = cat.render();
        assert!(listing.contains("2  en-us           English (America) male  [gmw/en-US]"));
        assert!(listing.contains("4  en-us           us-mbrola-1 male  [mb/mb-us1]"));
        assert!(!listing.contains("mb-af1"));
        assert!(listing.contains("1 MBROLA voice definition is not installed"));
        assert!(listing.contains("voice = gmw/en-US"));
    }

    #[test]
    fn finds_voices_by_identifier_file_name_or_language() {
        let cat = sample();
        assert_eq!(cat.find("gmw/en-US").unwrap().name, "English (America)");
        assert_eq!(cat.find("en-US").unwrap().identifier, "gmw/en-US");
        assert_eq!(cat.find("en-us").unwrap().identifier, "gmw/en-US");
        assert_eq!(cat.find("en").unwrap().identifier, "gmw/en");
        assert_eq!(cat.find("EN-GB").unwrap().identifier, "gmw/en");
        assert_eq!(cat.find("mb-us1").unwrap().identifier, "mb/mb-us1");
        assert!(cat.find("klingon").is_none());
    }

    #[test]
    fn legacy_indices_name_the_old_table() {
        assert_eq!(legacy_voice_name(0), Some("en"));
        assert_eq!(legacy_voice_name(1), Some("en-us"));
        assert_eq!(legacy_voice_name(10), Some("mb-us1"));
        assert_eq!(legacy_voice_name(14), None);
    }

    #[test]
    fn empty_catalogue_selects_nothing() {
        let cat = VoiceCatalogue::default();
        assert!(cat.is_empty());
        assert!(cat.select(0).unwrap_err().to_string().contains("no voices"));
    }
}
