//! Pane management: split tree, tabs, and layout
//!
//! Provides a Ghostty-style binary tree for pane splitting,
//! tab management, and layout calculation.

pub mod layout;
pub mod split_tree;
pub mod tab;

use crate::terminal::Terminal;

/// Unique identifier for a pane
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u16);

/// Pixel rectangle for a pane's viewport
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PaneRect {
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Check if a pixel coordinate is within this rect
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }

    /// Grid size (cols, rows) that fits this rect at the given cell size.
    ///
    /// This is the single definition of "how big is a pane's terminal": the
    /// resize path and the new-tab path must agree, otherwise a terminal whose
    /// grid is taller than its rect draws its last row over whatever sits below
    /// (e.g. the tab bar) while the app inside believes it has that extra row.
    pub fn grid_size(&self, cell_w: f32, cell_h: f32) -> (usize, usize) {
        let cols = ((self.width / cell_w).floor() as usize).max(1);
        let rows = ((self.height / cell_h).floor() as usize).max(1);
        (cols, rows)
    }
}

/// Split direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Horizontal, // left | right
    Vertical,   // top / bottom
}

/// Navigation direction for moving between panes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDirection {
    Left,
    Right,
    Up,
    Down,
}

/// A single pane holding a terminal instance
pub struct Pane {
    #[allow(dead_code)]
    pub id: PaneId,
    pub terminal: Terminal,
    pub rect: PaneRect,
}

impl Pane {
    pub fn new(id: PaneId, terminal: Terminal, rect: PaneRect) -> Self {
        Self { id, terminal, rect }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_size_never_returns_zero() {
        assert_eq!(
            PaneRect::new(8.0, 8.0, 0.0, 0.0).grid_size(11.0, 30.0),
            (1, 1)
        );
    }

    /// The tab bar eats one row plus padding; the new-tab path and the resize
    /// path must derive the same grid size from the same rect.
    #[test]
    fn grid_size_matches_the_tab_bar_shrink() {
        let cell_w = 11.0;
        let cell_h = 30.0;
        let full = PaneRect::new(8.0, 8.0, 1920.0 - 16.0, 1080.0 - 16.0);
        assert_eq!(full.grid_size(cell_w, cell_h), (173, 35));

        let with_bar = PaneRect::new(8.0, 8.0, 1920.0 - 16.0, 1080.0 - 16.0 - (cell_h + 6.0));
        assert_eq!(with_bar.grid_size(cell_w, cell_h), (173, 34));
    }
}
