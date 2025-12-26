//! Speech buffer for accumulating text before speaking

use log::debug;

/// Buffer that accumulates text to be spoken
pub struct SpeechBuffer {
    buffer: String,
}

impl SpeechBuffer {
    /// Create a new empty speech buffer
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
        }
    }

    /// Write text to the buffer
    pub fn write(&mut self, text: &str) {
        self.buffer.push_str(text);
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
}
