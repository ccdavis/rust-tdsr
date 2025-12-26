//! Terminal emulator using vte
//!
//! The screen reader needs to understand terminal output to maintain an accurate
//! screen buffer for review cursor navigation.

use super::{Screen, performer::ScreenPerformer};
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
    pub fn process_with_speech(
        &mut self,
        bytes: &[u8],
        speech_buffer: &mut SpeechBuffer,
        last_drawn: &mut (u16, u16),
    ) -> Result<()> {
        trace!("Processing {} bytes from PTY", bytes.len());

        // Advance the parser byte by byte
        // The performer adds characters to speech buffer as they're drawn
        for &byte in bytes {
            let mut performer = ScreenPerformer {
                screen: &mut self.screen,
                speech_buffer,
                last_drawn,
            };
            self.parser.advance(&mut performer, byte);
        }

        Ok(())
    }

    /// Process bytes from PTY (without speech)
    ///
    /// Used when speech is not needed (e.g., during quiet mode)
    pub fn process(&mut self, bytes: &[u8]) -> Result<()> {
        let mut dummy_buffer = SpeechBuffer::new();
        let mut dummy_pos = (0, 0);
        self.process_with_speech(bytes, &mut dummy_buffer, &mut dummy_pos)
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
