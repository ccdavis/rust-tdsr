//! Review cursor for navigating terminal content
//!
//! The review cursor tracks the user's position for reading screen content.
//! It's independent of the terminal cursor and allows reading any part of the screen.
//! Navigation and speech methods are implemented in `state/mod.rs`.

/// Review cursor for navigating terminal content
pub struct ReviewCursor {
    /// Current position (x, y)
    pub pos: (u16, u16),

    /// Terminal dimensions
    pub bounds: (u16, u16),
}

impl ReviewCursor {
    /// Create a new review cursor
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            pos: (0, 0),
            bounds: (cols, rows),
        }
    }

    /// Update bounds when terminal resizes
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.bounds = (cols, rows);
        // Clamp position to new bounds
        self.pos.0 = self.pos.0.min(cols.saturating_sub(1));
        self.pos.1 = self.pos.1.min(rows.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_review_cursor() {
        let cursor = ReviewCursor::new(80, 24);
        assert_eq!(cursor.pos, (0, 0));
        assert_eq!(cursor.bounds, (80, 24));
    }

    #[test]
    fn test_resize() {
        let mut cursor = ReviewCursor::new(80, 24);
        cursor.pos = (79, 23);

        cursor.resize(40, 12);
        assert_eq!(cursor.bounds, (40, 12));
        // Position should be clamped to new size
        assert_eq!(cursor.pos, (39, 11));
    }

    #[test]
    fn test_resize_no_clamp() {
        let mut cursor = ReviewCursor::new(80, 24);
        cursor.pos = (10, 5);

        cursor.resize(100, 50);
        assert_eq!(cursor.bounds, (100, 50));
        // Position should remain unchanged
        assert_eq!(cursor.pos, (10, 5));
    }
}
