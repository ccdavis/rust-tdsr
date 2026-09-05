//! Terminal cell - represents a single character position on screen
//!
//! Screen readers need to track not just what's displayed, but also maintain
//! a stable representation for review cursor navigation and reading.

use super::Attrs;

/// A single character cell in the terminal
///
/// Each cell represents one character position that the screen reader can navigate to.
/// We store the visible character data so the review cursor can read back any part
/// of the screen even after new content has been drawn, and the rendition so
/// the TUI tracker can tell a highlighted item from its neighbours.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The character displayed at this position.
    /// `'\0'` for wide character continuation cells.
    pub data: char,

    /// Whether this cell is part of a wide character (CJK, emoji, etc.)
    /// Important for review cursor navigation - we need to skip continuation cells
    pub is_wide_continuation: bool,

    /// Colours and style the character was drawn with
    pub attrs: Attrs,
}

impl Cell {
    /// Create a new empty cell
    pub fn new() -> Self {
        Self::blank(Attrs::default())
    }

    /// A blank cell carrying the given rendition (the background survives
    /// an erase, see `Attrs::erase_attrs`)
    pub fn blank(attrs: Attrs) -> Self {
        Self {
            data: ' ',
            is_wide_continuation: false,
            attrs,
        }
    }

    /// Create a cell with specific character
    pub fn with_char(c: char) -> Self {
        Self {
            data: c,
            is_wide_continuation: false,
            attrs: Attrs::default(),
        }
    }

    /// Create a wide character continuation cell
    /// These are skipped during character-by-character navigation
    pub fn wide_continuation() -> Self {
        Self {
            data: '\0',
            is_wide_continuation: true,
            attrs: Attrs::default(),
        }
    }

    /// Reset cell to a blank space with default attributes
    pub fn clear(&mut self) {
        self.clear_with(Attrs::default());
    }

    /// Reset cell to a blank space with the given attributes
    pub fn clear_with(&mut self, attrs: Attrs) {
        self.data = ' ';
        self.is_wide_continuation = false;
        self.attrs = attrs;
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
    use crate::terminal::attrs::Color;

    #[test]
    fn test_new_cell() {
        let cell = Cell::new();
        assert_eq!(cell.data, ' ');
        assert!(!cell.is_wide_continuation);
        assert_eq!(cell.attrs, Attrs::default());
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
            attrs: Attrs {
                bold: true,
                ..Attrs::default()
            },
        };
        cell.clear();
        assert_eq!(cell.data, ' ');
        assert!(!cell.is_wide_continuation);
        assert_eq!(cell.attrs, Attrs::default());
    }

    #[test]
    fn test_clear_with_keeps_attrs() {
        let mut cell = Cell::with_char('X');
        let attrs = Attrs {
            bg: Color::Indexed(4),
            ..Attrs::default()
        };
        cell.clear_with(attrs);
        assert_eq!(cell.data, ' ');
        assert_eq!(cell.attrs, attrs);
        assert_eq!(Cell::blank(attrs), cell);
    }

    #[test]
    fn test_default() {
        let cell = Cell::default();
        assert_eq!(cell, Cell::new());
    }
}
