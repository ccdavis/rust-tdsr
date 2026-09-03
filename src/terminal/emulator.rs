//! Terminal emulator using vte
//!
//! The screen reader needs to understand terminal output to maintain an accurate
//! screen buffer for review cursor navigation.

use super::{performer::ScreenPerformer, Screen};
use crate::speech::SpeechBuffer;
use crate::Result;
use log::{debug, trace};
use vte::Parser;

/// Terminal emulator that processes ANSI escape sequences into a screen buffer
///
/// As terminal output arrives, the vte parser calls our Perform implementation
/// to update the screen buffer. The screen reader reads from this buffer to
/// provide speech feedback.
pub struct Emulator {
    /// The screen buffer that the screen reader navigates
    pub screen: Screen,

    /// VTE parser for processing ANSI escape sequences
    parser: Parser,
}

impl Emulator {
    /// Create a new terminal emulator
    pub fn new(cols: u16, rows: u16) -> Self {
        debug!("Creating emulator with {}x{} dimensions", cols, rows);
        Self {
            screen: Screen::new(cols, rows),
            parser: Parser::new(),
        }
    }

    /// Process bytes from PTY with speech buffer
    ///
    /// Feeds terminal output through vte parser, which calls our Perform
    /// implementation to update the screen buffer and speech buffer.
    ///
    /// If `line_pause` is true, the speech buffer will be segmented at line
    /// breaks so each line can be spoken with a pause between them.
    ///
    /// `echo_key` is the character the user just typed (if any); when the
    /// first character drawn matches it, that character is kept out of the
    /// speech buffer and returned so the caller can key-echo it.
    pub fn process_with_speech(
        &mut self,
        bytes: &[u8],
        speech_buffer: &mut SpeechBuffer,
        last_drawn: &mut (u16, u16),
        line_pause: bool,
        echo_key: &mut Option<char>,
    ) -> Result<Option<char>> {
        trace!("Processing {} bytes from PTY", bytes.len());

        // Advance the parser byte by byte
        // The performer adds characters to speech buffer as they're drawn
        let mut performer = ScreenPerformer {
            screen: &mut self.screen,
            speech_buffer,
            last_drawn,
            line_pause,
            echo_key,
            echoed: None,
        };
        for &byte in bytes {
            self.parser.advance(&mut performer, byte);
        }

        Ok(performer.echoed)
    }

    /// Process bytes from PTY (without speech)
    ///
    /// Used when speech is not needed (e.g., during quiet mode)
    pub fn process(&mut self, bytes: &[u8]) -> Result<()> {
        let mut dummy_buffer = SpeechBuffer::new();
        let mut dummy_pos = (0, 0);
        self.process_with_speech(bytes, &mut dummy_buffer, &mut dummy_pos, false, &mut None)
            .map(|_| ())
    }

    /// Resize the emulator
    pub fn resize(&mut self, cols: u16, rows: u16) {
        debug!("Resizing emulator to {}x{}", cols, rows);
        self.screen.resize(cols, rows);
    }

    /// Get cursor position for screen reader cursor tracking
    pub fn cursor(&self) -> (u16, u16) {
        self.screen.cursor
    }

    /// Get the screen buffer for review cursor access
    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    /// Get mutable screen buffer
    pub fn screen_mut(&mut self) -> &mut Screen {
        &mut self.screen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Alternate Screen Mode Tests ==========

    #[test]
    fn test_alternate_screen_1049_save_and_restore() {
        let mut emu = Emulator::new(10, 5);

        // Write content to main screen
        emu.process(b"Hi").unwrap();
        emu.screen.cursor = (3, 2);

        // CSI ?1049h - Enter alternate screen
        emu.process(b"\x1b[?1049h").unwrap();

        // Screen should be cleared, cursor at home
        assert_eq!(emu.screen.cursor, (0, 0));
        assert_eq!(emu.screen.get_line_trimmed(0), "");

        // Write content on alternate screen
        emu.process(b"ALT").unwrap();
        assert_eq!(emu.screen.get_line_trimmed(0), "ALT");

        // CSI ?1049l - Leave alternate screen
        emu.process(b"\x1b[?1049l").unwrap();

        // Original content and cursor should be restored
        assert_eq!(emu.screen.get_char(0, 0), Some('H'));
        assert_eq!(emu.screen.get_char(1, 0), Some('i'));
        assert_eq!(emu.screen.cursor, (3, 2));
    }

    #[test]
    fn test_alternate_screen_47_save_and_restore() {
        let mut emu = Emulator::new(10, 5);

        // Write content to main screen
        emu.process(b"AB").unwrap();

        // CSI ?47h - Enter alternate screen
        emu.process(b"\x1b[?47h").unwrap();

        // Screen should be cleared
        assert_eq!(emu.screen.get_line_trimmed(0), "");

        // CSI ?47l - Leave alternate screen
        emu.process(b"\x1b[?47l").unwrap();

        // Original content should be restored
        assert_eq!(emu.screen.get_char(0, 0), Some('A'));
        assert_eq!(emu.screen.get_char(1, 0), Some('B'));
    }

    #[test]
    fn test_alternate_screen_nested_content_isolation() {
        let mut emu = Emulator::new(10, 5);

        // Write on main screen
        emu.process(b"MAIN").unwrap();

        // Enter alt screen
        emu.process(b"\x1b[?1049h").unwrap();
        emu.process(b"ALT_CONTENT").unwrap();

        // Verify alt screen has the alt content
        assert_eq!(emu.screen.get_line_trimmed(0), "ALT_CONTEN");

        // Leave alt screen
        emu.process(b"\x1b[?1049l").unwrap();

        // Main screen should have only "MAIN", not "ALT_CONTENT"
        assert_eq!(emu.screen.get_line_trimmed(0), "MAIN");
    }

    // ========== Scroll Region + LF/Autowrap Tests ==========

    #[test]
    fn test_lf_respects_scroll_region() {
        let mut emu = Emulator::new(10, 5);

        // Fill rows with letters
        for y in 0..5 {
            emu.screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Set scroll region to rows 2-4 (1-indexed)
        emu.process(b"\x1b[2;4r").unwrap();

        // Move cursor to bottom of scroll region (row 3, 0-indexed)
        emu.screen.cursor = (0, 3);

        // LF at bottom of scroll region should scroll within region
        emu.process(b"\n").unwrap();

        // Row 0 should be unchanged (outside region)
        assert_eq!(emu.screen.get_char(0, 0), Some('A'));
        // Row 1 (top of region) should now have what was row 2
        assert_eq!(emu.screen.get_char(0, 1), Some('C'));
        // Row 2 should have what was row 3
        assert_eq!(emu.screen.get_char(0, 2), Some('D'));
        // Row 3 (bottom of region) should be blank
        assert_eq!(emu.screen.get_line_trimmed(3), "");
        // Row 4 should be unchanged (outside region)
        assert_eq!(emu.screen.get_char(0, 4), Some('E'));
    }

    #[test]
    fn test_autowrap_respects_scroll_region() {
        let mut emu = Emulator::new(5, 5);

        // Set scroll region to rows 2-4 (1-indexed, = rows 1-3 0-indexed)
        emu.process(b"\x1b[2;4r").unwrap();

        // Fill rows
        for y in 0..5 {
            emu.screen.buffer[y][0].data = (b'A' + y as u8) as char;
        }

        // Move cursor to end of bottom row of scroll region
        emu.screen.cursor = (4, 3);

        // Print a character - should wrap and scroll within region
        emu.process(b"XY").unwrap();

        // Row 0 should be unchanged (outside region)
        assert_eq!(emu.screen.get_char(0, 0), Some('A'));
        // Row 4 should be unchanged (outside region)
        assert_eq!(emu.screen.get_char(0, 4), Some('E'));
    }
}
