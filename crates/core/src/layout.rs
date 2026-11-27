//! Canonical layout model (CLAUDE.md §7).
//!
//! A binary tree of splits and leaves, identical in spirit to tmux/i3. Every
//! function here is **pure** — no UI, no I/O — so the invariants can be unit
//! tested. The renderer reads this tree and lays the panes out elsewhere.

/// Opaque pane identifier (also what MCP clients address a pane by).
pub type PaneId = u64;

/// `Row` = side-by-side (a vertical divider); `Column` = stacked (horizontal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Leaf(PaneId),
    Split {
        orientation: Orientation,
        /// Size of the first child, 0..1.
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Layout {
    pub root: Option<Node>,
    pub focused: Option<PaneId>,
    /// Non-null when a pane is zoomed. Purely presentational.
    pub zoomed: Option<PaneId>,
}

/// Ratios are clamped so a pane can never collapse to zero (CLAUDE.md §7).
pub const MIN_RATIO: f32 = 0.05;
pub const MAX_RATIO: f32 = 0.95;

/// A normalized rectangle in 0..1 space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl NRect {
    pub const UNIT: NRect = NRect {
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
    fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// Path to a node: child indices (0 = first, 1 = second).
pub type NodePath = Vec<u8>;

pub fn clamp_ratio(r: f32) -> f32 {
    if r.is_nan() {
        0.5
    } else {
        r.clamp(MIN_RATIO, MAX_RATIO)
    }
}

impl Layout {
    /// A layout containing a single pane.
    pub fn single(id: PaneId) -> Self {
        Self {
            root: Some(Node::Leaf(id)),
            focused: Some(id),
            zoomed: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    /// All pane ids, left-to-right / top-to-bottom (in order).
    pub fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            collect_leaves(r, &mut out);
        }
        out
    }

    pub fn has_pane(&self, id: PaneId) -> bool {
        self.root
            .as_ref()
            .is_some_and(|r| path_to_leaf(r, id).is_some())
    }

    /// The normalized rectangle of every leaf.
    pub fn rects(&self) -> Vec<(PaneId, NRect)> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            compute_rects(r, NRect::UNIT, &mut out);
        }
        out
    }

    /// Every split with its path, orientation, ratio and rectangle — for dividers.
    pub fn splits(&self) -> Vec<SplitInfo> {
        let mut out = Vec::new();
        if let Some(r) = &self.root {
            collect_splits(r, NRect::UNIT, &mut Vec::new(), &mut out);
        }
        out
    }

    /// Split the focused pane. `Down` stacks the new pane below (a horizontal
    /// divider, `Column`); `Right` places it beside (`Row`). Focus follows to the
    /// new pane: you split in order to start working in it.
    pub fn split_focused(&self, dir: Direction, new_id: PaneId) -> Layout {
        let orientation = match dir {
            Direction::Down | Direction::Up => Orientation::Column,
            Direction::Right | Direction::Left => Orientation::Row,
        };
        let (Some(root), Some(focused)) = (&self.root, self.focused) else {
            return Layout::single(new_id);
        };
        let root = map_leaf(root, focused, &|l| Node::Split {
            orientation,
            ratio: 0.5,
            first: Box::new(l),
            second: Box::new(Node::Leaf(new_id)),
        });
        Layout {
            root: Some(root),
            focused: Some(new_id),
            zoomed: self.zoomed,
        }
    }

    /// The neighbouring pane in a direction, if any.
    pub fn neighbor(&self, dir: Direction) -> Option<PaneId> {
        let focused = self.focused?;
        let rects = self.rects();
        let cur = rects.iter().find(|(id, _)| *id == focused)?.1;
        let horizontal = matches!(dir, Direction::Left | Direction::Right);
        let (cx, cy) = cur.center();
        let mut best = None;
        let mut best_dist = f32::INFINITY;
        for (id, r) in &rects {
            if *id == focused {
                continue;
            }
            let overlap = if horizontal {
                overlaps(r.y, r.y + r.h, cur.y, cur.y + cur.h)
            } else {
                overlaps(r.x, r.x + r.w, cur.x, cur.x + cur.w)
            };
            if !overlap {
                continue;
            }
            let (rx, ry) = r.center();
            let (on_side, dist) = match dir {
                Direction::Left => (rx < cx, cx - rx),
                Direction::Right => (rx > cx, rx - cx),
                Direction::Up => (ry < cy, cy - ry),
                Direction::Down => (ry > cy, ry - cy),
            };
            if on_side && dist < best_dist {
                best_dist = dist;
                best = Some(*id);
            }
        }
        best
    }

    /// Move focus to the adjacent pane in a direction (no-op if none).
    pub fn focus_direction(&self, dir: Direction) -> Layout {
        match self.neighbor(dir) {
            Some(n) => Layout {
                focused: Some(n),
                ..self.clone()
            },
            None => self.clone(),
        }
    }

    fn cycle(&self, delta: isize) -> Layout {
        let ids = self.leaves();
        let Some(f) = self.focused else {
            return self.clone();
        };
        let Some(idx) = ids.iter().position(|&i| i == f) else {
            return self.clone();
        };
        let n = ids.len() as isize;
        let next = ((idx as isize + delta) % n + n) % n;
        Layout {
            focused: Some(ids[next as usize]),
            ..self.clone()
        }
    }

    pub fn focus_next(&self) -> Layout {
        self.cycle(1)
    }

    pub fn focus_prev(&self) -> Layout {
        self.cycle(-1)
    }

    /// Focus a specific pane, if it is in the tree.
    pub fn focus(&self, id: PaneId) -> Layout {
        if self.has_pane(id) {
            Layout {
                focused: Some(id),
                ..self.clone()
            }
        } else {
            self.clone()
        }
    }

    /// Swap the focused pane with its neighbour in a direction. Focus follows
    /// the pane that moved.
    pub fn move_pane(&self, dir: Direction) -> Layout {
        let (Some(root), Some(f)) = (&self.root, self.focused) else {
            return self.clone();
        };
        let Some(other) = self.neighbor(dir) else {
            return self.clone();
        };
        Layout {
            root: Some(swap(root, f, other)),
            ..self.clone()
        }
    }

    /// Close a pane by id, collapse its split and promote the sibling. If it held
    /// focus, focus moves to a leaf of the promoted sibling. Zoom is cleared if the
    /// zoomed pane was removed. Unchanged (and `None`) if `id` isn't in the tree.
    pub fn close_pane(&self, id: PaneId) -> (Layout, Option<PaneId>) {
        let Some(root) = &self.root else {
            return (self.clone(), None);
        };
        let Some(path) = path_to_leaf(root, id) else {
            return (self.clone(), None);
        };
        if path.is_empty() {
            return (Layout::default(), Some(id));
        }
        let new_root = remove_at_path(root, &path);
        let mut focused = self.focused;
        if self.focused == Some(id) {
            let parent = &path[..path.len() - 1];
            focused = node_at_path(new_root.as_ref(), parent).map(first_leaf);
        }
        (
            Layout {
                root: new_root,
                focused,
                zoomed: if self.zoomed == Some(id) {
                    None
                } else {
                    self.zoomed
                },
            },
            Some(id),
        )
    }

    pub fn close_focused(&self) -> (Layout, Option<PaneId>) {
        match self.focused {
            Some(f) => self.close_pane(f),
            None => (self.clone(), None),
        }
    }

    /// Toggle zoom on the focused pane. The tree shape is untouched.
    pub fn toggle_zoom(&self) -> Layout {
        let zoomed = if self.zoomed.is_some() {
            None
        } else {
            self.focused
        };
        Layout {
            zoomed,
            ..self.clone()
        }
    }

    /// Resize the focused pane by nudging the nearest ancestor split of the
    /// matching orientation: `Right`/`Down` grow the first child, `Left`/`Up`
    /// shrink it.
    pub fn resize_focused(&self, dir: Direction, step: f32) -> Layout {
        let (Some(root), Some(f)) = (&self.root, self.focused) else {
            return self.clone();
        };
        let want = if matches!(dir, Direction::Left | Direction::Right) {
            Orientation::Row
        } else {
            Orientation::Column
        };
        let Some(path) = path_to_leaf(root, f) else {
            return self.clone();
        };
        for len in (0..path.len()).rev() {
            let ancestor = &path[..len];
            if let Some(Node::Split {
                orientation, ratio, ..
            }) = node_at_path(Some(root), ancestor)
                && *orientation == want
            {
                let delta = if matches!(dir, Direction::Right | Direction::Down) {
                    step
                } else {
                    -step
                };
                return self.set_ratio(ancestor, ratio + delta);
            }
        }
        self.clone()
    }

    /// Set the ratio of the split at `path`. No-op if it isn't a split.
    pub fn set_ratio(&self, path: &[u8], ratio: f32) -> Layout {
        let Some(root) = &self.root else {
            return self.clone();
        };
        let mut root = root.clone();
        if let Some(Node::Split { ratio: r, .. }) = node_at_path_mut(&mut root, path) {
            *r = clamp_ratio(ratio);
        }
        Layout {
            root: Some(root),
            ..self.clone()
        }
    }

    /// Insert `id` beside the focused pane (to the right), focusing it — used to
    /// restore a backgrounded pane or reattach a detached one. Seeds an empty
    /// layout.
    pub fn insert_beside(&self, id: PaneId) -> Layout {
        if self.has_pane(id) {
            return self.focus(id);
        }
        self.split_focused(Direction::Right, id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SplitInfo {
    pub path: NodePath,
    pub orientation: Orientation,
    pub ratio: f32,
    pub rect: NRect,
}

fn collect_leaves(n: &Node, out: &mut Vec<PaneId>) {
    match n {
        Node::Leaf(id) => out.push(*id),
        Node::Split { first, second, .. } => {
            collect_leaves(first, out);
            collect_leaves(second, out);
        }
    }
}

fn halves(r: NRect, o: Orientation, ratio: f32) -> (NRect, NRect) {
    match o {
        Orientation::Row => {
            let w1 = r.w * ratio;
            (
                NRect { w: w1, ..r },
                NRect {
                    x: r.x + w1,
                    w: r.w - w1,
                    ..r
                },
            )
        }
        Orientation::Column => {
            let h1 = r.h * ratio;
            (
                NRect { h: h1, ..r },
                NRect {
                    y: r.y + h1,
                    h: r.h - h1,
                    ..r
                },
            )
        }
    }
}

fn compute_rects(n: &Node, r: NRect, out: &mut Vec<(PaneId, NRect)>) {
    match n {
        Node::Leaf(id) => out.push((*id, r)),
        Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => {
            let (a, b) = halves(r, *orientation, *ratio);
            compute_rects(first, a, out);
            compute_rects(second, b, out);
        }
    }
}

fn collect_splits(n: &Node, r: NRect, path: &mut NodePath, out: &mut Vec<SplitInfo>) {
    if let Node::Split {
        orientation,
        ratio,
        first,
        second,
    } = n
    {
        out.push(SplitInfo {
            path: path.clone(),
            orientation: *orientation,
            ratio: *ratio,
            rect: r,
        });
        let (a, b) = halves(r, *orientation, *ratio);
        path.push(0);
        collect_splits(first, a, path, out);
        path.pop();
        path.push(1);
        collect_splits(second, b, path, out);
        path.pop();
    }
}

fn map_leaf(n: &Node, id: PaneId, f: &dyn Fn(Node) -> Node) -> Node {
    match n {
        Node::Leaf(l) if *l == id => f(n.clone()),
        Node::Leaf(_) => n.clone(),
        Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => Node::Split {
            orientation: *orientation,
            ratio: *ratio,
            first: Box::new(map_leaf(first, id, f)),
            second: Box::new(map_leaf(second, id, f)),
        },
    }
}

fn overlaps(a1: f32, a2: f32, b1: f32, b2: f32) -> bool {
    a1.max(b1) < a2.min(b2)
}

fn swap(n: &Node, a: PaneId, b: PaneId) -> Node {
    match n {
        Node::Leaf(id) if *id == a => Node::Leaf(b),
        Node::Leaf(id) if *id == b => Node::Leaf(a),
        Node::Leaf(_) => n.clone(),
        Node::Split {
            orientation,
            ratio,
            first,
            second,
        } => Node::Split {
            orientation: *orientation,
            ratio: *ratio,
            first: Box::new(swap(first, a, b)),
            second: Box::new(swap(second, a, b)),
        },
    }
}

fn path_to_leaf(n: &Node, id: PaneId) -> Option<NodePath> {
    match n {
        Node::Leaf(l) => (*l == id).then(Vec::new),
        Node::Split { first, second, .. } => {
            if let Some(mut p) = path_to_leaf(first, id) {
                p.insert(0, 0);
                return Some(p);
            }
            path_to_leaf(second, id).map(|mut p| {
                p.insert(0, 1);
                p
            })
        }
    }
}

fn remove_at_path(n: &Node, path: &[u8]) -> Option<Node> {
    let (&head, rest) = path.split_first()?;
    let Node::Split {
        orientation,
        ratio,
        first,
        second,
    } = n
    else {
        return Some(n.clone());
    };
    let (child, sibling) = if head == 0 {
        (first, second)
    } else {
        (second, first)
    };
    if rest.is_empty() {
        return Some((**sibling).clone());
    }
    match remove_at_path(child, rest) {
        None => Some((**sibling).clone()),
        Some(replaced) => {
            let (f, s) = if head == 0 {
                (replaced, (**second).clone())
            } else {
                ((**first).clone(), replaced)
            };
            Some(Node::Split {
                orientation: *orientation,
                ratio: *ratio,
                first: Box::new(f),
                second: Box::new(s),
            })
        }
    }
}

fn node_at_path<'a>(n: Option<&'a Node>, path: &[u8]) -> Option<&'a Node> {
    let mut cur = n?;
    for &step in path {
        match cur {
            Node::Leaf(_) => return None,
            Node::Split { first, second, .. } => {
                cur = if step == 0 { first } else { second };
            }
        }
    }
    Some(cur)
}

fn node_at_path_mut<'a>(n: &'a mut Node, path: &[u8]) -> Option<&'a mut Node> {
    let mut cur = n;
    for &step in path {
        match cur {
            Node::Leaf(_) => return None,
            Node::Split { first, second, .. } => {
                cur = if step == 0 { first } else { second };
            }
        }
    }
    Some(cur)
}

fn first_leaf(n: &Node) -> PaneId {
    let mut cur = n;
    loop {
        match cur {
            Node::Leaf(id) => return *id,
            Node::Split { first, .. } => cur = first,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::*;

    /// Every split has two children and a ratio in range; focus is a leaf.
    fn assert_invariants(l: &Layout) {
        fn walk(n: &Node) {
            if let Node::Split {
                ratio,
                first,
                second,
                ..
            } = n
            {
                assert!((MIN_RATIO..=MAX_RATIO).contains(ratio), "ratio {ratio}");
                walk(first);
                walk(second);
            }
        }
        if let Some(r) = &l.root {
            walk(r);
        }
        match l.focused {
            Some(f) => assert!(l.has_pane(f)),
            None => assert!(l.root.is_none()),
        }
    }

    fn three() -> Layout {
        // [1 | [2 / 3]]
        Layout::single(1)
            .split_focused(Right, 2)
            .split_focused(Down, 3)
    }

    #[test]
    fn split_in_both_orientations() {
        let l = Layout::single(1).split_focused(Right, 2);
        assert_eq!(l.leaves(), vec![1, 2]);
        assert_eq!(l.focused, Some(2));
        let Some(Node::Split { orientation, .. }) = &l.root else {
            panic!()
        };
        assert_eq!(*orientation, Orientation::Row);
        let l = Layout::single(1).split_focused(Down, 2);
        let Some(Node::Split { orientation, .. }) = &l.root else {
            panic!()
        };
        assert_eq!(*orientation, Orientation::Column);
        assert_invariants(&l);
        assert_eq!(Layout::default().split_focused(Down, 7), Layout::single(7));
    }

    #[test]
    fn focus_in_all_directions() {
        let l = three();
        assert_eq!(l.focused, Some(3));
        assert_eq!(l.focus_direction(Up).focused, Some(2));
        assert_eq!(l.focus_direction(Left).focused, Some(1));
        assert_eq!(l.focus_direction(Down).focused, Some(3)); // nothing below
        let l = l.focus(1);
        assert_eq!(
            l.focus_direction(Right).focused.map(|f| f == 2 || f == 3),
            Some(true)
        );
        assert_eq!(l.focus_direction(Left).focused, Some(1));
    }

    #[test]
    fn focus_next_wraps() {
        let l = three();
        assert_eq!(l.focus_next().focused, Some(1));
        assert_eq!(l.focus_prev().focused, Some(2));
        assert_eq!(l.focus(1).focus_prev().focused, Some(3));
    }

    #[test]
    fn move_swaps_with_neighbour_and_focus_follows() {
        let l = three().move_pane(Left);
        assert_eq!(l.leaves(), vec![3, 2, 1]);
        assert_eq!(l.focused, Some(3));
        assert_invariants(&l);
        // No neighbour: unchanged.
        assert_eq!(l.move_pane(Left), l);
    }

    #[test]
    fn close_promotes_sibling() {
        let l = three();
        let (l2, removed) = l.close_focused();
        assert_eq!(removed, Some(3));
        assert_eq!(l2.leaves(), vec![1, 2]);
        assert_eq!(l2.focused, Some(2));
        assert_invariants(&l2);
        // Close a non-focused pane: focus stays.
        let (l3, _) = l.close_pane(1);
        assert_eq!(l3.leaves(), vec![2, 3]);
        assert_eq!(l3.focused, Some(3));
        assert_invariants(&l3);
        // Close the last: empty.
        let (e, r) = Layout::single(1).close_focused();
        assert_eq!(r, Some(1));
        assert!(e.is_empty());
        assert_eq!(e.focused, None);
        // Unknown id: unchanged.
        assert_eq!(l.close_pane(99), (l.clone(), None));
    }

    #[test]
    fn zoom_keeps_shape_and_clears_on_close() {
        let l = three();
        let z = l.toggle_zoom();
        assert_eq!(z.zoomed, Some(3));
        assert_eq!(z.root, l.root);
        assert_eq!(z.toggle_zoom().zoomed, None);
        let (c, _) = z.close_focused();
        assert_eq!(c.zoomed, None);
    }

    #[test]
    fn ratios_clamp() {
        let l = Layout::single(1).split_focused(Right, 2);
        let l = l.set_ratio(&[], 2.0);
        assert_eq!(l.splits()[0].ratio, MAX_RATIO);
        let l = l.set_ratio(&[], -1.0);
        assert_eq!(l.splits()[0].ratio, MIN_RATIO);
        let l = l.set_ratio(&[], f32::NAN);
        assert_eq!(l.splits()[0].ratio, 0.5);
        assert_invariants(&l);
    }

    #[test]
    fn resize_moves_nearest_matching_split() {
        let l = three(); // focused 3, inside a Column inside a Row
        let r = l.resize_focused(Left, 0.1);
        assert!((r.splits()[0].ratio - 0.4).abs() < 1e-6);
        let r = l.resize_focused(Down, 0.1);
        assert!((r.splits()[1].ratio - 0.6).abs() < 1e-6);
        // No row split above a lone pane: unchanged.
        assert_eq!(
            Layout::single(1).resize_focused(Left, 0.1),
            Layout::single(1)
        );
    }

    #[test]
    fn rects_tile_the_unit_square() {
        let l = three();
        let area: f32 = l.rects().iter().map(|(_, r)| r.w * r.h).sum();
        assert!((area - 1.0).abs() < 1e-5);
        assert_eq!(l.splits().len(), 2);
    }

    #[test]
    fn insert_beside_seeds_and_focuses() {
        assert_eq!(Layout::default().insert_beside(4), Layout::single(4));
        let l = three().insert_beside(9);
        assert_eq!(l.focused, Some(9));
        assert!(l.has_pane(9));
        assert_invariants(&l);
    }
}
