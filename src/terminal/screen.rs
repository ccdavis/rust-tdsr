//! Terminal screen buffer
//!
//! The screen buffer is the core data structure for screen reader navigation.
//! It maintains a 2D grid of cells that represents what's currently visible
//! in the terminal, allowing the review cursor to read any position.

use std::collections::VecDeque;

use super::Cell;

/// Most scrolled-off lines kept for the review cursor to read back.
pub const MAX_HISTORY: usize = 2000;

/// A designatable character set (`ESC ( x` / `ESC ) x`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    /// Plain characters (`B`, and anything we don't translate)
    Ascii,
    /// DEC special graphics (`0`): the line-drawing set ncurses, tmux and
    /// dialog use for box borders under TERM=xterm*
    DecSpecialGraphics,
}

/// Map a character through the DEC special graphics set. Only `` ` `` to `~`
/// are redefined; everything else passes through.
pub fn dec_special_graphics(c: char) -> char {
    match c {
        '`' => '◆',
        'a' => '▒',
        'b' => '␉',
        'c' => '␌',
        'd' => '␍',
        'e' => '␊',
        'f' => '°',
        'g' => '±',
        'h' => '␤',
        'i' => '␋',
        'j' => '┘',
        'k' => '┐',
        'l' => '┌',
        'm' => '└',
        'n' => '┼',
        'o' => '⎺',
        'p' => '⎻',
        'q' => '─',
        'r' => '⎼',
        's' => '⎽',
        't' => '├',
        'u' => '┤',
        'v' => '┴',
        'w' => '┬',
        'x' => '│',
        'y' => '≤',
        'z' => '≥',
        '{' => 'π',
        '|' => '≠',
        '}' => '£',
        '~' => '·',
        other => other,
    }
}

/// Terminal screen buffer that holds the visual state for screen reader access
///
/// This is the primary data structure the screen reader reads from.
/// The review cursor (revx, revy) indexes into this buffer to read
/// lines, words, and characters for speech output.
pub struct Screen {
    /// 2D buffer: buffer[y][x] where y is row, x is column
    /// Screen readers navigate this to read content back to the user
    pub buffer: Vec<Vec<Cell>>,

    /// Current cursor position (x, y) - where new text will be drawn
    /// Screen reader tracks this to implement cursor tracking mode
    pub cursor: (u16, u16),

    /// Terminal dimensions (cols, rows)
    pub size: (u16, u16),

    /// Scroll region (top, bottom) for terminal scrolling behavior
    /// Used when programs like vim or less set custom scroll regions
    pub scroll_region: Option<(u16, u16)>,

    /// Main-screen cursor saved when entering the alternate screen (1049)
    pub saved_cursor: Option<(u16, u16)>,

    /// Cursor saved by ESC 7 (DECSC), restored by ESC 8 (DECRC). Kept apart
    /// from `saved_cursor` because apps use DECSC/DECRC freely while inside
    /// the alternate screen.
    pub decsc_cursor: Option<(u16, u16)>,

    /// Saved buffer for alternate screen mode
    /// Allows screen reader to restore previous content when apps exit
    saved_buffer: Option<Vec<Vec<Cell>>>,

    /// Whether the alternate screen is active. Re-entering it must not
    /// overwrite the saved main screen with alternate-screen content.
    pub in_alt_screen: bool,

    /// Character sets designated to G0 and G1 (`ESC (` / `ESC )`), and
    /// which one is active (SI selects G0, SO selects G1). Only the DEC
    /// special graphics set is translated; everything else is ASCII.
    pub charsets: [Charset; 2],
    pub active_charset: usize,

    /// Set after printing in the last column: the cursor is shown there but
    /// the next printed character wraps to the next line first (DECAWM).
    /// Cleared by any explicit cursor movement or erase.
    pub pending_wrap: bool,

    /// Accumulated scroll count since last check
    /// Positive = scrolled up (content moved up, so review cursor should move up to follow)
    /// Used by screen reader to adjust review cursor after processing PTY output
    scroll_offset: i16,

    /// Lines that scrolled off the top of the main screen, oldest first,
    /// capped at `MAX_HISTORY`. The review cursor can move up into them
    /// (`ReviewCursor::above`) to read output that no longer fits on screen.
    /// Not recorded while the alternate screen is active (full-screen apps
    /// redraw rather than scroll) or when a scroll region excludes the top row.
    history: VecDeque<Vec<Cell>>,
}

impl Screen {
    /// Create a new screen buffer
    pub fn new(cols: u16, rows: u16) -> Self {
        let buffer = vec![vec![Cell::new(); cols as usize]; rows as usize];

        Self {
            buffer,
            cursor: (0, 0),
            size: (cols, rows),
            scroll_region: None,
            saved_cursor: None,
            decsc_cursor: None,
            saved_buffer: None,
            in_alt_screen: false,
            charsets: [Charset::Ascii, Charset::Ascii],
            active_charset: 0,
            pending_wrap: false,
            scroll_offset: 0,
            history: VecDeque::new(),
        }
    }

    /// Translate a printable character through the active character set.
    pub fn map_charset(&self, c: char) -> char {
        match self.charsets[self.active_charset] {
            Charset::Ascii => c,
            Charset::DecSpecialGraphics => dec_special_graphics(c),
        }
    }

    /// Move the cursor to an absolute position, clamped to the screen.
    /// Any explicit positioning cancels a pending auto-wrap.
    pub fn set_cursor(&mut self, x: u16, y: u16) {
        self.cursor = (
            x.min(self.size.0.saturating_sub(1)),
            y.min(self.size.1.saturating_sub(1)),
        );
        self.pending_wrap = false;
    }

    /// Text of a linear (reading-order) range, inclusive at both ends, as
    /// used by copy/selection. Rows are joined with newlines, each row's
    /// trailing spaces are dropped, and wide-character continuation cells
    /// are skipped. The endpoints may be given in either order.
    pub fn text_range(&self, start: (u16, u16), end: (u16, u16)) -> String {
        let (mut start, mut end) = (start, end);
        if (start.1, start.0) > (end.1, end.0) {
            std::mem::swap(&mut start, &mut end);
        }
        let last_col = self.size.0.saturating_sub(1);
        let mut text = String::new();
        for y in start.1..=end.1 {
            let from = if y == start.1 { start.0 } else { 0 };
            let to = if y == end.1 { end.0 } else { last_col };
            let mut row = String::new();
            for x in from..=to {
                match self.get_char(x, y) {
                    Some('\0') | None => {}
                    Some(ch) => row.push(ch),
                }
            }
            text.push_str(row.trim_end());
            if y < end.1 {
                text.push('\n');
            }
        }
        text
    }

    /// Get and reset scroll offset
    ///
    /// Returns the accumulated scroll offset since last call and resets it.
    /// Positive value means content scrolled up (review cursor should move up).
    /// Negative value means content scrolled down (review cursor should move down).
    pub fn take_scroll_offset(&mut self) -> i16 {
        std::mem::take(&mut self.scroll_offset)
    }

    /// Number of scrolled-off lines available to read back. Zero while the
    /// alternate screen is active: the history belongs to the main screen.
    pub fn history_len(&self) -> usize {
        if self.in_alt_screen {
            0
        } else {
            self.history.len()
        }
    }

    /// A row as the review cursor sees it: `above == 0` is screen row `y`;
    /// `above == n > 0` is the line that scrolled off `n` lines ago (1 is
    /// the most recent, just above the top of the screen).
    fn review_row(&self, above: usize, y: u16) -> Option<&[Cell]> {
        if above == 0 {
            self.buffer.get(y as usize).map(Vec::as_slice)
        } else if above <= self.history_len() {
            self.history
                .get(self.history.len() - above)
                .map(Vec::as_slice)
        } else {
            None
        }
    }

    /// Get character at position for screen reader to speak
    pub fn get_char(&self, x: u16, y: u16) -> Option<char> {
        self.get_char_at(0, x, y)
    }

    /// Character at `x` on the row `review_row(above, y)` selects
    pub fn get_char_at(&self, above: usize, x: u16, y: u16) -> Option<char> {
        self.review_row(above, y)
            .and_then(|row| row.get(x as usize))
            .map(|cell| cell.data)
    }

    /// Get entire line as string for screen reader line reading.
    /// Wide-character continuation cells are skipped so the text carries no
    /// NUL bytes.
    pub fn get_line(&self, y: u16) -> String {
        self.get_line_at(0, y)
    }

    /// Text of the row `review_row(above, y)` selects (see `get_line`)
    pub fn get_line_at(&self, above: usize, y: u16) -> String {
        match self.review_row(above, y) {
            Some(row) => row
                .iter()
                .filter(|cell| !cell.is_wide_continuation)
                .map(|cell| cell.data)
                .collect(),
            None => String::new(),
        }
    }

    /// Get line trimmed (removing trailing spaces) for cleaner speech output
    pub fn get_line_trimmed(&self, y: u16) -> String {
        self.get_line_trimmed_at(0, y)
    }

    /// `get_line_at` with trailing spaces removed
    pub fn get_line_trimmed_at(&self, above: usize, y: u16) -> String {
        self.get_line_at(above, y).trim_end().to_string()
    }

    /// Resize the screen buffer
    /// Called when terminal window size changes (SIGWINCH)
    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Preserve existing content as much as possible for screen reader continuity
        self.buffer = Self::resized_buffer(&self.buffer, cols, rows);

        // The alternate-screen copy (saved while vim/less is running) must be
        // resized too: `restore_screen` swaps it back in, and every row-level
        // operation assumes rows are exactly `size.0` wide.
        if let Some(saved) = self.saved_buffer.as_ref() {
            self.saved_buffer = Some(Self::resized_buffer(saved, cols, rows));
        }

        self.size = (cols, rows);

        // Clamp cursor positions and the scroll region to the new size
        let max_x = cols.saturating_sub(1);
        let max_y = rows.saturating_sub(1);
        self.cursor.0 = self.cursor.0.min(max_x);
        self.cursor.1 = self.cursor.1.min(max_y);
        if let Some((x, y)) = self.saved_cursor {
            self.saved_cursor = Some((x.min(max_x), y.min(max_y)));
        }
        if let Some((x, y)) = self.decsc_cursor {
            self.decsc_cursor = Some((x.min(max_x), y.min(max_y)));
        }
        self.pending_wrap = false;
        if let Some((top, bottom)) = self.scroll_region {
            let top = top.min(max_y);
            let bottom = bottom.min(max_y);
            // A region that no longer spans at least two rows is meaningless;
            // fall back to full-screen scrolling until the app resets it.
            self.scroll_region = if top < bottom {
                Some((top, bottom))
            } else {
                None
            };
        }
    }

    /// Copy `buffer` into a fresh `cols` x `rows` grid, keeping the top-left
    /// overlap and blanking anything new.
    fn resized_buffer(buffer: &[Vec<Cell>], cols: u16, rows: u16) -> Vec<Vec<Cell>> {
        let mut new_buffer = vec![vec![Cell::new(); cols as usize]; rows as usize];
        for (new_row, old_row) in new_buffer.iter_mut().zip(buffer.iter()) {
            let copy_cols = (cols as usize).min(old_row.len());
            new_row[..copy_cols].clone_from_slice(&old_row[..copy_cols]);
        }
        new_buffer
    }

    /// Clear the entire screen
    /// Used by terminal clear commands
    pub fn clear(&mut self) {
        for row in &mut self.buffer {
            for cell in row {
                cell.clear();
            }
        }
    }

    /// Clear from cursor to end of screen
    pub fn clear_to_end(&mut self) {
        let (x, y) = self.cursor;

        // Clear rest of current line
        if let Some(row) = self.buffer.get_mut(y as usize) {
            for cell in row.iter_mut().skip(x as usize) {
                cell.clear();
            }
        }

        // Clear all lines below
        for row in self.buffer.iter_mut().skip(y as usize + 1) {
            for cell in row {
                cell.clear();
            }
        }
    }

    /// Clear from start of screen to cursor
    pub fn clear_to_start(&mut self) {
        let (x, y) = self.cursor;

        // Clear all lines above
        for row in self.buffer.iter_mut().take(y as usize) {
            for cell in row {
                cell.clear();
            }
        }

        // Clear start of current line to cursor
        if let Some(row) = self.buffer.get_mut(y as usize) {
            for cell in row.iter_mut().take(x as usize + 1) {
                cell.clear();
            }
        }
    }

    /// Scroll the screen up (content moves up, new line at bottom)
    /// Important for screen reader to track as new content appears
    ///
    /// This shifts lines within the scroll region upward. The top line
    /// is discarded and a new blank line appears at the bottom.
    pub fn scroll_up(&mut self, lines: u16) {
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let top = top as usize;
        let bottom = bottom as usize;

        // Ensure indices are within buffer bounds
        if top >= self.buffer.len() || bottom >= self.buffer.len() || top > bottom {
            return;
        }

        for _ in 0..lines {
            // The discarded top line goes to the history when it really is
            // the top of the main screen (alternate-screen apps and regions
            // that start lower down are redrawing, not scrolling output).
            if top == 0 && !self.in_alt_screen {
                if self.history.len() >= MAX_HISTORY {
                    self.history.pop_front();
                }
                self.history.push_back(self.buffer[0].clone());
            }

            // Shift each line in the scroll region up by one
            // This discards the top line and leaves space at bottom
            for y in top..bottom {
                // Move line y+1 to position y
                if y + 1 < self.buffer.len() {
                    self.buffer.swap(y, y + 1);
                }
            }
            // Clear the bottom line (it now contains the old top line after swaps)
            if bottom < self.buffer.len() {
                let cols = self.size.0 as usize;
                self.buffer[bottom] = vec![Cell::new(); cols];
            }

            // Track scroll for review cursor adjustment
            self.scroll_offset = self.scroll_offset.saturating_add(1);
        }
    }

    /// Scroll the screen down (content moves down, new line at top)
    ///
    /// This shifts lines within the scroll region downward. The bottom line
    /// is discarded and a new blank line appears at the top.
    pub fn scroll_down(&mut self, lines: u16) {
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let top = top as usize;
        let bottom = bottom as usize;

        // Ensure indices are within buffer bounds
        if top >= self.buffer.len() || bottom >= self.buffer.len() || top > bottom {
            return;
        }

        for _ in 0..lines {
            // Shift each line in the scroll region down by one
            // This discards the bottom line and leaves space at top
            for y in (top..bottom).rev() {
                // Move line y to position y+1
                if y + 1 < self.buffer.len() {
                    self.buffer.swap(y, y + 1);
                }
            }
            // Clear the top line (it now contains the old bottom line after swaps)
            if top < self.buffer.len() {
                let cols = self.size.0 as usize;
                self.buffer[top] = vec![Cell::new(); cols];
            }

            // Track scroll for review cursor adjustment (negative = scrolled down)
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        }
    }

    /// Insert n blank lines at cursor position
    /// Lines below cursor shift down, bottom lines are lost
    /// Only operates when cursor is within the scroll region
    pub fn insert_lines(&mut self, n: u16) {
        let y = self.cursor.1 as usize;
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let top = top as usize;
        let bottom = bottom as usize;
        let cols = self.size.0 as usize;

        if y < top || y > bottom {
            return;
        }

        for _ in 0..n {
            // Shift lines down from cursor to bottom
            for row_idx in (y..bottom).rev() {
                if row_idx + 1 < self.buffer.len() {
                    self.buffer.swap(row_idx, row_idx + 1);
                }
            }
            // Clear the line at cursor position
            if y < self.buffer.len() {
                self.buffer[y] = vec![Cell::new(); cols];
            }
        }
    }

    /// Delete n lines at cursor position
    /// Lines below shift up, blank lines appear at bottom
    /// Only operates when cursor is within the scroll region
    pub fn delete_lines(&mut self, n: u16) {
        let y = self.cursor.1 as usize;
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let top = top as usize;
        let bottom = bottom as usize;
        let cols = self.size.0 as usize;

        if y < top || y > bottom {
            return;
        }

        for _ in 0..n {
            // Shift lines up from cursor to bottom
            for row_idx in y..bottom {
                if row_idx + 1 < self.buffer.len() {
                    self.buffer.swap(row_idx, row_idx + 1);
                }
            }
            // Clear the bottom line
            if bottom < self.buffer.len() {
                self.buffer[bottom] = vec![Cell::new(); cols];
            }
        }
    }

    /// Insert n blank characters at cursor position
    /// Characters to the right shift right, rightmost characters are lost
    pub fn insert_chars(&mut self, n: u16) {
        let (x, y) = (self.cursor.0 as usize, self.cursor.1 as usize);

        if let Some(row) = self.buffer.get_mut(y) {
            let cols = row.len();
            for _ in 0..n {
                if x < cols {
                    // Shift characters right
                    for i in (x..cols - 1).rev() {
                        row.swap(i, i + 1);
                    }
                    // Insert blank at cursor
                    row[x] = Cell::new();
                }
            }
        }
    }

    /// Erase n characters at the cursor in place (ECH, `CSI n X`).
    /// Unlike DCH nothing shifts; the cells simply become blank.
    pub fn erase_chars(&mut self, n: u16) {
        let (x, y) = (self.cursor.0 as usize, self.cursor.1 as usize);
        if let Some(row) = self.buffer.get_mut(y) {
            for cell in row.iter_mut().skip(x).take(n as usize) {
                cell.clear();
            }
        }
    }

    /// Delete n characters at cursor position
    /// Characters to the right shift left, blank characters appear at end
    pub fn delete_chars(&mut self, n: u16) {
        let (x, y) = (self.cursor.0 as usize, self.cursor.1 as usize);

        if let Some(row) = self.buffer.get_mut(y) {
            let cols = row.len();
            for _ in 0..n {
                if x < cols {
                    // Shift characters left
                    for i in x..cols - 1 {
                        row.swap(i, i + 1);
                    }
                    // Clear the last character
                    if cols > 0 {
                        row[cols - 1] = Cell::new();
                    }
                }
            }
        }
    }

    /// Set scroll region (DECSTBM)
    /// top and bottom are 1-indexed row numbers
    pub fn set_scroll_region(&mut self, top: u16, bottom: u16) {
        // Convert from 1-indexed to 0-indexed
        let top = top.saturating_sub(1);
        let bottom = bottom.saturating_sub(1).min(self.size.1 - 1);

        if top < bottom {
            self.scroll_region = Some((top, bottom));
        } else {
            // Invalid region, reset to full screen
            self.scroll_region = None;
        }

        // Move cursor to home position
        self.cursor = (0, 0);
    }

    /// Save current screen state (alternate buffer mode)
    /// Apps like vim use this to preserve shell content
    pub fn save_screen(&mut self) {
        if self.in_alt_screen {
            // Some apps re-emit 1049h; the main screen is already saved.
            return;
        }
        self.in_alt_screen = true;
        self.saved_cursor = Some(self.cursor);
        self.saved_buffer = Some(self.buffer.clone());
        self.pending_wrap = false;
    }

    /// Restore saved screen state
    /// Allows screen reader to return to previous content when app exits
    pub fn restore_screen(&mut self) {
        if !self.in_alt_screen {
            return;
        }
        self.in_alt_screen = false;
        if let Some(buffer) = self.saved_buffer.take() {
            self.buffer = buffer;
        }
        if let Some(cursor) = self.saved_cursor.take() {
            self.cursor = cursor;
        }
        self.pending_wrap = false;
    }

    /// Full reset (RIS, `ESC c`): blank screen, cursor home, no scroll
    /// region, no saved state.
    pub fn reset(&mut self) {
        self.clear();
        self.cursor = (0, 0);
        self.scroll_region = None;
        self.saved_cursor = None;
        self.decsc_cursor = None;
        self.saved_buffer = None;
        self.in_alt_screen = false;
        self.charsets = [Charset::Ascii, Charset::Ascii];
        self.active_charset = 0;
        self.pending_wrap = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_screen() {
        let screen = Screen::new(80, 24);
        assert_eq!(screen.size, (80, 24));
        assert_eq!(screen.cursor, (0, 0));
        assert_eq!(screen.buffer.len(), 24);
        assert_eq!(screen.buffer[0].len(), 80);
    }

    #[test]
    fn test_get_char() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[2][3].data = 'A';

        assert_eq!(screen.get_char(3, 2), Some('A'));
        assert_eq!(screen.get_char(0, 0), Some(' '));
        assert_eq!(screen.get_char(100, 100), None);
    }

    #[test]
    fn test_get_line() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[0][0].data = 'H';
        screen.buffer[0][1].data = 'i';

        let line = screen.get_line(0);
        assert!(line.starts_with("Hi"));
        assert_eq!(line.len(), 10);
    }

    #[test]
    fn test_get_line_trimmed() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[0][0].data = 'A';
        screen.buffer[0][1].data = 'B';

        let line = screen.get_line_trimmed(0);
        assert_eq!(line, "AB");
    }

    #[test]
    fn test_resize() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[2][3].data = 'X';

        screen.resize(20, 10);
        assert_eq!(screen.size, (20, 10));
        assert_eq!(screen.buffer.len(), 10);
        assert_eq!(screen.buffer[0].len(), 20);

        // Old data should be preserved
        assert_eq!(screen.get_char(3, 2), Some('X'));
    }

    #[test]
    fn test_clear() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[2][3].data = 'A';
        screen.clear();

        assert_eq!(screen.get_char(3, 2), Some(' '));
    }

    #[test]
    fn test_scroll_up() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';

        screen.scroll_up(1);

        // First line should now be what was second line
        assert_eq!(screen.get_char(0, 0), Some('B'));
        assert_eq!(screen.get_char(0, 1), Some('C'));
    }

    #[test]
    fn test_scroll_up_keeps_history_of_main_screen_only() {
        let mut screen = Screen::new(10, 3);
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';
        screen.scroll_up(2);
        assert_eq!(screen.history_len(), 2);
        // above = 1 is the line just off the top, 2 the one before it
        assert_eq!(screen.get_line_trimmed_at(1, 0), "B");
        assert_eq!(screen.get_line_trimmed_at(2, 0), "A");
        assert_eq!(screen.get_char_at(2, 0, 0), Some('A'));
        assert_eq!(screen.get_line_trimmed_at(3, 0), "");
        // above = 0 is the screen itself
        assert_eq!(screen.get_line_trimmed_at(0, 0), "C");

        // A scroll region that leaves the top row alone is not output
        // scrolling (DECSTBM parameters are 1-indexed: rows 2-3 here)
        screen.set_scroll_region(2, 3);
        screen.scroll_up(1);
        assert_eq!(screen.history_len(), 2);
        screen.set_scroll_region(1, 3);

        // Nor is anything that happens on the alternate screen; the history
        // is hidden while it is active and back afterwards
        screen.save_screen();
        screen.scroll_up(1);
        assert_eq!(screen.history_len(), 0);
        screen.restore_screen();
        assert_eq!(screen.history_len(), 2);
        assert_eq!(screen.get_line_trimmed_at(2, 0), "A");

        // Capped: the oldest lines go first
        for _ in 0..MAX_HISTORY {
            screen.scroll_up(1);
        }
        assert_eq!(screen.history_len(), MAX_HISTORY);
        // "A" and "B" were dropped; "C" is now the oldest line
        assert_eq!(screen.get_line_trimmed_at(MAX_HISTORY, 0), "C");
        assert_eq!(screen.get_line_trimmed_at(MAX_HISTORY - 1, 0), "");
    }

    #[test]
    fn test_scroll_up_preserves_buffer_size() {
        let mut screen = Screen::new(10, 5);
        let original_len = screen.buffer.len();

        // Mark all rows
        for y in 0..5 {
            screen.buffer[y][0].data = char::from_digit(y as u32, 10).unwrap();
        }

        // Scroll multiple times
        for _ in 0..10 {
            screen.scroll_up(1);
        }

        // Buffer size must remain constant
        assert_eq!(screen.buffer.len(), original_len);
        assert_eq!(screen.buffer.len(), 5);

        // All rows should have correct width
        for row in &screen.buffer {
            assert_eq!(row.len(), 10);
        }
    }

    #[test]
    fn test_scroll_up_bottom_is_blank() {
        let mut screen = Screen::new(10, 5);

        // Fill all rows with 'X'
        for y in 0..5 {
            for x in 0..10 {
                screen.buffer[y][x].data = 'X';
            }
        }

        screen.scroll_up(1);

        // Bottom row should be blank (spaces)
        let bottom_line = screen.get_line_trimmed(4);
        assert_eq!(
            bottom_line, "",
            "Bottom line should be blank after scroll_up"
        );
    }

    #[test]
    fn test_scroll_up_multiple_lines() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';
        screen.buffer[3][0].data = 'D';
        screen.buffer[4][0].data = 'E';

        screen.scroll_up(2);

        // First line should now be what was third line
        assert_eq!(screen.get_char(0, 0), Some('C'));
        assert_eq!(screen.get_char(0, 1), Some('D'));
        assert_eq!(screen.get_char(0, 2), Some('E'));
        // Last two lines should be blank
        assert_eq!(screen.get_line_trimmed(3), "");
        assert_eq!(screen.get_line_trimmed(4), "");
    }

    #[test]
    fn test_scroll_down() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';

        screen.scroll_down(1);

        // Top line should be blank
        assert_eq!(screen.get_line_trimmed(0), "");
        // Second line should now be what was first line
        assert_eq!(screen.get_char(0, 1), Some('A'));
        assert_eq!(screen.get_char(0, 2), Some('B'));
    }

    #[test]
    fn test_scroll_down_preserves_buffer_size() {
        let mut screen = Screen::new(10, 5);
        let original_len = screen.buffer.len();

        // Scroll multiple times
        for _ in 0..10 {
            screen.scroll_down(1);
        }

        // Buffer size must remain constant
        assert_eq!(screen.buffer.len(), original_len);
    }

    #[test]
    fn test_scroll_with_scroll_region() {
        let mut screen = Screen::new(10, 10);

        // Set scroll region to middle rows (2-7, 0-indexed)
        screen.scroll_region = Some((2, 7));

        // Fill rows with letters
        for y in 0..10 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        screen.scroll_up(1);

        // Rows outside scroll region should be unchanged
        assert_eq!(screen.get_char(0, 0), Some('A'));
        assert_eq!(screen.get_char(0, 1), Some('B'));
        assert_eq!(screen.get_char(0, 8), Some('I'));
        assert_eq!(screen.get_char(0, 9), Some('J'));

        // Rows inside scroll region should have shifted up
        assert_eq!(screen.get_char(0, 2), Some('D')); // Was row 3
        assert_eq!(screen.get_char(0, 6), Some('H')); // Was row 7

        // Bottom of scroll region should be blank
        assert_eq!(screen.get_line_trimmed(7), "");
    }

    #[test]
    fn test_save_restore_screen() {
        let mut screen = Screen::new(10, 5);
        screen.buffer[2][3].data = 'X';
        screen.cursor = (5, 3);

        screen.save_screen();

        // Modify screen
        screen.buffer[2][3].data = 'Y';
        screen.cursor = (0, 0);

        screen.restore_screen();

        // Should be back to saved state
        assert_eq!(screen.get_char(3, 2), Some('X'));
        assert_eq!(screen.cursor, (5, 3));
    }

    // ========== Insert/Delete Lines Tests ==========

    #[test]
    fn test_insert_lines() {
        let mut screen = Screen::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Insert 1 line at row 2
        screen.cursor = (0, 2);
        screen.insert_lines(1);

        // Lines should shift down
        assert_eq!(screen.get_char(0, 0), Some('A'));
        assert_eq!(screen.get_char(0, 1), Some('B'));
        assert_eq!(screen.get_line_trimmed(2), ""); // New blank line
        assert_eq!(screen.get_char(0, 3), Some('C')); // Shifted down
        assert_eq!(screen.get_char(0, 4), Some('D')); // Shifted down
                                                      // 'E' is pushed off the bottom
    }

    #[test]
    fn test_delete_lines() {
        let mut screen = Screen::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Delete 1 line at row 2
        screen.cursor = (0, 2);
        screen.delete_lines(1);

        // Lines should shift up
        assert_eq!(screen.get_char(0, 0), Some('A'));
        assert_eq!(screen.get_char(0, 1), Some('B'));
        assert_eq!(screen.get_char(0, 2), Some('D')); // Was row 3
        assert_eq!(screen.get_char(0, 3), Some('E')); // Was row 4
        assert_eq!(screen.get_line_trimmed(4), ""); // New blank line at bottom
    }

    // ========== Insert/Delete Characters Tests ==========

    #[test]
    fn test_insert_chars() {
        let mut screen = Screen::new(10, 5);

        // Fill a row with "ABCDEFGHIJ"
        for x in 0..10 {
            screen.buffer[0][x].data = (b'A' + x as u8) as char;
        }

        // Insert 2 chars at position 3
        screen.cursor = (3, 0);
        screen.insert_chars(2);

        // Characters should shift right
        assert_eq!(screen.get_line_trimmed(0), "ABC  DEFGH");
    }

    #[test]
    fn test_delete_chars() {
        let mut screen = Screen::new(10, 5);

        // Fill a row with "ABCDEFGHIJ"
        for x in 0..10 {
            screen.buffer[0][x].data = (b'A' + x as u8) as char;
        }

        // Delete 2 chars at position 3
        screen.cursor = (3, 0);
        screen.delete_chars(2);

        // Characters should shift left, blanks appear at end
        assert_eq!(screen.get_line_trimmed(0), "ABCFGHIJ");
    }

    // ========== Scroll Region Tests ==========

    #[test]
    fn test_set_scroll_region() {
        let mut screen = Screen::new(10, 10);

        // Set scroll region to rows 3-7 (1-indexed)
        screen.set_scroll_region(3, 7);

        assert_eq!(screen.scroll_region, Some((2, 6))); // 0-indexed
        assert_eq!(screen.cursor, (0, 0)); // Cursor should move home
    }

    #[test]
    fn test_scroll_region_with_insert_delete() {
        let mut screen = Screen::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Set scroll region to rows 2-4 (1-indexed = rows 1-3, 0-indexed)
        screen.scroll_region = Some((1, 3));

        // Delete line at row 1 (within scroll region)
        screen.cursor = (0, 1);
        screen.delete_lines(1);

        // Only rows within scroll region should shift
        assert_eq!(screen.get_char(0, 0), Some('A')); // Outside region, unchanged
        assert_eq!(screen.get_char(0, 1), Some('C')); // Was row 2
        assert_eq!(screen.get_char(0, 2), Some('D')); // Was row 3
        assert_eq!(screen.get_line_trimmed(3), ""); // New blank line at bottom of region
        assert_eq!(screen.get_char(0, 4), Some('E')); // Outside region, unchanged
    }

    #[test]
    fn test_insert_lines_above_top_margin_is_noop() {
        let mut screen = Screen::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Set scroll region to rows 2-4 (0-indexed)
        screen.scroll_region = Some((2, 4));

        // Cursor above top margin (row 0)
        screen.cursor = (0, 0);
        screen.insert_lines(1);

        // Nothing should change
        for y in 0..5 {
            assert_eq!(screen.get_char(0, y as u16), Some((b'A' + y as u8) as char));
        }
    }

    #[test]
    fn test_delete_lines_above_top_margin_is_noop() {
        let mut screen = Screen::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Set scroll region to rows 2-4 (0-indexed)
        screen.scroll_region = Some((2, 4));

        // Cursor above top margin (row 1)
        screen.cursor = (0, 1);
        screen.delete_lines(1);

        // Nothing should change
        for y in 0..5 {
            assert_eq!(screen.get_char(0, y as u16), Some((b'A' + y as u8) as char));
        }
    }

    #[test]
    fn test_resize_while_in_alternate_screen_keeps_buffers_consistent() {
        // Shell content on the main screen, then an app enters the alt screen.
        let mut screen = Screen::new(10, 5);
        screen.buffer[1][2].data = 'S';
        screen.cursor = (9, 4);
        screen.save_screen();
        screen.clear();
        screen.set_scroll_region(2, 5);

        // Window is widened and made shorter while the app is running.
        screen.resize(20, 3);
        assert_eq!(screen.scroll_region, Some((1, 2)));

        // App exits and restores the main screen: it must match the new size.
        screen.restore_screen();
        assert_eq!(screen.size, (20, 3));
        assert_eq!(screen.buffer.len(), 3);
        assert!(screen.buffer.iter().all(|row| row.len() == 20));
        assert_eq!(screen.get_char(2, 1), Some('S'));
        assert_eq!(screen.cursor, (9, 2));

        // Row-level edits at the far right must not index out of bounds.
        screen.cursor = (19, 2);
        screen.insert_chars(3);
        screen.delete_chars(3);
        screen.cursor = (0, 2);
        screen.insert_chars(1);
        screen.delete_chars(1);
    }

    #[test]
    fn test_resize_collapsed_scroll_region_resets_to_full_screen() {
        let mut screen = Screen::new(10, 10);
        screen.set_scroll_region(8, 10);
        screen.resize(10, 8);
        assert_eq!(screen.scroll_region, None);
        // Scrolling must still work on the full screen afterwards.
        screen.buffer[1][0].data = 'x';
        screen.scroll_up(1);
        assert_eq!(screen.get_char(0, 0), Some('x'));
    }

    #[test]
    fn test_get_line_skips_wide_continuation_cells() {
        let mut screen = Screen::new(6, 1);
        screen.buffer[0][0].data = '日';
        screen.buffer[0][1] = Cell::wide_continuation();
        screen.buffer[0][2].data = 'x';
        assert_eq!(screen.get_line_trimmed(0), "日x");
        assert!(!screen.get_line(0).contains('\0'));
    }

    #[test]
    fn test_text_range_linear_selection() {
        let mut screen = Screen::new(10, 5);
        for y in 0..5 {
            let ch = (b'A' + y as u8) as char;
            for x in 0..10 {
                screen.buffer[y][x].data = ch;
            }
        }
        assert_eq!(screen.text_range((2, 1), (5, 1)), "BBBB");
        assert_eq!(screen.text_range((5, 1), (3, 2)), "BBBBB\nCCCC");
        assert_eq!(screen.text_range((3, 2), (5, 1)), "BBBBB\nCCCC");
        assert_eq!(screen.text_range((7, 1), (2, 3)), "BBB\nCCCCCCCCCC\nDDD");
        assert_eq!(screen.text_range((5, 2), (5, 2)), "C");
    }

    #[test]
    fn test_erase_chars_in_place() {
        let mut screen = Screen::new(5, 1);
        for x in 0..5 {
            screen.buffer[0][x].data = (b'a' + x as u8) as char;
        }
        screen.cursor = (1, 0);
        screen.erase_chars(2);
        assert_eq!(screen.get_line(0), "a  de");
        screen.erase_chars(100); // clamps to the row
        assert_eq!(screen.get_line_trimmed(0), "a");
    }

    #[test]
    fn test_nested_alt_screen_entry_keeps_main_screen() {
        let mut screen = Screen::new(5, 2);
        screen.buffer[0][0].data = 'M';
        screen.save_screen();
        screen.clear();
        screen.buffer[0][0].data = 'A';
        screen.save_screen(); // re-entry must not save alt content
        screen.restore_screen();
        assert_eq!(screen.get_char(0, 0), Some('M'));
        assert!(!screen.in_alt_screen);
        screen.restore_screen(); // spurious exit is a no-op
        assert_eq!(screen.get_char(0, 0), Some('M'));
    }

    #[test]
    fn test_text_range_trims_trailing_spaces_per_row() {
        let mut screen = Screen::new(8, 2);
        for (x, ch) in "ab".chars().enumerate() {
            screen.buffer[0][x].data = ch;
        }
        screen.buffer[1][0].data = 'c';
        assert_eq!(screen.text_range((0, 0), (7, 1)), "ab\nc");
    }
}
