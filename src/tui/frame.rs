//! Box frames and decoration glyphs.
//!
//! Full-screen programs draw windows, dialogs and drop-down menus inside
//! box-drawing frames and decorate them with scroll bars, shadows and
//! block-graphic art. Finding the frames tells the tracker where a dialog
//! is (to read it once, and again on demand); knowing the glyphs lets it
//! strip decoration from what it speaks.

use crate::terminal::Screen;

/// Inclusive rectangle of screen cells. A frame whose sides run off the
/// bottom of the screen (a menu taller than the screen) has `bottom`
/// equal to the number of rows: one past the last row, which then holds
/// content rather than a border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub top: u16,
    pub left: u16,
    pub bottom: u16,
    pub right: u16,
}

impl Rect {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        (self.top..=self.bottom).contains(&y) && (self.left..=self.right).contains(&x)
    }

    pub fn contains_row(&self, y: u16) -> bool {
        (self.top..=self.bottom).contains(&y)
    }

    pub fn width(&self) -> u16 {
        self.right - self.left + 1
    }

    pub fn height(&self) -> u16 {
        self.bottom - self.top + 1
    }

    pub fn area(&self) -> u32 {
        u32::from(self.width()) * u32::from(self.height())
    }

    /// Whether `other` lies within this rectangle
    pub fn encloses(&self, other: &Rect) -> bool {
        self.top <= other.top
            && self.left <= other.left
            && self.bottom >= other.bottom
            && self.right >= other.right
    }
}

fn is_top_left(c: char) -> bool {
    matches!(c, '┌' | '╔' | '╭' | '┏' | '╒' | '╓')
}

fn is_top_right(c: char) -> bool {
    matches!(c, '┐' | '╗' | '╮' | '┓' | '╕' | '╖')
}

fn is_bottom_left(c: char) -> bool {
    matches!(c, '└' | '╚' | '╰' | '┗' | '╘' | '╙')
}

fn is_bottom_right(c: char) -> bool {
    matches!(c, '┘' | '╝' | '╯' | '┛' | '╛' | '╜')
}

/// Horizontal line pieces, including junctions that sit in a horizontal border
pub fn is_horizontal(c: char) -> bool {
    matches!(
        c,
        '─' | '═'
            | '━'
            | '╌'
            | '┄'
            | '┈'
            | '┬'
            | '┴'
            | '╤'
            | '╧'
            | '╦'
            | '╩'
            | '┼'
            | '╪'
            | '╫'
            | '╬'
    )
}

/// Vertical line pieces, including junctions that sit in a vertical border
pub fn is_vertical(c: char) -> bool {
    matches!(
        c,
        '│' | '║'
            | '┃'
            | '╎'
            | '┆'
            | '┊'
            | '├'
            | '┤'
            | '╟'
            | '╢'
            | '╠'
            | '╣'
            | '┼'
            | '╪'
            | '╫'
            | '╬'
    )
}

/// Any box-drawing character (U+2500..U+257F)
pub fn is_box_drawing(c: char) -> bool {
    ('\u{2500}'..='\u{257F}').contains(&c)
}

/// Decoration that carries no text: block elements (shadows, scroll bars,
/// desktop art), shades, and the small geometric shapes Turbo Vision and
/// friends use for scroll arrows, close buttons and sub-menu markers.
pub fn is_glyph(c: char) -> bool {
    ('\u{2580}'..='\u{259F}').contains(&c)
        || matches!(
            c,
            '■' | '□'
                | '▪'
                | '▫'
                | '▲'
                | '▼'
                | '◄'
                | '►'
                | '◀'
                | '▶'
                | '↕'
                | '↑'
                | '↓'
                | '◆'
                | '●'
                | '○'
                | '•'
        )
}

/// Frame or decoration: not part of any text worth speaking
pub fn is_decoration(c: char) -> bool {
    is_box_drawing(c) || is_glyph(c)
}

/// Text with decoration replaced by spaces, runs of whitespace collapsed,
/// and the ends trimmed
pub fn clean_text(chars: impl Iterator<Item = char>) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in chars {
        let c = if is_decoration(c) || c == '\0' {
            ' '
        } else {
            c
        };
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Characters of one screen row within `left..=right` (wide continuation
/// cells skipped)
fn row_chars(screen: &Screen, y: u16, left: u16, right: u16) -> impl Iterator<Item = char> + '_ {
    screen.buffer[y as usize]
        .iter()
        .skip(left as usize)
        .take((right - left + 1) as usize)
        .filter(|cell| !cell.is_wide_continuation)
        .map(|cell| cell.data)
}

fn cell_char(screen: &Screen, x: u16, y: u16) -> char {
    screen.buffer[y as usize][x as usize].data
}

/// Share of cells in a column range of one row that satisfy `pred`
fn row_share(screen: &Screen, y: u16, left: u16, right: u16, pred: impl Fn(char) -> bool) -> f32 {
    if right < left {
        return 0.0;
    }
    let n = (right - left + 1) as f32;
    let hits = (left..=right)
        .filter(|&x| pred(cell_char(screen, x, y)))
        .count() as f32;
    hits / n
}

fn column_share(
    screen: &Screen,
    x: u16,
    top: u16,
    bottom: u16,
    pred: impl Fn(char) -> bool,
) -> f32 {
    if bottom < top {
        return 0.0;
    }
    let n = (bottom - top + 1) as f32;
    let hits = (top..=bottom)
        .filter(|&y| pred(cell_char(screen, x, y)))
        .count() as f32;
    hits / n
}

/// Every frame on the screen: a top-left corner, a matching top-right
/// corner on the same row (title text between them is fine as long as the
/// row is mostly line), a left border that is line all the way down to a
/// bottom-left corner, and a bottom-right corner across from it. Right
/// borders and bottom rows may be scroll bars (Turbo Vision editors), so
/// they only need to be mostly line or decoration. A frame whose sides
/// reach the last row without a bottom border is taken to run off the
/// screen (`bottom == rows`). The nearest top-right corner that completes
/// a frame wins, so two boxes side by side (mc's panels) are not also read
/// as one wide box. Smallest frames first.
pub fn find_frames(screen: &Screen) -> Vec<Rect> {
    let (cols, rows) = screen.size;
    if cols < 3 || rows < 3 {
        return Vec::new();
    }
    let mut frames = Vec::new();
    for top in 0..rows - 2 {
        for left in 0..cols - 2 {
            if !is_top_left(cell_char(screen, left, top)) {
                continue;
            }
            let before = frames.len();
            for right in left + 2..cols {
                if frames.len() > before {
                    break;
                }
                if !is_top_right(cell_char(screen, right, top)) {
                    continue;
                }
                // Mostly line, but a long title on a narrow box leaves little of it
                if row_share(screen, top, left + 1, right - 1, is_horizontal) < 0.2 {
                    continue;
                }
                for bottom in top + 2..rows {
                    let c = cell_char(screen, left, bottom);
                    if bottom == rows - 1 && is_vertical(c) {
                        // Runs off the screen: sides only
                        if column_share(screen, left, top + 1, bottom, is_vertical) >= 0.8
                            && column_share(screen, right, top + 1, bottom, |c| {
                                is_vertical(c) || is_glyph(c)
                            }) >= 0.5
                        {
                            frames.push(Rect {
                                top,
                                left,
                                bottom: rows,
                                right,
                            });
                        }
                        break;
                    }
                    if is_bottom_left(c) {
                        if is_bottom_right(cell_char(screen, right, bottom))
                            && column_share(screen, left, top + 1, bottom - 1, is_vertical) >= 0.8
                            && column_share(screen, right, top + 1, bottom - 1, |c| {
                                is_vertical(c) || is_glyph(c)
                            }) >= 0.5
                            && row_share(screen, bottom, left + 1, right - 1, |c| {
                                is_horizontal(c) || is_glyph(c)
                            }) >= 0.2
                        {
                            frames.push(Rect {
                                top,
                                left,
                                bottom,
                                right,
                            });
                        }
                        break;
                    }
                    if !is_vertical(c) {
                        break;
                    }
                }
            }
        }
    }
    frames.sort_by_key(|r| r.area());
    frames.dedup();
    frames
}

/// The smallest frame containing the cell, if any
pub fn innermost_frame(screen: &Screen, x: u16, y: u16) -> Option<Rect> {
    find_frames(screen).into_iter().find(|r| r.contains(x, y))
}

/// Share of a frame's border cells that are marked in `changed` (a
/// per-row list of changed columns); 1.0 when the whole border was just
/// drawn.
pub fn border_changed_share(rect: &Rect, rows: u16, changed: &dyn Fn(u16, u16) -> bool) -> f32 {
    let mut total = 0u32;
    let mut hits = 0u32;
    let mut count = |x: u16, y: u16| {
        total += 1;
        if changed(x, y) {
            hits += 1;
        }
    };
    for x in rect.left..=rect.right {
        count(x, rect.top);
        if rect.bottom < rows {
            count(x, rect.bottom);
        }
    }
    for y in rect.top + 1..rect.bottom {
        count(rect.left, y);
        count(rect.right, y);
    }

    if total == 0 {
        0.0
    } else {
        hits as f32 / total as f32
    }
}

/// Title text from the top border: what is written between the corners,
/// minus line pieces, `[x]`-style widgets and bare numbers (Turbo Vision
/// puts the window number there).
pub fn title(screen: &Screen, rect: &Rect) -> Option<String> {
    if rect.right < rect.left + 2 {
        return None;
    }
    let raw: String = row_chars(screen, rect.top, rect.left + 1, rect.right - 1)
        .map(|c| if is_horizontal(c) { ' ' } else { c })
        .collect();
    let mut words: Vec<&str> = Vec::new();
    for piece in raw.split_whitespace() {
        let is_widget = piece.len() >= 2 && piece.starts_with('[') && piece.ends_with(']');
        let is_number = piece.chars().all(|c| c.is_ascii_digit());
        let bare = clean_text(piece.chars());
        if is_widget || is_number || bare.is_empty() {
            continue;
        }
        words.push(piece);
    }
    if words.is_empty() {
        None
    } else {
        Some(clean_text(words.join(" ").chars()))
    }
}

/// The text inside a frame, one entry per non-blank row, decoration
/// stripped and whitespace collapsed
pub fn lines(screen: &Screen, rect: &Rect) -> Vec<String> {
    if rect.bottom < rect.top + 2 || rect.right < rect.left + 2 {
        return Vec::new();
    }
    (rect.top + 1..rect.bottom.min(screen.size.1))
        .map(|y| clean_text(row_chars(screen, y, rect.left + 1, rect.right - 1)))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Non-blank rows of the whole screen, decoration stripped (for reading a
/// screen that has no frames)
pub fn screen_lines(screen: &Screen) -> Vec<String> {
    rows_text(screen, 0, screen.size.1.saturating_sub(1))
}

/// Non-blank rows `top..=bottom` at full width, decoration stripped
pub fn rows_text(screen: &Screen, top: u16, bottom: u16) -> Vec<String> {
    let (cols, rows) = screen.size;
    if cols == 0 || top >= rows {
        return Vec::new();
    }
    (top..=bottom.min(rows - 1))
        .map(|y| clean_text(row_chars(screen, y, 0, cols - 1)))
        .filter(|line| !line.is_empty())
        .collect()
}

/// The text on row `y` around column `x`, cut at the nearest vertical
/// frame pieces on either side, decoration stripped. Empty when the row
/// segment is blank.
pub fn segment_around(screen: &Screen, x: u16, y: u16) -> String {
    let cols = screen.size.0;
    if cols == 0 || y >= screen.size.1 {
        return String::new();
    }
    let x = x.min(cols - 1);
    let row = &screen.buffer[y as usize];
    let is_wall = |x: u16| is_vertical(row[x as usize].data);
    let mut left = x;
    while left > 0 && !is_wall(left) {
        left -= 1;
    }
    if is_wall(left) && left < x {
        left += 1;
    }
    let mut right = x;
    while right + 1 < cols && !is_wall(right + 1) {
        right += 1;
    }
    if is_wall(right) && right > left {
        right -= 1;
    }
    clean_text(row_chars(screen, y, left, right))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::Emulator;

    fn screen_from(rows: &[&str]) -> Screen {
        let cols = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0) as u16;
        let mut emu = Emulator::new(cols, rows.len() as u16);
        let mut bytes = Vec::new();
        for (y, row) in rows.iter().enumerate() {
            bytes.extend_from_slice(format!("\x1b[{};1H", y + 1).as_bytes());
            bytes.extend_from_slice(row.as_bytes());
        }
        emu.process(&bytes).unwrap();
        emu.screen
    }

    #[test]
    fn finds_single_and_double_frames_with_titles() {
        let screen = screen_from(&[
            "                    ",
            " ┌──── Menu ─────┐  ",
            " │ New           │  ",
            " │ Open       F3 │  ",
            " ├───────────────┤  ",
            " │ Exit    Alt+X │  ",
            " └───────────────┘  ",
            "╔═[■]═ Dialog ═1═╗  ",
            "║ Text here      ║  ",
            "╚════════════════╝  ",
        ]);
        let frames = find_frames(&screen);
        assert_eq!(frames.len(), 2);
        let dialog = Rect {
            top: 7,
            left: 0,
            bottom: 9,
            right: 17,
        };
        let menu = Rect {
            top: 1,
            left: 1,
            bottom: 6,
            right: 17,
        };
        assert_eq!(frames[0], dialog);
        assert_eq!(frames[1], menu);
        assert_eq!(title(&screen, &menu).as_deref(), Some("Menu"));
        assert_eq!(title(&screen, &dialog).as_deref(), Some("Dialog"));
        assert_eq!(lines(&screen, &menu), vec!["New", "Open F3", "Exit Alt+X"]);
        assert_eq!(lines(&screen, &dialog), vec!["Text here"]);
        assert_eq!(innermost_frame(&screen, 3, 2), Some(menu));
        assert_eq!(innermost_frame(&screen, 19, 2), None);
    }

    #[test]
    fn scroll_bars_may_replace_the_right_and_bottom_borders() {
        let screen = screen_from(&[
            "╔═[■]══ noname01.pas ═══1═[↕]═╗",
            "║abc                          ▲",
            "║                             ▓",
            "║                             ▼",
            "╚═══ 1:1 ═◄■▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒▒►┘",
        ]);
        let frames = find_frames(&screen);
        assert_eq!(frames.len(), 1);
        assert_eq!(title(&screen, &frames[0]).as_deref(), Some("noname01.pas"));
        assert_eq!(lines(&screen, &frames[0]), vec!["abc"]);
    }

    #[test]
    fn frame_running_off_the_bottom_of_the_screen() {
        let screen = screen_from(&["┌── m ──┐", "│ one   │", "│ two   │", "│ three │"]);
        let frames = find_frames(&screen);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bottom, 4);
        assert_eq!(title(&screen, &frames[0]).as_deref(), Some("m"));
        assert_eq!(lines(&screen, &frames[0]), vec!["one", "two", "three"]);
        assert_eq!(innermost_frame(&screen, 3, 3), Some(frames[0]));
    }

    #[test]
    fn untitled_frame_and_no_frame() {
        let screen = screen_from(&["┌───┐", "│ a │", "└───┘"]);
        let frames = find_frames(&screen);
        assert_eq!(frames.len(), 1);
        assert_eq!(title(&screen, &frames[0]), None);
        let screen = screen_from(&["┌───┐", "│ a  ", "└───┘"]);
        assert!(find_frames(&screen).is_empty());
    }

    #[test]
    fn border_share_counts_changed_border_cells() {
        let rect = Rect {
            top: 0,
            left: 0,
            bottom: 2,
            right: 2,
        };
        assert_eq!(border_changed_share(&rect, 3, &|_, _| true), 1.0);
        assert_eq!(border_changed_share(&rect, 3, &|_, _| false), 0.0);
        let half = border_changed_share(&rect, 3, &|_, y| y == 0);
        assert!((half - 3.0 / 8.0).abs() < 1e-6);
        // Off-screen bottom: only the top and the sides count
        let open = Rect {
            top: 0,
            left: 0,
            bottom: 3,
            right: 2,
        };
        assert_eq!(border_changed_share(&open, 3, &|_, y| y == 0), 3.0 / 7.0);
    }

    #[test]
    fn clean_text_strips_decoration() {
        assert_eq!(
            clean_text("│ Undo        Alt+BkSp │".chars()),
            "Undo Alt+BkSp"
        );
        assert_eq!(clean_text("Environment     ► ".chars()), "Environment");
        assert_eq!(clean_text("▀▀▄██ ▄▀▄▄".chars()), "");
        assert_eq!(clean_text("  OK   ▄ ".chars()), "OK");
    }

    #[test]
    fn segment_is_cut_at_vertical_frame_pieces() {
        let screen = screen_from(&["║bc      ▓", "│ left │ right │", "plain text"]);
        assert_eq!(segment_around(&screen, 2, 0), "bc");
        assert_eq!(segment_around(&screen, 3, 1), "left");
        assert_eq!(segment_around(&screen, 10, 1), "right");
        assert_eq!(segment_around(&screen, 0, 1), "left");
        assert_eq!(segment_around(&screen, 4, 2), "plain text");
        assert_eq!(segment_around(&screen, 5, 0), "bc");
    }

    #[test]
    fn screen_lines_skip_blank_and_art_rows() {
        let screen = screen_from(&[" File  Edit ", "▀▀▄██ ▄▀▄▄", "", " F1 Help "]);
        assert_eq!(screen_lines(&screen), vec!["File Edit", "F1 Help"]);
    }
}
#[cfg(test)]
mod side_by_side_tests {
    use super::*;
    use crate::terminal::Emulator;

    #[test]
    fn boxes_side_by_side_are_two_frames_not_one() {
        let rows = ["┌─ a ─┐┌─ b ─┐", "│ x   ││ y   │", "└─────┘└─────┘"];
        let cols = rows.iter().map(|r| r.chars().count()).max().unwrap() as u16;
        let mut emu = Emulator::new(cols, rows.len() as u16);
        let mut bytes = Vec::new();
        for (y, row) in rows.iter().enumerate() {
            bytes.extend_from_slice(format!("\x1b[{};1H", y + 1).as_bytes());
            bytes.extend_from_slice(row.as_bytes());
        }
        emu.process(&bytes).unwrap();
        let frames = find_frames(&emu.screen);
        assert_eq!(frames.len(), 2, "{:?}", frames);
        assert!(frames.iter().all(|f| f.width() == 7));
    }
}
