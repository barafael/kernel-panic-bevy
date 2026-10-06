//! Port of Recoil's QTPFS (`rts/Sim/Path/QTPFS/`): a quad-tree over the
//! per-square speed modifiers, searched bidirectionally through the
//! midpoints of the edges leaf nodes share, then smoothed once.
//!
//! What is kept from the engine, with its constants:
//! - root nodes of 64 squares (32 when the map is not a multiple of 64),
//!   every node larger than 16 squares split, smaller nodes split only
//!   while they mix closed and open squares (`QTNode::UpdateMoveCost`);
//! - a node's move cost is `1 / mean(relative speed)` over its squares,
//!   `2²⁴ · closed / xsize²` when some squares are closed, infinite when
//!   all are (`QTPFS_CLOSED_NODE_COST`);
//! - neighbours are edge-adjacent passable leaves plus corner leaves
//!   reachable past two fully open edge leaves
//!   (`QTPFS_CORNER_CONNECTED_NODES`), each with the midpoint of the
//!   shared edge as its transition point;
//! - the search (`PathSearch::ExecutePathSearch`) alternates one forward
//!   and one backward expansion per iteration, costs a move by the node
//!   being left times the distance between transition points, uses the
//!   cheapest leaf's cost as the heuristic multiplier, meets in the
//!   middle, stops early inside the goal radius, falls back to the node
//!   closest to the goal when the budget runs out (a partial path), and
//!   redirects a goal inside a closed node to the nearest open leaf;
//! - `TracePath` + one `SmoothPathIter` pass;
//! - `NextWayPoint`'s first-call scan, and the repath trigger placed
//!   half-way along a long partial path.
//!
//! Left out: per-thread registries, path sharing between units with the
//! same start and goal leaves (a cache, not a behaviour), exit-only
//! yardmap squares (Kernel Panic has none) and path repair (the dead
//! path is simply re-searched).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::block::{BlockMask, line_clear};
use crate::cost::{SQUARE_SIZE, SpeedMap};

/// `QTPFS_MAX_NODE_SIZE` / `QTPFS_BAD_ROOT_NODE_SIZE`.
const MAX_ROOT_SIZE: u32 = 64;
const BAD_ROOT_SIZE: u32 = 32;
/// `QTPFS_MAP_DAMAGE_SIZE`: the largest leaf, and the block terrain
/// changes are applied in.
pub const DAMAGE_SIZE: u32 = 16;
/// `QTPFS_CLOSED_NODE_COST`.
const CLOSED_COST: f32 = (1u32 << 24) as f32;
/// `NodeLayer::MAX_SPEEDMOD_VALUE` (speed mods are mapped onto `[0, 2]`).
const MAX_SPEEDMOD: f32 = 2.0;
/// `qtMaxNodesSearched` (ModInfo.cpp:136) and the relative share of the
/// layer's open leaves (`qtMaxNodesSearchedRelativeToMapOpenNodes`).
const MAX_NODES_SEARCHED: usize = 8192;
const MAX_NODES_RELATIVE: f32 = 0.25;
/// `qtRefreshPathMinDist`: partial paths at least this long get a
/// repath trigger half-way (ModInfo.cpp:137).
const REFRESH_PATH_MIN_DIST: f32 = 512.0;

/// Index of a node in a layer's pool; leaf ids stay valid until the
/// layer is re-tesselated.
pub type NodeId = u32;
const NONE: NodeId = u32::MAX;

#[derive(Clone, Copy, Debug)]
struct Neighbor {
    node: NodeId,
    /// `GetNeighborEdgeTransitionPoint(.., alpha = 0.5)`, elmos.
    net: [f32; 2],
}

#[derive(Clone, Debug)]
struct Node {
    xmin: u16,
    zmin: u16,
    xmax: u16,
    zmax: u16,
    /// First of four consecutive children, or [`NONE`] for a leaf.
    child_base: NodeId,
    /// `moveCostAvg`.
    move_cost: f32,
    neighbors: Vec<Neighbor>,
}

impl Node {
    fn xsize(&self) -> u32 {
        (self.xmax - self.xmin) as u32
    }
    fn zsize(&self) -> u32 {
        (self.zmax - self.zmin) as u32
    }
    fn is_leaf(&self) -> bool {
        self.child_base == NONE
    }
    /// `AllSquaresImpassable`.
    fn impassable(&self) -> bool {
        self.move_cost == f32::INFINITY
    }
    /// `AllSquaresAccessible`: no closed square at all.
    fn accessible(&self) -> bool {
        self.move_cost < CLOSED_COST / (self.xsize() * self.zsize()) as f32
    }
    fn intersects(&self, r: &Rect) -> bool {
        (self.xmin as i32) < r.x1
            && (self.xmax as i32) > r.x0
            && (self.zmin as i32) < r.z1
            && (self.zmax as i32) > r.z0
    }
    fn rect(&self) -> Rect {
        Rect {
            x0: self.xmin as i32,
            z0: self.zmin as i32,
            x1: self.xmax as i32,
            z1: self.zmax as i32,
        }
    }
    /// Centre in elmos.
    fn mid(&self) -> [f32; 2] {
        [
            (self.xmin + self.xmax) as f32 * 0.5 * SQUARE_SIZE,
            (self.zmin + self.zmax) as f32 * 0.5 * SQUARE_SIZE,
        ]
    }
}

/// A half-open square rectangle `[x0, x1) × [z0, z1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x0: i32,
    pub z0: i32,
    pub x1: i32,
    pub z1: i32,
}

impl Rect {
    fn clamped(self, w: u32, h: u32) -> Self {
        Rect {
            x0: self.x0.clamp(0, w as i32),
            z0: self.z0.clamp(0, h as i32),
            x1: self.x1.clamp(0, w as i32),
            z1: self.z1.clamp(0, h as i32),
        }
    }
    fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.z1 <= self.z0
    }
    fn dilated(self, d: i32) -> Self {
        Rect {
            x0: self.x0 - d,
            z0: self.z0 - d,
            x1: self.x1 + d,
            z1: self.z1 + d,
        }
    }
    fn union(self, o: Rect) -> Self {
        Rect {
            x0: self.x0.min(o.x0),
            z0: self.z0.min(o.z0),
            x1: self.x1.max(o.x1),
            z1: self.z1.max(o.z1),
        }
    }
}

/// One move class's quad tree (`QTPFS::NodeLayer`).
#[derive(Clone)]
pub struct NodeLayer {
    width: u32,
    height: u32,
    root_size: u32,
    x_roots: u32,
    nodes: Vec<Node>,
    /// Bases of freed child blocks, reused by the next split.
    free_blocks: Vec<NodeId>,
    /// `curSpeedMods`: `u8(relSpeedMod · 255)` per square; 0 = closed.
    speed_u8: Vec<u8>,
    /// `relSpeedModinfos[layer].max` as its reciprocal: the cheapest
    /// leaf's move cost (`hCostMult`), recomputed after changes.
    min_move_cost: Option<f32>,
    open_leaves: usize,
}

impl NodeLayer {
    /// Build the tree for `speed_map` with `mask`'s squares closed
    /// (`PathManager::InitNodeLayer` + the initial `UpdateNodeLayer`).
    pub fn new(speed_map: &SpeedMap, mask: Option<&BlockMask>) -> Self {
        let (width, height) = (speed_map.width, speed_map.height);
        // `InitRootSize`: the largest power-of-two factor of both
        // dimensions up to 64, else 32.
        let mut root_size = BAD_ROOT_SIZE;
        let mut factor = MAX_ROOT_SIZE;
        while factor <= width.min(height) {
            if width.is_multiple_of(factor) && height.is_multiple_of(factor) {
                root_size = factor;
            }
            factor *= 2;
        }
        let root_size = root_size.min(MAX_ROOT_SIZE);
        let x_roots = width.div_ceil(root_size);
        let z_roots = height.div_ceil(root_size);
        let mut layer = Self {
            width,
            height,
            root_size,
            x_roots,
            nodes: Vec::with_capacity((x_roots * z_roots * 4) as usize),
            free_blocks: Vec::new(),
            speed_u8: vec![0; (width * height) as usize],
            min_move_cost: None,
            open_leaves: 0,
        };
        for rz in 0..z_roots {
            for rx in 0..x_roots {
                let (x0, z0) = (rx * root_size, rz * root_size);
                layer.nodes.push(Node {
                    xmin: x0 as u16,
                    zmin: z0 as u16,
                    xmax: (x0 + root_size).min(width) as u16,
                    zmax: (z0 + root_size).min(height) as u16,
                    child_base: NONE,
                    move_cost: f32::INFINITY,
                    neighbors: Vec::new(),
                });
            }
        }
        let all = Rect {
            x0: 0,
            z0: 0,
            x1: width as i32,
            z1: height as i32,
        };
        layer.fill_squares(speed_map, mask, all);
        for root in 0..(x_roots * z_roots) {
            layer.tesselate(root);
        }
        layer.relink(all);
        layer
    }

    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Number of leaves (the engine's `numLeafNodes`).
    pub fn leaf_count(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| n.is_leaf() && n.xmin != u16::MAX)
            .count()
    }

    /// `NodeLayer::Update` for `rect`: the per-square relative speed
    /// (`clamp(speedMod, 0, 2) / 2`, 0 when a structure closes the
    /// square), as `u8(rel · 255)`. Returns whether any square changed.
    fn fill_squares(&mut self, speed_map: &SpeedMap, mask: Option<&BlockMask>, rect: Rect) -> bool {
        let rect = rect.clamped(self.width, self.height);
        let mut changed = false;
        for z in rect.z0..rect.z1 {
            for x in rect.x0..rect.x1 {
                let i = (z as u32 * self.width + x as u32) as usize;
                let blocked = mask.is_some_and(|m| m.cells[i]);
                let abs = if blocked {
                    0.0
                } else {
                    speed_map.speeds[i].clamp(0.0, MAX_SPEEDMOD)
                };
                let rel = abs / MAX_SPEEDMOD;
                let v = (rel * 255.0) as u8;
                changed |= self.speed_u8[i] != v;
                self.speed_u8[i] = v;
            }
        }
        changed
    }

    /// Terrain or structures changed in `rects` (squares): re-tesselate
    /// every 16-square block they touch whose squares changed, then
    /// relink the leaves around them once (`PathManager::UpdateNodeLayer`).
    /// Returns the squares whose nodes were rebuilt.
    pub fn update(
        &mut self,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        rects: impl IntoIterator<Item = Rect>,
    ) -> Option<Rect> {
        let d = DAMAGE_SIZE as i32;
        let mut rebuilt: Option<Rect> = None;
        for rect in rects {
            let rect = rect.clamped(self.width, self.height);
            if rect.is_empty() {
                continue;
            }
            for bz in (rect.z0 / d)..=((rect.z1 - 1) / d) {
                for bx in (rect.x0 / d)..=((rect.x1 - 1) / d) {
                    let block = Rect {
                        x0: bx * d,
                        z0: bz * d,
                        x1: (bx + 1) * d,
                        z1: (bz + 1) * d,
                    };
                    if !self.fill_squares(speed_map, mask, block) {
                        continue;
                    }
                    // `GetNodeThatEncasesPowerOfTwoArea`: the deepest
                    // node containing the block.
                    let node = self.encasing_node(block);
                    let re = self.nodes[node as usize].rect();
                    self.merge(node);
                    self.tesselate(node);
                    rebuilt = Some(rebuilt.map_or(re, |r| r.union(re)));
                }
            }
        }
        if let Some(re) = rebuilt {
            self.relink(re);
            self.min_move_cost = None;
        }
        rebuilt
    }

    fn root_at(&self, x: i32, z: i32) -> NodeId {
        let rs = self.root_size as i32;
        (z / rs) as u32 * self.x_roots + (x / rs) as u32
    }

    /// The leaf containing square `(x, z)` (clamped into the map).
    pub fn leaf_at(&self, x: i32, z: i32) -> NodeId {
        let x = x.clamp(0, self.width as i32 - 1);
        let z = z.clamp(0, self.height as i32 - 1);
        let mut cur = self.root_at(x, z);
        loop {
            let n = &self.nodes[cur as usize];
            if n.is_leaf() {
                return cur;
            }
            let right = x >= ((n.xmin + n.xmax) >> 1) as i32;
            let down = z >= ((n.zmin + n.zmax) >> 1) as i32;
            cur = n.child_base + right as u32 + 2 * down as u32;
        }
    }

    /// The deepest node whose rectangle contains `r`.
    fn encasing_node(&self, r: Rect) -> NodeId {
        let mut cur = self.root_at(r.x0, r.z0);
        loop {
            let n = &self.nodes[cur as usize];
            if n.is_leaf() {
                return cur;
            }
            let right = r.x0 >= ((n.xmin + n.xmax) >> 1) as i32;
            let down = r.z0 >= ((n.zmin + n.zmax) >> 1) as i32;
            let child = n.child_base + right as u32 + 2 * down as u32;
            let c = &self.nodes[child as usize];
            if r.x0 >= c.xmin as i32
                && r.x1 <= c.xmax as i32
                && r.z0 >= c.zmin as i32
                && r.z1 <= c.zmax as i32
            {
                cur = child;
            } else {
                return cur;
            }
        }
    }

    /// `QTNode::Tesselate`: set the node's cost and split while it mixes
    /// closed and open squares (or is larger than a damage block).
    fn tesselate(&mut self, node: NodeId) {
        let need_split = self.update_move_cost(node);
        let n = &self.nodes[node as usize];
        if need_split && n.xsize() > 1 && n.zsize() > 1 {
            self.split(node);
            let base = self.nodes[node as usize].child_base;
            for i in 0..4 {
                self.tesselate(base + i);
            }
        }
    }

    /// `QTNode::UpdateMoveCost`; returns `needSplit`.
    fn update_move_cost(&mut self, node: NodeId) -> bool {
        let n = &self.nodes[node as usize];
        let (xsize, zsize) = (n.xsize(), n.zsize());
        let area = (xsize * zsize) as f32;
        let mut closed = 0u32;
        let mut sum = 0.0f32;
        for z in n.zmin..n.zmax {
            let row = z as u32 * self.width;
            for x in n.xmin..n.xmax {
                let v = self.speed_u8[(row + x as u32) as usize];
                closed += (v == 0) as u32;
                sum += v as f32 / 255.0;
            }
        }
        let avg = sum / area;
        let mut cost = if avg <= 0.001 {
            f32::INFINITY
        } else {
            1.0 / avg
        };
        if closed > 0 {
            cost = if (closed as f32) < area {
                CLOSED_COST * closed as f32 / (xsize * xsize) as f32
            } else {
                f32::INFINITY
            };
        }
        self.nodes[node as usize].move_cost = cost;
        (closed > 0 && (closed as f32) < area) || xsize > DAMAGE_SIZE
    }

    fn split(&mut self, node: NodeId) {
        let n = self.nodes[node as usize].clone();
        let (xmid, zmid) = ((n.xmin + n.xmax) >> 1, (n.zmin + n.zmax) >> 1);
        let rects = [
            (n.xmin, n.zmin, xmid, zmid),
            (xmid, n.zmin, n.xmax, zmid),
            (n.xmin, zmid, xmid, n.zmax),
            (xmid, zmid, n.xmax, n.zmax),
        ];
        let base = match self.free_blocks.pop() {
            Some(b) => b,
            None => {
                let b = self.nodes.len() as NodeId;
                self.nodes.extend(std::iter::repeat_n(n.clone(), 4));
                b
            }
        };
        for (i, (xmin, zmin, xmax, zmax)) in rects.into_iter().enumerate() {
            self.nodes[base as usize + i] = Node {
                xmin,
                zmin,
                xmax,
                zmax,
                child_base: NONE,
                move_cost: f32::INFINITY,
                neighbors: Vec::new(),
            };
        }
        let n = &mut self.nodes[node as usize];
        n.child_base = base;
        n.neighbors.clear();
    }

    fn merge(&mut self, node: NodeId) {
        let base = self.nodes[node as usize].child_base;
        if base == NONE {
            return;
        }
        for i in 0..4 {
            self.merge(base + i);
            // Deactivate (`FreePoolNode` sets `_xmin = 0xFFFF`).
            self.nodes[(base + i) as usize].xmin = u16::MAX;
            self.nodes[(base + i) as usize].neighbors = Vec::new();
        }
        self.free_blocks.push(base);
        self.nodes[node as usize].child_base = NONE;
    }

    /// Every leaf overlapping `rect`.
    fn leaves_in(&self, rect: Rect, out: &mut Vec<NodeId>) {
        let rect = rect.clamped(self.width, self.height);
        if rect.is_empty() {
            return;
        }
        let rs = self.root_size as i32;
        for rz in (rect.z0 / rs)..=((rect.z1 - 1) / rs) {
            for rx in (rect.x0 / rs)..=((rect.x1 - 1) / rs) {
                let mut stack = vec![rz as u32 * self.x_roots + rx as u32];
                while let Some(id) = stack.pop() {
                    let n = &self.nodes[id as usize];
                    if !n.intersects(&rect) {
                        continue;
                    }
                    if n.is_leaf() {
                        out.push(id);
                    } else {
                        stack.extend((0..4).map(|i| n.child_base + i));
                    }
                }
            }
        }
    }

    /// `UpdateNeighborCache` for every leaf overlapping `rect` dilated
    /// by one square: edge neighbours along each side (passable leaves
    /// only), corner neighbours past two fully open edge leaves. Each
    /// transition point is the midpoint of the shared edge segment, or
    /// the shared corner.
    fn relink(&mut self, rect: Rect) {
        let mut leaves = Vec::new();
        self.leaves_in(rect.dilated(1), &mut leaves);
        for &id in &leaves {
            let n = self.nodes[id as usize].clone();
            let (xmin, zmin, xmax, zmax) =
                (n.xmin as i32, n.zmin as i32, n.xmax as i32, n.zmax as i32);
            let mut list = Vec::new();
            let side = |this: &Self,
                        list: &mut Vec<Neighbor>,
                        outside_x: Option<i32>,
                        outside_z: Option<i32>| {
                // Walk the edge, stepping by each neighbour's extent.
                match (outside_x, outside_z) {
                    (Some(x), None) => {
                        if x < 0 || x >= this.width as i32 {
                            return;
                        }
                        let mut z = zmin;
                        while z < zmax {
                            let ngb = this.leaf_at(x, z);
                            let nn = &this.nodes[ngb as usize];
                            if !nn.impassable() {
                                list.push(Neighbor {
                                    node: ngb,
                                    net: this.transition(&n, nn),
                                });
                            }
                            z = nn.zmax as i32;
                        }
                    }
                    (None, Some(z)) => {
                        if z < 0 || z >= this.height as i32 {
                            return;
                        }
                        let mut x = xmin;
                        while x < xmax {
                            let ngb = this.leaf_at(x, z);
                            let nn = &this.nodes[ngb as usize];
                            if !nn.impassable() {
                                list.push(Neighbor {
                                    node: ngb,
                                    net: this.transition(&n, nn),
                                });
                            }
                            x = nn.xmax as i32;
                        }
                    }
                    _ => {}
                }
            };
            side(self, &mut list, Some(xmin - 1), None);
            side(self, &mut list, Some(xmax), None);
            side(self, &mut list, None, Some(zmin - 1));
            side(self, &mut list, None, Some(zmax));
            // Corners: the diagonal leaf past two fully open edge leaves.
            for (cx, cz, ex, ez) in [
                (xmin - 1, zmin - 1, xmin, zmin),
                (xmax, zmin - 1, xmax - 1, zmin),
                (xmin - 1, zmax, xmin, zmax - 1),
                (xmax, zmax, xmax - 1, zmax - 1),
            ] {
                if cx < 0 || cz < 0 || cx >= self.width as i32 || cz >= self.height as i32 {
                    continue;
                }
                let corner = self.leaf_at(cx, cz);
                let along_x = self.leaf_at(cx, ez);
                let along_z = self.leaf_at(ex, cz);
                if corner == along_x || corner == along_z {
                    continue;
                }
                if !self.nodes[along_x as usize].accessible()
                    || !self.nodes[along_z as usize].accessible()
                {
                    continue;
                }
                let cn = &self.nodes[corner as usize];
                if cn.impassable() {
                    continue;
                }
                list.push(Neighbor {
                    node: corner,
                    net: self.transition(&n, cn),
                });
            }
            self.nodes[id as usize].neighbors = list;
        }
        self.open_leaves = self
            .nodes
            .iter()
            .filter(|n| n.xmin != u16::MAX && n.is_leaf() && !n.impassable())
            .count();
    }

    /// `GetNeighborEdgeTransitionPoint(ngb, .., alpha = 0.5)`: the
    /// midpoint of the shared edge segment, the shared vertex for a
    /// diagonal neighbour. Elmos.
    fn transition(&self, a: &Node, b: &Node) -> [f32; 2] {
        let left = a.xmin == b.xmax;
        let right = a.xmax == b.xmin;
        let top = a.zmin == b.zmax;
        let bottom = a.zmax == b.zmin;
        let minx = a.xmin.max(b.xmin) as f32;
        let maxx = a.xmax.min(b.xmax) as f32;
        let minz = a.zmin.max(b.zmin) as f32;
        let maxz = a.zmax.min(b.zmax) as f32;
        let (midx, midz) = ((minx + maxx) * 0.5, (minz + maxz) * 0.5);
        let p = match (left, right, top, bottom) {
            (true, _, true, _) => [a.xmin as f32, a.zmin as f32],
            (_, true, true, _) => [a.xmax as f32, a.zmin as f32],
            (_, true, _, true) => [a.xmax as f32, a.zmax as f32],
            (true, _, _, true) => [a.xmin as f32, a.zmax as f32],
            (_, _, true, _) => [midx, a.zmin as f32],
            (_, true, _, _) => [a.xmax as f32, midz],
            (_, _, _, true) => [midx, a.zmax as f32],
            _ => [a.xmin as f32, midz],
        };
        [p[0] * SQUARE_SIZE, p[1] * SQUARE_SIZE]
    }

    /// `PathSearch::GenerateHash`: the `(source leaf, goal leaf)` key
    /// under which a finished path from `src` to `dst` may be shared
    /// with later requests of the same class, or `None` when the source
    /// leaf is too small — narrower than `max(2, nextPow2(mover xsize))`
    /// squares — or the mover too large (`QTPFS_SHARE_PATH_MAX_SIZE`).
    pub fn share_key(
        &self,
        src: [f32; 2],
        dst: [f32; 2],
        mover_xsize: u32,
    ) -> Option<(NodeId, NodeId)> {
        let sq = |p: [f32; 2]| ((p[0] / SQUARE_SIZE) as i32, (p[1] / SQUARE_SIZE) as i32);
        let (sx, sz) = sq(src);
        let (tx, tz) = sq(dst);
        let src_leaf = self.leaf_at(sx, sz);
        let min_size = mover_xsize.next_power_of_two().max(2);
        if min_size > 16 || self.nodes[src_leaf as usize].xsize() < min_size {
            return None;
        }
        Some((src_leaf, self.leaf_at(tx, tz)))
    }

    /// `hCostMult`: the cheapest leaf's move cost.
    fn min_move_cost(&mut self) -> f32 {
        *self.min_move_cost.get_or_insert_with(|| {
            self.nodes
                .iter()
                .filter(|n| n.xmin != u16::MAX && n.is_leaf())
                .map(|n| n.move_cost)
                .fold(f32::INFINITY, f32::min)
                .max(1e-3)
        })
    }

    /// `GetNearestNodeInArea`: the passable leaf in `rect` nearest `ref`
    /// (squares), by the engine's score (closest of mid and corners,
    /// then mid distance).
    fn nearest_open_leaf(&self, rect: Rect, rx: i32, rz: i32) -> Option<NodeId> {
        let mut leaves = Vec::new();
        self.leaves_in(rect, &mut leaves);
        leaves
            .into_iter()
            .filter(|&id| !self.nodes[id as usize].impassable())
            .map(|id| {
                let n = &self.nodes[id as usize];
                let (mx, mz) = (
                    ((n.xmin + n.xmax) >> 1) as i64,
                    ((n.zmin + n.zmax) >> 1) as i64,
                );
                let d2 = |x: i64, z: i64| (x - rx as i64).pow(2) + (z - rz as i64).pow(2);
                let mid = d2(mx, mz);
                let corners = [
                    d2(n.xmin as i64, n.zmin as i64),
                    d2(n.xmax as i64, n.zmin as i64),
                    d2(n.xmin as i64, n.zmax as i64),
                    d2(n.xmax as i64, n.zmax as i64),
                ];
                let closest = corners.iter().copied().fold(mid, i64::min);
                ((closest << 32) + (mid << 2), id)
            })
            .min()
            .map(|(_, id)| id)
    }
}

/// A finished search (`IPath`), in elmos.
#[derive(Clone, Debug)]
pub struct QtPath {
    /// `[source, edge midpoints…, target]` (y is the caller's business).
    pub points: Vec<[f32; 2]>,
    /// `haveFullPath`: the last point is the goal (or lies inside its
    /// radius). `false` for a partial path to the node closest to an
    /// unreachable goal, and for a goal redirected out of a closed node.
    pub full: bool,
    /// `repathAtPointIndex`: re-search once this waypoint is reached.
    pub repath_at: Option<usize>,
    /// Square rectangle of the leaf each point was traced through (the
    /// leaf containing the point for the ends, the entered leaf for the
    /// edge points), for the dirty test when terrain changes.
    pub node_rects: Vec<Rect>,
}

#[derive(Clone, Copy)]
struct SearchNode {
    generation: u32,
    g: f32,
    h: f32,
    f: f32,
    prev: NodeId,
    net: [f32; 2],
}

impl SearchNode {
    const EMPTY: SearchNode = SearchNode {
        generation: 0,
        g: f32::INFINITY,
        h: f32::INFINITY,
        f: f32::INFINITY,
        prev: NONE,
        net: [0.0; 2],
    };
}

#[derive(Clone, Copy)]
struct HeapEntry {
    priority: f32,
    node: NodeId,
}

impl PartialEq for HeapEntry {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for HeapEntry {}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for HeapEntry {
    /// Lowest priority first, then the lower node index (a max-heap).
    fn cmp(&self, o: &Self) -> Ordering {
        o.priority
            .total_cmp(&self.priority)
            .then_with(|| o.node.cmp(&self.node))
    }
}

/// Per-direction state (`DirectionalSearchData`).
struct Dir {
    heap: BinaryHeap<HeapEntry>,
    src: NodeId,
    tgt: NodeId,
    src_pt: [f32; 2],
    tgt_pt: [f32; 2],
    min_node: NodeId,
    searched: usize,
    connected: bool,
}

/// Reusable per-node search state for both directions
/// (`allSearchedNodes`), generation-tagged so nothing is cleared.
#[derive(Default)]
pub struct QtScratch {
    fwd: Vec<SearchNode>,
    bwd: Vec<SearchNode>,
    generation: u32,
}

impl QtScratch {
    fn state(&mut self, fwd: bool) -> &mut Vec<SearchNode> {
        if fwd { &mut self.fwd } else { &mut self.bwd }
    }
    fn get(&mut self, fwd: bool, id: NodeId) -> &mut SearchNode {
        let generation = self.generation;
        let v = self.state(fwd);
        let s = &mut v[id as usize];
        if s.generation != generation {
            *s = SearchNode {
                generation,
                ..SearchNode::EMPTY
            };
        }
        s
    }
    fn touched(&self, fwd: bool, id: NodeId) -> bool {
        let v = if fwd { &self.fwd } else { &self.bwd };
        v[id as usize].generation == self.generation
    }
    /// `IsNodeActive`: reached by its search (not merely allocated).
    fn active(&self, fwd: bool, id: NodeId) -> bool {
        let v = if fwd { &self.fwd } else { &self.bwd };
        let s = &v[id as usize];
        s.generation == self.generation && (s.prev != NONE || s.h != f32::INFINITY)
    }
}

/// One path search in flight (`QTPFS::PathSearch`), resumable in
/// bounded steps.
pub struct QtSearch {
    fwd: Dir,
    bwd: Dir,
    goal: [f32; 2],
    goal_dist: f32,
    h_mult: f32,
    limit: usize,
    limit_lowered: bool,
    bad_goal: bool,
    full: bool,
    use_fwd_only: bool,
    src_impassable: bool,
    /// A repair search expands no node outside this rectangle.
    bounds: Option<Rect>,
}

impl QtSearch {
    /// `Initialize` + `InitializeThread` + `InitStartingSearchNodes`:
    /// a search from `src` to `dst` (elmos) for a goal radius of
    /// `goal_dist`. `Err(path)` is a path that needed no search: the
    /// straight line when it is clear (`ExecuteRawSearch`, owned units
    /// try that first) or source and goal in one leaf.
    #[allow(clippy::result_large_err)]
    pub fn begin(
        layer: &mut NodeLayer,
        scratch: &mut QtScratch,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        src: [f32; 2],
        dst: [f32; 2],
        goal_dist: f32,
    ) -> Result<QtSearch, QtPath> {
        Self::begin_inner(layer, scratch, speed_map, mask, src, dst, goal_dist, None)
    }

    /// `DoRawSearch` alone: the two-point path when the straight line
    /// `src → dst` is clear.
    pub fn raw_path(
        layer: &NodeLayer,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        src: [f32; 2],
        dst: [f32; 2],
    ) -> Option<QtPath> {
        let sq = |p: [f32; 2]| ((p[0] / SQUARE_SIZE) as i32, (p[1] / SQUARE_SIZE) as i32);
        let (sx, sz) = sq(src);
        line_clear(src, dst, speed_map, mask)
            .then(|| finish_raw(layer, src, dst, layer.leaf_at(sx, sz)))
    }

    /// Path repair (`LoadRepairPath`): a search from `src` to `target`,
    /// the first clean point of a dirtied path, confined to the square
    /// box around them (`mid ± max(|dx|, |dz|)`) and without the goal
    /// radius exit. A result that is not `full` means the repair failed
    /// and the whole path must be searched again.
    pub fn begin_repair(
        layer: &mut NodeLayer,
        scratch: &mut QtScratch,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        src: [f32; 2],
        target: [f32; 2],
    ) -> Result<QtSearch, QtPath> {
        let mid = [(src[0] + target[0]) * 0.5, (src[1] + target[1]) * 0.5];
        let r = ((target[0] - src[0]).abs()).max((target[1] - src[1]).abs()) * 0.5;
        let bounds = Rect {
            x0: ((mid[0] - r) / SQUARE_SIZE).floor() as i32,
            z0: ((mid[1] - r) / SQUARE_SIZE).floor() as i32,
            x1: ((mid[0] + r) / SQUARE_SIZE).floor() as i32 + 1,
            z1: ((mid[1] + r) / SQUARE_SIZE).floor() as i32 + 1,
        };
        Self::begin_inner(
            layer,
            scratch,
            speed_map,
            mask,
            src,
            target,
            -1.0,
            Some(bounds),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn begin_inner(
        layer: &mut NodeLayer,
        scratch: &mut QtScratch,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        src: [f32; 2],
        dst: [f32; 2],
        goal_dist: f32,
        bounds: Option<Rect>,
    ) -> Result<QtSearch, QtPath> {
        let clamp = |p: [f32; 2]| {
            [
                p[0].clamp(0.0, (layer.width as f32 - 1.0) * SQUARE_SIZE),
                p[1].clamp(0.0, (layer.height as f32 - 1.0) * SQUARE_SIZE),
            ]
        };
        let (src, dst) = (clamp(src), clamp(dst));
        let sq = |p: [f32; 2]| ((p[0] / SQUARE_SIZE) as i32, (p[1] / SQUARE_SIZE) as i32);
        let (sx, sz) = sq(src);
        let (tx, tz) = sq(dst);
        let src_node = layer.leaf_at(sx, sz);
        let mut tgt_node = layer.leaf_at(tx, tz);

        // Raw search: the straight line, footprint and terrain tested.
        if line_clear(src, dst, speed_map, mask) {
            return Err(finish_raw(layer, src, dst, src_node));
        }
        // A goal inside a closed node is moved to the nearest open leaf
        // within `max(goalDist / 8, 16)` squares.
        let mut bad_goal = false;
        if layer.nodes[tgt_node as usize].impassable() {
            let w = ((goal_dist / SQUARE_SIZE) as i32).max(16);
            let t = layer.nodes[tgt_node as usize].rect().dilated(w);
            if let Some(alt) = layer.nearest_open_leaf(t, tx, tz) {
                tgt_node = alt;
                bad_goal = true;
            }
        }
        // Source and goal in one leaf: `haveFullPath` with two points.
        if src_node == tgt_node && !bad_goal {
            return Err(finish_raw(layer, src, dst, src_node));
        }

        let n = layer.nodes.len();
        scratch.fwd.resize(n, SearchNode::EMPTY);
        scratch.bwd.resize(n, SearchNode::EMPTY);
        scratch.generation = scratch.generation.wrapping_add(1).max(1);
        let h_mult = layer.min_move_cost();
        // Per-direction expansion budget (`InitStartingSearchNodes`).
        let limit =
            MAX_NODES_SEARCHED.max((layer.open_leaves as f32 * MAX_NODES_RELATIVE) as usize) >> 1;
        // The backward search starts from the nearest point of the
        // (possibly redirected) target node to the goal.
        let bwd_src_pt = if bad_goal {
            let mid = layer.nodes[tgt_node as usize].mid();
            nearest_point_on_node(&layer.nodes[tgt_node as usize], mid, dst)
        } else {
            dst
        };
        let mut search = QtSearch {
            fwd: Dir {
                heap: BinaryHeap::new(),
                src: src_node,
                tgt: tgt_node,
                src_pt: src,
                tgt_pt: dst,
                min_node: src_node,
                searched: 0,
                connected: false,
            },
            bwd: Dir {
                heap: BinaryHeap::new(),
                src: tgt_node,
                tgt: src_node,
                src_pt: bwd_src_pt,
                tgt_pt: src,
                min_node: tgt_node,
                searched: 0,
                connected: false,
            },
            goal: dst,
            goal_dist,
            h_mult,
            limit,
            limit_lowered: false,
            bad_goal,
            full: false,
            use_fwd_only: false,
            src_impassable: layer.nodes[src_node as usize].impassable(),
            bounds,
        };
        for fwd in [true, false] {
            let dir = if fwd {
                &mut search.fwd
            } else {
                &mut search.bwd
            };
            let s = scratch.get(fwd, dir.src);
            s.g = 0.0;
            s.h = dist(dir.src_pt, dir.tgt_pt) * h_mult;
            s.f = s.h;
            s.net = dir.src_pt;
            dir.heap.push(HeapEntry {
                priority: 0.0,
                node: dir.src,
            });
        }
        Ok(search)
    }

    /// Run up to `max_iters` iterations (one forward and one backward
    /// expansion each); `Some` when finished. `None` inside the `Some`
    /// means no path at all (the source leaf is closed and nothing was
    /// reached).
    pub fn step(
        &mut self,
        layer: &NodeLayer,
        scratch: &mut QtScratch,
        max_iters: usize,
    ) -> Option<Option<QtPath>> {
        let mut iters = 0;
        while !self.fwd.heap.is_empty() && iters < max_iters {
            iters += 1;
            self.expand_step(layer, scratch, true);
            if !self.bwd.heap.is_empty() {
                self.expand_step(layer, scratch, false);
            }
            if self.bwd.heap.is_empty() && !self.bwd.connected && !self.limit_lowered {
                self.lower_node_limit(layer);
            }
        }
        if !self.fwd.heap.is_empty() {
            return None;
        }
        Some(self.finish(layer, scratch))
    }

    /// `SetNodeSearchLimit`: the goal region turned out enclosed, so the
    /// forward budget shrinks with the straight-line distance.
    fn lower_node_limit(&mut self, layer: &NodeLayer) {
        self.limit_lowered = true;
        let leaves = layer.leaf_count().max(1) as f32;
        let avg_node_len = ((layer.width * layer.height) as f32 / leaves).sqrt()
            * (1.0 + std::f32::consts::SQRT_2)
            * 0.5
            * SQUARE_SIZE;
        let max_dist = ((MAX_NODES_SEARCHED >> 1) as f32).sqrt() * 2.0 * avg_node_len;
        let t = (dist(self.fwd.src_pt, self.fwd.tgt_pt) / max_dist).clamp(0.0, 1.0);
        let limit =
            ((MAX_NODES_SEARCHED >> 1) as f32 * (1.0 - (t - 1.0) * (t - 1.0)).sqrt()) as usize;
        self.limit = limit.max(256);
    }

    /// One `IterateNodes` of direction `fwd` plus the meeting test.
    fn expand_step(&mut self, layer: &NodeLayer, scratch: &mut QtScratch, fwd: bool) {
        // Pop, dropping stale entries (`RemoveOutdatedOpenNodesFromQueue`).
        let cur = loop {
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            let Some(e) = dir.heap.pop() else { return };
            if e.priority <= scratch.get(fwd, e.node).f {
                break e.node;
            }
        };
        let adjusted_goal = self.goal_dist * self.h_mult;
        {
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            dir.searched += 1;
        }
        let other_active = scratch.active(!fwd, cur);
        let other_touched = scratch.touched(!fwd, cur);
        let cur_state = *scratch.get(fwd, cur);
        // A repair search never expands outside its box.
        let in_bounds = self
            .bounds
            .is_none_or(|b| layer.nodes[cur as usize].intersects(&b));
        if !other_active && in_bounds {
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            if cur_state.h < scratch.get(fwd, dir.min_node).h {
                dir.min_node = cur;
            }
            let at_goal = fwd && cur_state.h <= adjusted_goal;
            if !at_goal {
                self.expand(layer, scratch, fwd, cur, cur_state);
            }
        }
        {
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            if dir.searched >= self.limit {
                dir.heap.clear();
            }
        }
        if other_touched {
            // Met the other search.
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            dir.connected = other_active;
            if other_active {
                self.full = true;
                let other_prev = scratch.get(!fwd, cur).prev;
                let other_net = scratch.get(!fwd, cur).net;
                if fwd {
                    if cur_state.prev == NONE {
                        self.bwd.tgt = other_prev;
                        self.fwd.tgt = cur;
                        self.bwd.tgt_pt = other_net;
                    } else {
                        self.fwd.tgt = cur_state.prev;
                        self.bwd.tgt = cur;
                        self.bwd.tgt_pt = cur_state.net;
                    }
                    self.bwd.heap.clear();
                } else {
                    if cur_state.prev == NONE {
                        self.use_fwd_only = true;
                        self.fwd.tgt = cur;
                    } else {
                        self.bwd.tgt = cur_state.prev;
                        self.fwd.tgt = cur;
                        self.bwd.tgt_pt = cur_state.net;
                    }
                    self.fwd.heap.clear();
                }
            }
        } else if fwd && cur_state.h <= adjusted_goal {
            // Inside the goal radius: done with the forward path alone.
            self.use_fwd_only = true;
            self.full = true;
            self.bad_goal = false;
            self.fwd.tgt = cur;
            self.fwd.heap.clear();
            self.bwd.heap.clear();
        }
    }

    /// `IterateNodeNeighbors`.
    fn expand(
        &mut self,
        layer: &NodeLayer,
        scratch: &mut QtScratch,
        fwd: bool,
        cur: NodeId,
        cur_state: SearchNode,
    ) {
        let node = &layer.nodes[cur as usize];
        let cur_cost = if node.impassable() {
            CLOSED_COST
        } else {
            node.move_cost
        };
        let (tgt, tgt_pt) = {
            let dir = if fwd { &self.fwd } else { &self.bwd };
            (dir.tgt, dir.tgt_pt)
        };
        for ngb in &node.neighbors {
            if ngb.node == cur_state.prev {
                continue;
            }
            let g_dist = dist(cur_state.net, ngb.net);
            let h_dist = dist(tgt_pt, ngb.net);
            let mut g = cur_state.g + cur_cost * g_dist;
            let is_target = ngb.node == tgt;
            let h = if is_target { 0.0 } else { h_dist * self.h_mult };
            if is_target {
                g += layer.nodes[ngb.node as usize].move_cost * h_dist;
            }
            let nxt = scratch.get(fwd, ngb.node);
            if g >= nxt.g {
                continue;
            }
            nxt.prev = cur;
            nxt.g = g;
            nxt.h = h;
            nxt.f = g + h;
            nxt.net = ngb.net;
            let dir = if fwd { &mut self.fwd } else { &mut self.bwd };
            dir.heap.push(HeapEntry {
                priority: g + h,
                node: ngb.node,
            });
        }
    }

    /// The post-loop of `ExecutePathSearch`, `TracePath` and `SmoothPath`.
    fn finish(&mut self, layer: &NodeLayer, scratch: &mut QtScratch) -> Option<QtPath> {
        if !self.full {
            // Partial: toward the node that came closest to the goal.
            let have_part = self.fwd.min_node != self.fwd.src || !self.src_impassable;
            if !have_part {
                return None;
            }
            self.fwd.tgt = self.fwd.min_node;
            self.use_fwd_only = true;
            let net = scratch.get(true, self.fwd.tgt).net;
            self.fwd.tgt_pt =
                nearest_point_on_node(&layer.nodes[self.fwd.tgt as usize], net, self.goal);
        } else if self.bad_goal {
            self.fwd.tgt_pt = self.bwd.src_pt;
        } else if self.use_fwd_only {
            let net = scratch.get(true, self.fwd.tgt).net;
            self.fwd.tgt_pt =
                nearest_point_on_node(&layer.nodes[self.fwd.tgt as usize], net, self.goal);
        }

        // TracePath: nodes in path order; points[0] = source, points[i]
        // = edge into nodes[i], points[last] = target in nodes[last].
        let mut nodes: Vec<NodeId> = Vec::new();
        let mut points: Vec<[f32; 2]> = Vec::new();
        let mut tmp = self.fwd.tgt;
        while tmp != self.fwd.src && tmp != NONE {
            let s = *scratch.get(true, tmp);
            if s.prev == NONE {
                break;
            }
            nodes.push(tmp);
            points.push(s.net);
            tmp = s.prev;
        }
        nodes.push(self.fwd.src);
        points.push(self.fwd.src_pt);
        nodes.reverse();
        points.reverse();
        if self.full && !self.use_fwd_only {
            let mut prv_point = self.bwd.tgt_pt;
            let mut node = self.bwd.tgt;
            while node != NONE {
                let s = *scratch.get(false, node);
                nodes.push(node);
                points.push(prv_point);
                prv_point = s.net;
                node = s.prev;
            }
        }
        points.push(self.fwd.tgt_pt);
        debug_assert_eq!(points.len(), nodes.len() + 1);

        if points.len() > 2 {
            smooth(layer, &mut points, &nodes);
        }
        let node_rects: Vec<Rect> = nodes
            .iter()
            .map(|&n| layer.nodes[n as usize].rect())
            .collect();
        let mut node_rects = node_rects;
        node_rects.push(*node_rects.last().expect("at least the source leaf"));
        let full = self.full && !self.bad_goal;
        let repath_at = if full { None } else { repath_trigger(&points) };
        Some(QtPath {
            points,
            full,
            repath_at,
            node_rects,
        })
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// A two-point path (a clear straight line, or source and goal in one
/// leaf).
fn finish_raw(layer: &NodeLayer, src: [f32; 2], dst: [f32; 2], src_node: NodeId) -> QtPath {
    let r = layer.nodes[src_node as usize].rect();
    QtPath {
        points: vec![src, dst],
        full: true,
        repath_at: None,
        node_rects: vec![r, r],
    }
}

/// `FindNearestPointOnNodeToGoal`: the goal itself when it lies in the
/// node, else where the segment goal → entry point enters the node's
/// rectangle, else the entry point.
fn nearest_point_on_node(node: &Node, net: [f32; 2], goal: [f32; 2]) -> [f32; 2] {
    if net == goal {
        return goal;
    }
    let (x0, z0) = (
        node.xmin as f32 * SQUARE_SIZE,
        node.zmin as f32 * SQUARE_SIZE,
    );
    let (x1, z1) = (
        node.xmax as f32 * SQUARE_SIZE,
        node.zmax as f32 * SQUARE_SIZE,
    );
    let inside = |p: [f32; 2]| p[0] >= x0 && p[0] <= x1 && p[1] >= z0 && p[1] <= z1;
    if inside(goal) {
        return goal;
    }
    // Slab clip of goal → net against the box; the entry parameter.
    let d = [net[0] - goal[0], net[1] - goal[1]];
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for (p, dd, lo, hi) in [(goal[0], d[0], x0, x1), (goal[1], d[1], z0, z1)] {
        if dd.abs() < 1e-6 {
            if p < lo || p > hi {
                return net;
            }
            continue;
        }
        let (mut a, mut b) = ((lo - p) / dd, (hi - p) / dd);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        t0 = t0.max(a);
        t1 = t1.min(b);
        if t0 > t1 {
            return net;
        }
    }
    [goal[0] + d[0] * t0, goal[1] + d[1] * t0]
}

/// `SmoothPathIter` (one pass, from the goal end): each interior point
/// slides along its edge toward the straight line between its
/// neighbours, staying on the segment the two leaves share.
fn smooth(layer: &NodeLayer, points: &mut [[f32; 2]], nodes: &[NodeId]) {
    for i in (1..points.len() - 1).rev() {
        let (p0, p1, p2) = (points[i + 1], points[i], points[i - 1]);
        let nn0 = layer.nodes[nodes[i] as usize].rect();
        let nn1 = layer.nodes[nodes[i - 1] as usize].rect();
        if let Some(pi) = smooth_point(nn0, nn1, p0, p1, p2) {
            points[i] = pi;
        }
    }
}

/// `PathSearch::SharedFinalize` + `SmoothSharedPath`: a later request
/// with the same source and goal leaves takes `head`'s points, starts
/// them at its own `src`, ends them at its own `dst` when the head
/// reached its goal, and smooths the first corner once.
pub fn shared_path(head: &QtPath, src: [f32; 2], dst: [f32; 2]) -> QtPath {
    let mut path = head.clone();
    path.points[0] = src;
    if path.full
        && let Some(last) = path.points.last_mut()
    {
        *last = dst;
    }
    if path.points.len() > 2 {
        let (p0, p1, p2) = (path.points[2], path.points[1], path.points[0]);
        if let Some(pi) = smooth_point(path.node_rects[1], path.node_rects[0], p0, p1, p2) {
            path.points[1] = pi;
        }
    }
    path
}

fn norm(v: [f32; 2]) -> [f32; 2] {
    let l = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if l > 0.0 { [v[0] / l, v[1] / l] } else { v }
}

fn sub(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn dot(a: [f32; 2], b: [f32; 2]) -> f32 {
    a[0] * b[0] + a[1] * b[1]
}

/// Join a finished repair search (ending at a dirtied path's first clean
/// point) with that path's clean remainder.
pub fn splice_repair(
    mut repaired: QtPath,
    tail_points: &[[f32; 2]],
    tail_rects: &[Rect],
    full: bool,
    repath_at: Option<usize>,
) -> QtPath {
    let joint = repaired.points.len() - 1;
    repaired.points.extend_from_slice(&tail_points[1..]);
    repaired.node_rects.extend_from_slice(&tail_rects[1..]);
    repaired.full = full;
    repaired.repath_at = repath_at.map(|i| i + joint);
    repaired
}

/// `SmoothPathPoints`: `nn0` holds the segment `p1 → p0`, `nn1` the
/// segment `p2 → p1`; `p1` lies on their shared edge.
fn smooth_point(
    nn0: Rect,
    nn1: Rect,
    p0: [f32; 2],
    p1: [f32; 2],
    p2: [f32; 2],
) -> Option<[f32; 2]> {
    let d_in = norm(sub(p1, p0));
    let d_out = norm(sub(p2, p1));
    let dotp = dot(d_in, d_out);
    if dotp >= 0.995 {
        return None;
    }
    let h_edge = nn0.z0 == nn1.z1 || nn0.z1 == nn1.z0;
    let v_edge = nn0.x0 == nn1.x1 || nn0.x1 == nn1.x0;
    let xmin = nn0.x0.max(nn1.x0) as f32 * SQUARE_SIZE;
    let xmax = nn0.x1.min(nn1.x1) as f32 * SQUARE_SIZE;
    let zmin = nn0.z0.max(nn1.z0) as f32 * SQUARE_SIZE;
    let zmax = nn0.z1.min(nn1.z1) as f32 * SQUARE_SIZE;
    let d = norm(sub(p2, p0));
    let dfx = if d[0] > 0.0 {
        nn0.x1 as f32 * SQUARE_SIZE - p0[0]
    } else {
        nn0.x0 as f32 * SQUARE_SIZE - p0[0]
    };
    let dfz = if d[1] > 0.0 {
        nn0.z1 as f32 * SQUARE_SIZE - p0[1]
    } else {
        nn0.z0 as f32 * SQUARE_SIZE - p0[1]
    };
    let dx = if d[0].abs() > 0.001 { d[0] } else { 0.001 };
    let dz = if d[1].abs() > 0.001 { d[1] } else { 0.001 };
    let (tx, tz) = (dfx / dx, dfz / dz);
    let mut pi = p1;
    if h_edge {
        pi = [p0[0] + d[0] * tz, p1[1]];
    }
    if v_edge {
        pi = [p1[0], p0[1] + d[1] * tx];
    }
    let moved =
        |q: [f32; 2]| ((q[0] - p1[0]).powi(2) + (q[1] - p1[1]).powi(2) > 0.05 * 0.05).then_some(q);
    if pi[0] >= xmin && pi[0] <= xmax && pi[1] >= zmin && pi[1] <= zmax {
        return moved(pi);
    }
    if h_edge != v_edge {
        let (e0, e1) = if h_edge {
            ([xmin, p1[1]], [xmax, p1[1]])
        } else {
            ([p1[0], zmin], [p1[0], zmax])
        };
        let d0 = dot(norm(sub(e0, p0)), norm(sub(p2, e0)));
        let d1 = dot(norm(sub(e1, p0)), norm(sub(p2, e1)));
        if dotp >= d0.max(d1) {
            return None;
        }
        let pick = if d1 >= d0.max(dotp) { e1 } else { e0 };
        return moved(pick);
    }
    None
}

/// The repath trigger of a partial path (`TracePath`): on paths of at
/// least `qtRefreshPathMinDist`, the earliest point past half the
/// distance (or the last one before it), never before the third point.
fn repath_trigger(points: &[[f32; 2]]) -> Option<usize> {
    let n = points.len();
    if n < 3 {
        return None;
    }
    let mut dists = Vec::with_capacity(n);
    let mut acc = 0.0;
    dists.push(0.0);
    for i in 1..n {
        acc += dist(points[i - 1], points[i]);
        dists.push(acc);
    }
    // `pathDist` excludes the final segment to the target point.
    let path_dist = dists[n - 2];
    if path_dist < REFRESH_PATH_MIN_DIST {
        return None;
    }
    let quarter = REFRESH_PATH_MIN_DIST / 4.0;
    let mut chosen = None;
    for i in (0..n - 1).rev() {
        let d = dists[i];
        if d > path_dist - quarter {
            continue;
        }
        if d < quarter || i <= 2 || (chosen.is_some() && d < path_dist / 2.0) {
            break;
        }
        chosen = Some(i);
    }
    chosen
}

/// `NextWayPoint`'s first call: the first point not yet behind the unit
/// — within `8 · 8 · 1.42` elmos² of it, or whose segment the unit is
/// beside (`v0 · v1 <= 0`) — then one further. Points are indexed from
/// the source (index 0).
pub fn first_waypoint(points: &[[f32; 2]], pos: [f32; 2]) -> usize {
    const R: f32 = SQUARE_SIZE * SQUARE_SIZE * 1.42;
    let last = points.len() - 1;
    let mut next = 1;
    for i in 0..last {
        let v0 = sub(points[i], pos);
        let v1 = sub(points[i + 1], pos);
        if dot(v0, v0) < R {
            next = i + 1;
            break;
        }
        if dot(v1, v1) < R {
            next = i + 2;
            break;
        }
        if dot(v0, v1) <= 0.0 {
            next = i + 1;
            break;
        }
    }
    next.min(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32) -> SpeedMap {
        SpeedMap::uniform(w, h, 1.0)
    }

    fn search(
        layer: &mut NodeLayer,
        map: &SpeedMap,
        mask: Option<&BlockMask>,
        a: [f32; 2],
        b: [f32; 2],
    ) -> Option<QtPath> {
        let mut scratch = QtScratch::default();
        match QtSearch::begin(layer, &mut scratch, map, mask, a, b, 8.0) {
            Err(p) => Some(p),
            Ok(mut s) => s
                .step(layer, &mut scratch, usize::MAX)
                .expect("unbounded step finishes"),
        }
    }

    fn path_len(p: &QtPath) -> f32 {
        p.points.windows(2).map(|w| dist(w[0], w[1])).sum()
    }

    /// A flat 128×64 map: 2 roots of 64, each split to four 32s and
    /// sixteen 16s (everything above 16 is force-split), nothing below.
    #[test]
    fn flat_map_tesselates_to_sixteens() {
        let map = flat(128, 64);
        let layer = NodeLayer::new(&map, None);
        assert_eq!(layer.root_size, 64);
        assert_eq!(layer.leaf_count(), 32);
        let leaf = &layer.nodes[layer.leaf_at(5, 5) as usize];
        assert_eq!((leaf.xsize(), leaf.zsize()), (16, 16));
        // Full speed 1.0 is relative 0.5 → move cost ≈ 2.
        assert!((leaf.move_cost - 255.0 / 127.0).abs() < 1e-3);
        // Interior leaves have 8 neighbours (4 edges + 4 corners).
        let inner = &layer.nodes[layer.leaf_at(40, 40) as usize];
        assert_eq!(inner.neighbors.len(), 8);
    }

    /// A one-square wall splits its blocks down to single squares along
    /// it; removing it merges them back.
    #[test]
    fn wall_splits_to_squares_and_merges_back() {
        let mut map = flat(64, 64);
        for z in 0..64 {
            map.speeds[(z * 64 + 30) as usize] = 0.0;
        }
        let mut layer = NodeLayer::new(&map, None);
        let wall = &layer.nodes[layer.leaf_at(30, 10) as usize];
        assert_eq!((wall.xsize(), wall.zsize()), (1, 1));
        assert!(wall.impassable());
        let before = layer.leaf_count();
        assert!(before > 16);
        for z in 0..64 {
            map.speeds[(z * 64 + 30) as usize] = 1.0;
        }
        layer.update(
            &map,
            None,
            [Rect {
                x0: 30,
                z0: 0,
                x1: 31,
                z1: 64,
            }],
        );
        assert_eq!(layer.leaf_count(), 16);
        assert!(!layer.free_blocks.is_empty());
    }

    /// Open ground: the raw search answers with the straight line.
    #[test]
    fn straight_line_when_clear() {
        let map = flat(64, 64);
        let mut layer = NodeLayer::new(&map, None);
        let p = search(&mut layer, &map, None, [20.0, 20.0], [400.0, 300.0]).unwrap();
        assert_eq!(p.points.len(), 2);
        assert!(p.full);
    }

    /// Around a wall with a gap, the path goes through the gap and is
    /// not much longer than the two straight legs.
    #[test]
    fn routes_through_the_gap_in_a_wall() {
        let mut map = flat(64, 64);
        for z in 0..64 {
            if !(28..32).contains(&z) {
                map.speeds[(z * 64 + 32) as usize] = 0.0;
            }
        }
        let mut layer = NodeLayer::new(&map, None);
        let (a, b) = ([100.0, 100.0], [400.0, 100.0]);
        let p = search(&mut layer, &map, None, a, b).unwrap();
        assert!(p.full, "{p:?}");
        assert_eq!(*p.points.last().unwrap(), b);
        // Passes the gap (x = 256..264) at z inside 224..256.
        let through = p
            .points
            .iter()
            .any(|q| (248.0..=272.0).contains(&q[0]) && (220.0..=260.0).contains(&q[1]));
        assert!(through, "{:?}", p.points);
        let gap = [260.0, 240.0];
        let legs = dist(a, gap) + dist(gap, b);
        assert!(path_len(&p) < legs * 1.15, "{} vs {legs}", path_len(&p));
        // Every point lies on open ground (points may sit on square
        // corners; `UpdatePos` handles those when following).
        for q in &p.points {
            assert!(
                map.get((q[0] / 8.0) as u32, (q[1] / 8.0) as u32) > 0.0,
                "{q:?}"
            );
        }
    }

    /// An enclosed goal gives a partial path ending in the open leaf
    /// nearest it, flagged not full.
    #[test]
    fn enclosed_goal_gives_a_partial_path() {
        let mut map = flat(64, 64);
        for i in 20..44 {
            for (x, z) in [(i, 20), (i, 43), (20, i), (43, i)] {
                map.speeds[(z * 64 + x) as usize] = 0.0;
            }
        }
        let mut layer = NodeLayer::new(&map, None);
        let p = search(&mut layer, &map, None, [40.0, 40.0], [256.0, 256.0]).unwrap();
        assert!(!p.full);
        let end = *p.points.last().unwrap();
        let inside = (160.0..352.0).contains(&end[0]) && (160.0..352.0).contains(&end[1]);
        assert!(!inside, "stops outside the box: {end:?}");
        assert!(dist(end, [256.0, 256.0]) < 150.0, "but near it: {end:?}");
    }

    /// A goal inside a closed node is moved to the nearest open leaf.
    #[test]
    fn goal_in_closed_node_is_redirected() {
        let mut map = flat(64, 64);
        for z in 24..40 {
            for x in 24..40 {
                map.speeds[(z * 64 + x) as usize] = 0.0;
            }
        }
        let mut layer = NodeLayer::new(&map, None);
        let p = search(&mut layer, &map, None, [40.0, 40.0], [256.0, 256.0]).unwrap();
        assert!(!p.full, "redirected goals are not full paths");
        let end = *p.points.last().unwrap();
        assert!(
            end[0] <= 192.0 || end[1] <= 192.0 || end[0] >= 320.0 || end[1] >= 320.0,
            "{end:?}"
        );
    }

    /// Sharing: a second request from the same source leaf to the same
    /// goal leaf reuses the head's points with its own ends; a source
    /// leaf narrower than the mover's power-of-two footprint cannot share.
    #[test]
    fn shared_paths_take_the_heads_route_with_own_ends() {
        let mut map = flat(64, 64);
        for z in 0..64 {
            if !(28..32).contains(&z) {
                map.speeds[(z * 64 + 32) as usize] = 0.0;
            }
        }
        let mut layer = NodeLayer::new(&map, None);
        let (a, b) = ([100.0, 100.0], [400.0, 100.0]);
        let head = search(&mut layer, &map, None, a, b).unwrap();
        let key = layer
            .share_key(a, b, 3)
            .expect("16-square source leaf shares");
        assert_eq!(
            key,
            layer.share_key([110.0, 90.0], [410.0, 95.0], 3).unwrap()
        );
        let shared = shared_path(&head, [110.0, 90.0], [410.0, 95.0]);
        assert_eq!(shared.points[0], [110.0, 90.0]);
        assert_eq!(*shared.points.last().unwrap(), [410.0, 95.0]);
        assert_eq!(shared.points.len(), head.points.len());
        assert!(shared.points[2..shared.points.len() - 1] == head.points[2..head.points.len() - 1]);
        // Beside the wall's closed squares the leaves are one square
        // wide: too narrow for the Bit's 4-square sharing minimum;
        // oversized movers never share.
        assert!(layer.nodes[layer.leaf_at(33, 25) as usize].xsize() < 4);
        assert!(layer.share_key([268.0, 204.0], b, 3).is_none());
        assert!(layer.share_key(a, b, 64).is_none());
    }

    /// Repair: a wall appears across a path; the search from the unit to
    /// the first clean point stays inside its box and the clean tail is
    /// spliced on unchanged.
    #[test]
    fn repair_search_rejoins_the_clean_tail() {
        let mut map = flat(64, 64);
        let mut layer = NodeLayer::new(&map, None);
        let a = [40.0, 256.0];
        let b = [480.0, 256.0];
        let head = search(&mut layer, &map, None, a, b).unwrap();
        assert_eq!(head.points.len(), 2, "open ground: raw");
        // A wall with a gap closes the middle of the route.
        for z in 0..64 {
            if !(40..44).contains(&z) {
                map.speeds[(z * 64 + 20) as usize] = 0.0;
            }
        }
        layer.update(
            &map,
            None,
            [Rect {
                x0: 20,
                z0: 0,
                x1: 21,
                z1: 64,
            }],
        );
        // Pretend the clean remainder starts at (300, 256).
        let tail = [[300.0, 256.0], [400.0, 256.0], [480.0, 256.0]];
        let rects = [layer.nodes[layer.leaf_at(37, 32) as usize].rect(); 3];
        let mut scratch = QtScratch::default();
        let repaired =
            match QtSearch::begin_repair(&mut layer, &mut scratch, &map, None, a, tail[0]) {
                Err(p) => p,
                Ok(mut s) => s.step(&layer, &mut scratch, usize::MAX).unwrap().unwrap(),
            };
        assert!(repaired.full, "{:?}", repaired.points);
        assert_eq!(*repaired.points.last().unwrap(), tail[0]);
        let joined = splice_repair(repaired, &tail, &rects, true, None);
        assert_eq!(*joined.points.last().unwrap(), b);
        assert_eq!(joined.points.len(), joined.node_rects.len());
        // Through the gap, inside the box.
        assert!(
            joined
                .points
                .iter()
                .any(|q| (152.0..=176.0).contains(&q[0]) && (310.0..=360.0).contains(&q[1])),
            "{:?}",
            joined.points
        );
    }

    #[test]
    fn first_waypoint_skips_points_behind_the_unit() {
        let pts = [[0.0, 0.0], [100.0, 0.0], [200.0, 0.0], [300.0, 0.0]];
        assert_eq!(first_waypoint(&pts, [0.0, 0.0]), 1);
        assert_eq!(first_waypoint(&pts, [150.0, 0.0]), 2);
        assert_eq!(first_waypoint(&pts, [198.0, 1.0]), 3);
        // Past the end, nothing qualifies: the engine's default is point 1.
        assert_eq!(first_waypoint(&pts, [500.0, 0.0]), 1);
    }

    #[test]
    fn repath_trigger_sits_past_the_middle_of_long_partial_paths() {
        let pts: Vec<[f32; 2]> = (0..12).map(|i| [i as f32 * 100.0, 0.0]).collect();
        assert_eq!(repath_trigger(&pts), Some(5));
        let short: Vec<[f32; 2]> = (0..4).map(|i| [i as f32 * 100.0, 0.0]).collect();
        assert_eq!(repath_trigger(&short), None);
    }
}
