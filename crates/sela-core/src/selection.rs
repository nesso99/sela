//! Grid selection rectangles for copy and keyboard navigation.
//!
//! Coordinates are inclusive. Callers normalize before iterating.

/// Zero-based cell coordinate in the result grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellPos {
    pub row: usize,
    pub col: usize,
}

impl CellPos {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

/// Inclusive rectangular selection (or a degenerate single cell / row / column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRect {
    pub start: CellPos,
    pub end: CellPos,
}

impl SelectionRect {
    pub fn cell(row: usize, col: usize) -> Self {
        let pos = CellPos::new(row, col);
        Self {
            start: pos,
            end: pos,
        }
    }

    pub fn row(row: usize, col_count: usize) -> Option<Self> {
        if col_count == 0 {
            return None;
        }
        Some(Self {
            start: CellPos::new(row, 0),
            end: CellPos::new(row, col_count - 1),
        })
    }

    pub fn column(col: usize, row_count: usize) -> Option<Self> {
        if row_count == 0 {
            return None;
        }
        Some(Self {
            start: CellPos::new(0, col),
            end: CellPos::new(row_count - 1, col),
        })
    }

    pub fn from_corners(a: CellPos, b: CellPos) -> Self {
        Self { start: a, end: b }
    }

    /// Inclusive bounds as `(min_row, max_row, min_col, max_col)`.
    pub fn normalized(&self) -> (usize, usize, usize, usize) {
        let min_row = self.start.row.min(self.end.row);
        let max_row = self.start.row.max(self.end.row);
        let min_col = self.start.col.min(self.end.col);
        let max_col = self.start.col.max(self.end.col);
        (min_row, max_row, min_col, max_col)
    }

    pub fn row_count(&self) -> usize {
        let (min_row, max_row, _, _) = self.normalized();
        max_row - min_row + 1
    }

    pub fn col_count(&self) -> usize {
        let (_, _, min_col, max_col) = self.normalized();
        max_col - min_col + 1
    }

    /// Clamp the rectangle into an available grid size.
    pub fn clamp(self, row_count: usize, col_count: usize) -> Option<Self> {
        if row_count == 0 || col_count == 0 {
            return None;
        }
        let (min_row, max_row, min_col, max_col) = self.normalized();
        if min_row >= row_count || min_col >= col_count {
            return None;
        }
        Some(Self {
            start: CellPos::new(min_row, min_col),
            end: CellPos::new(max_row.min(row_count - 1), max_col.min(col_count - 1)),
        })
    }

    /// Extend (or start) a range with Shift-style selection semantics.
    ///
    /// Without an anchor, `focus` becomes both corners. With an anchor, the
    /// rectangle spans `anchor`..`focus`.
    pub fn extend_with_focus(anchor: Option<CellPos>, focus: CellPos) -> (CellPos, Self) {
        let anchor = anchor.unwrap_or(focus);
        (anchor, Self::from_corners(anchor, focus))
    }
}

/// Step the active result-strip index by `delta` (−1 / +1). Returns `None` when
/// the strip has fewer than two results or the step would leave bounds (no wrap).
pub fn step_result_index(active: usize, count: usize, delta: isize) -> Option<usize> {
    if count < 2 {
        return None;
    }
    let next = active as isize + delta;
    if next < 0 || next as usize >= count {
        return None;
    }
    Some(next as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_inverted_corners() {
        let rect = SelectionRect::from_corners(CellPos::new(3, 5), CellPos::new(1, 2));
        assert_eq!(rect.normalized(), (1, 3, 2, 5));
        assert_eq!(rect.row_count(), 3);
        assert_eq!(rect.col_count(), 4);
    }

    #[test]
    fn row_and_column_helpers() {
        assert_eq!(SelectionRect::row(2, 4).unwrap().normalized(), (2, 2, 0, 3));
        assert_eq!(
            SelectionRect::column(1, 5).unwrap().normalized(),
            (0, 4, 1, 1)
        );
        assert!(SelectionRect::row(0, 0).is_none());
    }

    #[test]
    fn clamp_rejects_empty_grid() {
        let rect = SelectionRect::cell(0, 0);
        assert!(rect.clamp(0, 3).is_none());
        assert!(rect.clamp(3, 0).is_none());
    }

    #[test]
    fn clamp_shrinks_to_bounds() {
        let rect = SelectionRect::from_corners(CellPos::new(0, 0), CellPos::new(99, 99));
        let clamped = rect.clamp(10, 4).unwrap();
        assert_eq!(clamped.normalized(), (0, 9, 0, 3));
    }

    #[test]
    fn extend_with_focus_keeps_anchor() {
        let (anchor, rect) =
            SelectionRect::extend_with_focus(Some(CellPos::new(1, 1)), CellPos::new(3, 4));
        assert_eq!(anchor, CellPos::new(1, 1));
        assert_eq!(rect.normalized(), (1, 3, 1, 4));

        let (anchor, rect) = SelectionRect::extend_with_focus(None, CellPos::new(2, 0));
        assert_eq!(anchor, CellPos::new(2, 0));
        assert_eq!(rect, SelectionRect::cell(2, 0));
    }

    #[test]
    fn result_strip_steps_without_wrap() {
        assert_eq!(step_result_index(1, 3, -1), Some(0));
        assert_eq!(step_result_index(0, 3, -1), None);
        assert_eq!(step_result_index(1, 3, 1), Some(2));
        assert_eq!(step_result_index(2, 3, 1), None);
        assert_eq!(step_result_index(0, 1, 1), None);
    }
}
