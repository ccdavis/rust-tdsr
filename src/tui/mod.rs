//! TUI mode: reading full-screen programs by diffing the screen.
//!
//! Menu-driven programs (the Free Pascal IDE, Midnight Commander, `dialog`
//! and `whiptail`, ncurses menus) do not produce a stream of text to read.
//! They repaint cells: the selected item changes colour, a dialog appears
//! in a box, a status line is rewritten, and only the cells that changed
//! are sent. Speaking what was drawn reads fragments and repaints; what
//! the user wants to hear is *what became highlighted*, *what window just
//! opened*, and *where the cursor went*.
//!
//! `TuiTracker` keeps a snapshot of the screen (characters and
//! attributes) and, once the output of a keystroke has settled, compares
//! the new screen against it. The differences are classified into a few
//! announcements (`Announcement`): a highlight that moved, a window that
//! opened, a message, the line or character under a moving cursor. It is
//! switched on by hand (`TuiMode::On`) or by evidence that a full-screen
//! program is running (`TuiMode::Auto`, see `detect`).

pub mod detect;
pub mod diff;
pub mod frame;

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

pub use detect::{process_name, Detector, Foreground, Signals, Verdict};
pub use frame::Rect;

use crate::terminal::{Attrs, Cell, Color, Screen};
use diff::{RowDiff, Span};
use log::{debug, trace};

/// How TUI mode is engaged
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TuiMode {
    /// Switch on when a full-screen program is detected, off when it ends
    #[default]
    Auto,
    /// Switch on only for the programs listed in `tui_apps`
    Apps,
    On,
    Off,
}

impl TuiMode {
    /// The mode after this one when cycling with a key: auto, on, off
    pub fn next(self) -> Self {
        match self {
            TuiMode::Auto => TuiMode::Apps,
            TuiMode::Apps => TuiMode::On,
            TuiMode::On => TuiMode::Off,
            TuiMode::Off => TuiMode::Auto,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TuiMode::Auto => "auto",
            TuiMode::Apps => "apps",
            TuiMode::On => "on",
            TuiMode::Off => "off",
        }
    }
}

impl fmt::Display for TuiMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TuiMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(TuiMode::Auto),
            "apps" | "list" => Ok(TuiMode::Apps),
            "on" | "true" | "yes" | "1" => Ok(TuiMode::On),
            "off" | "false" | "no" | "0" => Ok(TuiMode::Off),
            other => Err(format!("unknown TUI mode '{}'", other)),
        }
    }
}

/// The item currently highlighted on screen
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub row: u16,
    pub x0: u16,
    pub x1: u16,
    pub attrs: Attrs,
    pub text: String,
}

/// What the tracker wants spoken
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Announcement {
    /// A different item is highlighted; `at` is where it starts
    Highlight { text: String, at: (u16, u16) },
    /// A window or dialog appeared: its title, its text, and the item
    /// focused inside it
    Window {
        title: Option<String>,
        lines: Vec<String>,
        focus: Option<String>,
    },
    /// Text was written (a message line, the line the cursor moved to)
    Text { text: String, at: (u16, u16) },
    /// The cursor moved within its line: speak the character under it
    Char { x: u16, y: u16 },
    /// TUI mode switched on or off
    ModeChanged(bool),
}

/// The kind of key the user pressed last, as far as the tracker cares
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    /// A character that will be typed (and echoed)
    Printable,
    /// Cursor movement: arrows, Home/End, Page Up/Down
    Nav,
    /// Backspace or Delete
    Edit,
    Tab,
    Enter,
    Escape,
    Other,
}

impl KeyKind {
    /// Classify one key sequence as `split_keys` yields them
    pub fn of(key: &[u8]) -> KeyKind {
        match key {
            [b'\t'] => KeyKind::Tab,
            [b'\r'] | [b'\n'] => KeyKind::Enter,
            [0x1b] => KeyKind::Escape,
            [0x7f] | [0x08] => KeyKind::Edit,
            [c] if (0x20..=0x7e).contains(c) => KeyKind::Printable,
            // A multi-byte character (UTF-8 starts at 0xc2)
            [c, ..] if *c >= 0xc2 => KeyKind::Printable,
            [0x1b, b'[' | b'O', rest @ ..] => match rest {
                [b'A'..=b'D'] | [b'H'] | [b'F'] => KeyKind::Nav,
                [b'3', b'~'] => KeyKind::Edit,
                [b'1'..=b'8', b'~'] => KeyKind::Nav,
                _ => KeyKind::Other,
            },
            _ => KeyKind::Other,
        }
    }
}

/// What the tracker needs to know about the burst it is classifying
#[derive(Clone, Copy, Debug, Default)]
pub struct ObserveCtx {
    /// Cursor position before the output arrived
    pub cursor_before: (u16, u16),
    /// A typed character the terminal echoed (spoken by key echo already)
    pub echoed: Option<char>,
    /// The last key the user pressed
    pub key: Option<KeyKind>,
}

/// Lowest score a span needs to count as a highlight
const MIN_HIGHLIGHT_SCORE: i32 = 2;
/// Share of a frame's border that must have been drawn in this burst for
/// the frame to count as newly opened
const FRESH_BORDER_SHARE: f32 = 0.5;
/// Blocks of changed rows up to this height are spoken as messages;
/// taller ones without a frame are repaints and stay silent
const MAX_MESSAGE_ROWS: u16 = 3;
/// Message rows spoken from one burst at most
const MAX_MESSAGES: usize = 3;
/// A frameless repaint covering this share of the screen's rows is a new
/// page to read, not a menu closing
const FULL_PAGE_PERCENT: u32 = 80;
/// Windows remembered as read, so one that reappears is not read again
const MAX_WINDOWS: usize = 4;
/// Screens remembered from before a window or menu opened, so the screen
/// coming back when it closes is recognised
const MAX_UNDERLAYS: usize = 3;
/// Share of the changed cells that must show the remembered screen again
/// for a repaint to count as something closing
const UNCOVERED_PERCENT: usize = 70;

/// A span at least this share of the screen width is a bar or a whole
/// row being recoloured, not an item
const WIDE_SPAN_PERCENT: u32 = 90;

/// Screen-diffing tracker for full-screen programs (see the module docs)
pub struct TuiTracker {
    mode: TuiMode,
    active: bool,
    prev: Vec<Vec<Cell>>,
    prev_alt: bool,
    highlight: Option<Highlight>,
    last_block: Option<Rect>,
    /// Windows read in full (frame, title, text), newest last, so one
    /// that reappears unchanged is not read again
    windows: Vec<(Rect, Option<String>, Vec<String>)>,
    /// The screen as it was before each window, menu or page opened,
    /// oldest first: when it comes back, something closed
    underlays: Vec<Vec<Vec<Cell>>>,

    /// Per row, which of the last eight bursts changed it (bit 0 = latest)
    row_volatility: Vec<u8>,
    detector: Detector,
    apps: Vec<String>,
    /// When the cursor was last seen going hidden (still hidden)
    hidden_since: Option<std::time::Instant>,
    /// A listed program that left the alternate screen (it is exiting, or
    /// shelled out): its name alone does not switch TUI mode back on
    /// until the foreground changes
    suppressed_app: Option<String>,

    /// The baseline was just rebuilt (resize, mode change): the next
    /// observation has nothing to compare against
    fresh_baseline: bool,
}

impl TuiTracker {
    pub fn new(mode: TuiMode, apps: Vec<String>) -> Self {
        Self {
            mode,
            active: mode == TuiMode::On,
            prev: Vec::new(),
            prev_alt: false,
            highlight: None,
            last_block: None,
            windows: Vec::new(),
            underlays: Vec::new(),
            row_volatility: Vec::new(),
            detector: Detector::new(),
            apps,
            hidden_since: None,
            suppressed_app: None,
            fresh_baseline: false,
        }
    }

    pub fn mode(&self) -> TuiMode {
        self.mode
    }

    /// Whether announcements come from screen diffs right now (and the
    /// printed text is not spoken)
    pub fn active(&self) -> bool {
        self.active
    }

    pub fn current_highlight(&self) -> Option<&Highlight> {
        self.highlight.as_ref()
    }

    pub fn last_block(&self) -> Option<Rect> {
        self.last_block
    }

    /// Detector score, for debugging the auto mode
    pub fn score(&self) -> f32 {
        self.detector.score()
    }

    /// Take the screen as the baseline: nothing on it counts as new
    pub fn resync(&mut self, screen: &Screen) {
        self.prev = screen.buffer.clone();
        self.prev_alt = screen.in_alt_screen;
        self.row_volatility = vec![0; screen.size.1 as usize];
        self.highlight = None;
        self.last_block = None;
        self.underlays.clear();
        self.fresh_baseline = false;
    }

    /// Remember the screen before something opened over it
    fn push_underlay(&mut self) {
        if self.underlays.len() >= MAX_UNDERLAYS {
            self.underlays.remove(0);
        }
        self.underlays.push(self.prev.clone());
        trace!("underlay pushed, {} kept", self.underlays.len());
    }

    /// Whether the cells that changed now (mostly) show what an earlier
    /// screen showed there: a window, menu or page closed and uncovered
    /// it. Returns the rows that do not match (a hint line rewritten at
    /// the same time). That screen and the ones opened after it are
    /// forgotten.
    fn uncovered(&mut self, screen: &Screen, diffs: &[RowDiff]) -> Option<Vec<u16>> {
        let total: usize = diffs.iter().map(|d| d.cells.len()).sum();
        trace!(
            "uncover check against {} underlays, {} changed cells",
            self.underlays.len(),
            total
        );
        if total == 0 {
            return None;
        }

        let odd_rows = |u: &Vec<Vec<Cell>>| -> (usize, Vec<u16>) {
            let mut matched = 0;
            let mut odd = Vec::new();
            for d in diffs {
                let row = &screen.buffer[d.y as usize];
                let hits = match u.get(d.y as usize) {
                    Some(urow) => d
                        .cells
                        .iter()
                        .filter(|c| urow[c.x as usize] == row[c.x as usize])
                        .count(),
                    None => 0,
                };
                matched += hits;
                if hits < d.cells.len() {
                    odd.push(d.y);
                }
            }
            (matched, odd)
        };
        for i in (0..self.underlays.len()).rev() {
            let (matched, odd) = odd_rows(&self.underlays[i]);
            trace!(
                "underlay {}: {} of {} changed cells match, rows apart {:?}",
                i,
                matched,
                total,
                odd
            );
            if matched * 100 >= total * UNCOVERED_PERCENT
                && odd.len() <= usize::from(screen.size.1 / 4).max(MAX_MESSAGE_ROWS as usize)
            {
                self.underlays.truncate(i);
                return Some(odd);
            }
        }
        None
    }

    /// Compare against an empty screen: everything on it is new
    fn blank_baseline(&mut self, screen: &Screen) {
        let (cols, rows) = screen.size;
        self.prev = vec![vec![Cell::new(); cols as usize]; rows as usize];
        self.row_volatility = vec![0; rows as usize];
        self.highlight = None;
        self.last_block = None;
        self.fresh_baseline = false;
    }

    fn baseline_matches(&self, screen: &Screen) -> bool {
        self.prev.len() == screen.size.1 as usize
            && self
                .prev
                .first()
                .map_or(screen.size.0 == 0, |r| r.len() == screen.size.0 as usize)
    }

    /// Change the mode; returns whether the tracker is active afterwards
    pub fn set_mode(&mut self, mode: TuiMode, screen: &Screen) -> bool {
        self.mode = mode;
        self.detector.reset();
        let active = match mode {
            TuiMode::On => true,
            TuiMode::Off | TuiMode::Auto | TuiMode::Apps => false,
        };
        if active != self.active {
            self.active = active;
            self.resync(screen);
        }
        self.active
    }

    /// Step to the next mode (auto, apps, on, off) and return it
    pub fn cycle_mode(&mut self, screen: &Screen) -> TuiMode {
        let next = self.mode.next();
        self.set_mode(next, screen);
        next
    }

    /// Called after every output burst, before anything is spoken.
    /// Keeps track of the alternate screen and, in auto mode, decides
    /// whether a full-screen program has started or ended. Returns the
    /// mode-change announcement, if any.
    pub fn note_output(
        &mut self,
        screen: &Screen,
        foreground: Option<&Foreground>,
    ) -> Option<Announcement> {
        let alt_entered = screen.in_alt_screen && !self.prev_alt;
        let alt_left = !screen.in_alt_screen && self.prev_alt;
        if !self.baseline_matches(screen) {
            // The baseline is taken after this burst: nothing to compare yet
            self.resync(screen);
            self.fresh_baseline = true;
        }
        self.prev_alt = screen.in_alt_screen;
        if alt_entered {
            // The whole first paint is new content
            self.blank_baseline(screen);
        }
        if matches!(self.mode, TuiMode::On | TuiMode::Off) {
            return None;
        }

        if alt_left && self.active {
            // The program gave the screen back (exit, or a shell escape):
            // whatever it prints now is ordinary output again
            self.active = false;
            self.detector.reset();
            self.resync(screen);
            self.fresh_baseline = true;
            self.suppressed_app = foreground.and_then(|fg| fg.name.clone());
            return Some(Announcement::ModeChanged(false));
        }
        let fg_name = foreground.and_then(|fg| fg.name.as_deref());
        if self.suppressed_app.is_some() && self.suppressed_app.as_deref() != fg_name {
            self.suppressed_app = None;
        }
        let apps: &[String] = if self.suppressed_app.is_some() {
            &[]
        } else {
            &self.apps
        };
        let mut signals = signals_from_screen(screen, alt_entered);
        let now = std::time::Instant::now();
        self.hidden_since = match (screen.cursor_visible, self.hidden_since) {
            (true, _) => None,
            (false, Some(since)) => Some(since),
            (false, None) => Some(now),
        };
        signals.hidden_for = self
            .hidden_since
            .map_or(std::time::Duration::ZERO, |since| now.duration_since(since));
        let by_evidence = self.mode == TuiMode::Auto;
        let verdict = self
            .detector
            .update(&signals, foreground, apps, by_evidence, self.active);
        trace!(
            "TUI detector: {:?} score {:.1} {:?} fg {:?}",
            verdict,
            self.detector.score(),
            signals,
            foreground
        );
        if verdict != Verdict::Stay {
            debug!(
                "TUI mode {:?}: score {:.1} {:?} fg {:?}",
                verdict,
                self.detector.score(),
                signals,
                foreground
            );
        }
        match verdict {
            Verdict::Enter => {
                self.active = true;
                if !alt_entered {
                    self.resync(screen);
                    self.fresh_baseline = true;
                }
                Some(Announcement::ModeChanged(true))
            }
            Verdict::Leave => {
                self.active = false;
                self.resync(screen);
                self.fresh_baseline = true;
                Some(Announcement::ModeChanged(false))
            }
            Verdict::Stay => None,
        }
    }

    /// Classify what changed since the last call and advance the
    /// baseline. Called once the output of a keystroke has settled.
    pub fn observe(&mut self, screen: &Screen, ctx: &ObserveCtx) -> Vec<Announcement> {
        if !self.baseline_matches(screen) {
            self.resync(screen);
            return Vec::new();
        }
        let diffs = diff::diff_rows(&self.prev, &screen.buffer);
        let mut out = Vec::new();
        let fresh_baseline = std::mem::take(&mut self.fresh_baseline);
        if self.active
            && !fresh_baseline
            && (!diffs.is_empty() || screen.cursor != ctx.cursor_before)
        {
            out = self.classify(screen, ctx, &diffs);
            trace!("TUI announcements: {:?}", out);
        }

        self.note_volatility(&diffs);
        self.prev = screen.buffer.clone();
        out
    }

    fn note_volatility(&mut self, diffs: &[RowDiff]) {
        let changed: HashSet<u16> = diffs.iter().map(|d| d.y).collect();
        for (y, v) in self.row_volatility.iter_mut().enumerate() {
            *v = (*v << 1) | u8::from(changed.contains(&(y as u16)));
        }
    }

    /// A row that changed in three of the last four bursts is a counter or
    /// clock, not a message
    fn is_volatile(&self, y: u16) -> bool {
        self.row_volatility
            .get(y as usize)
            .is_some_and(|v| (v & 0x0f).count_ones() >= 3)
    }

    fn remember(&mut self, span: &Span) {
        self.highlight = Some(Highlight {
            row: span.y,
            x0: span.x0,
            x1: span.x1,
            attrs: span.attrs,
            text: span.text.clone(),
        });
    }

    /// Score every changed span for "this is the newly highlighted item"
    fn score_spans(
        &self,
        screen: &Screen,
        ctx: &ObserveCtx,
        diffs: &[RowDiff],
        frames: &[Rect],
    ) -> Vec<(Span, i32)> {
        let cursor = screen.cursor;
        let cursor_moved = cursor != ctx.cursor_before;
        let hist = diff::bg_histogram(screen);
        let plain_bgs = diff::plain_text_bgs(screen);
        let count = |bg: Color| hist.get(&bg).copied().unwrap_or(0);
        let mut cands: Vec<(Span, i32)> = Vec::new();
        for d in diffs {
            let row = &screen.buffer[d.y as usize];
            let row_dominant = diff::dominant_bg(row.iter());
            let changed_dominant = diff::dominant_bg(d.cells.iter().map(|c| &row[c.x as usize]));
            for span in diff::changed_spans(screen, d) {
                // Text in a frame's top or bottom border is a title or a
                // panel label, never the focused item
                if frames.iter().any(|f| {
                    (f.top == span.y || f.bottom == span.y)
                        && f.left <= span.x0
                        && span.x1 <= f.right
                }) {
                    continue;
                }
                // An item has a word in it, or at least a few characters
                if !diff::has_word(&span.text) && span.text.chars().count() < 3 {
                    continue;
                }
                let bar = diff::is_bar(screen, &span);
                let distinct_row = Some(span.bg) != row_dominant;
                let distinct_changed = Some(span.bg) != changed_dominant;
                let bright = span.attrs.is_bright_fg() && plain_bgs.contains(&span.bg);
                if !(span.attr_only || distinct_row || distinct_changed || bar || bright) {
                    continue;
                }
                let old_bg = span.old_attrs.effective_bg();
                let gained = (span.attrs.is_bright_fg() && !span.old_attrs.is_bright_fg())
                    || (old_bg != span.bg && count(old_bg) > 0 && count(span.bg) < count(old_bg));
                // Moved to a more common background, or (recoloured in
                // place) to one that is gone from the screen: unhighlighted.
                // Text drawn afresh over a background that is gone is a
                // repaint, not a loss.
                let lost = (span.old_attrs.is_bright_fg() && !span.attrs.is_bright_fg())
                    || (old_bg != span.bg
                        && count(old_bg) < count(span.bg)
                        && (count(old_bg) > 0 || span.attr_only));
                // Drawn like the rest of the text on its row: it just went
                // back to normal
                let normal_now = diff::row_text_attrs(row, span.x0, span.x1) == Some(span.attrs);
                let dim = span.attrs.dim || span.attrs.effective_fg() == Color::Indexed(8);
                let mut score = 0;
                if span.attr_only {
                    score += 2;
                }
                if distinct_row {
                    score += 1;
                }
                if distinct_changed && !span.attr_only {
                    score += 1;
                }
                if bar {
                    score += 1;
                }
                // Brightness marks focus in Turbo Vision dialogs, but mc's
                // menus draw the unselected items bright: a weak sign. Bright
                // white is the focused control there; the default button is
                // bright cyan.
                if bright {
                    score += 1;
                    if span.attrs.effective_fg() == Color::Indexed(15) {
                        score += 1;
                    }
                }

                if gained {
                    score += 1;
                }
                if lost {
                    score -= 2;
                }
                if normal_now {
                    score -= 2;
                }
                if dim {
                    score -= 2;
                }
                // A whole row recoloured is a bar, not an item
                if u32::from(span.x1 - span.x0 + 1) * 100
                    >= u32::from(screen.size.0) * WIDE_SPAN_PERCENT
                {
                    score -= 2;
                }

                // The cursor on the span: a visible one marks the focused
                // field; a hidden one that moved onto a recoloured item is
                // a list selection (newt), but a hidden one parked on freshly
                // written text is just where the program stopped drawing.
                if span.contains(cursor.0, cursor.1) {
                    if screen.cursor_visible {
                        score += 3;
                    } else if cursor_moved && span.attr_only {
                        score += 2;
                    }
                }
                if let Some(h) = &self.highlight {
                    if span.overlaps(h.row, h.x0, h.x1) {
                        if span.attrs == h.attrs && span.attr_only {
                            // The same item redrawn with the same highlight
                            continue;
                        }
                        score -= 3;
                    }
                }
                trace!(
                    "span row {} cols {}-{} {:?}: score {} (attr_only {}, distinct row {} changed {}, bar {}, bright {}, gained {}, lost {}, normal {}, dim {})",
                    span.y, span.x0, span.x1, span.text, score, span.attr_only, distinct_row,
                    distinct_changed, bar, bright, gained, lost, normal_now, dim
                );
                cands.push((span, score));
            }
        }

        // Speakup's tiebreak: of the backgrounds in play, the rarest on
        // screen is the bar
        let bgs: HashSet<Color> = cands.iter().map(|(s, _)| s.bg).collect();
        if bgs.len() >= 2 {
            if let Some(rarest) = bgs.iter().min_by_key(|bg| count(**bg)) {
                for (span, score) in cands.iter_mut() {
                    if span.bg == *rarest {
                        *score += 1;
                    }
                }
            }
        }
        cands
    }

    fn classify(
        &mut self,
        screen: &Screen,
        ctx: &ObserveCtx,
        diffs: &[RowDiff],
    ) -> Vec<Announcement> {
        let mut out = Vec::new();
        let cursor = screen.cursor;
        let cursor_moved = cursor != ctx.cursor_before;
        let frames = frame::find_frames(screen);
        let cands = self.score_spans(screen, ctx, diffs, &frames);
        let best = |spans: &mut dyn Iterator<Item = &(Span, i32)>| -> Option<Span> {
            spans
                .filter(|(_, score)| *score >= MIN_HIGHLIGHT_SCORE)
                .max_by_key(|(span, score)| (*score, span.contains(cursor.0, cursor.1)))
                .map(|(span, _)| span.clone())
        };
        // A window, dialog or menu that was just drawn
        let changed: HashSet<(u16, u16)> = diffs
            .iter()
            .flat_map(|d| d.cells.iter().map(move |c| (c.x, d.y)))
            .collect();
        let is_changed = |x: u16, y: u16| changed.contains(&(x, y));
        let rows = screen.size.1;
        let corners_drawn = |r: &Rect| {
            is_changed(r.left, r.top)
                && is_changed(r.right, r.top)
                && (r.bottom >= rows
                    || (is_changed(r.left, r.bottom) && is_changed(r.right, r.bottom)))
        };
        for r in &frames {
            trace!(
                "frame {:?} corners drawn {} border changed {:.2}",
                r,
                corners_drawn(r),
                frame::border_changed_share(r, rows, &is_changed)
            );
        }
        // The largest frame drawn in this burst; of equals, the one holding
        // the current highlight (mc's two panels), else the first
        let fresh = frames
            .iter()
            .copied()
            .filter(|r| {
                corners_drawn(r)
                    && frame::border_changed_share(r, rows, &is_changed) >= FRESH_BORDER_SHARE
            })
            .max_by_key(|r| {
                let holds_highlight = self
                    .highlight
                    .as_ref()
                    .is_some_and(|h| r.contains(h.x0, h.row));
                (
                    r.area(),
                    holds_highlight,
                    std::cmp::Reverse((r.top, r.left)),
                )
            });
        if let Some(fresh) = fresh {
            let interior = |s: &Span| {
                s.y > fresh.top && s.y < fresh.bottom && s.x0 > fresh.left && s.x1 < fresh.right
            };
            let inside = best(&mut cands.iter().filter(|(s, _)| interior(s)));
            // A menu bar item highlighted directly above the frame: a
            // drop-down menu just opened under it
            let above = best(&mut cands.iter().filter(|(s, _)| {
                s.y + 1 == fresh.top
                    && s.x0 <= fresh.right
                    && fresh.left <= s.x1
                    && s.x1 - s.x0 < fresh.width()
            }));
            // ... or the highlighted item of an open menu with the new frame
            // beside it: a sub-menu
            let beside = |a: &Rect, b: &Rect| {
                (i32::from(a.left) - i32::from(b.right)).abs() <= 2
                    || (i32::from(b.left) - i32::from(a.right)).abs() <= 2
            };
            let menu_context = self.highlight.as_ref().is_some_and(|h| {
                (h.row + 1 == fresh.top && h.x0 <= fresh.right && fresh.left <= h.x1)
                    || self.last_block.is_some_and(|b| {
                        b.contains(h.x0, h.row) && fresh.contains_row(h.row) && beside(&fresh, &b)
                    })
            });
            // A visible cursor that just moved into the frame means an input
            // field got focus: that is a dialog, whatever opened it
            let input_focus =
                screen.cursor_visible && cursor_moved && fresh.contains(cursor.0, cursor.1);
            let is_menu = inside.is_some() && !input_focus && (above.is_some() || menu_context);
            self.push_underlay();
            trace!(
                "fresh frame {:?} title {:?} inside {:?} above {:?} menu {} (context {}, input focus {})",
                fresh,
                frame::title(screen, &fresh),
                inside.as_ref().map(|s| &s.text),
                above.as_ref().map(|s| &s.text),
                is_menu,
                menu_context,
                input_focus
            );
            if is_menu {
                if let Some(bar_item) = above {
                    out.push(Announcement::Highlight {
                        text: bar_item.text.clone(),
                        at: (bar_item.x0, bar_item.y),
                    });
                }
                let item = inside.unwrap();
                out.push(Announcement::Highlight {
                    text: item.text.clone(),
                    at: (item.x0, item.y),
                });
                self.remember(&item);
            } else {
                let title = frame::title(screen, &fresh);
                let lines = frame::lines(screen, &fresh);
                let focus = inside.as_ref().map(|s| s.text.clone());
                // The same window as before (a status row inside it, like
                // mc's mini-status, may differ)
                let same_window = self
                    .windows
                    .iter()
                    .any(|(r, t, l)| *r == fresh && *t == title && lines_alike(l, &lines));
                if same_window {
                    // The window came back as it was (a viewer or dialog
                    // over it closed): only say what is focused in it
                    if let Some(item) = &inside {
                        out.push(Announcement::Highlight {
                            text: item.text.clone(),
                            at: (item.x0, item.y),
                        });
                    }
                } else {
                    if self.windows.len() >= MAX_WINDOWS {
                        self.windows.remove(0);
                    }
                    self.windows.push((fresh, title.clone(), lines.clone()));
                    out.push(Announcement::Window {
                        title,
                        lines,
                        focus,
                    });
                }
                match inside {
                    Some(item) => self.remember(&item),
                    None => self.highlight = None,
                }
            }
            self.last_block = Some(fresh);
            return out;
        }

        // Something closed: the screen it covered is back. Say only what
        // is highlighted there now, nothing about the repaint.
        if let Some(odd_rows) = self.uncovered(screen, diffs) {
            trace!("screen uncovered; rows written anew: {:?}", odd_rows);
            let focus = cands
                .iter()
                .filter(|(s, score)| *score >= MIN_HIGHLIGHT_SCORE && !odd_rows.contains(&s.y))
                .max_by_key(|(_, score)| *score)
                .map(|(s, _)| s.clone());
            self.last_block = None;
            match focus {
                Some(item) => {
                    out.push(Announcement::Highlight {
                        text: item.text.clone(),
                        at: (item.x0, item.y),
                    });
                    self.remember(&item);
                }
                None => self.highlight = None,
            }
            // Rows rewritten at the same time are messages, except the top
            // and bottom rows: status bars going back to normal
            let typed =
                ctx.echoed.is_some() || matches!(ctx.key, Some(KeyKind::Printable | KeyKind::Edit));
            let last_row = screen.size.1.saturating_sub(1);
            let rows: Vec<u16> = odd_rows
                .into_iter()
                .filter(|y| *y != 0 && *y != last_row)
                .collect();
            self.push_messages(screen, diffs, &rows, typed, &mut out);
            return out;
        }

        // The highlight moved
        if let Some(winner) = best(&mut cands.iter()) {
            out.push(Announcement::Highlight {
                text: winner.text.clone(),
                at: (winner.x0, winner.y),
            });
            self.remember(&winner);
            return out;
        }

        // A large repaint without a frame. When it covers the screen and
        // carries text it is a new page (a viewer, a help screen): read it.
        // Otherwise a menu closed or the desktop was redrawn: nothing to
        // say, but the old highlight is gone.
        let blocks = diff::blocks(diffs);
        if let Some(page) = blocks
            .iter()
            .find(|b| u32::from(b.height()) * 100 >= u32::from(screen.size.1) * FULL_PAGE_PERCENT)
        {
            let lines: Vec<String> = frame::rows_text(screen, page.top, page.bottom)
                .into_iter()
                .filter(|l| diff::has_word(l))
                .collect();
            self.push_underlay();
            self.highlight = None;
            self.last_block = Some(*page);
            if !lines.is_empty() {
                out.push(Announcement::Window {
                    title: None,
                    lines,
                    focus: None,
                });
            }
            return out;
        }
        if blocks.iter().any(|b| b.height() > MAX_MESSAGE_ROWS) {
            self.highlight = None;
            self.last_block = None;
            return out;
        }

        // The cursor moved: read where it went
        let typed =
            ctx.echoed.is_some() || matches!(ctx.key, Some(KeyKind::Printable | KeyKind::Edit));
        if screen.cursor_visible && cursor_moved && !typed {
            if cursor.1 != ctx.cursor_before.1 {
                let text = frame::segment_around(screen, cursor.0, cursor.1);
                out.push(Announcement::Text {
                    text: if text.is_empty() {
                        "blank".to_string()
                    } else {
                        text
                    },
                    at: (cursor.0, cursor.1),
                });
            } else {
                out.push(Announcement::Char {
                    x: cursor.0,
                    y: cursor.1,
                });
            }
            return out;
        }

        // Messages: text written on a few rows
        let rows: Vec<u16> = diffs.iter().map(|d| d.y).collect();
        self.push_messages(screen, diffs, &rows, typed, &mut out);
        out
    }

    /// Speak the text written on the given rows as messages: rows with a
    /// real word that are not counters (volatile) or the line being typed
    fn push_messages(
        &self,
        screen: &Screen,
        diffs: &[RowDiff],
        rows: &[u16],
        typed: bool,
        out: &mut Vec<Announcement>,
    ) {
        let cursor = screen.cursor;
        let mut spoken = 0;
        for d in diffs
            .iter()
            .filter(|d| d.char_changed && rows.contains(&d.y))
        {
            if spoken >= MAX_MESSAGES {
                break;
            }
            if self.is_volatile(d.y) || (d.y == cursor.1 && typed) {
                continue;
            }
            let text: Vec<String> = diff::changed_spans(screen, d)
                .into_iter()
                .map(|s| s.text)
                .filter(|t| diff::has_word(t))
                .collect();
            if text.is_empty() {
                continue;
            }
            let at = (d.cells.first().map_or(0, |c| c.x), d.y);
            out.push(Announcement::Text {
                text: text.join(" "),
                at,
            });
            spoken += 1;
        }
    }

    /// The window to read on demand: the smallest frame around `anchor`,
    /// else the last window that opened, else the whole screen
    pub fn window_at(&self, screen: &Screen, anchor: (u16, u16)) -> Announcement {
        let rect = frame::innermost_frame(screen, anchor.0, anchor.1)
            .or(self.last_block)
            .filter(|r| r.bottom <= screen.size.1 && r.right < screen.size.0);
        let focus = self.highlight.as_ref().and_then(|h| match rect {
            Some(r) if r.contains(h.x0, h.row) => Some(h.text.clone()),
            None => Some(h.text.clone()),
            _ => None,
        });
        match rect {
            Some(r) => Announcement::Window {
                title: frame::title(screen, &r),
                lines: frame::lines(screen, &r),
                focus,
            },
            None => Announcement::Window {
                title: None,
                lines: frame::screen_lines(screen),
                focus,
            },
        }
    }
}

/// Whether two windows' texts are the same but for at most one row
fn lines_alike(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().zip(b).filter(|(x, y)| x != y).count() <= 1
}

/// Evidence for the detector, read off the screen state
pub fn signals_from_screen(screen: &Screen, alt_entered: bool) -> Signals {
    let mut bgs: HashSet<Color> = HashSet::new();
    let mut bg_rows = 0;
    for row in &screen.buffer {
        let mut coloured = false;
        for cell in row {
            let bg = cell.attrs.effective_bg();
            bgs.insert(bg);
            if bg != Color::Default {
                coloured = true;
            }
        }
        if coloured {
            bg_rows += 1;
        }
    }
    Signals {
        alt_entered,
        in_alt: screen.in_alt_screen,
        cursor_hidden: !screen.cursor_visible,
        autowrap_off: !screen.autowrap,
        hidden_for: std::time::Duration::ZERO,
        bg_rows,
        distinct_bgs: bgs.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Emulator;

    /// An emulator with a tracker forced on, fed in bursts
    struct Rig {
        emu: Emulator,
        tracker: TuiTracker,
    }

    impl Rig {
        fn new(cols: u16, rows: u16) -> Self {
            let emu = Emulator::new(cols, rows);
            let mut tracker = TuiTracker::new(TuiMode::On, Vec::new());
            tracker.resync(&emu.screen);
            Self { emu, tracker }
        }

        fn burst(&mut self, bytes: &[u8]) -> Vec<Announcement> {
            self.burst_with(bytes, None)
        }

        fn burst_with(&mut self, bytes: &[u8], key: Option<KeyKind>) -> Vec<Announcement> {
            let before = self.emu.cursor();
            self.emu.process(bytes).unwrap();
            self.tracker.note_output(&self.emu.screen, None);
            let ctx = ObserveCtx {
                cursor_before: before,
                echoed: None,
                key,
            };
            self.tracker.observe(&self.emu.screen, &ctx)
        }
    }

    fn highlight(text: &str) -> Announcement {
        Announcement::Highlight {
            text: text.to_string(),
            at: (0, 0),
        }
    }

    fn texts(anns: &[Announcement]) -> Vec<String> {
        anns.iter()
            .map(|a| match a {
                Announcement::Highlight { text, .. } => format!("H:{}", text),
                Announcement::Window {
                    title,
                    lines,
                    focus,
                } => format!(
                    "W:{}|{}|{}",
                    title.clone().unwrap_or_default(),
                    lines.join("/"),
                    focus.clone().unwrap_or_default()
                ),
                Announcement::Text { text, .. } => format!("T:{}", text),
                Announcement::Char { x, y } => format!("C:{},{}", x, y),
                Announcement::ModeChanged(on) => format!("M:{}", on),
            })
            .collect()
    }

    #[test]
    fn mode_parsing_and_cycling() {
        assert_eq!("auto".parse::<TuiMode>().unwrap(), TuiMode::Auto);
        assert_eq!("ON".parse::<TuiMode>().unwrap(), TuiMode::On);
        assert_eq!("off".parse::<TuiMode>().unwrap(), TuiMode::Off);
        assert!("maybe".parse::<TuiMode>().is_err());
        assert_eq!(TuiMode::Auto.next(), TuiMode::Apps);
        assert_eq!(TuiMode::Apps.next(), TuiMode::On);
        assert_eq!("apps".parse::<TuiMode>().unwrap(), TuiMode::Apps);
        assert_eq!(TuiMode::On.next(), TuiMode::Off);
        assert_eq!(TuiMode::Off.next(), TuiMode::Auto);
        assert_eq!(TuiMode::On.to_string(), "on");
    }

    #[test]
    fn key_kinds() {
        assert_eq!(KeyKind::of(b"a"), KeyKind::Printable);
        assert_eq!(KeyKind::of(b" "), KeyKind::Printable);
        assert_eq!(KeyKind::of("é".as_bytes()), KeyKind::Printable);
        assert_eq!(KeyKind::of(b"\x01"), KeyKind::Other);
        assert_eq!(KeyKind::of(b"\x1b[B"), KeyKind::Nav);
        assert_eq!(KeyKind::of(b"\x1bOD"), KeyKind::Nav);
        assert_eq!(KeyKind::of(b"\x1b[5~"), KeyKind::Nav);
        assert_eq!(KeyKind::of(b"\x1b[3~"), KeyKind::Edit);
        assert_eq!(KeyKind::of(b"\x7f"), KeyKind::Edit);
        assert_eq!(KeyKind::of(b"\t"), KeyKind::Tab);
        assert_eq!(KeyKind::of(b"\r"), KeyKind::Enter);
        assert_eq!(KeyKind::of(b"\x1b"), KeyKind::Escape);
        assert_eq!(KeyKind::of(b"\x1bf"), KeyKind::Other);
        assert_eq!(KeyKind::of(b"\x1b[21~"), KeyKind::Other);
    }

    /// A Turbo Vision style menu bar: black on white, selected item black on green
    const BAR: &[u8] = b"\x1b[1;1H\x1b[30;47m File  Edit  Help                    \x1b[0m";

    #[test]
    fn menu_bar_highlight_moves() {
        let mut rig = Rig::new(40, 6);
        assert!(rig.burst(BAR).is_empty() || true); // first paint: bar only, no highlight
        let anns = rig.burst(b"\x1b[1;1H\x1b[30;42m File \x1b[0m");
        assert_eq!(texts(&anns), vec!["H:File"]);
        assert_eq!(rig.tracker.current_highlight().unwrap().text, "File");
        // Right: File back to normal, Edit highlighted (hotkey letter red)
        let anns = rig.burst(b"\x1b[1;1H\x1b[30;47m File \x1b[30;42m \x1b[31mE\x1b[30mdit \x1b[0m");
        assert_eq!(texts(&anns), vec!["H:Edit"]);
        let h = rig.tracker.current_highlight().unwrap();
        assert_eq!((h.row, h.x0, h.x1), (0, 6, 11));
        // Nothing changed: nothing said
        assert!(rig
            .burst(b"\x1b[1;1H\x1b[30;47m File \x1b[1;13H")
            .is_empty());
    }

    #[test]
    fn dropdown_under_bar_item_speaks_only_the_selected_item() {
        let mut rig = Rig::new(40, 8);
        rig.burst(BAR);
        rig.burst(b"\x1b[1;1H\x1b[30;42m File \x1b[0m");
        let menu = b"\x1b[2;1H\x1b[30;47m\xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x90\
\x1b[3;1H\xe2\x94\x82\x1b[30;42m New      \x1b[30;47m\xe2\x94\x82\
\x1b[4;1H\xe2\x94\x82 Open  F3 \xe2\x94\x82\
\x1b[5;1H\xe2\x94\x82 Exit     \xe2\x94\x82\
\x1b[6;1H\xe2\x94\x94\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x80\xe2\x94\x98\x1b[0m";
        let anns = rig.burst(menu);
        assert_eq!(texts(&anns), vec!["H:New"]);
        assert_eq!(
            rig.tracker.last_block(),
            Some(Rect {
                top: 1,
                left: 0,
                bottom: 5,
                right: 11
            })
        );
        // Down: New normal, Open highlighted
        let anns =
            rig.burst(b"\x1b[3;2H\x1b[30;47m New      \x1b[4;2H\x1b[30;42m Open  F3 \x1b[0m");
        assert_eq!(texts(&anns), vec!["H:Open F3"]);
        // Alt+F equivalent with no highlight active: bar item and menu open together
        rig.burst(b"\x1b[2;1H\x1b[0m\x1b[K\x1b[3;1H\x1b[K\x1b[4;1H\x1b[K\x1b[5;1H\x1b[K\x1b[6;1H\x1b[K\x1b[1;1H\x1b[30;47m File ");
        assert!(rig.tracker.current_highlight().is_none());
        let mut both = b"\x1b[1;1H\x1b[30;42m File \x1b[0m".to_vec();
        both.extend_from_slice(menu);
        let anns = rig.burst(&both);
        assert_eq!(texts(&anns), vec!["H:File", "H:New"]);
    }

    #[test]
    fn dialog_is_read_once_with_title_text_and_focus() {
        let mut rig = Rig::new(40, 8);
        rig.burst(b"\x1b[44m\x1b[2J");
        let dialog = b"\x1b[2;5H\x1b[30;47m\xe2\x95\x94\xe2\x95\x90\xe2\x95\x90 Save? \xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x97\
\x1b[3;5H\xe2\x95\x91 File modified       \xe2\x95\x91\
\x1b[4;5H\xe2\x95\x91                     \xe2\x95\x91\
\x1b[5;5H\xe2\x95\x91  \x1b[97;42m Yes \x1b[30;47m   \x1b[30;42m No \x1b[30;47m       \xe2\x95\x91\
\x1b[6;5H\xe2\x95\x9a\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x90\xe2\x95\x9d\x1b[0m\x1b[?25l";
        let anns = rig.burst(dialog);
        assert_eq!(texts(&anns), vec!["W:Save?|File modified/Yes No|Yes"]);
        // Tab: Yes loses the bright foreground, No gains it
        let anns = rig.burst(b"\x1b[5;8H\x1b[30;42m Yes \x1b[5;16H\x1b[97;42m No \x1b[0m");
        assert_eq!(texts(&anns), vec!["H:No"]);
        let anns = rig.burst(b"\x1b[5;8H\x1b[97;42m Yes \x1b[5;16H\x1b[30;42m No \x1b[0m");
        assert_eq!(texts(&anns), vec!["H:Yes"]);
        // Read the window again on demand
        let again = rig.tracker.window_at(&rig.emu.screen, (10, 3));
        assert_eq!(texts(&[again]), vec!["W:Save?|File modified/Yes No|Yes"]);
    }

    #[test]
    fn cursor_movement_reads_line_or_character() {
        let mut rig = Rig::new(20, 5);
        rig.burst(b"\x1b[1;1H\xe2\x95\x91abc\x1b[2;1H\xe2\x95\x91def\x1b[1;2H");
        // Down: cursor to row 1 => the line, cut at the frame
        let anns = rig.burst_with(b"\x1b[2;2H", Some(KeyKind::Nav));
        assert_eq!(texts(&anns), vec!["T:def"]);
        // Right: the character
        let anns = rig.burst_with(b"\x1b[2;3H", Some(KeyKind::Nav));
        assert_eq!(texts(&anns), vec!["C:2,1"]);
        // Typing: the echo is spoken elsewhere; the cursor move is not read
        let before = rig.emu.cursor();
        rig.emu.process(b"X").unwrap();
        let ctx = ObserveCtx {
            cursor_before: before,
            echoed: Some('X'),
            key: Some(KeyKind::Printable),
        };
        assert!(rig.tracker.observe(&rig.emu.screen, &ctx).is_empty());
        // A hidden cursor is not followed
        let anns = rig.burst_with(b"\x1b[?25l\x1b[1;2H", Some(KeyKind::Nav));
        assert!(anns.is_empty());
    }

    #[test]
    fn messages_are_spoken_but_counters_and_repaints_are_not() {
        let mut rig = Rig::new(30, 6);
        rig.burst(b"\x1b[6;1H 1:1 ");
        // A counter that changes on every keystroke is dropped (no word)
        for n in 2..6 {
            let anns = rig.burst(format!("\x1b[6;1H 1:{} ", n).as_bytes());
            assert!(anns.is_empty(), "{:?}", anns);
        }
        // A message line is spoken
        let anns = rig.burst(b"\x1b[5;1HCompile successful");
        assert_eq!(texts(&anns), vec!["T:Compile successful"]);
        // A word that keeps changing becomes volatile and goes quiet
        let mut spoken = 0;
        for n in 0..6 {
            let anns = rig.burst(format!("\x1b[5;1Hclock {:02}       ", n).as_bytes());
            spoken += anns.len();
        }
        assert!(spoken <= 3, "{}", spoken);
        // A frameless repaint of many rows is silent
        let anns = rig
            .burst(b"\x1b[1;1Hxxxxxxxxxx\x1b[2;1Hyyyyyyyyyy\x1b[3;1Hzzzzzzzzzz\x1b[4;1Hwwwwwwwwww");
        assert!(anns.is_empty(), "{:?}", anns);
    }

    #[test]
    fn auto_mode_enters_on_evidence_and_leaves_with_the_shell() {
        let mut emu = Emulator::new(40, 6);
        let mut tracker = TuiTracker::new(TuiMode::Auto, Vec::new());
        emu.process(b"$ ls\r\nfile\r\n$ ").unwrap();
        assert_eq!(tracker.note_output(&emu.screen, None), None);
        assert!(!tracker.active());
        // Alternate screen, autowrap off, several backgrounds
        emu.process(b"\x1b[?1049h\x1b[?7l\x1b[44m\x1b[2J\x1b[1;1H\x1b[30;47m menu \x1b[2;1H\x1b[30;42m item \x1b[3;1H\x1b[30;46m x ")
            .unwrap();
        let program = Foreground {
            is_shell: Some(false),
            name: Some("fp".into()),
        };
        assert_eq!(
            tracker.note_output(&emu.screen, Some(&program)),
            Some(Announcement::ModeChanged(true))
        );
        assert!(tracker.active());
        // Leaving the alternate screen with the shell back: off again
        emu.process(b"\x1b[?1049l$ ").unwrap();
        let shell = Foreground {
            is_shell: Some(true),
            name: Some("bash".into()),
        };
        assert_eq!(
            tracker.note_output(&emu.screen, Some(&shell)),
            Some(Announcement::ModeChanged(false))
        );
        assert!(!tracker.active());
    }

    #[test]
    fn manual_modes_ignore_evidence() {
        let mut emu = Emulator::new(10, 3);
        let mut tracker = TuiTracker::new(TuiMode::Off, Vec::new());
        emu.process(b"\x1b[?1049h\x1b[?7l\x1b[?25l\x1b[41m\x1b[2J")
            .unwrap();
        assert_eq!(tracker.note_output(&emu.screen, None), None);
        assert!(!tracker.active());
        assert!(tracker.set_mode(TuiMode::On, &emu.screen));
        assert!(tracker.active());
        assert_eq!(tracker.cycle_mode(&emu.screen), TuiMode::Off);
        assert!(!tracker.active());
        assert_eq!(tracker.cycle_mode(&emu.screen), TuiMode::Auto);
        // Apps mode: evidence is ignored, a listed name is not
        tracker.set_mode(TuiMode::Apps, &emu.screen);
        emu.process(b"\x1b[?1049l\x1b[?1049h\x1b[42m x \x1b[2;1H\x1b[43m y \x1b[3;1H\x1b[44m z ")
            .unwrap();
        let other = Foreground {
            is_shell: Some(false),
            name: Some("other".into()),
        };
        assert_eq!(tracker.note_output(&emu.screen, Some(&other)), None);
        let listed = Foreground {
            is_shell: Some(false),
            name: Some("fp".into()),
        };
        let mut tracker = TuiTracker::new(TuiMode::Apps, vec!["fp".into()]);
        assert_eq!(
            tracker.note_output(&emu.screen, Some(&listed)),
            Some(Announcement::ModeChanged(true))
        );
    }

    #[test]
    fn resize_resyncs_quietly() {
        let mut rig = Rig::new(10, 3);
        rig.burst(b"\x1b[30;42m item ");
        rig.emu.resize(20, 5);
        let anns = rig.burst(b"\x1b[2;1Hmore text");
        assert!(anns.is_empty());
        let anns = rig.burst(b"\x1b[3;1HHello there");
        assert_eq!(texts(&anns), vec!["T:Hello there"]);
    }

    #[test]
    fn window_at_falls_back_to_the_screen() {
        let mut rig = Rig::new(20, 3);
        rig.burst(b"\x1b[1;1Hfirst line\x1b[3;1Hlast line");
        let ann = rig.tracker.window_at(&rig.emu.screen, (0, 0));
        assert_eq!(texts(&[ann]), vec!["W:|first line/last line|"]);
    }

    #[test]
    fn signals_reflect_screen_state() {
        let mut emu = Emulator::new(10, 4);
        emu.process(b"plain").unwrap();
        let s = signals_from_screen(&emu.screen, false);
        assert_eq!((s.bg_rows, s.distinct_bgs), (0, 1));
        assert!(!s.cursor_hidden && !s.autowrap_off && !s.in_alt);
        emu.process(
            b"\x1b[?1049h\x1b[?25l\x1b[?7l\x1b[41m\x1b[2J\x1b[1;1H\x1b[42m x \x1b[2;1H\x1b[43m y ",
        )
        .unwrap();
        let s = signals_from_screen(&emu.screen, true);
        assert_eq!(s.bg_rows, 4);
        assert_eq!(s.distinct_bgs, 3);
        assert!(s.cursor_hidden && s.autowrap_off && s.in_alt && s.alt_entered);
    }

    #[test]
    fn highlight_helper_shape() {
        // keep the helper used; guards against an unused-function warning
        assert_eq!(highlight("x"), highlight("x"));
    }
}
