//! Pane tree, layout, hit-testing and focus.
//!
//! Not a general UI framework — just what an IDE needs: a pane tree that
//! splits horizontally and vertically and resizes from its dividers. Writing
//! this instead of all of flexbox is the point.
//!
//! No dependencies and no knowledge of windows: layout math is hard to debug
//! by eye and easy to debug with assertions.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PaneId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NodeId(usize);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    /// Side by side — vertical dividers.
    Horizontal,
    /// Stacked — horizontal dividers.
    Vertical,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - d * 2.0).max(0.0), (self.h - d * 2.0).max(0.0))
    }
}

/// Drawn thickness of a divider.
pub const DIVIDER: f32 = 1.0;
/// Grab thickness. Wider than drawn, or hitting a 1px line is torture.
pub const DIVIDER_GRAB: f32 = 8.0;
/// Smallest a pane may shrink to.
pub const MIN_PANE: f32 = 80.0;

#[derive(Clone, Debug)]
enum Node {
    Leaf {
        pane: PaneId,
    },
    Split {
        axis: Axis,
        children: Vec<NodeId>,
        /// One per child, summing to 1.0.
        ratios: Vec<f32>,
    },
}

/// A divider: which gap of which split.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DividerRef {
    split: NodeId,
    /// Between children `index` and `index + 1`.
    index: usize,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hit {
    Pane(PaneId),
    Divider(DividerRef, Axis),
}

#[derive(Clone, Default, Debug)]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<(DividerRef, Axis, Rect)>,
}

impl Layout {
    pub fn rect_of(&self, pane: PaneId) -> Option<Rect> {
        self.panes.iter().find(|(p, _)| *p == pane).map(|(_, r)| *r)
    }
}

pub struct Tree {
    nodes: Vec<Node>,
    root: NodeId,
    next_pane: u32,
}

impl Tree {
    /// A tree with a single pane.
    pub fn new() -> (Self, PaneId) {
        let pane = PaneId(0);
        let me = Self {
            nodes: vec![Node::Leaf { pane }],
            root: NodeId(0),
            next_pane: 1,
        };
        (me, pane)
    }

    fn alloc(&mut self, n: Node) -> NodeId {
        self.nodes.push(n);
        NodeId(self.nodes.len() - 1)
    }

    fn find_leaf(&self, pane: PaneId) -> Option<NodeId> {
        self.nodes.iter().position(|n| matches!(n, Node::Leaf { pane: p } if *p == pane)).map(NodeId)
    }

    fn parent_of(&self, child: NodeId) -> Option<(NodeId, usize)> {
        self.nodes.iter().enumerate().find_map(|(i, n)| match n {
            Node::Split { children, .. } => {
                children.iter().position(|c| *c == child).map(|k| (NodeId(i), k))
            }
            _ => None,
        })
    }

    /// Splits `pane` along `axis` and returns the new pane.
    ///
    /// If the parent already splits on the same axis we add a child there
    /// instead of nesting. Nesting deepens the tree for nothing and makes
    /// dragging unintuitive, because the ratios compound.
    pub fn split(&mut self, pane: PaneId, axis: Axis) -> Option<PaneId> {
        self.split_at(pane, axis, 0.5)
    }

    /// Splits with a chosen share for the original pane.
    ///
    /// A sidebar wants a quarter of the width, not half, and dragging the
    /// divider into place afterwards needs a computed layout that does not
    /// exist yet at startup.
    pub fn split_at(&mut self, pane: PaneId, axis: Axis, ratio: f32) -> Option<PaneId> {
        let ratio = ratio.clamp(0.05, 0.95);
        let leaf = self.find_leaf(pane)?;
        let new_pane = PaneId(self.next_pane);
        self.next_pane += 1;
        let new_leaf = self.alloc(Node::Leaf { pane: new_pane });

        if let Some((parent, idx)) = self.parent_of(leaf) {
            let same_axis = matches!(&self.nodes[parent.0], Node::Split { axis: a, .. } if *a == axis);
            if same_axis {
                if let Node::Split { children, ratios, .. } = &mut self.nodes[parent.0] {
                    // Divide the sibling's share; leave the others alone.
                    let total = ratios[idx];
                    ratios[idx] = total * ratio;
                    ratios.insert(idx + 1, total * (1.0 - ratio));
                    children.insert(idx + 1, new_leaf);
                }
                return Some(new_pane);
            }
        }

        // New split node takes the leaf's place.
        let old_leaf_copy = self.nodes[leaf.0].clone();
        let moved = self.alloc(old_leaf_copy);
        self.nodes[leaf.0] = Node::Split {
            axis,
            children: vec![moved, new_leaf],
            ratios: vec![ratio, 1.0 - ratio],
        };
        Some(new_pane)
    }

    /// Removes a pane; its sibling takes the parent's place.
    pub fn close(&mut self, pane: PaneId) -> bool {
        let Some(leaf) = self.find_leaf(pane) else { return false };
        let Some((parent, idx)) = self.parent_of(leaf) else {
            return false; // root leaf — the last pane can't be closed
        };
        let Node::Split { children, ratios, .. } = &mut self.nodes[parent.0] else {
            return false;
        };
        children.remove(idx);
        let freed = ratios.remove(idx);
        if children.len() == 1 {
            let only = children[0];
            self.nodes[parent.0] = self.nodes[only.0].clone();
        } else {
            // Spread the freed share proportionally.
            let total: f32 = ratios.iter().sum();
            if total > 0.0 {
                for r in ratios.iter_mut() {
                    *r += freed * (*r / total);
                }
            }
        }
        true
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(self.root, &mut out);
        out
    }

    fn collect(&self, node: NodeId, out: &mut Vec<PaneId>) {
        match &self.nodes[node.0] {
            Node::Leaf { pane } => out.push(*pane),
            Node::Split { children, .. } => {
                for c in children {
                    self.collect(*c, out);
                }
            }
        }
    }

    pub fn compute(&self, area: Rect) -> Layout {
        let mut out = Layout::default();
        self.place(self.root, area, &mut out);
        out
    }

    fn place(&self, node: NodeId, area: Rect, out: &mut Layout) {
        match &self.nodes[node.0] {
            Node::Leaf { pane } => out.panes.push((*pane, area)),
            Node::Split { axis, children, ratios } => {
                let n = children.len();
                let gaps = (n.saturating_sub(1)) as f32 * DIVIDER;
                let total = match axis {
                    Axis::Horizontal => (area.w - gaps).max(0.0),
                    Axis::Vertical => (area.h - gaps).max(0.0),
                };
                let mut cursor = match axis {
                    Axis::Horizontal => area.x,
                    Axis::Vertical => area.y,
                };
                for (i, child) in children.iter().enumerate() {
                    let extent = total * ratios[i];
                    let child_area = match axis {
                        Axis::Horizontal => Rect::new(cursor, area.y, extent, area.h),
                        Axis::Vertical => Rect::new(area.x, cursor, area.w, extent),
                    };
                    self.place(*child, child_area, out);
                    cursor += extent;
                    if i + 1 < n {
                        let d = match axis {
                            Axis::Horizontal => Rect::new(cursor, area.y, DIVIDER, area.h),
                            Axis::Vertical => Rect::new(area.x, cursor, area.w, DIVIDER),
                        };
                        out.dividers.push((DividerRef { split: node, index: i }, *axis, d));
                        cursor += DIVIDER;
                    }
                }
            }
        }
    }

    /// Dividers are tested before panes: their grab areas overhang the panes,
    /// and clicking near an edge must not miss the divider.
    pub fn hit(&self, layout: &Layout, x: f32, y: f32) -> Option<Hit> {
        for (d, axis, r) in &layout.dividers {
            let grab = match axis {
                Axis::Horizontal => Rect::new(
                    r.x - DIVIDER_GRAB * 0.5,
                    r.y,
                    r.w + DIVIDER_GRAB,
                    r.h,
                ),
                Axis::Vertical => Rect::new(
                    r.x,
                    r.y - DIVIDER_GRAB * 0.5,
                    r.w,
                    r.h + DIVIDER_GRAB,
                ),
            };
            if grab.contains(x, y) {
                return Some(Hit::Divider(*d, *axis));
            }
        }
        layout.panes.iter().find(|(_, r)| r.contains(x, y)).map(|(p, _)| Hit::Pane(*p))
    }

    /// Moves a divider by a pixel delta. Only the two neighbours change, and
    /// neither drops below `MIN_PANE`.
    pub fn drag(&mut self, d: DividerRef, delta_px: f32, area: Rect) {
        // Recompute the layout to find the split's span. Trees are small, so
        // this is cheaper than keeping state in sync.
        let extent = {
            let Node::Split { axis, .. } = &self.nodes[d.split.0] else { return };
            let span = self.span_of(d.split, area, *axis);
            if span <= 0.0 {
                return;
            }
            span
        };

        let Node::Split { ratios, .. } = &mut self.nodes[d.split.0] else { return };
        let delta = delta_px / extent;
        let (a, b) = (ratios[d.index], ratios[d.index + 1]);
        let min = MIN_PANE / extent;
        let new_a = (a + delta).clamp(min, a + b - min);
        ratios[d.index] = new_a;
        ratios[d.index + 1] = a + b - new_a;
    }

    /// How long a split node is along its own axis.
    fn span_of(&self, split: NodeId, area: Rect, axis: Axis) -> f32 {
        let layout = self.compute(area);
        let mut panes = Vec::new();
        self.collect(split, &mut panes);
        let rects: Vec<Rect> = panes.iter().filter_map(|p| layout.rect_of(*p)).collect();
        if rects.is_empty() {
            return 0.0;
        }
        match axis {
            Axis::Horizontal => {
                let l = rects.iter().map(|r| r.x).fold(f32::MAX, f32::min);
                let r = rects.iter().map(|r| r.right()).fold(f32::MIN, f32::max);
                (r - l - (self.child_count(split) - 1) as f32 * DIVIDER).max(0.0)
            }
            Axis::Vertical => {
                let t = rects.iter().map(|r| r.y).fold(f32::MAX, f32::min);
                let b = rects.iter().map(|r| r.bottom()).fold(f32::MIN, f32::max);
                (b - t - (self.child_count(split) - 1) as f32 * DIVIDER).max(0.0)
            }
        }
    }

    fn child_count(&self, node: NodeId) -> usize {
        match &self.nodes[node.0] {
            Node::Split { children, .. } => children.len(),
            _ => 1,
        }
    }
}

/// A tree as plain data, for saving and restoring a layout.
///
/// Deliberately not the internal `Node`: that one is an arena of indices,
/// which is the wrong thing to write to a file — a stale index is a crash or
/// worse, and the arena's shape is an implementation detail that should stay
/// free to change.
#[derive(Clone, PartialEq, Debug)]
pub enum Desc {
    Leaf(PaneId),
    Split {
        axis: Axis,
        ratios: Vec<f32>,
        children: Vec<Desc>,
    },
}

impl Tree {
    /// The current layout as data.
    pub fn describe(&self) -> Desc {
        self.describe_node(self.root)
    }

    fn describe_node(&self, node: NodeId) -> Desc {
        match &self.nodes[node.0] {
            Node::Leaf { pane } => Desc::Leaf(*pane),
            Node::Split { axis, children, ratios } => Desc::Split {
                axis: *axis,
                ratios: ratios.clone(),
                children: children.iter().map(|c| self.describe_node(*c)).collect(),
            },
        }
    }

    /// Rebuilds a tree from data.
    ///
    /// Pane ids are taken from the description rather than reassigned, so the
    /// caller's map of what each pane holds still lines up. A split with fewer
    /// than two children collapses, because a description read from a file is
    /// not to be trusted to be well formed.
    pub fn from_desc(desc: &Desc) -> (Self, Vec<PaneId>) {
        let mut me = Self {
            nodes: Vec::new(),
            root: NodeId(0),
            next_pane: 0,
        };
        let mut panes = Vec::new();
        match me.build(desc, &mut panes) {
            Some(root) if !panes.is_empty() => me.root = root,
            // Never zero panes: every other operation assumes at least one.
            _ => {
                me.nodes.clear();
                panes.clear();
                me.root = me.alloc(Node::Leaf { pane: PaneId(0) });
                panes.push(PaneId(0));
            }
        }
        // The next new pane must not collide with a restored one.
        me.next_pane = panes.iter().map(|p| p.0.saturating_add(1)).max().unwrap_or(1);
        (me, panes)
    }

    /// `None` when the description holds nothing usable, which a file can.
    fn build(&mut self, desc: &Desc, panes: &mut Vec<PaneId>) -> Option<NodeId> {
        match desc {
            Desc::Leaf(pane) => {
                panes.push(*pane);
                Some(self.alloc(Node::Leaf { pane: *pane }))
            }
            Desc::Split { axis, ratios, children } => {
                let kids: Vec<NodeId> = children
                    .iter()
                    .filter_map(|c| self.build(c, panes))
                    .collect();
                if kids.len() < 2 {
                    // Malformed: a split of one is just that child, and a
                    // split of none is nothing at all.
                    return kids.first().copied();
                }
                // Ratios are normalised rather than trusted: a hand-edited or
                // truncated file would otherwise lay panes out on top of each
                // other or off the screen.
                let mut ratios = ratios.clone();
                ratios.resize(kids.len(), 1.0 / kids.len() as f32);
                let total: f32 = ratios.iter().sum();
                if total > 0.0 {
                    for r in &mut ratios {
                        *r /= total;
                    }
                } else {
                    ratios.fill(1.0 / kids.len() as f32);
                }
                Some(self.alloc(Node::Split { axis: *axis, children: kids, ratios }))
            }
        }
    }
}

/// Directional focus movement.
///
/// Geometric, not structural: "the pane on the right" means the one drawn to
/// the right, not the sibling in the tree.
pub fn focus_in_dir(layout: &Layout, from: PaneId, dir: Dir) -> Option<PaneId> {
    let src = layout.rect_of(from)?;
    let (cx, cy) = src.center();

    layout
        .panes
        .iter()
        .filter(|(p, _)| *p != from)
        .filter(|(_, r)| match dir {
            Dir::Left => r.right() <= src.x + 1.0,
            Dir::Right => r.x >= src.right() - 1.0,
            Dir::Up => r.bottom() <= src.y + 1.0,
            Dir::Down => r.y >= src.bottom() - 1.0,
        })
        .min_by(|(_, a), (_, b)| {
            // Distance along the direction first, drift second, so the pane
            // straight ahead beats a nearer diagonal one.
            let score = |r: &Rect| {
                let (ox, oy) = r.center();
                let (primary, secondary) = match dir {
                    Dir::Left => (cx - ox, (oy - cy).abs()),
                    Dir::Right => (ox - cx, (oy - cy).abs()),
                    Dir::Up => (cy - oy, (ox - cx).abs()),
                    Dir::Down => (oy - cy, (ox - cx).abs()),
                };
                primary.max(0.0) + secondary * 2.0
            };
            score(a).total_cmp(&score(b))
        })
        .map(|(p, _)| *p)
}

/// Panes in reading order: top to bottom, then left to right.
///
/// For numbered jumps. The number has to mean the same thing every time you
/// press it, and `Layout::panes` is in tree order — which is an artefact of
/// the order panes were split in, so pane 2 would move when you closed
/// something elsewhere. Where a pane sits on screen is what you can see, and
/// therefore the only thing worth counting.
///
/// Rows before columns, because a sidebar plus a stack of editors is the
/// common shape and numbering it down the sidebar first would be surprising.
pub fn panes_in_reading_order(layout: &Layout) -> Vec<PaneId> {
    let mut panes: Vec<(PaneId, Rect)> = layout.panes.clone();
    panes.sort_by(|(_, a), (_, b)| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
    panes.into_iter().map(|(p, _)| p).collect()
}

/// The divider between a pane and whatever is on the given side of it.
///
/// Geometric, like `focus_in_dir`, and for the same reason: the divider you
/// mean is the one drawn against that edge, not whichever ancestor split
/// happens to sit nearest in the tree. A pane deep in a nested split has its
/// left edge formed by a divider several levels up.
///
/// `None` at the outside of the layout, where the edge is the window.
pub fn divider_in_dir(layout: &Layout, from: PaneId, dir: Dir) -> Option<DividerRef> {
    let src = layout.rect_of(from)?;
    // A divider is DIVIDER wide and sits in the gap between panes, so its
    // near edge is within a pixel or two of the pane's edge rather than on it.
    let touching = DIVIDER + 2.0;

    layout
        .dividers
        .iter()
        .filter(|(_, axis, r)| match dir {
            // A vertical line splits left from right, which the axis calls
            // Horizontal — the axis names the direction the split runs in.
            Dir::Left => *axis == Axis::Horizontal && (src.x - r.right()).abs() <= touching,
            Dir::Right => *axis == Axis::Horizontal && (r.x - src.right()).abs() <= touching,
            Dir::Up => *axis == Axis::Vertical && (src.y - r.bottom()).abs() <= touching,
            Dir::Down => *axis == Axis::Vertical && (r.y - src.bottom()).abs() <= touching,
        })
        // Overlapping the pane along the other axis, or a divider on the far
        // side of the window would qualify on its edge alone.
        .filter(|(_, axis, r)| match axis {
            Axis::Horizontal => r.y < src.bottom() && r.bottom() > src.y,
            Axis::Vertical => r.x < src.right() && r.right() > src.x,
        })
        .map(|(d, _, _)| *d)
        .next()
}

/// Per-pane scratch state, such as scroll position.
#[derive(Default)]
pub struct PaneState<T> {
    map: HashMap<PaneId, T>,
}

impl<T: Default> PaneState<T> {
    pub fn get(&mut self, id: PaneId) -> &mut T {
        self.map.entry(id).or_default()
    }
    pub fn peek(&self, id: PaneId) -> Option<&T> {
        self.map.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0.0, y: 0.0, w: 1000.0, h: 600.0 };

    /// A sidebar down the left, two panes stacked on the right. The shape
    /// numbered jumps and keyboard resizing both have to get right.
    fn sidebar_and_stack() -> (Tree, PaneId, PaneId, PaneId) {
        let (mut tree, left) = Tree::new();
        let right = tree.split_at(left, Axis::Horizontal, 0.25).unwrap();
        let below = tree.split(right, Axis::Vertical).unwrap();
        (tree, left, right, below)
    }

    #[test]
    fn numbering_follows_the_screen_not_the_tree() {
        let (tree, left, right, below) = sidebar_and_stack();
        let l = tree.compute(AREA);
        // `below` was split off last, so tree order would put it second.
        assert_eq!(panes_in_reading_order(&l), vec![left, right, below]);
    }

    #[test]
    fn numbering_survives_the_panes_arriving_in_another_order() {
        // Same picture, built the other way round: the numbers must not
        // depend on which pane happened to be split first.
        let (tree, left, right, below) = sidebar_and_stack();
        let mut l = tree.compute(AREA);
        l.panes.reverse();
        assert_eq!(panes_in_reading_order(&l), vec![left, right, below]);
    }

    #[test]
    fn a_pane_finds_the_divider_against_each_of_its_edges() {
        let (tree, left, right, below) = sidebar_and_stack();
        let l = tree.compute(AREA);

        // The sidebar's right edge and the stack's left edge are the same
        // divider, seen from either side.
        let from_left = divider_in_dir(&l, left, Dir::Right).unwrap();
        let from_right = divider_in_dir(&l, right, Dir::Left).unwrap();
        assert_eq!(from_left, from_right);

        // And the one between the stacked pair is a different divider.
        let between = divider_in_dir(&l, right, Dir::Down).unwrap();
        assert_ne!(between, from_left);
        assert_eq!(divider_in_dir(&l, below, Dir::Up), Some(between));
    }

    #[test]
    fn the_outside_of_the_layout_has_no_divider() {
        let (tree, left, right, below) = sidebar_and_stack();
        let l = tree.compute(AREA);
        assert_eq!(divider_in_dir(&l, left, Dir::Left), None);
        assert_eq!(divider_in_dir(&l, left, Dir::Up), None);
        assert_eq!(divider_in_dir(&l, right, Dir::Up), None);
        assert_eq!(divider_in_dir(&l, below, Dir::Down), None);
        assert_eq!(divider_in_dir(&l, below, Dir::Right), None);
    }

    #[test]
    fn the_sidebar_does_not_claim_the_stacks_divider() {
        // The horizontal divider between the two right-hand panes never
        // touches the sidebar, and asking the sidebar to resize downwards
        // must not hand it back.
        let (tree, left, _right, _below) = sidebar_and_stack();
        let l = tree.compute(AREA);
        assert_eq!(divider_in_dir(&l, left, Dir::Down), None);
    }

    #[test]
    fn a_found_divider_actually_moves_that_edge() {
        // The point of finding it is dragging it, so check the pane grows.
        let (mut tree, left, _right, _below) = sidebar_and_stack();
        let l = tree.compute(AREA);
        let before = l.rect_of(left).unwrap().w;
        let d = divider_in_dir(&l, left, Dir::Right).unwrap();
        tree.drag(d, 40.0, AREA);
        let after = tree.compute(AREA).rect_of(left).unwrap().w;
        assert!(after > before, "{before} -> {after}");
    }

    #[test]
    fn one_pane_fills_the_area() {
        let (tree, a) = Tree::new();
        let l = tree.compute(AREA);
        assert_eq!(l.panes.len(), 1);
        assert_eq!(l.rect_of(a).unwrap(), AREA);
        assert!(l.dividers.is_empty());
    }

    #[test]
    fn splitting_accounts_for_the_divider() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        let (ra, rb) = (l.rect_of(a).unwrap(), l.rect_of(b).unwrap());
        assert_eq!(l.dividers.len(), 1);
        assert!((ra.w + rb.w + DIVIDER - AREA.w).abs() < 0.01);
        assert!((ra.w - rb.w).abs() < 0.01);
        assert!(rb.x > ra.x);
    }

    #[test]
    fn split_at_honours_the_ratio() {
        // A sidebar is a quarter of the window, not half.
        let (mut tree, a) = Tree::new();
        let b = tree.split_at(a, Axis::Horizontal, 0.25).unwrap();
        let l = tree.compute(AREA);
        let (ra, rb) = (l.rect_of(a).unwrap(), l.rect_of(b).unwrap());
        assert!((ra.w / (ra.w + rb.w) - 0.25).abs() < 0.01, "{ra:?} {rb:?}");
    }

    #[test]
    fn same_axis_split_does_not_deepen_the_tree() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let _c = tree.split(b, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        assert_eq!(l.panes.len(), 3);
        // Nesting would also give two dividers but skew the ratios, so the
        // widths are what actually proves it.
        assert_eq!(l.dividers.len(), 2);
        let sum: f32 = l.panes.iter().map(|(_, r)| r.w).sum();
        assert!((sum + 2.0 * DIVIDER - AREA.w).abs() < 0.01);
    }

    #[test]
    fn dragging_moves_neighbours_and_keeps_the_total() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        let (d, _, _) = l.dividers[0];
        tree.drag(d, 100.0, AREA);
        let l2 = tree.compute(AREA);
        let (ra, rb) = (l2.rect_of(a).unwrap(), l2.rect_of(b).unwrap());
        assert!(ra.w > rb.w, "dragging right should grow the left pane");
        assert!((ra.w + rb.w + DIVIDER - AREA.w).abs() < 0.01);
    }

    #[test]
    fn dragging_stops_at_min_pane() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        let (d, _, _) = l.dividers[0];
        tree.drag(d, -100_000.0, AREA);
        let l2 = tree.compute(AREA);
        assert!(l2.rect_of(a).unwrap().w >= MIN_PANE - 0.5);
        assert!(l2.rect_of(b).unwrap().w > 0.0);
    }

    #[test]
    fn grab_area_is_wider_than_the_line() {
        let (mut tree, a) = Tree::new();
        let _b = tree.split(a, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        let (_, _, r) = l.dividers[0];
        assert!(matches!(
            tree.hit(&l, r.x - 3.0, 300.0),
            Some(Hit::Divider(..))
        ));
    }

    #[test]
    fn closing_promotes_the_sibling() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        assert!(tree.close(b));
        let l = tree.compute(AREA);
        assert_eq!(l.panes.len(), 1);
        assert_eq!(l.rect_of(a).unwrap(), AREA);
        assert!(l.dividers.is_empty());
    }

    #[test]
    fn the_last_pane_cannot_be_closed() {
        let (mut tree, a) = Tree::new();
        assert!(!tree.close(a));
        assert_eq!(tree.panes().len(), 1);
    }

    #[test]
    fn directional_focus_follows_geometry() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let c = tree.split(b, Axis::Vertical).unwrap();
        let l = tree.compute(AREA);

        assert_eq!(focus_in_dir(&l, a, Dir::Right), Some(b));
        assert_eq!(focus_in_dir(&l, a, Dir::Left), None);
        assert_eq!(focus_in_dir(&l, b, Dir::Down), Some(c));
        assert_eq!(focus_in_dir(&l, c, Dir::Up), Some(b));
        assert_eq!(focus_in_dir(&l, c, Dir::Left), Some(a));
    }

    #[test]
    fn every_pane_appears_once() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let c = tree.split(b, Axis::Vertical).unwrap();
        let _d = tree.split(c, Axis::Horizontal).unwrap();
        let l = tree.compute(AREA);
        let mut ids: Vec<u32> = l.panes.iter().map(|(p, _)| p.0).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), l.panes.len());
        assert_eq!(ids.len(), tree.panes().len());
    }

    #[test]
    fn a_layout_survives_a_round_trip() {
        let (mut tree, a) = Tree::new();
        let b = tree.split_at(a, Axis::Horizontal, 0.25).unwrap();
        let _c = tree.split(b, Axis::Vertical).unwrap();
        let before = tree.compute(AREA);

        let (restored, panes) = Tree::from_desc(&tree.describe());
        assert_eq!(panes.len(), 3);
        assert_eq!(restored.compute(AREA).panes, before.panes, "same rects");
    }

    #[test]
    fn restored_panes_keep_their_ids() {
        // The caller's map of what each pane holds is keyed by id; renumbering
        // would hand every pane someone else's content.
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let (_, panes) = Tree::from_desc(&tree.describe());
        assert!(panes.contains(&a) && panes.contains(&b));
    }

    #[test]
    fn a_new_pane_cannot_collide_with_a_restored_one() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let (mut restored, _) = Tree::from_desc(&tree.describe());
        let fresh = restored.split(a, Axis::Vertical).unwrap();
        assert_ne!(fresh, a);
        assert_ne!(fresh, b);
    }

    #[test]
    fn a_malformed_description_still_produces_a_usable_tree() {
        // It comes from a file, so it can be truncated, hand-edited or from an
        // older version. None of that may leave the app with zero panes.
        let empty = Desc::Split { axis: Axis::Horizontal, ratios: vec![], children: vec![] };
        let (tree, panes) = Tree::from_desc(&empty);
        assert_eq!(panes.len(), 1);
        assert_eq!(tree.compute(AREA).panes.len(), 1);
    }

    #[test]
    fn nonsense_ratios_are_normalised() {
        // Otherwise a hand-edited file lays panes on top of each other.
        let desc = Desc::Split {
            axis: Axis::Horizontal,
            ratios: vec![5.0, 5.0],
            children: vec![Desc::Leaf(PaneId(0)), Desc::Leaf(PaneId(1))],
        };
        let (tree, _) = Tree::from_desc(&desc);
        let l = tree.compute(AREA);
        let sum: f32 = l.panes.iter().map(|(_, r)| r.w).sum();
        assert!((sum + DIVIDER - AREA.w).abs() < 0.01, "{l:?}");
    }

    #[test]
    fn panes_never_overlap() {
        let (mut tree, a) = Tree::new();
        let b = tree.split(a, Axis::Horizontal).unwrap();
        let c = tree.split(b, Axis::Vertical).unwrap();
        let _d = tree.split(a, Axis::Vertical).unwrap();
        let l = tree.compute(AREA);
        for (i, (_, r1)) in l.panes.iter().enumerate() {
            for (_, r2) in l.panes.iter().skip(i + 1) {
                let overlap = r1.x < r2.right()
                    && r2.x < r1.right()
                    && r1.y < r2.bottom()
                    && r2.y < r1.bottom();
                assert!(!overlap, "{r1:?} overlaps {r2:?}");
            }
        }
        let _ = c;
    }
}
