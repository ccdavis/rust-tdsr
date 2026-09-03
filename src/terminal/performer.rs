//! VTE Performer implementation
//!
//! Separated from Emulator to avoid borrow checker issues

use super::{Cell, Charset, Screen};
use crate::speech::SpeechBuffer;
use log::trace;
use unicode_width::UnicodeWidthChar;
use vte::{Params, Perform};

/// Performer that updates the screen buffer in response to terminal sequences
///
/// This implements the vte::Perform trait to interpret ANSI escape sequences
/// and update the screen buffer that the screen reader navigates.
///
/// As text is drawn, it's added to the speech buffer for automatic reading.
pub struct ScreenPerformer<'a> {
    pub screen: &'a mut Screen,
    pub speech_buffer: &'a mut SpeechBuffer,
    /// Position where the *next* character would land if output continued
    /// uninterrupted: the cell after the last printed one. Printing anywhere
    /// else means the cursor jumped, and speech gets a separator so words
    /// drawn in different places don't run together.
    pub last_drawn: &'a mut (u16, u16),
    /// When true, insert line breaks in speech buffer at newlines
    pub line_pause: bool,
    /// The character the user just typed, if any. The first character the
    /// terminal draws afterwards is compared against it: a match is the
    /// shell echoing the keystroke, which is reported in `echoed` and kept
    /// out of the speech buffer (key echo speaks it separately). Cleared by
    /// the first draw either way, as in the original TDSR.
    pub echo_key: &'a mut Option<char>,
    /// Set when a drawn character matched `echo_key`.
    pub echoed: Option<char>,
}

/// Numeric CSI parameter `idx`, or `default` when absent or zero (for CSI
/// commands where 0 means "default", which is all cursor/edit commands).
fn param(params: &Params, idx: usize, default: u16) -> u16 {
    match params.iter().nth(idx).and_then(|p| p.first().copied()) {
        Some(0) | None => default,
        Some(n) => n,
    }
}

impl<'a> ScreenPerformer<'a> {
    /// Bottom row of the scroll region (or of the screen)
    fn region_bottom(&self) -> u16 {
        self.screen
            .scroll_region
            .map_or(self.screen.size.1.saturating_sub(1), |(_, b)| b)
    }

    /// Top row of the scroll region (or of the screen)
    fn region_top(&self) -> u16 {
        self.screen.scroll_region.map_or(0, |(t, _)| t)
    }

    /// Move down one row, scrolling the region when at its bottom (LF/IND)
    fn linefeed(&mut self) {
        if self.screen.cursor.1 >= self.region_bottom() {
            self.screen.scroll_up(1);
        } else {
            self.screen.cursor.1 += 1;
        }
    }

    /// Speech separator for a line break: a pause in line_pause mode,
    /// otherwise a space so words on adjacent lines don't merge
    fn speech_line_break(&mut self) {
        if self.line_pause {
            self.speech_buffer.line_break();
        } else {
            self.speech_buffer.push(' ');
            self.speech_buffer.begin_row();
        }
    }

    /// After a line feed the separator is already in the buffer; make the
    /// cursor's new position count as "continuing" so the next character
    /// doesn't add a second one.
    fn continue_at_cursor(&mut self) {
        *self.last_drawn = self.screen.cursor;
    }

    /// The cursor moved to a (possibly) different row by an explicit
    /// command: cancel any pending auto-wrap and, when landing at the start
    /// of the row that is currently being drawn, treat further text as a
    /// rewrite of that row (status lines redrawn with CUP/CHA).
    fn cursor_moved(&mut self) {
        self.screen.pending_wrap = false;
        let (x, y) = self.screen.cursor;
        if x == 0 && y == self.last_drawn.1 {
            self.speech_buffer.mark_overwrite();
        }
    }

    /// Handle private CSI modes (CSI ? Pn h/l)
    ///
    /// These control terminal modes like alternate screen, cursor visibility, etc.
    fn handle_private_mode(&mut self, params: &Params, action: char) {
        let mode = params
            .iter()
            .next()
            .and_then(|p| p.first().copied())
            .unwrap_or(0);

        match (mode, action) {
            // CSI ?1049h - Save cursor + switch to alternate screen + clear
            // CSI ?47h / ?1047h - Switch to alternate screen
            (1049 | 47 | 1047, 'h') => {
                trace!("Entering alternate screen mode ({})", mode);
                self.screen.save_screen();
                self.screen.clear();
                if mode == 1049 {
                    self.screen.cursor = (0, 0);
                }
                self.speech_buffer.begin_row();
            }
            // CSI ?1049l / ?47l / ?1047l - Restore from alternate screen
            (1049 | 47 | 1047, 'l') => {
                trace!("Leaving alternate screen mode ({})", mode);
                self.screen.restore_screen();
                self.speech_buffer.begin_row();
            }
            _ => {
                trace!("Unhandled private mode: ?{}{}", mode, action);
            }
        }
    }
}

impl<'a> Perform for ScreenPerformer<'a> {
    /// Print a character to the screen
    ///
    /// This is the core operation - as programs print text, we add it to the screen
    /// buffer so the screen reader can read it back. We handle wide characters
    /// (CJK, emoji) by marking continuation cells.
    ///
    /// Auto-wrap behavior (DECAWM mode, enabled by default): printing in the
    /// last column leaves the cursor there with `pending_wrap` set; the next
    /// character wraps to the start of the following line first, scrolling
    /// the region when already at its bottom. A wide character that doesn't
    /// fit in the remaining columns wraps too.
    fn print(&mut self, c: char) {
        let (cols, rows) = self.screen.size;
        if cols == 0 || rows == 0 {
            return;
        }

        // Zero-width characters (combining marks, variation selectors, ZWJ)
        // attach to the previous glyph. Our one-char cells can't hold them,
        // so they go to speech only and leave the cursor alone.
        let width = c.width().unwrap_or(0) as u16;
        if width == 0 {
            self.speech_buffer.push(c);
            return;
        }

        // DEC special graphics (box drawing) when that charset is active
        let c = self.screen.map_charset(c);

        // Is this the shell echoing the key the user just typed?
        let is_echo = self.echo_key.take() == Some(c);
        if is_echo {
            self.echoed = Some(c);
        }

        let mut wrapped = false;
        if self.screen.pending_wrap || self.screen.cursor.0 + width > cols {
            self.screen.pending_wrap = false;
            self.screen.cursor.0 = 0;
            self.linefeed();
            wrapped = true;
        }

        let (x, y) = self.screen.cursor;
        if y >= rows || x >= cols {
            return;
        }

        // Speech: text continuing right after the last printed character
        // (including across an auto-wrap) is part of the same run. Anything
        // else is a cursor jump and needs a separator — or, after a carriage
        // return, replaces this row's queued text. An echoed keystroke is
        // drawn but not queued.
        let (last_x, last_y) = *self.last_drawn;
        if is_echo {
            // Nothing queued for it, but the run continues from here.
        } else if !wrapped && (x, y) != (last_x, last_y) {
            if y != last_y {
                self.speech_buffer.begin_row();
                if self.line_pause {
                    self.speech_buffer.line_break();
                } else {
                    self.speech_buffer.push(' ');
                }
            } else if !self.speech_buffer.apply_overwrite() {
                self.speech_buffer.push(' ');
            }
        } else if !wrapped {
            // Continuing the row: a pending CR overwrite no longer applies
            // once text resumes exactly where it left off.
            self.speech_buffer.apply_overwrite();
        }

        // Write character to screen buffer
        if let Some(row) = self.screen.buffer.get_mut(y as usize) {
            if let Some(cell) = row.get_mut(x as usize) {
                cell.data = c;
                cell.is_wide_continuation = false;
            }

            // For wide characters, mark the next cell as a continuation
            // Screen reader will skip these during character navigation
            if width > 1 {
                if let Some(next_cell) = row.get_mut((x + 1) as usize) {
                    *next_cell = Cell::wide_continuation();
                }
            }
        }

        // Add character to speech buffer for automatic reading
        if !is_echo {
            self.speech_buffer.push(c);
        }

        // Advance the cursor; in the last column it stays put with a wrap pending
        let next_x = x + width;
        if next_x >= cols {
            self.screen.cursor.0 = cols - 1;
            self.screen.pending_wrap = true;
        } else {
            self.screen.cursor.0 = next_x;
        }
        *self.last_drawn = (next_x, y);
    }

    /// Execute a control character (e.g., \n, \r, \t)
    fn execute(&mut self, byte: u8) {
        match byte {
            // Line feed - move cursor down, scrolling if at bottom
            // Screen reader can optionally pause speech at newlines
            b'\n' | b'\x0b' | b'\x0c' => {
                self.speech_line_break();
                self.screen.pending_wrap = false;
                self.linefeed();
                self.continue_at_cursor();
            }
            // Carriage return - move cursor to start of line. Text drawn next
            // overwrites this row, so it replaces the row's queued speech
            // (progress bars); a following LF cancels that (normal CRLF).
            b'\r' => {
                self.screen.cursor.0 = 0;
                self.screen.pending_wrap = false;
                if self.screen.cursor.1 == self.last_drawn.1 {
                    self.speech_buffer.mark_overwrite();
                }
            }
            // Tab - advance to next tab stop (every 8 columns)
            // Add space to speech for clarity
            b'\t' => {
                self.speech_buffer.push(' ');
                self.screen.pending_wrap = false;
                let next = ((self.screen.cursor.0 / 8) + 1) * 8;
                self.screen.cursor.0 = next.min(self.screen.size.0.saturating_sub(1));
            }
            // Shift Out / Shift In: select G1 / G0 character set
            b'\x0e' => self.screen.active_charset = 1,
            b'\x0f' => self.screen.active_charset = 0,
            // Backspace - move cursor left
            // Speech buffer position is adjusted by removing last char
            b'\x08' => {
                self.screen.pending_wrap = false;
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
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if intermediates.first() == Some(&b'?') {
            self.handle_private_mode(params, action);
            return;
        }

        let (cols, rows) = self.screen.size;
        let max_x = cols.saturating_sub(1);
        let max_y = rows.saturating_sub(1);
        let (x, y) = self.screen.cursor;

        match action {
            // Cursor position: CSI row;col H (1-based; 0 or absent means 1)
            'H' | 'f' => {
                let row = param(params, 0, 1) - 1;
                let col = param(params, 1, 1) - 1;
                self.screen.set_cursor(col, row);
                self.cursor_moved();
            }
            // Cursor up, stopping at the top margin when inside the region
            'A' => {
                let top = if y >= self.region_top() {
                    self.region_top()
                } else {
                    0
                };
                let new_y = y.saturating_sub(param(params, 0, 1)).max(top);
                self.screen.set_cursor(x, new_y);
                self.cursor_moved();
            }
            // Cursor down, stopping at the bottom margin when inside the region
            'B' => {
                let bottom = if y <= self.region_bottom() {
                    self.region_bottom()
                } else {
                    max_y
                };
                let new_y = (y + param(params, 0, 1)).min(bottom);
                self.screen.set_cursor(x, new_y);
                self.cursor_moved();
            }
            // Cursor right
            'C' => {
                let new_x = (x + param(params, 0, 1)).min(max_x);
                self.screen.set_cursor(new_x, y);
                self.cursor_moved();
            }
            // Cursor left
            'D' => {
                let new_x = x.saturating_sub(param(params, 0, 1));
                self.screen.set_cursor(new_x, y);
                self.cursor_moved();
            }
            // Cursor next line (CNL) / previous line (CPL): move rows, column 0
            'E' => {
                let new_y = (y + param(params, 0, 1)).min(max_y);
                self.screen.set_cursor(0, new_y);
                self.cursor_moved();
            }
            'F' => {
                let new_y = y.saturating_sub(param(params, 0, 1));
                self.screen.set_cursor(0, new_y);
                self.cursor_moved();
            }
            // Cursor Character Absolute (CHA) / Horizontal Position Absolute (HPA)
            'G' | '`' => {
                let col = param(params, 0, 1) - 1;
                self.screen.set_cursor(col, y);
                self.cursor_moved();
            }
            // Line Position Absolute (VPA) - CSI n d
            'd' => {
                let row = param(params, 0, 1) - 1;
                self.screen.set_cursor(x, row);
                self.cursor_moved();
            }

            // Erase commands - important for screen reader to know when content is cleared
            'J' => {
                self.screen.pending_wrap = false;
                match param(params, 0, 0) {
                    0 => self.screen.clear_to_end(),   // Clear to end of screen
                    1 => self.screen.clear_to_start(), // Clear to start of screen
                    2 => self.screen.clear(),          // Clear entire screen
                    // 3 clears the scrollback in xterm; the screen is untouched
                    _ => {}
                }
            }
            'K' => {
                // Erase line
                self.screen.pending_wrap = false;
                let mode = param(params, 0, 0);
                if let Some(row) = self.screen.buffer.get_mut(y as usize) {
                    match mode {
                        // Clear to end of line
                        0 => row.iter_mut().skip(x as usize).for_each(Cell::clear),
                        // Clear to start of line
                        1 => row.iter_mut().take(x as usize + 1).for_each(Cell::clear),
                        // Clear entire line
                        2 => row.iter_mut().for_each(Cell::clear),
                        _ => {}
                    }
                }
            }
            // Erase characters in place (ECH) - used heavily by ncurses and zsh
            'X' => {
                self.screen.pending_wrap = false;
                self.screen.erase_chars(param(params, 0, 1));
            }

            // Scrolling - important for screen reader to track content movement
            'S' => self.screen.scroll_up(param(params, 0, 1)),
            'T' => self.screen.scroll_down(param(params, 0, 1)),

            // Insert lines (IL) - insert blank lines at cursor
            'L' => self.screen.insert_lines(param(params, 0, 1)),
            // Delete lines (DL) - delete lines at cursor
            'M' => self.screen.delete_lines(param(params, 0, 1)),

            // Delete characters (DCH) - delete chars at cursor
            'P' => {
                self.screen.pending_wrap = false;
                self.screen.delete_chars(param(params, 0, 1));
            }
            // Insert characters (ICH) - insert blank chars at cursor
            '@' => {
                self.screen.pending_wrap = false;
                self.screen.insert_chars(param(params, 0, 1));
            }

            // Set scroll region (DECSTBM) - CSI top;bottom r
            'r' => {
                let top = param(params, 0, 1);
                let bottom = param(params, 1, rows);
                self.screen.set_scroll_region(top, bottom);
                self.cursor_moved();
            }

            _ => {
                trace!("Unhandled CSI: {} with {:?}", action, params);
            }
        }
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
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
    /// - ESC c: Full reset (RIS)
    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // Charset designation: ESC ( x -> G0, ESC ) x -> G1. `0` is the DEC
        // special graphics (line drawing) set; anything else is treated as
        // plain text.
        if let [b'(' | b')'] = intermediates {
            let slot = usize::from(intermediates[0] == b')');
            self.screen.charsets[slot] = if byte == b'0' {
                Charset::DecSpecialGraphics
            } else {
                Charset::Ascii
            };
            return;
        }
        // Other sequences with intermediates (like ESC # 8 for DECALN)
        if !intermediates.is_empty() {
            trace!("ESC with intermediates {:?} byte {}", intermediates, byte);
            return;
        }

        match byte {
            // DECSC - Save cursor position
            b'7' => {
                self.screen.decsc_cursor = Some(self.screen.cursor);
            }
            // DECRC - Restore cursor position
            b'8' => {
                if let Some((sx, sy)) = self.screen.decsc_cursor {
                    self.screen.set_cursor(sx, sy);
                    self.cursor_moved();
                }
            }
            // RI - Reverse Index (move cursor up, scroll down if at top)
            b'M' => {
                self.screen.pending_wrap = false;
                if self.screen.cursor.1 == self.region_top() {
                    self.screen.scroll_down(1);
                } else if self.screen.cursor.1 > 0 {
                    self.screen.cursor.1 -= 1;
                }
            }
            // IND - Index (move cursor down, scroll up if at bottom)
            b'D' => {
                self.screen.pending_wrap = false;
                self.linefeed();
            }
            // NEL - Next Line (CR + LF)
            b'E' => {
                self.speech_line_break();
                self.screen.pending_wrap = false;
                self.screen.cursor.0 = 0;
                self.linefeed();
                self.continue_at_cursor();
            }
            // RIS - full reset
            b'c' => {
                self.screen.reset();
                self.speech_buffer.begin_row();
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
            };

            for c in "ABCDE".chars() {
                performer.print(c);
            }
        }

        // Cursor stays in the last column with a wrap pending,
        // which will trigger wrap on next character
        assert_eq!(screen.cursor.0, 4);
        assert!(screen.pending_wrap);
        assert_eq!(screen.cursor.1, 0);

        // Print one more character - should wrap to next line
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
            };

            for c in "ABCDEFGHIJKLMNO".chars() {
                performer.print(c);
            }
        }

        // Now cursor is at (4, 2) - last column of bottom line, wrap pending
        assert_eq!(screen.cursor.0, 4);
        assert!(screen.pending_wrap);
        assert_eq!(screen.cursor.1, 2);

        // Print one more character - should wrap and scroll
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
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
                echo_key: &mut None,
                echoed: None,
            };
            // ESC E - Next Line (CR + LF)
            performer.esc_dispatch(&[], false, b'E');
        }

        // Should be at start of next line
        assert_eq!(screen.cursor, (0, 3));
    }

    // ========== Speech separation / overwrite / wrap tests ==========

    /// Feed a byte string through a real vte parser into a fresh performer.
    fn run_bytes(cols: u16, rows: u16, line_pause: bool, bytes: &[u8]) -> (Screen, SpeechBuffer) {
        let mut screen = Screen::new(cols, rows);
        let mut speech_buffer = SpeechBuffer::new();
        let mut last_drawn = (0, 0);
        let mut parser = vte::Parser::new();
        {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause,
                echo_key: &mut None,
                echoed: None,
            };
            for &b in bytes {
                parser.advance(&mut performer, b);
            }
        }
        (screen, speech_buffer)
    }

    /// Like `run_bytes` but with a typed key pending; returns the echoed char too.
    fn run_bytes_echo(bytes: &[u8], key: char) -> (SpeechBuffer, Option<char>) {
        let mut screen = Screen::new(20, 5);
        let mut speech_buffer = SpeechBuffer::new();
        let mut last_drawn = (0, 0);
        let mut echo_key = Some(key);
        let mut parser = vte::Parser::new();
        let echoed = {
            let mut performer = ScreenPerformer {
                screen: &mut screen,
                speech_buffer: &mut speech_buffer,
                last_drawn: &mut last_drawn,
                line_pause: false,
                echo_key: &mut echo_key,
                echoed: None,
            };
            for &b in bytes {
                parser.advance(&mut performer, b);
            }
            performer.echoed
        };
        (speech_buffer, echoed)
    }

    #[test]
    fn test_key_echo_is_matched_at_draw_time() {
        // Plain echo
        let (sb, echoed) = run_bytes_echo(b"a", 'a');
        assert_eq!(echoed, Some('a'));
        assert_eq!(sb.contents(), "");
        // zsh-style echo wrapped in SGR: still recognised, rest still spoken
        let (sb, echoed) = run_bytes_echo(b"\x1b[32ma\x1b[0m", 'a');
        assert_eq!(echoed, Some('a'));
        assert_eq!(sb.contents(), "");
        // First drawn char differs: no echo, key forgotten, text spoken
        let (sb, echoed) = run_bytes_echo(b"xa", 'a');
        assert_eq!(echoed, None);
        assert_eq!(sb.contents(), "xa");
    }

    #[test]
    fn test_dec_special_graphics_charset() {
        let (screen, sb) = run_bytes(20, 2, false, b"\x1b(0lqqk\x1b(Bok");
        assert_eq!(screen.get_line_trimmed(0), "┌──┐ok");
        assert_eq!(sb.contents(), "┌──┐ok");
        // G1 via SO/SI
        let (screen, _) = run_bytes(20, 2, false, b"\x1b)0a\x0eq\x0fq");
        assert_eq!(screen.get_line_trimmed(0), "a─q");
        // RIS resets charsets
        let (screen, _) = run_bytes(20, 2, false, b"\x1b(0\x1bcq");
        assert_eq!(screen.get_line_trimmed(0), "q");
    }

    #[test]
    fn test_ed3_does_not_clear_screen() {
        let (screen, _) = run_bytes(10, 2, false, b"keep\x1b[3J");
        assert_eq!(screen.get_line_trimmed(0), "keep");
    }

    #[test]
    fn test_cup_to_other_row_separates_speech() {
        // Full-screen apps paint rows with CUP and no newline
        let (_, sb) = run_bytes(20, 5, false, b"\x1b[1;1Hfoo\x1b[2;1Hbar");
        assert_eq!(sb.contents(), "foo bar");
        let (_, mut sb) = run_bytes(20, 5, true, b"\x1b[1;1Hfoo\x1b[2;1Hbar");
        assert_eq!(sb.drain_lines(), vec!["foo".to_string()]);
        assert_eq!(sb.contents(), "bar");
    }

    #[test]
    fn test_crlf_keeps_text_and_adds_single_space() {
        let (_, sb) = run_bytes(20, 5, false, b"Hi\r\nBye");
        assert_eq!(sb.contents(), "Hi Bye");
        let (_, sb) = run_bytes(20, 5, false, b"Hi\nBye");
        assert_eq!(sb.contents(), "Hi Bye");
    }

    #[test]
    fn test_carriage_return_rewrite_replaces_row_speech() {
        // A progress bar redrawn in place is spoken once, not once per redraw
        let (screen, sb) = run_bytes(20, 5, false, b"10%\r20%\r30%");
        assert_eq!(sb.contents(), "30%");
        assert_eq!(screen.get_line_trimmed(0), "30%");
        // ...and a previous, completed row is untouched
        let (_, sb) = run_bytes(20, 5, false, b"done\r\n10%\r20%");
        assert_eq!(sb.contents(), "done 20%");
        // CHA back to column 1 on the same row is a rewrite too
        let (_, sb) = run_bytes(20, 5, false, b"10%\x1b[1G20%");
        assert_eq!(sb.contents(), "20%");
    }

    #[test]
    fn test_wide_chars_do_not_get_spaces_between_them() {
        let (screen, sb) = run_bytes(10, 2, false, "日本語".as_bytes());
        assert_eq!(sb.contents(), "日本語");
        assert_eq!(screen.cursor, (6, 0));
        assert!(screen.buffer[0][1].is_wide_continuation);
        assert_eq!(screen.get_line_trimmed(0), "日本語");
    }

    #[test]
    fn test_wide_char_in_last_column_wraps_first() {
        let (screen, _) = run_bytes(5, 2, false, "abcd日".as_bytes());
        assert_eq!(screen.get_line_trimmed(0), "abcd");
        assert_eq!(screen.get_char(0, 1), Some('日'));
    }

    #[test]
    fn test_zero_width_chars_go_to_speech_only() {
        // e + combining acute, then heart + VS16
        let (screen, sb) = run_bytes(10, 1, false, "e\u{301}x".as_bytes());
        assert_eq!(sb.contents(), "e\u{301}x");
        assert_eq!(screen.get_char(0, 0), Some('e'));
        assert_eq!(screen.get_char(1, 0), Some('x'));
    }

    #[test]
    fn test_wrapped_word_is_not_split_in_speech() {
        let (screen, sb) = run_bytes(5, 3, false, b"abcdefgh");
        assert_eq!(sb.contents(), "abcdefgh");
        assert_eq!(screen.get_line_trimmed(0), "abcde");
        assert_eq!(screen.get_line_trimmed(1), "fgh");
    }

    #[test]
    fn test_backspace_after_full_line_lands_before_last_column() {
        let (screen, _) = run_bytes(5, 2, false, b"abcde\x08X");
        // Real terminals: cursor shown on 'e' (wrap pending), BS moves to 'd'
        assert_eq!(screen.get_line_trimmed(0), "abcXe");
    }

    #[test]
    fn test_sgr_does_not_cancel_pending_wrap() {
        let (screen, _) = run_bytes(5, 2, false, b"abcde\x1b[0mF");
        assert_eq!(screen.get_char(0, 1), Some('F'));
    }

    #[test]
    fn test_ech_erases_in_place() {
        let (screen, _) = run_bytes(10, 1, false, b"abcdef\x1b[2G\x1b[3X");
        assert_eq!(screen.get_line_trimmed(0), "a   ef");
    }

    #[test]
    fn test_csi_param_zero_means_one() {
        let (screen, _) = run_bytes(10, 5, false, b"\x1b[3;3H\x1b[0A\x1b[0D");
        assert_eq!(screen.cursor, (1, 1));
        let (screen, _) = run_bytes(10, 5, false, b"\x1b[3;3H\x1b[0;0H");
        assert_eq!(screen.cursor, (0, 0));
    }

    #[test]
    fn test_cnl_cpl_hpa() {
        let (screen, _) = run_bytes(10, 5, false, b"\x1b[2;5H\x1b[2E");
        assert_eq!(screen.cursor, (0, 3));
        let (screen, _) = run_bytes(10, 5, false, b"\x1b[4;5H\x1b[F");
        assert_eq!(screen.cursor, (0, 2));
        let (screen, _) = run_bytes(10, 5, false, b"\x1b[7`");
        assert_eq!(screen.cursor, (6, 0));
    }

    #[test]
    fn test_decsc_inside_alt_screen_does_not_clobber_main_cursor() {
        let bytes = b"\x1b[2;3Hshell\x1b[?1049h\x1b[5;5H\x1b7\x1b[1;1H\x1b8x\x1b[?1049l";
        let (screen, _) = run_bytes(20, 6, false, bytes);
        // Back on the main screen at the cursor saved by 1049h
        assert_eq!(screen.get_line_trimmed(1), "  shell");
        assert_eq!(screen.cursor, (7, 1));
        assert!(!screen.in_alt_screen);
    }

    #[test]
    fn test_ris_resets_everything() {
        let (screen, _) = run_bytes(10, 5, false, b"abc\x1b[2;4r\x1b[?1049h\x1bc");
        assert_eq!(screen.cursor, (0, 0));
        assert_eq!(screen.scroll_region, None);
        assert!(!screen.in_alt_screen);
        assert_eq!(screen.get_line_trimmed(0), "");
    }
}
