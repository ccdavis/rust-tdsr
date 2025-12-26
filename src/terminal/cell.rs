//! Terminal cell - represents a single character position on screen
//!
//! Screen readers need to track not just what's displayed, but also maintain
//! a stable representation for review cursor navigation and reading.

/// A single character cell in the terminal
///
/// Each cell represents one character position that the screen reader can navigate to.
/// We store the visible character data so the review cursor can read back any part
/// of the screen even after new content has been drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character displayed at this position.
    /// Empty string for wide character continuation cells.
    pub data: char,

    /// Whether this cell is part of a wide character (CJK, emoji, etc.)
    /// Important for review cursor navigation - we need to skip continuation cells
    pub is_wide_continuation: bool,
}

impl Cell {
    /// Create a new empty cell
    pub fn new() -> Self {
        Self {
            data: ' ',
            is_wide_continuation: false,
        }
    }

    /// Create a cell with specific character
    pub fn with_char(c: char) -> Self {
        Self {
            data: c,
            is_wide_continuation: false,
        }
    }

    /// Create a wide character continuation cell
    /// These are skipped during character-by-character navigation
    pub fn wide_continuation() -> Self {
        Self {
            data: '\0',
            is_wide_continuation: true,
        }
    }

    /// Reset cell to blank space
    pub fn clear(&mut self) {
        self.data = ' ';
        self.is_wide_continuation = false;
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_cell() {
        let cell = Cell::new();
        assert_eq!(cell.data, ' ');
        assert!(!cell.is_wide_continuation);
    }

    #[test]
    fn test_with_char() {
        let cell = Cell::with_char('A');
        assert_eq!(cell.data, 'A');
        assert!(!cell.is_wide_continuation);
    }

    #[test]
    fn test_wide_continuation() {
        let cell = Cell::wide_continuation();
        assert_eq!(cell.data, '\0');
        assert!(cell.is_wide_continuation);
    }

    #[test]
    fn test_clear() {
        let mut cell = Cell {
            data: 'X',
            is_wide_continuation: true,
        };
        cell.clear();
        assert_eq!(cell.data, ' ');
        assert!(!cell.is_wide_continuation);
    }

    #[test]
    fn test_default() {
        let cell = Cell::default();
        assert_eq!(cell, Cell::new());
    }
}
