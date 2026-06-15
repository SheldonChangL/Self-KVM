//! Screen layout, neighbour adjacency, and edge-crossing detection.
//!
//! The server holds one [`ScreenLayout`] describing every participating screen
//! (its own plus each client's) and how they border one another. The cursor
//! lives at a position within whichever screen is *active*; when it runs off an
//! edge that has a neighbour, [`ScreenLayout::detect_crossing`] reports the
//! target screen and the proportionally-mapped entry point on it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The edge a cursor arrives on when it crosses *this* edge into a
    /// neighbour (crossing the right edge means entering the neighbour's left).
    pub fn opposite(self) -> Edge {
        match self {
            Edge::Left => Edge::Right,
            Edge::Right => Edge::Left,
            Edge::Top => Edge::Bottom,
            Edge::Bottom => Edge::Top,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSize {
    pub w: i32,
    pub h: i32,
}

impl ScreenSize {
    pub fn new(w: i32, h: i32) -> Self {
        Self { w, h }
    }
}

/// Which screen lies on each side of a given screen (by name).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Neighbors {
    pub left: Option<String>,
    pub right: Option<String>,
    pub top: Option<String>,
    pub bottom: Option<String>,
}

impl Neighbors {
    pub fn get(&self, edge: Edge) -> Option<&String> {
        match edge {
            Edge::Left => self.left.as_ref(),
            Edge::Right => self.right.as_ref(),
            Edge::Top => self.top.as_ref(),
            Edge::Bottom => self.bottom.as_ref(),
        }
    }

    pub fn set(&mut self, edge: Edge, screen: Option<String>) {
        match edge {
            Edge::Left => self.left = screen,
            Edge::Right => self.right = screen,
            Edge::Top => self.top = screen,
            Edge::Bottom => self.bottom = screen,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenNode {
    pub size: ScreenSize,
    #[serde(default)]
    pub neighbors: Neighbors,
}

/// The result of a cursor crossing off one screen onto a neighbour.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crossing {
    /// Name of the screen the cursor enters.
    pub to: String,
    /// Edge of the *source* screen that was crossed.
    pub from_edge: Edge,
    /// Entry coordinates within the target screen.
    pub entry_x: i32,
    pub entry_y: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenLayout {
    pub nodes: HashMap<String, ScreenNode>,
}

impl ScreenLayout {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_screen(&mut self, name: impl Into<String>, size: ScreenSize) -> &mut Self {
        self.nodes.insert(
            name.into(),
            ScreenNode {
                size,
                neighbors: Neighbors::default(),
            },
        );
        self
    }

    /// Link `a`'s `edge` to `b`, and reciprocally `b`'s opposite edge to `a`.
    /// This keeps the adjacency graph symmetric so the cursor can always travel
    /// back the way it came.
    pub fn link(&mut self, a: &str, edge: Edge, b: &str) -> &mut Self {
        if let Some(na) = self.nodes.get_mut(a) {
            na.neighbors.set(edge, Some(b.to_string()));
        }
        if let Some(nb) = self.nodes.get_mut(b) {
            nb.neighbors.set(edge.opposite(), Some(a.to_string()));
        }
        self
    }

    pub fn size_of(&self, screen: &str) -> Option<ScreenSize> {
        self.nodes.get(screen).map(|n| n.size)
    }

    pub fn contains(&self, screen: &str) -> bool {
        self.nodes.contains_key(screen)
    }

    /// If `(x, y)` (in `active`'s coordinate space) has run off an edge with a
    /// neighbour, return where the cursor lands on that neighbour. Horizontal
    /// crossings take priority over vertical ones (matches the reference feel
    /// at corners). Returns `None` if the cursor is still on-screen or ran off
    /// an edge with no neighbour.
    pub fn detect_crossing(&self, active: &str, x: i32, y: i32) -> Option<Crossing> {
        let node = self.nodes.get(active)?;
        let (w, h) = (node.size.w, node.size.h);

        let edge = if x < 0 {
            Edge::Left
        } else if x >= w {
            Edge::Right
        } else if y < 0 {
            Edge::Top
        } else if y >= h {
            Edge::Bottom
        } else {
            return None; // still inside
        };

        let to = node.neighbors.get(edge)?.clone();
        let target = self.nodes.get(&to)?;
        let (tw, th) = (target.size.w, target.size.h);

        let (entry_x, entry_y) = match edge {
            // Crossing left/right keeps the vertical position (scaled); the
            // cursor lands on the neighbour's opposite vertical edge.
            Edge::Left => (tw - 1, scale(clamp(y, 0, h - 1), h, th)),
            Edge::Right => (0, scale(clamp(y, 0, h - 1), h, th)),
            // Crossing top/bottom keeps the horizontal position (scaled).
            Edge::Top => (scale(clamp(x, 0, w - 1), w, tw), th - 1),
            Edge::Bottom => (scale(clamp(x, 0, w - 1), w, tw), 0),
        };

        Some(Crossing {
            to,
            from_edge: edge,
            entry_x,
            entry_y,
        })
    }

    /// Clamp a position to a screen's bounds (used when the cursor pushes
    /// against an edge that has no neighbour).
    pub fn clamp_to(&self, screen: &str, x: i32, y: i32) -> (i32, i32) {
        match self.nodes.get(screen) {
            Some(n) => (clamp(x, 0, n.size.w - 1), clamp(y, 0, n.size.h - 1)),
            None => (x, y),
        }
    }
}

fn clamp(v: i32, lo: i32, hi: i32) -> i32 {
    v.max(lo).min(hi.max(lo))
}

/// Map `v` from a `from`-wide axis onto a `to`-wide axis, proportionally.
fn scale(v: i32, from: i32, to: i32) -> i32 {
    if from <= 1 || to <= 0 {
        return 0;
    }
    let mapped = (v as i64 * to as i64) / from as i64;
    clamp(mapped as i32, 0, to - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_screens() -> ScreenLayout {
        let mut l = ScreenLayout::new();
        l.add_screen("srv", ScreenSize::new(1920, 1080));
        l.add_screen("lap", ScreenSize::new(1280, 800));
        l.link("srv", Edge::Right, "lap"); // srv.right == lap, lap.left == srv
        l
    }

    #[test]
    fn linking_is_symmetric() {
        let l = two_screens();
        assert_eq!(l.nodes["srv"].neighbors.right.as_deref(), Some("lap"));
        assert_eq!(l.nodes["lap"].neighbors.left.as_deref(), Some("srv"));
    }

    #[test]
    fn no_crossing_when_inside() {
        let l = two_screens();
        assert_eq!(l.detect_crossing("srv", 960, 540), None);
    }

    #[test]
    fn right_edge_crosses_into_left_of_neighbour() {
        let l = two_screens();
        let c = l.detect_crossing("srv", 1920, 540).unwrap();
        assert_eq!(c.to, "lap");
        assert_eq!(c.from_edge, Edge::Right);
        assert_eq!(c.entry_x, 0); // lands on lap's left edge
        assert_eq!(c.entry_y, 540 * 800 / 1080); // proportional => 400
    }

    #[test]
    fn crossing_back_returns_to_origin_edge() {
        let l = two_screens();
        // On lap, moving off the left edge returns to srv's right edge.
        let c = l.detect_crossing("lap", -1, 400).unwrap();
        assert_eq!(c.to, "srv");
        assert_eq!(c.entry_x, 1919); // srv's right edge
        assert_eq!(c.entry_y, 400 * 1080 / 800); // 540
    }

    #[test]
    fn edge_without_neighbour_yields_none() {
        let l = two_screens();
        // srv has no left neighbour.
        assert_eq!(l.detect_crossing("srv", -5, 500), None);
    }

    #[test]
    fn vertical_layout_maps_horizontal_position() {
        let mut l = ScreenLayout::new();
        l.add_screen("top", ScreenSize::new(1000, 1000));
        l.add_screen("bot", ScreenSize::new(500, 500));
        l.link("top", Edge::Bottom, "bot");
        let c = l.detect_crossing("top", 800, 1000).unwrap();
        assert_eq!(c.to, "bot");
        assert_eq!(c.entry_y, 0); // top of bot
        assert_eq!(c.entry_x, 800 * 500 / 1000); // 400
    }
}
