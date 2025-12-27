//! Terminal screen buffer
//!
//! The screen buffer is the core data structure for screen reader navigation.
//! It maintains a 2D grid of cells that represents what's currently visible
//! in the terminal, allowing the review cursor to read any position.

use super::Cell;

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

    /// Saved cursor position for ESC 7/8 (DECSC/DECRC) sequences
    /// Full-screen apps (vim, less) save/restore cursor when switching screens
    pub saved_cursor: Option<(u16, u16)>,

    /// Saved buffer for alternate screen mode
    /// Allows screen reader to restore previous content when apps exit
    saved_buffer: Option<Vec<Vec<Cell>>>,

    /// Accumulated scroll count since last check
    /// Positive = scrolled up (content moved up, so review cursor should move up to follow)
    /// Used by screen reader to adjust review cursor after processing PTY output
    scroll_offset: i16,
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
            saved_buffer: None,
            scroll_offset: 0,
        }
    }

    /// Get and reset scroll offset
    ///
    /// Returns the accumulated scroll offset since last call and resets it.
    /// Positive value means content scrolled up (review cursor should move up).
    /// Negative value means content scrolled down (review cursor should move down).
    pub fn take_scroll_offset(&mut self) -> i16 {
        std::mem::take(&mut self.scroll_offset)
    }

    /// Get character at position for screen reader to speak
    pub fn get_char(&self, x: u16, y: u16) -> Option<char> {
        self.buffer
            .get(y as usize)
            .and_then(|row| row.get(x as usize))
            .map(|cell| cell.data)
    }

    /// Get entire line as string for screen reader line reading
    pub fn get_line(&self, y: u16) -> String {
        if let Some(row) = self.buffer.get(y as usize) {
            row.iter().map(|cell| cell.data).collect()
        } else {
            String::new()
        }
    }

    /// Get line trimmed (removing trailing spaces) for cleaner speech output
    pub fn get_line_trimmed(&self, y: u16) -> String {
        self.get_line(y).trim_end().to_string()
    }

    /// Resize the screen buffer
    /// Called when terminal window size changes (SIGWINCH)
    pub fn resize(&mut self, cols: u16, rows: u16) {
        // Preserve existing content as much as possible for screen reader continuity
        let mut new_buffer = vec![vec![Cell::new(); cols as usize]; rows as usize];

        // Copy old content into new buffer
        let copy_rows = (rows as usize).min(self.buffer.len());
        for (y, row) in new_buffer.iter_mut().enumerate().take(copy_rows) {
            let copy_cols = (cols as usize).min(self.buffer[y].len());
            row[..copy_cols].clone_from_slice(&self.buffer[y][..copy_cols]);
        }

        self.buffer = new_buffer;
        self.size = (cols, rows);

        // Clamp cursor to new size
        self.cursor.0 = self.cursor.0.min(cols.saturating_sub(1));
        self.cursor.1 = self.cursor.1.min(rows.saturating_sub(1));
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
    pub fn insert_lines(&mut self, n: u16) {
        let y = self.cursor.1 as usize;
        let (_, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let bottom = bottom as usize;
        let cols = self.size.0 as usize;

        if y > bottom {
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
    pub fn delete_lines(&mut self, n: u16) {
        let y = self.cursor.1 as usize;
        let (_, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));
        let bottom = bottom as usize;
        let cols = self.size.0 as usize;

        if y > bottom {
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
        let cols = self.size.0 as usize;

        if let Some(row) = self.buffer.get_mut(y) {
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

    /// Delete n characters at cursor position
    /// Characters to the right shift left, blank characters appear at end
    pub fn delete_chars(&mut self, n: u16) {
        let (x, y) = (self.cursor.0 as usize, self.cursor.1 as usize);
        let cols = self.size.0 as usize;

        if let Some(row) = self.buffer.get_mut(y) {
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
        self.saved_cursor = Some(self.cursor);
        self.saved_buffer = Some(self.buffer.clone());
    }

    /// Restore saved screen state
    /// Allows screen reader to return to previous content when app exits
    pub fn restore_screen(&mut self) {
        if let Some(buffer) = self.saved_buffer.take() {
            self.buffer = buffer;
        }
        if let Some(cursor) = self.saved_cursor.take() {
            self.cursor = cursor;
        }
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
}
