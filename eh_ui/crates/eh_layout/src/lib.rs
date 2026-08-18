//! eh_layout — responsive layout over taffy.
//!
//! Taffy gives real CSS flex/grid layout, which is what the "web-like
//! breakpoints" wish actually needs.  This crate is a thin, opinionated
//! wrapper: a [`Layout`] owns a `TaffyTree`, and the shell computes a
//! [`Breakpoint`] from the screen width once per frame, then re-lays-out the
//! tree against it.  Containers attached to a breakpoint branch are shown /
//! hidden by toggling their size to zero — the taffy equivalent of CSS
//! media-query `display: none`.

use taffy::{AvailableSpace, Dimension, FlexDirection, Size, TaffyTree, NodeId};

pub use taffy::Style;

pub use taffy;

/// A named size class resolved from the screen width — the sole place
/// "am I narrow / standard / wide?" is answered.  Mirrors the C app's
/// `eh_view_cols()` thresholds but as data, not scattered `if`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Breakpoint {
    /// ≤758 px (6-inch panels): top bar spans the source button, 2 columns.
    Narrow,
    /// Standard 1024–1264 px panels: 3 columns.
    Std,
    /// ≥1380 px (1404-class) wide panels: 4 columns.
    Wide,
}

impl Breakpoint {
    pub fn from_width(w: u32) -> Self {
        if w <= 758 {
            Breakpoint::Narrow
        } else if w < 1380 {
            Breakpoint::Std
        } else {
            Breakpoint::Wide
        }
    }

    /// Match an exact breakpoint (for per-branch containers).
    pub fn is(self, b: Breakpoint) -> bool {
        self == b
    }
}

/// Convenience lengths.
pub fn px(v: f32) -> Dimension {
    Dimension::length(v)
}
pub fn pct(p: f32) -> Dimension {
    Dimension::percent(p)
}
pub fn auto() -> Dimension {
    Dimension::auto()
}

/// The layout engine for one screen.  Owns the taffy tree and the mapping
/// between widget identifiers and taffy node ids.
pub struct Layout {
    tree: TaffyTree<()>,
    /// Root node used by every layout pass.
    root: NodeId,
}

impl Default for Layout {
    fn default() -> Self {
        Self::new()
    }
}

impl Layout {
    pub fn new() -> Self {
        let mut tree = TaffyTree::new();
        // The root MUST explicitly fill the available space: a flexbox root
        // with auto size resolves children's percent widths against a
        // circular (==0) box.  percent(1.0) makes the root the viewport.
        let root_style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Style::default()
        };
        let root = tree.new_leaf(root_style).expect("root node allocation");
        Self { tree, root }
    }

    pub fn tree_mut(&mut self) -> &mut TaffyTree<()> {
        &mut self.tree
    }

    /// Create a leaf node and return its id.
    pub fn leaf(&mut self, style: impl Into<Style>) -> NodeId {
        self.tree.new_leaf(style.into()).expect("leaf allocation")
    }

    /// Create a node with children.
    pub fn node(&mut self, style: impl Into<Style>, children: &[NodeId]) -> NodeId {
        self.tree
            .new_with_children(style.into(), children)
            .expect("node allocation")
    }

    /// Set the root's children (the top-level screen content).
    pub fn set_root_children(&mut self, children: &[NodeId]) {
        self.tree.set_children(self.root, children).expect("set root children");
    }

    /// Make the root a wrapping flex row (grid of children) — the common
    /// case for a cover shelf.
    pub fn root_flex_wrap(&mut self) {
        let mut style = self.root_style();
        style.flex_wrap = taffy::style::FlexWrap::Wrap;
        self.tree.set_style(self.root, style).expect("root style");
    }

    /// Make the root a vertical flex column (top-bar + content stacking).
    pub fn root_flex_column(&mut self) {
        let mut style = self.root_style();
        style.flex_direction = FlexDirection::Column;
        self.tree.set_style(self.root, style).expect("root style");
    }

    fn root_style(&self) -> Style {
        self.tree.style(self.root).expect("root style").clone()
    }

    /// Compute layout of the whole tree against `screen`, snapping to the
    /// pixel grid (rounding is on by default in taffy).
    pub fn compute(&mut self, width: f32, height: f32) {
        let avail = Size {
            width: AvailableSpace::Definite(width),
            height: AvailableSpace::Definite(height),
        };
        self.tree.compute_layout(self.root, avail).expect("layout compute");
    }

    /// Output rect for a node (taffy layout: position + size).
    pub fn rect(&self, id: NodeId) -> eh_hal::Rect {
        let l = self.tree.layout(id).expect("node layout");
        eh_hal::Rect::from_xy(l.location.x as i32, l.location.y as i32, l.size.width as i32, l.size.height as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_classes() {
        assert_eq!(Breakpoint::from_width(758), Breakpoint::Narrow);
        assert_eq!(Breakpoint::from_width(1024), Breakpoint::Std);
        assert_eq!(Breakpoint::from_width(1264), Breakpoint::Std);
        assert_eq!(Breakpoint::from_width(1380), Breakpoint::Wide);
        assert_eq!(Breakpoint::from_width(1404), Breakpoint::Wide);
    }

    #[test]
    fn wrap_driven_by_breakpoint_columns() {
        // The shelf grid: a wrapping flex row of percent-width covers.  On a
        // 600px (narrow) screen 50% covers => column 0 on row 0 at x=0; the
        // 4th cover wraps to row 1.  Proves the breakpoint drives layout.
        let mut lay = Layout::new();
        lay.root_flex_wrap();
        let cols = match Breakpoint::from_width(600) {
            Breakpoint::Narrow => 2,
            Breakpoint::Std => 3,
            Breakpoint::Wide => 4,
        };
        let children: Vec<NodeId> = (0..4)
            .map(|_| lay.leaf(Style {
                size: Size { width: Dimension::percent(1.0 / cols as f32), height: Dimension::length(100.0) },
                ..Style::default()
            }))
            .collect();
        lay.set_root_children(&children);
        lay.compute(600.0, 400.0);
        let r0 = lay.rect(children[0]);
        let r2 = lay.rect(children[2]);
        let r3 = lay.rect(children[3]);
        assert_eq!(r0.w, (600.0 / cols as f32) as u32, "column width from breakpoint");
        // 2 columns => 3rd & 4th tiles wrap onto a second row.
        assert_eq!(r2.x, 0, "3rd tile starts row 2");
        assert!(r3.y > 0, "4th tile on a later row than tile 0");
        assert_eq!(r2.y, r3.y, "3rd and 4th tiles share a row");
    }
}