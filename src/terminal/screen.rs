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

    /// Saved cursor position for alternate screen buffer
    /// Full-screen apps (vim, less) save/restore cursor when switching screens
    saved_cursor: Option<(u16, u16)>,

    /// Saved buffer for alternate screen mode
    /// Allows screen reader to restore previous content when apps exit
    saved_buffer: Option<Vec<Vec<Cell>>>,
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
        }
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
    pub fn scroll_up(&mut self, lines: u16) {
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));

        for _ in 0..lines {
            // Remove top line in scroll region
            if top < self.buffer.len() as u16 {
                self.buffer.remove(top as usize);

                // Add blank line at bottom of scroll region
                if bottom < self.buffer.len() as u16 {
                    self.buffer.insert(bottom as usize, vec![Cell::new(); self.size.0 as usize]);
                }
            }
        }
    }

    /// Scroll the screen down (content moves down, new line at top)
    pub fn scroll_down(&mut self, lines: u16) {
        let (top, bottom) = self.scroll_region.unwrap_or((0, self.size.1 - 1));

        for _ in 0..lines {
            // Remove bottom line in scroll region
            if bottom < self.buffer.len() as u16 {
                self.buffer.remove(bottom as usize);

                // Add blank line at top of scroll region
                self.buffer.insert(top as usize, vec![Cell::new(); self.size.0 as usize]);
            }
        }
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
}
