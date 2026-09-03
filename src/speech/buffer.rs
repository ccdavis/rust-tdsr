//! Speech buffer for accumulating text before speaking

use log::debug;

/// Buffer that accumulates text to be spoken
///
/// Supports two modes:
/// - Normal mode: accumulates all text until flushed
/// - Line mode (line_pause): accumulates text and returns lines when completed
pub struct SpeechBuffer {
    /// Current line being accumulated
    buffer: String,

    /// Lines ready to be spoken (when line_pause is enabled)
    pending_lines: Vec<String>,

    /// Byte offset in `buffer` where text for the current screen row began.
    /// Lets a carriage-return rewrite of the same row (progress bars,
    /// spinners) replace what was queued for that row instead of appending
    /// another copy of it.
    row_start: usize,

    /// Set by a carriage return: the next text drawn on this row replaces
    /// the row's queued text. Cleared when the row changes.
    overwrite_pending: bool,
}

impl SpeechBuffer {
    /// Create a new empty speech buffer
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            pending_lines: Vec::new(),
            row_start: 0,
            overwrite_pending: false,
        }
    }

    /// Write text to the buffer
    pub fn write(&mut self, text: &str) {
        self.buffer.push_str(text);
    }

    /// Append a single character (no allocation; the hot path for printed text)
    pub fn push(&mut self, c: char) {
        self.buffer.push(c);
    }

    /// Text drawn from now on belongs to a new screen row.
    pub fn begin_row(&mut self) {
        self.row_start = self.buffer.len();
        self.overwrite_pending = false;
    }

    /// The cursor returned to the start of the current row (carriage return):
    /// text drawn next will overwrite the row on screen, so it should replace
    /// the row's queued speech too. Takes effect on the next `apply_overwrite`.
    pub fn mark_overwrite(&mut self) {
        self.overwrite_pending = true;
    }

    /// If an overwrite is pending, drop the current row's queued text.
    /// Returns true when something was discarded.
    pub fn apply_overwrite(&mut self) -> bool {
        if !self.overwrite_pending {
            return false;
        }
        self.overwrite_pending = false;
        if self.row_start < self.buffer.len() {
            self.buffer.truncate(self.row_start);
            return true;
        }
        false
    }

    /// Mark a line break (for line_pause mode)
    ///
    /// When line_pause is enabled, this moves the current buffer
    /// to pending_lines and starts a new line.
    pub fn line_break(&mut self) {
        if !self.buffer.is_empty() {
            let line = std::mem::take(&mut self.buffer);
            debug!("Line break: queuing {} chars for speech", line.len());
            self.pending_lines.push(line);
        }
        self.begin_row();
    }

    /// Check if there are pending lines to speak
    pub fn has_pending_lines(&self) -> bool {
        !self.pending_lines.is_empty()
    }

    /// Get and clear pending lines for speaking
    pub fn drain_lines(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_lines)
    }

    /// Get the current buffer contents
    pub fn contents(&self) -> &str {
        &self.buffer
    }

    /// Clear the buffer and return its contents
    pub fn flush(&mut self) -> String {
        debug!("Flushing speech buffer: {} chars", self.buffer.len());
        self.row_start = 0;
        std::mem::take(&mut self.buffer)
    }

    /// Check if buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get buffer length
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Remove the last character from the buffer
    ///
    /// Used for backspace handling - O(1) operation
    pub fn pop(&mut self) -> Option<char> {
        let c = self.buffer.pop();
        self.row_start = self.row_start.min(self.buffer.len());
        c
    }
}

impl Default for SpeechBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer() {
        let buffer = SpeechBuffer::new();
        assert!(buffer.is_empty());
        assert_eq!(buffer.contents(), "");
    }

    #[test]
    fn test_write() {
        let mut buffer = SpeechBuffer::new();
        buffer.write("Hello");
        buffer.write(" ");
        buffer.write("World");

        assert!(!buffer.is_empty());
        assert_eq!(buffer.contents(), "Hello World");
    }

    #[test]
    fn test_flush() {
        let mut buffer = SpeechBuffer::new();
        buffer.write("Test");

        let contents = buffer.flush();
        assert_eq!(contents, "Test");
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_is_empty() {
        let mut buffer = SpeechBuffer::new();
        assert!(buffer.is_empty());

        buffer.write("x");
        assert!(!buffer.is_empty());

        buffer.flush();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_overwrite_replaces_current_row_only() {
        let mut buffer = SpeechBuffer::new();
        buffer.write("first row");
        buffer.line_break(); // starts a new row
        buffer.write("10%");
        buffer.mark_overwrite();
        assert!(buffer.apply_overwrite());
        buffer.write("20%");
        assert_eq!(buffer.contents(), "20%");
        assert_eq!(buffer.drain_lines(), vec!["first row".to_string()]);

        // CR followed by a newline (the normal CRLF case) must not discard
        // the row that was just completed.
        let mut buffer = SpeechBuffer::new();
        buffer.write("done");
        buffer.mark_overwrite();
        buffer.begin_row(); // linefeed
        assert!(!buffer.apply_overwrite());
        buffer.write("next");
        assert_eq!(buffer.contents(), "donenext");
    }

    #[test]
    fn test_pop() {
        let mut buffer = SpeechBuffer::new();
        buffer.write("Hello");

        assert_eq!(buffer.pop(), Some('o'));
        assert_eq!(buffer.contents(), "Hell");

        assert_eq!(buffer.pop(), Some('l'));
        assert_eq!(buffer.pop(), Some('l'));
        assert_eq!(buffer.pop(), Some('e'));
        assert_eq!(buffer.pop(), Some('H'));
        assert_eq!(buffer.pop(), None);
        assert!(buffer.is_empty());
    }
}
