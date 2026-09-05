//! Screen diffing: what changed between two snapshots, grouped into the
//! horizontal spans a highlight or a message occupies.

use std::collections::{HashMap, HashSet};

use super::frame::{clean_text, is_decoration};
use crate::terminal::{Attrs, Cell, Color, Screen};

/// One changed cell
#[derive(Clone, Copy, Debug)]
pub struct CellChange {
    pub x: u16,
    pub old: Attrs,
    pub new: Attrs,
    pub old_char: char,
    pub new_char: char,
}

/// A row that differs from the previous snapshot
#[derive(Clone, Debug)]
pub struct RowDiff {
    pub y: u16,
    pub cells: Vec<CellChange>,
    /// Whether any character (not just attributes) changed on the row
    pub char_changed: bool,
}

/// Rows that differ between `prev` and `cur` (which must have the same shape)
pub fn diff_rows(prev: &[Vec<Cell>], cur: &[Vec<Cell>]) -> Vec<RowDiff> {
    let mut out = Vec::new();
    for (y, (p, c)) in prev.iter().zip(cur).enumerate() {
        if p == c {
            continue;
        }
        let mut cells = Vec::new();
        let mut char_changed = false;
        for (x, (pc, cc)) in p.iter().zip(c).enumerate() {
            if pc != cc {
                if pc.data != cc.data {
                    char_changed = true;
                }
                cells.push(CellChange {
                    x: x as u16,
                    old: pc.attrs,
                    new: cc.attrs,
                    old_char: pc.data,
                    new_char: cc.data,
                });
            }
        }
        if !cells.is_empty() {
            out.push(RowDiff {
                y: y as u16,
                cells,
                char_changed,
            });
        }
    }
    out
}

/// A horizontal run of changed cells sharing a background: a menu item
/// that was just (un)highlighted, a message that was just written
#[derive(Clone, Debug)]
pub struct Span {
    pub y: u16,
    pub x0: u16,
    pub x1: u16,
    /// Background the span is painted in now
    pub bg: Color,
    /// The rendition most of the span's text has now (hotkey letters in
    /// another colour don't count)
    pub attrs: Attrs,
    /// The rendition most of that text had before
    pub old_attrs: Attrs,
    /// The span's text, decoration stripped
    pub text: String,
    /// Only attributes changed; every character is what it was
    pub attr_only: bool,
}

impl Span {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        y == self.y && (self.x0..=self.x1).contains(&x)
    }

    pub fn overlaps(&self, y: u16, x0: u16, x1: u16) -> bool {
        y == self.y && self.x0 <= x1 && x0 <= self.x1
    }
}

/// Unchanged cells tolerated inside a span (a redraw that skipped cells
/// which already held the right character)
const SPAN_GAP: u16 = 3;

/// The changed spans of one row. Changed cells are grouped when they are
/// contiguous (or separated by at most `SPAN_GAP` unchanged cells of the
/// same background) and share their new background. Spans without text
/// are dropped.
pub fn changed_spans(screen: &Screen, diff: &RowDiff) -> Vec<Span> {
    let row = &screen.buffer[diff.y as usize];
    let mut spans: Vec<Span> = Vec::new();
    let mut group: Vec<CellChange> = Vec::new();

    let flush = |group: &mut Vec<CellChange>, spans: &mut Vec<Span>| {
        if group.is_empty() {
            return;
        }
        let x0 = group[0].x;
        let x1 = group[group.len() - 1].x;
        let text = clean_text(
            row[x0 as usize..=x1 as usize]
                .iter()
                .filter(|c| !c.is_wide_continuation)
                .map(|c| c.data),
        );
        if !text.is_empty() {
            let text_cells: Vec<&CellChange> = group
                .iter()
                .filter(|c| !c.new_char.is_whitespace() && !is_decoration(c.new_char))
                .collect();
            let attrs = dominant_attrs(text_cells.iter().map(|c| c.new)).unwrap_or(group[0].new);
            let old_attrs =
                dominant_attrs(text_cells.iter().map(|c| c.old)).unwrap_or(group[0].old);
            spans.push(Span {
                y: diff.y,
                x0,
                x1,
                bg: attrs.effective_bg(),
                attrs,
                old_attrs,
                text,
                attr_only: group.iter().all(|c| c.old_char == c.new_char),
            });
        }
        group.clear();
    };

    for change in &diff.cells {
        if let Some(last) = group.last() {
            let same_bg = last.new.effective_bg() == change.new.effective_bg();
            let gap = change.x - last.x - 1;
            let gap_ok = gap <= SPAN_GAP
                && (last.x + 1..change.x)
                    .all(|x| row[x as usize].attrs.effective_bg() == change.new.effective_bg());
            if !(same_bg && gap_ok) {
                flush(&mut group, &mut spans);
            }
        }
        group.push(*change);
    }
    flush(&mut group, &mut spans);
    spans
}

/// The most common rendition among the given ones
pub fn dominant_attrs(attrs: impl Iterator<Item = Attrs>) -> Option<Attrs> {
    let mut hist: HashMap<Attrs, usize> = HashMap::new();
    for a in attrs {
        *hist.entry(a).or_insert(0) += 1;
    }
    hist.into_iter().max_by_key(|(_, n)| *n).map(|(a, _)| a)
}

/// The rendition most text on a row is drawn in, leaving out the cells
/// `x0..=x1` (a span that just changed): what "ordinary" looks like there
pub fn row_text_attrs(row: &[Cell], x0: u16, x1: u16) -> Option<Attrs> {
    dominant_attrs(
        row.iter()
            .enumerate()
            .filter(|(x, c)| {
                !(x0 as usize..=x1 as usize).contains(x)
                    && !c.data.is_whitespace()
                    && !is_decoration(c.data)
            })
            .map(|(_, c)| c.attrs),
    )
}

/// How many cells of each background the screen shows
pub fn bg_histogram(screen: &Screen) -> HashMap<Color, usize> {
    let mut hist = HashMap::new();
    for row in &screen.buffer {
        for cell in row {
            *hist.entry(cell.attrs.effective_bg()).or_insert(0) += 1;
        }
    }
    hist
}

/// The most common background among the given cells
pub fn dominant_bg<'a>(cells: impl Iterator<Item = &'a Cell>) -> Option<Color> {
    let mut hist: HashMap<Color, usize> = HashMap::new();
    for cell in cells {
        *hist.entry(cell.attrs.effective_bg()).or_insert(0) += 1;
    }
    hist.into_iter().max_by_key(|(_, n)| *n).map(|(bg, _)| bg)
}

/// Backgrounds on which some text is drawn in a plain (not bright)
/// foreground: a bright span on one of these stands out from siblings
/// drawn the ordinary way (a focused button next to unfocused ones).
pub fn plain_text_bgs(screen: &Screen) -> HashSet<Color> {
    let mut set = HashSet::new();
    for row in &screen.buffer {
        for cell in row {
            if !cell.data.is_whitespace() && !is_decoration(cell.data) && !cell.attrs.is_bright_fg()
            {
                set.insert(cell.attrs.effective_bg());
            }
        }
    }
    set
}

/// Whether the cells directly above and below the span are (mostly) in a
/// different background: the span is a one-row bar, like a highlighted
/// menu item or list row, rather than part of a coloured area
pub fn is_bar(screen: &Screen, span: &Span) -> bool {
    let rows = screen.size.1;
    let differs = |y: i32| -> bool {
        if y < 0 || y >= i32::from(rows) {
            return true;
        }
        let row = &screen.buffer[y as usize];
        let n = (span.x1 - span.x0 + 1) as usize;
        let other = row[span.x0 as usize..=span.x1 as usize]
            .iter()
            .filter(|c| c.attrs.effective_bg() != span.bg)
            .count();
        other * 2 > n
    };
    differs(i32::from(span.y) - 1) && differs(i32::from(span.y) + 1)
}

/// Rectangles of rows that changed together: consecutive changed rows
/// with character changes form one block (a window or menu being drawn).
/// Single rows are returned too; callers decide what size counts.
pub fn blocks(diffs: &[RowDiff]) -> Vec<super::frame::Rect> {
    let mut out: Vec<super::frame::Rect> = Vec::new();
    for d in diffs.iter().filter(|d| d.char_changed) {
        let left = d.cells.first().map_or(0, |c| c.x);
        let right = d.cells.last().map_or(0, |c| c.x);
        match out.last_mut() {
            Some(rect) if rect.bottom + 1 == d.y => {
                rect.bottom = d.y;
                rect.left = rect.left.min(left);
                rect.right = rect.right.max(right);
            }
            _ => out.push(super::frame::Rect {
                top: d.y,
                left,
                bottom: d.y,
                right,
            }),
        }
    }
    out
}

/// Whether the text contains a word: two or more letters in a row.
/// Counters (`1:4`), clocks and bare punctuation don't.
pub fn has_word(text: &str) -> bool {
    let mut run = 0;
    for c in text.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= 2 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Emulator;

    fn emulate(cols: u16, rows: u16, bytes: &[u8]) -> Screen {
        let mut emu = Emulator::new(cols, rows);
        emu.process(bytes).unwrap();
        emu.screen
    }

    #[test]
    fn diff_reports_changed_rows_and_cells() {
        let a = emulate(6, 2, b"abc");
        let b = emulate(6, 2, b"a\x1b[7mb\x1b[0mX");
        let diffs = diff_rows(&a.buffer, &b.buffer);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].y, 0);
        assert!(diffs[0].char_changed);
        let xs: Vec<u16> = diffs[0].cells.iter().map(|c| c.x).collect();
        assert_eq!(xs, vec![1, 2]);
        assert!(diff_rows(&a.buffer, &a.buffer).is_empty());
    }

    #[test]
    fn spans_group_by_background_and_merge_hotkey_colours() {
        let before = emulate(20, 1, b"\x1b[30;47m File  Edit  Help ");
        let after = emulate(
            20,
            1,
            b"\x1b[30;47m File \x1b[30;42m \x1b[31mE\x1b[30mdit \x1b[30;47m Help ",
        );
        let diffs = diff_rows(&before.buffer, &after.buffer);
        let spans = changed_spans(&after, &diffs[0]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Edit");
        assert_eq!(spans[0].bg, Color::Indexed(2));
        assert!(spans[0].attr_only);
        assert_eq!((spans[0].x0, spans[0].x1), (6, 11));
    }

    #[test]
    fn spans_split_on_background_change_and_skip_gaps() {
        let before = emulate(30, 1, b"File management commands");
        let after = emulate(30, 1, b"Clipboard editing commands");
        let diffs = diff_rows(&before.buffer, &after.buffer);
        let spans = changed_spans(&after, &diffs[0]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Clipboard editing commands");
        assert!(!spans[0].attr_only);

        let before = emulate(12, 1, b"aaaaaaaaaaaa");
        let after = emulate(12, 1, b"\x1b[41mbb\x1b[42mcc\x1b[0maaaaaaaa");
        let diffs = diff_rows(&before.buffer, &after.buffer);
        let spans = changed_spans(&after, &diffs[0]);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "bb");
        assert_eq!(spans[1].text, "cc");
    }

    #[test]
    fn spans_without_text_are_dropped() {
        let before = emulate(6, 1, b"\xe2\x96\x80\xe2\x96\x80\xe2\x96\x80   ");
        let after = emulate(
            6,
            1,
            b"\x1b[90;40m\xe2\x96\x80\xe2\x96\x80\xe2\x96\x80\x1b[0m   ",
        );
        let diffs = diff_rows(&before.buffer, &after.buffer);
        assert!(changed_spans(&after, &diffs[0]).is_empty());
    }

    #[test]
    fn bar_test_looks_above_and_below() {
        let screen = emulate(
            6,
            3,
            b"\x1b[47m      \x1b[1;1H\x1b[42mitem  \x1b[2;1H\x1b[47m      \x1b[3;1H      ",
        );
        // row 0 is green, rows 1 and 2 white
        let sample = |y: u16, x0: u16, x1: u16, bg: Color| Span {
            y,
            x0,
            x1,
            bg,
            attrs: Attrs::default(),
            old_attrs: Attrs::default(),
            text: String::new(),
            attr_only: true,
        };
        assert!(is_bar(&screen, &sample(0, 0, 3, Color::Indexed(2))));
        assert!(!is_bar(&screen, &sample(1, 0, 3, Color::Indexed(7))));
        assert!(!is_bar(&screen, &sample(2, 0, 3, Color::Indexed(7))));
    }

    #[test]
    fn blocks_group_consecutive_rows() {
        let before = emulate(5, 5, b"");
        let after = emulate(5, 5, b"\x1b[1;1Hx\x1b[3;2Hyy\x1b[4;1Hz");
        let diffs = diff_rows(&before.buffer, &after.buffer);
        let b = blocks(&diffs);
        assert_eq!(b.len(), 2);
        assert_eq!((b[0].top, b[0].bottom), (0, 0));
        assert_eq!((b[1].top, b[1].bottom, b[1].left, b[1].right), (2, 3, 0, 2));
    }

    #[test]
    fn histogram_dominant_and_plain_text() {
        let screen = emulate(
            4,
            2,
            b"\x1b[44m\x1b[2J\x1b[1;1H\x1b[97;44mA\x1b[30;44mb\x1b[0m",
        );
        let hist = bg_histogram(&screen);
        assert_eq!(hist[&Color::Indexed(4)], 8);
        assert_eq!(
            dominant_bg(screen.buffer[0].iter()),
            Some(Color::Indexed(4))
        );
        let plain = plain_text_bgs(&screen);
        assert!(plain.contains(&Color::Indexed(4)));
        let screen = emulate(4, 1, b"\x1b[97;44mAB\x1b[0m");
        assert!(!plain_text_bgs(&screen).contains(&Color::Indexed(4)));
    }

    #[test]
    fn words_versus_counters() {
        assert!(has_word("File management"));
        assert!(has_word("ok"));
        assert!(!has_word("1:4"));
        assert!(!has_word("12:30:01"));
        assert!(!has_word("* a b"));
    }
}
