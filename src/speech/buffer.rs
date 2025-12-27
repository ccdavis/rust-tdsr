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
}

impl SpeechBuffer {
    /// Create a new empty speech buffer
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            pending_lines: Vec::new(),
        }
    }

    /// Write text to the buffer
    pub fn write(&mut self, text: &str) {
        self.buffer.push_str(text);
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
        self.buffer.pop()
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
