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
    fn print(&mut self, c: char) {
        let (x, y) = self.screen.cursor;
        let (cols, rows) = self.screen.size;

        // Check if we're within bounds
        if y >= rows {
            return;
        }

        // Get character width for proper cursor advancement
        let width = c.width().unwrap_or(1);

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

        // Advance cursor
        self.screen.cursor.0 = (x + width as u16).min(cols - 1);
    }

    /// Execute a control character (e.g., \n, \r, \t)
    fn execute(&mut self, byte: u8) {
        match byte {
            // Line feed - move cursor down
            // Screen reader can optionally pause speech at newlines
            b'\n' => {
                self.screen.cursor.1 = (self.screen.cursor.1 + 1).min(self.screen.size.1 - 1);
                // Note: Line pause handling happens in main loop via config.line_pause()
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
                    // Remove last character from speech buffer
                    if !self.speech_buffer.is_empty() {
                        let contents = self.speech_buffer.contents();
                        if !contents.is_empty() {
                            let new_contents: String = contents.chars()
                                .take(contents.chars().count() - 1)
                                .collect();
                            *self.speech_buffer = SpeechBuffer::new();
                            self.speech_buffer.write(&new_contents);
                        }
                    }
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

            _ => {
                trace!("Unhandled CSI: {} with {:?}", action, params);
            }
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}
