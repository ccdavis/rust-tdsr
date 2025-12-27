//! VTE Performer implementation
//!
//! Separated from Emulator to avoid borrow checker issues

use super::{Cell, Screen};
use crate::speech::SpeechBuffer;
use log::trace;
use unicode_width::UnicodeWidthChar;
use vte::Perform;

/// Performer that updates the screen buffer in response to terminal sequences
///
/// This implements the vte::Perform trait to interpret ANSI escape sequences
/// and update the screen buffer that the screen reader navigates.
///
/// As text is drawn, it's added to the speech buffer for automatic reading.
pub struct ScreenPerformer<'a> {
    pub screen: &'a mut Screen,
    pub speech_buffer: &'a mut SpeechBuffer,
    pub last_drawn: &'a mut (u16, u16),
    /// When true, insert line breaks in speech buffer at newlines
    pub line_pause: bool,
}

impl<'a> ScreenPerformer<'a> {
    /// Check if we should add space to speech buffer
    ///
    /// Screen reader adds spaces when cursor jumps (non-continuous drawing)
    fn should_add_space(&self) -> bool {
        let (x, y) = self.screen.cursor;
        let (last_x, last_y) = *self.last_drawn;

        // If on same line but cursor jumped forward
        if y == last_y && x > last_x + 1 {
            return true;
        }

        false
    }
}

impl<'a> Perform for ScreenPerformer<'a> {
    /// Print a character to the screen
    ///
    /// This is the core operation - as programs print text, we add it to the screen
    /// buffer so the screen reader can read it back. We handle wide characters
    /// (CJK, emoji) by marking continuation cells.
    ///
    /// Auto-wrap behavior (DECAWM mode, enabled by default):
    /// - When cursor is at or past the right margin and a new character arrives,
    ///   wrap to the beginning of the next line before printing
    /// - If already at the bottom line, scroll the screen up first
    fn print(&mut self, c: char) {
        let (cols, rows) = self.screen.size;

        // Get character width for proper cursor advancement
        let width = c.width().unwrap_or(1) as u16;

        // Handle auto-wrap: if cursor is at or past right margin, wrap to next line
        // This implements DECAWM (auto-wrap mode) which is enabled by default
        if self.screen.cursor.0 >= cols {
            self.screen.cursor.0 = 0;
            // Perform linefeed with scrolling
            if self.screen.cursor.1 >= rows - 1 {
                // At bottom of screen - scroll up
                self.screen.scroll_up(1);
                // Cursor stays at bottom row after scroll
            } else {
                self.screen.cursor.1 += 1;
            }
        }

        let (x, y) = self.screen.cursor;

        // Check if we're within bounds (should always be true after wrapping)
        if y >= rows || x >= cols {
            return;
        }

        // Add space to speech buffer if cursor jumped
        if self.should_add_space() {
            self.speech_buffer.write(" ");
        }

        // Write character to screen buffer
        if let Some(row) = self.screen.buffer.get_mut(y as usize) {
            if let Some(cell) = row.get_mut(x as usize) {
                cell.data = c;
                cell.is_wide_continuation = false;
            }

            // For wide characters, mark the next cell as a continuation
            // Screen reader will skip these during character navigation
            if width > 1 && (x + 1) < cols {
                if let Some(next_cell) = row.get_mut((x + 1) as usize) {
                    *next_cell = Cell::wide_continuation();
                }
            }
        }

        // Add character to speech buffer for automatic reading
        self.speech_buffer.write(&c.to_string());

        // Update last drawn position for screen reader
        *self.last_drawn = (x, y);

        // Advance cursor - allow it to go to cols (one past the last column)
        // to trigger wrapping on the next character
        self.screen.cursor.0 = x + width;
    }

    /// Execute a control character (e.g., \n, \r, \t)
    fn execute(&mut self, byte: u8) {
        match byte {
            // Line feed - move cursor down, scrolling if at bottom
            // Screen reader can optionally pause speech at newlines
            b'\n' => {
                // If line_pause is enabled, segment speech at line breaks
                if self.line_pause {
                    self.speech_buffer.line_break();
                } else {
                    // Without line pause, add a space for continuity
                    self.speech_buffer.write(" ");
                }

                let (_, rows) = self.screen.size;
                if self.screen.cursor.1 >= rows - 1 {
                    // At bottom of screen - scroll up to make room
                    self.screen.scroll_up(1);
                    // Cursor stays at bottom row after scroll
                } else {
                    self.screen.cursor.1 += 1;
                }
            }
            // Carriage return - move cursor to start of line
            b'\r' => {
                self.screen.cursor.0 = 0;
            }
            // Tab - advance to next tab stop (every 8 columns)
            // Add space to speech for clarity
            b'\t' => {
                self.speech_buffer.write(" ");
                self.screen.cursor.0 = ((self.screen.cursor.0 / 8) + 1) * 8;
                self.screen.cursor.0 = self.screen.cursor.0.min(self.screen.size.0 - 1);
            }
            // Backspace - move cursor left
            // Speech buffer position is adjusted by removing last char
            b'\x08' => {
                if self.screen.cursor.0 > 0 {
                    self.screen.cursor.0 -= 1;
                    // Remove last character from speech buffer - O(1) operation
                    self.speech_buffer.pop();
                }
            }
            _ => {
                trace!("Unhandled execute: 0x{:02x}", byte);
            }
        }
    }

    /// Handle CSI sequences (most common terminal commands)
    ///
    /// These control cursor movement, clearing, colors, etc.
    /// Screen reader needs to track cursor position and screen content changes.
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        match action {
            // Cursor movement commands
            'H' | 'f' => {
                // Cursor position: CSI row;col H
                let row = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1);
                let col = params
                    .iter()
                    .nth(1)
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1);

                self.screen.cursor = (
                    col.min(self.screen.size.0 - 1),
                    row.min(self.screen.size.1 - 1),
                );
            }
            'A' => {
                // Cursor up
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.cursor.1 = self.screen.cursor.1.saturating_sub(n);
            }
            'B' => {
                // Cursor down
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.cursor.1 = (self.screen.cursor.1 + n).min(self.screen.size.1 - 1);
            }
            'C' => {
                // Cursor right
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.cursor.0 = (self.screen.cursor.0 + n).min(self.screen.size.0 - 1);
            }
            'D' => {
                // Cursor left
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.cursor.0 = self.screen.cursor.0.saturating_sub(n);
            }

            // Erase commands - important for screen reader to know when content is cleared
            'J' => {
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                match mode {
                    0 => self.screen.clear_to_end(),   // Clear to end of screen
                    1 => self.screen.clear_to_start(), // Clear to start of screen
                    2 | 3 => self.screen.clear(),      // Clear entire screen
                    _ => {}
                }
            }
            'K' => {
                // Erase line
                let mode = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(0);
                let (x, y) = self.screen.cursor;

                if let Some(row) = self.screen.buffer.get_mut(y as usize) {
                    match mode {
                        0 => {
                            // Clear to end of line
                            for cell in row.iter_mut().skip(x as usize) {
                                cell.clear();
                            }
                        }
                        1 => {
                            // Clear to start of line
                            for cell in row.iter_mut().take(x as usize + 1) {
                                cell.clear();
                            }
                        }
                        2 => {
                            // Clear entire line
                            for cell in row {
                                cell.clear();
                            }
                        }
                        _ => {}
                    }
                }
            }

            // Scrolling - important for screen reader to track content movement
            'S' => {
                // Scroll up
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.scroll_up(n);
            }
            'T' => {
                // Scroll down
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.scroll_down(n);
            }

            // Insert lines (IL) - insert blank lines at cursor
            'L' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.insert_lines(n);
            }

            // Delete lines (DL) - delete lines at cursor
            'M' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.delete_lines(n);
            }

            // Delete characters (DCH) - delete chars at cursor
            'P' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.delete_chars(n);
            }

            // Insert characters (ICH) - insert blank chars at cursor
            '@' => {
                let n = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                self.screen.insert_chars(n);
            }

            // Set scroll region (DECSTBM) - CSI top;bottom r
            'r' => {
                let top = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1);
                let bottom = params
                    .iter()
                    .nth(1)
                    .and_then(|p| p.first().copied())
                    .unwrap_or(self.screen.size.1);
                self.screen.set_scroll_region(top, bottom);
            }

            // Line Position Absolute (VPA) - CSI n d
            'd' => {
                let row = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1);
                self.screen.cursor.1 = row.min(self.screen.size.1 - 1);
            }

            // Cursor Character Absolute (CHA) - CSI n G
            'G' => {
                let col = params
                    .iter()
                    .next()
                    .and_then(|p| p.first().copied())
                    .unwrap_or(1)
                    .saturating_sub(1);
                self.screen.cursor.0 = col.min(self.screen.size.0 - 1);
            }

            _ => {
                trace!("Unhandled CSI: {} with {:?}", action, params);
            }
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {
    }
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}

    /// Handle ESC sequences
    ///
    /// Implements escape sequences for cursor save/restore and scrolling:
    /// - ESC 7 (DECSC): Save cursor position
    /// - ESC 8 (DECRC): Restore cursor position
    /// - ESC M: Reverse index (move up, scroll down if at top)
    /// - ESC D: Index (move down, scroll up if at bottom)
    /// - ESC E: Next line (CR + LF)
    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Handle sequences with intermediates (like ESC # 8 for DECALN)
        if !intermediates.is_empty() {
            trace!("ESC with intermediates {:?} byte {}", intermediates, byte);
            return;
        }

        match byte {
            // DECSC - Save cursor position
            b'7' => {
                self.screen.saved_cursor = Some(self.screen.cursor);
            }
            // DECRC - Restore cursor position
            b'8' => {
                if let Some(saved) = self.screen.saved_cursor {
                    self.screen.cursor = saved;
                }
            }
            // RI - Reverse Index (move cursor up, scroll down if at top)
            b'M' => {
                let (top, _) = self
                    .screen
                    .scroll_region
                    .unwrap_or((0, self.screen.size.1 - 1));
                if self.screen.cursor.1 == top {
                    // At top of scroll region - scroll down
                    self.screen.scroll_down(1);
                } else if self.screen.cursor.1 > 0 {
                    self.screen.cursor.1 -= 1;
                }
            }
            // IND - Index (move cursor down, scroll up if at bottom)
            b'D' => {
                let (_, bottom) = self
                    .screen
                    .scroll_region
                    .unwrap_or((0, self.screen.size.1 - 1));
                if self.screen.cursor.1 == bottom {
                    // At bottom of scroll region - scroll up
                    self.screen.scroll_up(1);
                } else if self.screen.cursor.1 < self.screen.size.1 - 1 {
                    self.screen.cursor.1 += 1;
                }
            }
            // NEL - Next Line (CR + LF)
            b'E' => {
                self.screen.cursor.0 = 0;
                let (_, bottom) = self
                    .screen
                    .scroll_region
                    .unwrap_or((0, self.screen.size.1 - 1));
                if self.screen.cursor.1 == bottom {
                    self.screen.scroll_up(1);
                } else if self.screen.cursor.1 < self.screen.size.1 - 1 {
                    self.screen.cursor.1 += 1;
                }
            }
            _ => {
                trace!("Unhandled ESC: 0x{:02x} ('{}')", byte, byte as char);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vte::Perform;

    /// Helper to create a performer for testing
    fn create_test_performer(cols: u16, rows: u16) -> (Screen, SpeechBuffer, (u16, u16)) {
        let screen = Screen::new(cols, rows);
        let speech_buffer = SpeechBuffer::new();
        let last_drawn = (0, 0);
        (screen, speech_buffer, last_drawn)
    }

    #[test]
    fn test_print_basic() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.print('H');
            performer.print('i');
        }

        assert_eq!(screen.get_char(0, 0), Some('H'));
        assert_eq!(screen.get_char(1, 0), Some('i'));
        assert_eq!(screen.cursor, (2, 0));
    }

    #[test]
    fn test_print_wraps_at_right_edge() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(5, 3);

        // Print 5 characters to fill the first line
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };

            for c in "ABCDE".chars() {
                performer.print(c);
            }
        }

        // Cursor should be at position 5 (one past the last column)
        // which will trigger wrap on next character
        assert_eq!(screen.cursor.0, 5);
        assert_eq!(screen.cursor.1, 0);

        // Print one more character - should wrap to next line
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.print('F');
        }

        // F should be on the second line at column 0
        assert_eq!(screen.get_char(0, 1), Some('F'));
        assert_eq!(screen.cursor, (1, 1));

        // First line should still have ABCDE
        assert_eq!(screen.get_line_trimmed(0), "ABCDE");
    }

    #[test]
    fn test_print_wraps_and_scrolls_at_bottom() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(5, 3);

        // Fill all 3 lines (5 chars each = 15 chars total)
        // Line 0: ABCDE
        // Line 1: FGHIJ
        // Line 2: KLMNO
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };

            for c in "ABCDEFGHIJKLMNO".chars() {
                performer.print(c);
            }
        }

        // Now cursor is at (5, 2) - past right edge of bottom line
        assert_eq!(screen.cursor.0, 5);
        assert_eq!(screen.cursor.1, 2);

        // Print one more character - should wrap and scroll
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.print('P');
        }

        // After scroll:
        // Line 0 should now have what was line 1: FGHIJ
        // Line 1 should now have what was line 2: KLMNO
        // Line 2 should have P at position 0
        assert_eq!(screen.get_line_trimmed(0), "FGHIJ");
        assert_eq!(screen.get_line_trimmed(1), "KLMNO");
        assert_eq!(screen.get_char(0, 2), Some('P'));
        assert_eq!(screen.cursor, (1, 2));

        // Buffer size should remain constant
        assert_eq!(screen.buffer.len(), 3);
    }

    #[test]
    fn test_linefeed_moves_cursor_down() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);
        screen.cursor = (5, 1);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.execute(b'\n');
        }

        // Cursor should move down, x unchanged
        assert_eq!(screen.cursor, (5, 2));
    }

    #[test]
    fn test_linefeed_scrolls_at_bottom() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(5, 3);

        // Put content on each line
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';
        screen.cursor = (0, 2); // At bottom line

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.execute(b'\n');
        }

        // Should have scrolled
        assert_eq!(screen.get_char(0, 0), Some('B')); // Was line 1
        assert_eq!(screen.get_char(0, 1), Some('C')); // Was line 2
        assert_eq!(screen.get_line_trimmed(2), ""); // New blank line

        // Cursor should stay at bottom
        assert_eq!(screen.cursor.1, 2);

        // Buffer size should remain constant
        assert_eq!(screen.buffer.len(), 3);
    }

    #[test]
    fn test_carriage_return() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);
        screen.cursor = (5, 2);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.execute(b'\r');
        }

        // Cursor should move to column 0, row unchanged
        assert_eq!(screen.cursor, (0, 2));
    }

    #[test]
    fn test_crlf_sequence() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);
        screen.cursor = (5, 1);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.execute(b'\r');
            performer.execute(b'\n');
        }

        // Should be at start of next line
        assert_eq!(screen.cursor, (0, 2));
    }

    #[test]
    fn test_long_output_fills_screen_correctly() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };

            // Simulate output that fills and overflows the screen
            // This simulates something like a long `ls` output
            for i in 0..100 {
                let c = (b'A' + (i % 26)) as char;
                performer.print(c);
            }
        }

        // Screen should still be valid (5 rows, 10 cols)
        assert_eq!(screen.buffer.len(), 5);
        for row in &screen.buffer {
            assert_eq!(row.len(), 10);
        }

        // Last few characters should be visible on the last rows
        // 100 chars at 10 cols = 10 rows, so we've scrolled 5 times
        // The screen should show the last 50 characters (rows 5-9 of output, but we only have 5 rows)
    }

    #[test]
    fn test_line_pause_creates_pending_lines() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: true, // Enable line pause
            };

            // Print some text, then a newline
            performer.print('H');
            performer.print('i');
            performer.execute(b'\n');

            // Print more text
            performer.print('B');
            performer.print('y');
            performer.print('e');
        }

        // With line_pause enabled, "Hi" should be in pending lines
        assert!(speech_buffer.has_pending_lines());
        let lines = speech_buffer.drain_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], "Hi");

        // "Bye" should still be in the buffer (not flushed yet)
        assert_eq!(speech_buffer.contents(), "Bye");
    }

    #[test]
    fn test_no_line_pause_adds_space() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false, // Disable line pause
            };

            // Print some text, then a newline
            performer.print('H');
            performer.print('i');
            performer.execute(b'\n');

            // Print more text
            performer.print('B');
            performer.print('y');
            performer.print('e');
        }

        // Without line_pause, there should be no pending lines
        assert!(!speech_buffer.has_pending_lines());

        // All text should be in the buffer with a space for the newline
        assert_eq!(speech_buffer.contents(), "Hi Bye");
    }

    // ========== ESC Sequence Tests ==========

    #[test]
    fn test_esc_save_restore_cursor() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        // Move cursor to a specific position
        screen.cursor = (5, 3);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };

            // ESC 7 - Save cursor
            performer.esc_dispatch(&[], false, b'7');
        }

        // Move cursor elsewhere
        screen.cursor = (1, 1);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };

            // ESC 8 - Restore cursor
            performer.esc_dispatch(&[], false, b'8');
        }

        // Cursor should be back to saved position
        assert_eq!(screen.cursor, (5, 3));
    }

    #[test]
    fn test_esc_reverse_index() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        // Put content on lines
        screen.buffer[0][0].data = 'A';
        screen.buffer[1][0].data = 'B';
        screen.buffer[2][0].data = 'C';

        // Cursor in middle - ESC M should just move up
        screen.cursor = (0, 2);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.esc_dispatch(&[], false, b'M');
        }

        assert_eq!(screen.cursor.1, 1);

        // Move to top
        screen.cursor = (0, 0);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            // ESC M at top should scroll down
            performer.esc_dispatch(&[], false, b'M');
        }

        // Cursor should stay at top
        assert_eq!(screen.cursor.1, 0);
        // Content should have scrolled down (top line is now blank)
        assert_eq!(screen.get_line_trimmed(0), "");
        assert_eq!(screen.get_char(0, 1), Some('A'));
    }

    #[test]
    fn test_esc_index() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        screen.buffer[2][0].data = 'X';
        screen.buffer[3][0].data = 'Y';
        screen.buffer[4][0].data = 'Z';

        // Cursor in middle - ESC D should just move down
        screen.cursor = (0, 2);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            performer.esc_dispatch(&[], false, b'D');
        }

        assert_eq!(screen.cursor.1, 3);

        // Move to bottom
        screen.cursor = (0, 4);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            // ESC D at bottom should scroll up
            performer.esc_dispatch(&[], false, b'D');
        }

        // Cursor should stay at bottom
        assert_eq!(screen.cursor.1, 4);
        // Content should have scrolled up
        assert_eq!(screen.get_char(0, 1), Some('X'));
        assert_eq!(screen.get_char(0, 2), Some('Y'));
        assert_eq!(screen.get_char(0, 3), Some('Z'));
    }

    #[test]
    fn test_esc_next_line() {
        let (mut screen, mut speech_buffer, mut last_drawn) = create_test_performer(10, 5);

        screen.cursor = (5, 2);

        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
            };
            // ESC E - Next Line (CR + LF)
            performer.esc_dispatch(&[], false, b'E');
        }

        // Should be at start of next line
        assert_eq!(screen.cursor, (0, 3));
    }
}
