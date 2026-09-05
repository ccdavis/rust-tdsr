//! Deciding whether a full-screen program is running.
//!
//! Nothing announces "I am a TUI". The signs are the alternate screen,
//! a hidden text cursor, auto-wrap switched off, several background
//! colours on screen, and the shell no longer being the foreground
//! process of the terminal. Switching on takes several background
//! colours (menus and dialogs are drawn in colour; pagers and plain
//! editors are not) plus one strong sign: the alternate screen being
//! entered, or auto-wrap off. A hidden cursor is not one: editors hide
//! it for a moment while redrawing, and nano keeps it hidden for most of
//! a second in some states. Programs that show neither sign are switched
//! on by name (`tui_apps`). A decaying score of the evidence decides when
//! the program has gone quiet enough to switch off.

/// What is known about the terminal's foreground process
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Foreground {
    /// Whether the shell TDSR started is the foreground process group.
    /// `None` when TDSR runs a program directly (there is no shell).
    pub is_shell: Option<bool>,
    /// Name of the foreground process (`fp`, `mc`), when it could be read
    pub name: Option<String>,
}

/// Evidence gathered from one output burst
#[derive(Clone, Copy, Debug, Default)]
pub struct Signals {
    /// The alternate screen was entered during this burst
    pub alt_entered: bool,
    /// The alternate screen is active
    pub in_alt: bool,
    /// The text cursor is hidden
    pub cursor_hidden: bool,
    /// Auto-wrap is off
    pub autowrap_off: bool,
    /// How long the cursor has been hidden without a break (logged for
    /// tuning; not a sign, see the module docs)
    pub hidden_for: std::time::Duration,

    /// Rows showing at least one non-default background
    pub bg_rows: usize,
    /// Distinct backgrounds on screen, the default included
    pub distinct_bgs: usize,
}

impl Signals {
    fn multi_bg(&self) -> bool {
        self.bg_rows >= 3 && self.distinct_bgs >= 3
    }

    /// Independent signs present in this burst
    fn count(&self) -> usize {
        usize::from(self.in_alt)
            + usize::from(self.cursor_hidden)
            + usize::from(self.autowrap_off)
            + usize::from(self.multi_bg())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Enter,
    Leave,
    Stay,
}

/// Evidence accumulator with hysteresis
#[derive(Clone, Debug, Default)]
pub struct Detector {
    score: f32,
    quiet_bursts: u32,

    /// A process other than the shell has been seen in the foreground, so
    /// the shell coming back means the program ended. Shells without job
    /// control never leave the foreground; for them this stays false and
    /// the foreground is ignored.
    saw_foreign_fg: bool,
}

const QUIET_SCORE: f32 = 1.5;
const QUIET_BURSTS: u32 = 3;

impl Detector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn score(&self) -> f32 {
        self.score
    }

    pub fn reset(&mut self) {
        self.score = 0.0;
        self.quiet_bursts = 0;
    }

    /// Fold in one burst's evidence and say what the tracker should do.
    /// `active` is the tracker's current state; `apps` lists program names
    /// that are always treated as TUIs; with `by_evidence` false only
    /// those names switch TUI mode on.
    pub fn update(
        &mut self,
        signals: &Signals,
        foreground: Option<&Foreground>,
        apps: &[String],
        by_evidence: bool,
        active: bool,
    ) -> Verdict {
        if let Some(fg) = foreground {
            if fg.is_shell == Some(false) {
                self.saw_foreign_fg = true;
            }
            if self.saw_foreign_fg && fg.is_shell == Some(true) {
                self.reset();
                return if active {
                    Verdict::Leave
                } else {
                    Verdict::Stay
                };
            }
            if let Some(name) = fg.name.as_deref() {
                if apps.iter().any(|app| app == name) {
                    self.score = self.score.max(4.0);
                    return if active {
                        Verdict::Stay
                    } else {
                        Verdict::Enter
                    };
                }
            }
        }

        self.score = self.score * 0.5
            + if signals.alt_entered { 2.0 } else { 0.0 }
            + if signals.cursor_hidden { 1.0 } else { 0.0 }
            + if signals.autowrap_off { 2.0 } else { 0.0 }
            + if signals.multi_bg() { 2.0 } else { 0.0 };

        if !active {
            let strong = signals.alt_entered || signals.autowrap_off;
            if by_evidence && signals.multi_bg() && strong {
                self.quiet_bursts = 0;
                return Verdict::Enter;
            }
            return Verdict::Stay;
        }

        if !signals.in_alt && self.score < QUIET_SCORE && signals.count() == 0 {
            self.quiet_bursts += 1;
            if self.quiet_bursts >= QUIET_BURSTS {
                self.reset();
                return Verdict::Leave;
            }
        } else {
            self.quiet_bursts = 0;
        }
        Verdict::Stay
    }
}

/// Name of the process leading process group `pgid`, when the platform
/// offers a way to read it
#[cfg(target_os = "linux")]
pub fn process_name(pgid: i32) -> Option<String> {
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", pgid)).ok()?;
    let name = comm.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(target_os = "macos")]
pub fn process_name(pgid: i32) -> Option<String> {
    use std::ffi::CStr;
    // proc_pidpath fills a buffer with the executable path
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut u8, buffersize: u32) -> i32;
    }
    let mut buf = vec![0u8; 4096];
    let len = unsafe { proc_pidpath(pgid, buf.as_mut_ptr(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    let path = CStr::from_bytes_until_nul(&buf).ok()?.to_str().ok()?;
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_name(_pgid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp_start() -> Signals {
        Signals {
            alt_entered: true,
            in_alt: true,
            cursor_hidden: false,
            autowrap_off: true,
            bg_rows: 25,
            distinct_bgs: 5,
            ..Signals::default()
        }
    }

    #[test]
    fn full_screen_program_enters_on_first_burst() {
        let mut d = Detector::new();
        assert_eq!(
            d.update(&fp_start(), None, &[], true, false),
            Verdict::Enter
        );
    }

    #[test]
    fn pager_never_enters() {
        // less: alternate screen, cursor shown, one reverse-video line
        let mut d = Detector::new();
        let first = Signals {
            alt_entered: true,
            in_alt: true,
            bg_rows: 1,
            distinct_bgs: 2,
            ..Signals::default()
        };
        assert_eq!(d.update(&first, None, &[], true, false), Verdict::Stay);
        let later = Signals {
            alt_entered: false,
            ..first
        };
        for _ in 0..10 {
            assert_eq!(d.update(&later, None, &[], true, false), Verdict::Stay);
        }
    }

    #[test]
    fn editor_with_colours_stays_out_even_with_a_hidden_cursor() {
        // nano: alternate screen entered long ago, three backgrounds, and
        // the cursor hidden for one burst while it redraws
        let mut d = Detector::new();
        let steady = Signals {
            in_alt: true,
            bg_rows: 5,
            distinct_bgs: 3,
            ..Signals::default()
        };
        for _ in 0..30 {
            assert_eq!(d.update(&steady, None, &[], true, false), Verdict::Stay);
        }
        // nano keeps the cursor hidden for most of a second in some states
        let hidden = Signals {
            cursor_hidden: true,
            hidden_for: std::time::Duration::from_millis(800),
            ..steady
        };
        assert_eq!(d.update(&hidden, None, &[], true, false), Verdict::Stay);
        assert_eq!(d.update(&steady, None, &[], true, false), Verdict::Stay);
        // Auto-wrap off (a Turbo Vision program without alternate screen) does
        let wrap_off = Signals {
            autowrap_off: true,
            ..steady
        };
        assert_eq!(d.update(&wrap_off, None, &[], true, false), Verdict::Enter);
    }

    #[test]
    fn coloured_listing_never_enters() {
        let mut d = Detector::new();
        let ls = Signals {
            bg_rows: 2,
            distinct_bgs: 2,
            ..Signals::default()
        };
        for _ in 0..10 {
            assert_eq!(d.update(&ls, None, &[], true, false), Verdict::Stay);
        }
    }

    #[test]
    fn leaves_when_the_shell_returns_after_a_program() {
        let mut d = Detector::new();
        let program = Foreground {
            is_shell: Some(false),
            name: Some("fp".into()),
        };
        let shell = Foreground {
            is_shell: Some(true),
            name: Some("bash".into()),
        };
        // A shell that never leaves the foreground is ignored
        assert_eq!(
            d.update(&fp_start(), Some(&shell), &[], true, false),
            Verdict::Enter
        );
        assert_eq!(
            d.update(&Signals::default(), Some(&shell), &[], true, true),
            Verdict::Stay
        );
        assert_eq!(
            d.update(&fp_start(), Some(&program), &[], true, true),
            Verdict::Stay
        );
        assert_eq!(
            d.update(&Signals::default(), Some(&shell), &[], true, true),
            Verdict::Leave
        );
    }

    #[test]
    fn listed_apps_enter_regardless_of_evidence() {
        let mut d = Detector::new();
        let program = Foreground {
            is_shell: Some(false),
            name: Some("mc".into()),
        };
        let apps = vec!["fp".to_string(), "mc".to_string()];
        assert_eq!(
            d.update(&Signals::default(), Some(&program), &apps, true, false),
            Verdict::Enter
        );
    }

    #[test]
    fn leaves_after_quiet_bursts_outside_the_alternate_screen() {
        let mut d = Detector::new();
        assert_eq!(
            d.update(&fp_start(), None, &[], true, false),
            Verdict::Enter
        );
        let quiet = Signals::default();
        let mut verdicts = Vec::new();
        for _ in 0..6 {
            verdicts.push(d.update(&quiet, None, &[], true, true));
        }
        assert!(verdicts.contains(&Verdict::Leave));
        assert!(verdicts.iter().filter(|v| **v == Verdict::Leave).count() == 1);
    }
}
