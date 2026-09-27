//! Uniform-grid A* over the movement speed map.
//!
//! This is the classic Spring pathfinder (rts/Sim/Path/PathFinder):
//! 8-connected grid search with cost = distance / speed-mod, blocked
//! squares impassable, no corner cutting, octile heuristic, then a
//! line-of-sight pass that removes redundant waypoints (Spring's
//! `CPathOptimizer`). The retired QTPFS quad-tree was an acceleration
//! of exactly this — at Kernel Panic map sizes the plain grid wins on
//! simplicity and is fast enough by a wide margin.
//!
//! Semantics that matter for gameplay:
//!
//! - **No straight-line fallback.** If the destination is unreachable,
//!   the search returns the path to the *closest reachable cell*
//!   (best-`h`), mirroring upstream's `pathingFailed` behaviour: units
//!   gather at the obstacle instead of ghosting through it.
//! - **No corner cutting**: a diagonal step requires both orthogonal
//!   neighbours to be passable (units are not infinitely thin).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::cost::{SQUARE_SIZE, SpeedMap};
use crate::path::Path;

/// How strongly path heat (see [`crate::HeatMap`]) slows a cell in the
/// A\* cost model: `speed_eff = speed / (1 + heat · HEAT_COST_SOFTNESS)`.
/// Tuned so a single LIGHT-class trail (~10 heat) costs a few percent
/// while a marching column (~200 heat, Byte-class deposits) pushes
/// later units well off the line — soft avoidance, never blockage.
pub const HEAT_COST_SOFTNESS: f32 = 0.02;

/// Smoothing tolerance: the LOS pass refuses to shortcut across cells
/// whose heat slows travel by more than this fraction. Without it the
/// optimizer collapses a heat detour straight back through the crowd
/// the detour was avoiding (the pass only ever checked passability).
const HEAT_SMOOTH_TOLERANCE: f32 = 0.5;

/// A* over `speed_map` from world-space `src` to `dst` (XZ).
///
/// Returns the waypoint list in world coordinates (including both
/// endpoints), or `None` when the source itself is impassable. The
/// search terminates naturally — a closed set bounds it by the cell
/// count, so no expansion cap is needed (a fixed cap truncated long
/// detours mid-search and made units camp against the wall they were
/// trying to round).
///
/// `Path::reached_goal` is `false` when the goal cell is unreachable:
/// the path then leads to the closest reachable cell.
pub fn find_path(speed_map: &SpeedMap, src: [f32; 2], dst: [f32; 2]) -> Option<Path> {
    find_path_with_heat(speed_map, None, src, dst)
}

/// [`find_path`], with an optional congestion overlay: cells with heat
/// cost more (`speed / (1 + heat·HEAT_COST_SOFTNESS)`), so later units
/// route around columns that just marched through. Heat never blocks —
/// the overlay can only slow the search down, not seal a route.
pub fn find_path_with_heat(
    speed_map: &SpeedMap,
    heat: Option<&crate::heat::HeatMap>,
    src: [f32; 2],
    dst: [f32; 2],
) -> Option<Path> {
    find_path_masked(speed_map, None, heat, src, dst)
}

/// Per-cell blocking overlay on top of a [`SpeedMap`] — structures
/// (QTPFS `NodeLayer::Update` closes every node whose mover-footprint
/// window contains a `BLOCK_STRUCTURE` square, `NodeLayer.cpp:163-190`).
/// Same grid as the speed map; `true` = closed for this mover class.
#[derive(Debug, Clone)]
pub struct BlockMask {
    pub width: u32,
    pub height: u32,
    pub cells: Vec<bool>,
}

impl BlockMask {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cells: vec![false; (width * height) as usize],
        }
    }

    #[inline]
    pub fn blocked(&self, x: u32, z: u32) -> bool {
        x < self.width && z < self.height && self.cells[(z * self.width + x) as usize]
    }
}

/// Can a mover stand on cell `(x, z)`: terrain passable and not closed
/// by the mask.
#[inline]
fn open(speed_map: &SpeedMap, mask: Option<&BlockMask>, x: u32, z: u32) -> bool {
    speed_map.get(x, z) > 0.0 && !mask.is_some_and(|m| m.blocked(x, z))
}

/// Nearest open cell to `(x, z)` within `max_ring` rings (Chebyshev),
/// closest by Euclidean distance within the first ring that has one.
fn nearest_open(
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
    x: u32,
    z: u32,
    max_ring: i32,
) -> Option<(u32, u32)> {
    if open(speed_map, mask, x, z) {
        return Some((x, z));
    }
    for r in 1..=max_ring {
        let mut best: Option<((u32, u32), i32)> = None;
        for dz in -r..=r {
            for dx in -r..=r {
                if dx.abs() != r && dz.abs() != r {
                    continue;
                }
                let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                if nx < 0 || nz < 0 {
                    continue;
                }
                let (nx, nz) = (nx as u32, nz as u32);
                if open(speed_map, mask, nx, nz) {
                    let d = dx * dx + dz * dz;
                    if best.is_none_or(|(_, bd)| d < bd) {
                        best = Some(((nx, nz), d));
                    }
                }
            }
        }
        if let Some((c, _)) = best {
            return Some(c);
        }
    }
    None
}

/// Cells a unit standing on a closed square searches outward for an
/// open one to start from (it walks out of the structure's shadow or
/// off the steep patch it was pushed onto).
const START_ESCAPE_RINGS: i32 = 8;

/// [`find_path_with_heat`] with an optional structure [`BlockMask`]:
/// masked cells are impassable like blocked terrain. A source cell that
/// is closed (a unit jostled against a building, standing in a yard)
/// starts the search from the nearest open cell instead of failing.
pub fn find_path_masked(
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
    heat: Option<&crate::heat::HeatMap>,
    src: [f32; 2],
    dst: [f32; 2],
) -> Option<Path> {
    find_path_masked_in(&mut SearchScratch::default(), speed_map, mask, heat, src, dst)
}

/// Reusable A\* working memory: per-cell cost / parent / closed state,
/// sized to the grid on first use and invalidated between searches by
/// bumping a generation stamp instead of clearing it — a search only
/// pays for the cells it touches. Keep one per caller and hand it to
/// [`find_path_masked_in`].
#[derive(Default)]
pub struct SearchScratch {
    /// Even; this search's stamps are `generation` (seen) and
    /// `generation + 1` (closed). Anything lower is a previous search.
    generation: u32,
    nodes: Vec<Node>,
    open: BinaryHeap<Open>,
}

#[derive(Clone, Copy)]
struct Node {
    stamp: u32,
    g_cost: f32,
    came_from: u32,
}

const UNSEEN: Node = Node {
    stamp: 0,
    g_cost: f32::INFINITY,
    came_from: u32::MAX,
};

impl SearchScratch {
    /// Start a search over `cells` cells: every cell reads as unseen.
    fn begin(&mut self, cells: usize) {
        if self.nodes.len() != cells || self.generation >= u32::MAX - 2 {
            self.nodes.clear();
            self.nodes.resize(cells, UNSEEN);
            self.generation = 0;
        }
        self.generation += 2;
        self.open.clear();
    }

    fn node(&self, cell: usize) -> Node {
        let n = self.nodes[cell];
        if n.stamp >= self.generation { n } else { UNSEEN }
    }

    /// Record a cell's cost and parent (a closed cell stays closed).
    fn set(&mut self, cell: usize, g_cost: f32, came_from: usize) {
        self.nodes[cell] = Node {
            stamp: self.nodes[cell].stamp.max(self.generation),
            g_cost,
            came_from: if came_from == usize::MAX { u32::MAX } else { came_from as u32 },
        };
    }

    fn is_closed(&self, cell: usize) -> bool {
        self.nodes[cell].stamp == self.generation + 1
    }

    /// Close a seen cell.
    fn close(&mut self, cell: usize) {
        self.nodes[cell].stamp = self.generation + 1;
    }
}

/// [`find_path_masked`] reusing `scratch` across searches.
pub fn find_path_masked_in(
    scratch: &mut SearchScratch,
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
    heat: Option<&crate::heat::HeatMap>,
    src: [f32; 2],
    dst: [f32; 2],
) -> Option<Path> {
    match scratch.begin_search(speed_map, mask, src, dst) {
        Err(result) => result,
        Ok(mut search) => match scratch.step(&mut search, speed_map, mask, heat, usize::MAX) {
            SearchStatus::Done(path) => path,
            SearchStatus::Running => unreachable!("unbounded step always finishes"),
        },
    }
}

#[inline]
fn closed_cell(speed_map: &SpeedMap, mask: Option<&BlockMask>, i: usize) -> bool {
    speed_map.speeds[i] <= 0.0 || mask.is_some_and(|m| m.cells[i])
}

/// The open cells A\* may step to from `cell` (8-connected, diagonals
/// only past two open orthogonal neighbours) — the search's move rule,
/// shared with the component labelling so both agree on reachability.
fn open_neighbors<'a>(
    speed_map: &'a SpeedMap,
    mask: Option<&'a BlockMask>,
    cell: usize,
) -> impl Iterator<Item = usize> + 'a {
    let (width, height) = (speed_map.width, speed_map.height);
    let cx = (cell as u32) % width;
    let cz = (cell as u32) / width;
    neighbors(cx, cz, width, height).filter_map(move |(nx, nz, step_len)| {
        let n_idx = cell_idx(nx, nz, width);
        if closed_cell(speed_map, mask, n_idx) {
            return None;
        }
        if step_len > SQUARE_SIZE + 0.5
            && (closed_cell(speed_map, mask, cell_idx(nx, cz, width))
                || closed_cell(speed_map, mask, cell_idx(cx, nz, width)))
        {
            return None;
        }
        Some(n_idx)
    })
}

/// Connected components of the open cells of one speed map / mask
/// pair, under the search's own move rule (8-connected, no corner
/// cutting): two open cells share a label exactly when A\* can reach
/// one from the other. Label `0` marks closed cells.
///
/// Lets a host tell an unreachable goal apart before searching: the
/// plain search of such a goal floods the mover's whole component
/// (hundreds of thousands of nodes on a Kernel Panic map) to find the
/// closest reachable cell. With labels the closest cell's distance is
/// found by a ring walk around the goal, and the search stops at the
/// first cell that close — closing the same cells in the same order,
/// so the path is the one the flood would have returned.
#[derive(Debug, Clone)]
pub struct ComponentLabels {
    pub width: u32,
    pub height: u32,
    /// Raw label per cell; resolve through [`Self::find`] — components
    /// merged by [`Self::update_region`] keep their cells' raw labels
    /// and are joined through `alias`.
    labels: Vec<u32>,
    /// Union-find parent per raw label (`alias[l] == l` for a root).
    alias: Vec<u32>,
}

/// Cells around a changed rectangle within which the open cells next to
/// newly blocked squares must still reach each other for the labels to
/// stay valid without a rebuild; a structure's footprint is a few
/// squares, so a detour round it fits easily.
const RECONNECT_WINDOW: i32 = 32;

impl ComponentLabels {
    /// Label the open cells of `speed_map` under `mask`.
    pub fn build(speed_map: &SpeedMap, mask: Option<&BlockMask>) -> Self {
        let (width, height) = (speed_map.width, speed_map.height);
        let cells = (width * height) as usize;
        let mut this = Self {
            width,
            height,
            labels: vec![0u32; cells],
            alias: vec![0],
        };
        let mut stack: Vec<usize> = Vec::new();
        for seed in 0..cells {
            if this.labels[seed] != 0 || closed_cell(speed_map, mask, seed) {
                continue;
            }
            let label = this.fresh_label();
            this.labels[seed] = label;
            stack.push(seed);
            while let Some(cell) = stack.pop() {
                for n_idx in open_neighbors(speed_map, mask, cell) {
                    if this.labels[n_idx] == 0 {
                        this.labels[n_idx] = label;
                        stack.push(n_idx);
                    }
                }
            }
        }
        this
    }

    /// Number of components (roots).
    pub fn count(&self) -> u32 {
        (1..self.alias.len() as u32).filter(|&l| self.alias[l as usize] == l).count() as u32
    }

    fn fresh_label(&mut self) -> u32 {
        let label = self.alias.len() as u32;
        self.alias.push(label);
        label
    }

    /// Root of raw label `l` (path-halving).
    fn find(&self, mut l: u32) -> u32 {
        while self.alias[l as usize] != l {
            l = self.alias[l as usize];
        }
        l
    }

    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.alias[rb.max(ra) as usize] = rb.min(ra);
        }
    }

    /// Component of the cell at `(x, z)`; `0` when closed.
    #[inline]
    pub fn at(&self, x: u32, z: u32) -> u32 {
        self.find(self.labels[cell_idx(x, z, self.width)])
    }

    /// Bring the labels up to date after the cells in the rectangle
    /// `[x0, z0, x1, z1]` (inclusive, clamped to the grid) changed
    /// passability. Newly open cells join their neighbours' components
    /// (merging any they connect); newly blocked cells are dropped, and
    /// the open cells around them must still reach each other within
    /// [`RECONNECT_WINDOW`] — otherwise the change may have split a
    /// component and everything is rebuilt. Returns whether it rebuilt.
    pub fn update_region(&mut self, speed_map: &SpeedMap, mask: Option<&BlockMask>, bbox: [i32; 4]) -> bool {
        if self.width != speed_map.width || self.height != speed_map.height {
            // Not the grid these labels were built on.
            *self = Self::build(speed_map, mask);
            return true;
        }
        let (w, h) = (self.width as i32, self.height as i32);
        let x0 = bbox[0].clamp(0, w - 1);
        let z0 = bbox[1].clamp(0, h - 1);
        let x1 = bbox[2].clamp(0, w - 1);
        let z1 = bbox[3].clamp(0, h - 1);
        if x0 > x1 || z0 > z1 {
            return false;
        }
        let mut opened: Vec<usize> = Vec::new();
        let mut blocked: Vec<usize> = Vec::new();
        for z in z0..=z1 {
            for x in x0..=x1 {
                let i = cell_idx(x as u32, z as u32, self.width);
                let was_open = self.labels[i] != 0;
                let is_open = !closed_cell(speed_map, mask, i);
                match (was_open, is_open) {
                    (false, true) => opened.push(i),
                    (true, false) => blocked.push(i),
                    _ => {}
                }
            }
        }
        // Blocked cells leave their component; their open neighbours
        // are the cells whose mutual connectivity decides a split.
        let mut frontier: Vec<usize> = Vec::new();
        for &i in &blocked {
            self.labels[i] = 0;
        }
        for &i in &blocked {
            for n in open_neighbors(speed_map, mask, i) {
                if self.labels[n] != 0 && !frontier.contains(&n) {
                    frontier.push(n);
                }
            }
        }
        // Opened cells: adopt a neighbouring component, merging the
        // ones they bridge; a cell with no labelled neighbour starts a
        // component of its own (later cells of the same opening join it).
        for &i in &opened {
            let mut label = 0u32;
            for n in open_neighbors(speed_map, mask, i) {
                let l = self.labels[n];
                if l == 0 {
                    continue;
                }
                if label == 0 {
                    label = l;
                } else {
                    self.union(label, l);
                }
            }
            if label == 0 {
                label = self.fresh_label();
            }
            self.labels[i] = label;
        }
        // Also blocked cells' neighbours may have been bridged only
        // through the (now closed) diagonal rule; the window check below
        // covers every case: all frontier cells must reach each other.
        if frontier.len() > 1 && !self.connected_within(speed_map, mask, &frontier, [x0, z0, x1, z1]) {
            *self = Self::build(speed_map, mask);
            return true;
        }
        false
    }

    /// Do all `cells` reach each other by open cells inside the window
    /// `bbox` grown by [`RECONNECT_WINDOW`]? (Reaching each other inside
    /// the window proves they still share a component.)
    fn connected_within(&self, speed_map: &SpeedMap, mask: Option<&BlockMask>, cells: &[usize], bbox: [i32; 4]) -> bool {
        let (w, h) = (self.width as i32, self.height as i32);
        let wx0 = (bbox[0] - RECONNECT_WINDOW).max(0);
        let wz0 = (bbox[1] - RECONNECT_WINDOW).max(0);
        let wx1 = (bbox[2] + RECONNECT_WINDOW).min(w - 1);
        let wz1 = (bbox[3] + RECONNECT_WINDOW).min(h - 1);
        let ww = (wx1 - wx0 + 1) as usize;
        let wh = (wz1 - wz0 + 1) as usize;
        let mut seen = vec![false; ww * wh];
        let local = |i: usize| -> Option<usize> {
            let x = (i as u32 % self.width) as i32;
            let z = (i as u32 / self.width) as i32;
            (x >= wx0 && x <= wx1 && z >= wz0 && z <= wz1)
                .then(|| (z - wz0) as usize * ww + (x - wx0) as usize)
        };
        let mut stack = vec![cells[0]];
        seen[local(cells[0]).expect("frontier cell inside the window")] = true;
        let mut reached = 1usize;
        while let Some(cell) = stack.pop() {
            for n in open_neighbors(speed_map, mask, cell) {
                let Some(li) = local(n) else { continue };
                if !seen[li] {
                    seen[li] = true;
                    if cells.contains(&n) {
                        reached += 1;
                        if reached == cells.len() {
                            return true;
                        }
                    }
                    stack.push(n);
                }
            }
        }
        false
    }

    /// The smallest octile distance from `(dx, dz)` to any cell of
    /// component `label`: rings of growing Chebyshev radius `r` are
    /// walked until `r` alone exceeds the best distance found (a ring's
    /// cells are all at least `r` squares away).
    fn nearest_distance(&self, label: u32, dx: u32, dz: u32) -> Option<f32> {
        let max_r = self.width.max(self.height) as i32;
        let mut best: Option<f32> = None;
        for r in 0..=max_r {
            if best.is_some_and(|b| r as f32 * SQUARE_SIZE > b) {
                break;
            }
            let (x0, x1) = (dx as i32 - r, dx as i32 + r);
            let (z0, z1) = (dz as i32 - r, dz as i32 + r);
            let mut visit = |x: i32, z: i32| {
                if x < 0 || z < 0 || x >= self.width as i32 || z >= self.height as i32 {
                    return;
                }
                if self.at(x as u32, z as u32) == label {
                    let h = octile(x as u32, z as u32, dx, dz);
                    if best.is_none_or(|b| h < b) {
                        best = Some(h);
                    }
                }
            };
            for x in x0..=x1 {
                visit(x, z0);
                if r > 0 {
                    visit(x, z1);
                }
            }
            for z in (z0 + 1)..z1 {
                visit(x0, z);
                if r > 0 {
                    visit(x1, z);
                }
            }
        }
        best
    }
}

/// A search in progress inside a [`SearchScratch`] (see
/// [`SearchScratch::begin_search`]): the A\* frontier lives in the
/// scratch, this holds the endpoints and the best-so-far bookkeeping,
/// so a host can expand a bounded number of nodes per sim frame and
/// resume next frame — the way QTPFS executes queued searches across
/// `PathManager::Update` calls — with a result identical to running
/// the search in one go.
pub struct PathSearch {
    src: [f32; 2],
    dst: [f32; 2],
    width: u32,
    height: u32,
    start: usize,
    goal: usize,
    dx: u32,
    dz: u32,
    h_scale: f32,
    best: usize,
    best_h: f32,
    /// Heuristic of the closest reachable cell when the goal is known
    /// to be unreachable: the search stops at the first closed cell
    /// this close instead of flooding the component.
    stop_h: Option<f32>,
}

/// Result of [`SearchScratch::step`].
pub enum SearchStatus {
    /// The node budget ran out before the search settled.
    Running,
    /// Finished: the path (see [`find_path_masked`]) or `None` when
    /// the source has no open cell to start from.
    Done(Option<Path>),
}

impl SearchScratch {
    /// Start a search from world-space `src` to `dst`. Only one search
    /// may be in flight per scratch: beginning another invalidates it.
    /// `Err` carries a result that needed no search (same cell: a
    /// straight walk; no open start cell: `None`).
    pub fn begin_search(
        &mut self,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        src: [f32; 2],
        dst: [f32; 2],
    ) -> Result<PathSearch, Option<Path>> {
        self.begin_search_labelled(speed_map, mask, None, src, dst)
    }

    /// [`Self::begin_search`] with [`ComponentLabels`] of the same map
    /// and mask: an unreachable goal is detected up front and the
    /// search then ends at the closest reachable cell instead of
    /// flooding — same result, a fraction of the work.
    pub fn begin_search_labelled(
        &mut self,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        labels: Option<&ComponentLabels>,
        src: [f32; 2],
        dst: [f32; 2],
    ) -> Result<PathSearch, Option<Path>> {
        let width = speed_map.width;
        let height = speed_map.height;
        if width == 0 || height == 0 {
            return Err(None);
        }

        let Some((sx, sz)) = nearest_open(
            speed_map,
            mask,
            world_to_cell(src[0], width),
            world_to_cell(src[1], height),
            START_ESCAPE_RINGS,
        ) else {
            return Err(None);
        };
        let dx = world_to_cell(dst[0], width);
        let dz = world_to_cell(dst[1], height);

        // Same cell: identical passability by construction, walk straight.
        if (sx, sz) == (dx, dz) {
            return Err(Some(Path {
                points: vec![src, dst],
                reached_goal: true,
            }));
        }

        // Admissible octile heuristic scaled by the slowest possible travel:
        // never overestimates because every cell's cost-per-elmo is
        // `1/speed ≥ 1/max_speed`.
        let h_scale = 1.0 / speed_map.max_speed();
        let start_h = octile(sx, sz, dx, dz) * h_scale;
        let stop_h = labels
            .filter(|l| l.width == width && l.height == height)
            .and_then(|l| {
                let start_label = l.at(sx, sz);
                if start_label == 0 || l.at(dx, dz) == start_label {
                    return None;
                }
                l.nearest_distance(start_label, dx, dz)
                    .map(|h| h * h_scale)
            });

        self.begin((width * height) as usize);
        let start = cell_idx(sx, sz, width);
        self.set(start, 0.0, usize::MAX);
        self.open.push(Open {
            f: start_h,
            cell: start,
        });

        Ok(PathSearch {
            src,
            dst,
            width,
            height,
            start,
            goal: cell_idx(dx, dz, width),
            dx,
            dz,
            h_scale,
            best: start,
            best_h: start_h,
            stop_h,
        })
    }

    /// Expand up to `max_pops` frontier nodes of `search`. The maps must
    /// be the ones the search began on (same grid); their contents may
    /// have changed in between — the search simply sees the new values
    /// from here on, as a QTPFS search resumed after a node-layer
    /// update does.
    pub fn step(
        &mut self,
        search: &mut PathSearch,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        heat: Option<&crate::heat::HeatMap>,
        max_pops: usize,
    ) -> SearchStatus {
        let width = search.width;
        let height = search.height;
        let goal = search.goal;
        let heuristic = |x: u32, z: u32| octile(x, z, search.dx, search.dz) * search.h_scale;

        let mut pops = 0usize;
        loop {
            if pops >= max_pops {
                return SearchStatus::Running;
            }
            let Some(Open { cell, .. }) = self.open.pop() else {
                break;
            };
            pops += 1;
            if self.is_closed(cell) {
                continue; // stale heap entry
            }
            self.close(cell);

            if cell == goal {
                search.best = goal;
                break;
            }

            let cx = (cell as u32) % width;
            let cz = (cell as u32) / width;
            let cell_h = heuristic(cx, cz);
            if cell_h < search.best_h {
                search.best_h = cell_h;
                search.best = cell;
            }
            // Unreachable goal: the first cell closed this near the
            // goal is the one a full flood would settle on (cells close
            // in `f` order, and no cell of the component is nearer).
            if search.stop_h.is_some_and(|h| cell_h <= h) {
                break;
            }

            let g_here = self.node(cell).g_cost;

            for (nx, nz, step_len) in neighbors(cx, cz, width, height) {
                let n_idx = cell_idx(nx, nz, width);
                if mask.is_some_and(|m| m.cells[n_idx]) {
                    continue;
                }
                let mut speed = speed_map.speeds[n_idx];
                if let Some(hm) = heat {
                    let cell_heat = hm.heat[n_idx];
                    if cell_heat > 0.0 {
                        speed /= 1.0 + cell_heat * HEAT_COST_SOFTNESS;
                    }
                }
                if speed <= 0.0 {
                    continue; // impassable
                }
                // No corner cutting: diagonals need both orthogonal
                // neighbours passable. Diagonal steps carry √2·SQUARE_SIZE
                // length; orthogonals SQUARE_SIZE, so > SQUARE_SIZE + ½
                // selects exactly the diagonals.
                if step_len > SQUARE_SIZE + 0.5 {
                    let ax = cell_idx(nx, cz, width);
                    let az = cell_idx(cx, nz, width);
                    let closed = |i: usize| speed_map.speeds[i] <= 0.0 || mask.is_some_and(|m| m.cells[i]);
                    if closed(ax) || closed(az) {
                        continue;
                    }
                }

                let g_new = g_here + step_len / speed;
                if g_new < self.node(n_idx).g_cost {
                    self.set(n_idx, g_new, cell);
                    self.open.push(Open {
                        f: g_new + heuristic(nx, nz),
                        cell: n_idx,
                    });
                }
            }
        }
        SearchStatus::Done(self.trace(search, speed_map, mask, heat))
    }

    /// Goal reachable → trace it; otherwise trace the closest reachable
    /// cell and flag the order as failed so the host can refuse it
    /// (upstream `pathingFailed`) instead of parking units at the wall.
    fn trace(
        &self,
        search: &PathSearch,
        speed_map: &SpeedMap,
        mask: Option<&BlockMask>,
        heat: Option<&crate::heat::HeatMap>,
    ) -> Option<Path> {
        let PathSearch {
            src,
            dst,
            width,
            height,
            start,
            goal,
            ..
        } = *search;
        let reached_goal = self.is_closed(goal);
        let end = if reached_goal { goal } else { search.best };
        if end == start {
            return None;
        }

        let mut cells: Vec<usize> = Vec::new();
        let mut cur = end;
        while cur != usize::MAX {
            cells.push(cur);
            if cur == start {
                break;
            }
            cur = match self.node(cur).came_from {
                u32::MAX => usize::MAX,
                c => c as usize,
            };
        }
        cells.reverse();

        let world = |cell: usize| -> [f32; 2] {
            [
                (((cell as u32) % width) as f32 + 0.5) * SQUARE_SIZE,
                (((cell as u32) / width) as f32 + 0.5) * SQUARE_SIZE,
            ]
        };

        let mut points: Vec<[f32; 2]> = Vec::with_capacity(cells.len() + 1);
        points.push(src);
        // Escaping a closed start cell: walk to the open cell first.
        if cell_idx(world_to_cell(src[0], width), world_to_cell(src[1], height), width) != start {
            points.push(world(start));
        }
        for &c in &cells[1..] {
            points.push(world(c));
        }
        // Replace the goal cell centre with the exact clicked position so
        // units arrive where the player pointed.
        if end == goal {
            let last = points.last_mut().expect("non-empty");
            *last = dst;
        }

        smooth(&mut points, speed_map, mask, heat);
        Some(Path {
            points,
            reached_goal,
        })
    }
}

#[derive(Clone, Copy)]
struct Open {
    f: f32,
    cell: usize,
}

impl PartialEq for Open {
    fn eq(&self, other: &Self) -> bool {
        self.f == other.f
    }
}
impl Eq for Open {}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Open {
    fn cmp(&self, other: &Self) -> Ordering {
        // Max-heap → reverse for min-first.
        other
            .f
            .partial_cmp(&self.f)
            .unwrap_or(Ordering::Equal)
            .then_with(|| other.cell.cmp(&self.cell))
    }
}

/// 8-connected neighbour steps as `(cell_x, cell_z, world_step_length)`.
/// Yields in a fixed order; diagonals carry √2 length so the corner-cut
/// check can identify them by length.
fn neighbors(x: u32, z: u32, width: u32, height: u32) -> impl Iterator<Item = (u32, u32, f32)> {
    let diag = SQUARE_SIZE * std::f32::consts::SQRT_2;
    let (xi, zi) = (x as i32, z as i32);
    [
        (xi - 1, zi, SQUARE_SIZE),
        (xi + 1, zi, SQUARE_SIZE),
        (xi, zi - 1, SQUARE_SIZE),
        (xi, zi + 1, SQUARE_SIZE),
        (xi - 1, zi - 1, diag),
        (xi + 1, zi - 1, diag),
        (xi - 1, zi + 1, diag),
        (xi + 1, zi + 1, diag),
    ]
    .into_iter()
    .filter_map(move |(nx, nz, len)| {
        let in_bounds = nx >= 0 && nz >= 0 && nx < width as i32 && nz < height as i32;
        in_bounds.then_some((nx as u32, nz as u32, len))
    })
}

#[inline]
fn cell_idx(x: u32, z: u32, width: u32) -> usize {
    (z * width + x) as usize
}

#[inline]
fn world_to_cell(v: f32, cells: u32) -> u32 {
    ((v / SQUARE_SIZE) as u32).min(cells - 1)
}

/// Octile (chebyshev-with-diagonals) distance — exact optimal grid
/// distance when all cells cost the same.
fn octile(x: u32, z: u32, dx: u32, dz: u32) -> f32 {
    let ddx = (x as i32 - dx as i32).unsigned_abs() as f32;
    let ddz = (z as i32 - dz as i32).unsigned_abs() as f32;
    let (mn, mx) = (ddx.min(ddz), ddx.max(ddz));
    (mx - mn) * SQUARE_SIZE + mn * SQUARE_SIZE * std::f32::consts::SQRT_2
}

/// Greedy line-of-sight smoothing (Spring's `CPathOptimizer`): walk the
/// waypoint list dropping any intermediate point while a straight ray
/// to the furthest visible successor crosses only open cells.
fn smooth(
    points: &mut Vec<[f32; 2]>,
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
    heat: Option<&crate::heat::HeatMap>,
) {
    if points.len() <= 2 {
        return;
    }
    let mut out: Vec<[f32; 2]> = Vec::with_capacity(points.len());
    out.push(points[0]);
    let mut i = 0;
    while i + 1 < points.len() {
        // Furthest j ≥ i+2 visible from points[i]; default to the
        // immediate successor.
        let mut j = i + 1;
        while j + 1 < points.len() && line_clear(points[i], points[j + 1], speed_map, mask, heat) {
            j += 1;
        }
        out.push(points[j]);
        i = j;
    }
    *points = out;
}

/// Visit every grid cell the segment `a → b` passes through, in order
/// (Amanatides & Woo exact traversal). Where the segment crosses a
/// cell corner exactly, both side cells are visited too (supercover),
/// so a diagonal can't slip between two blocked squares. Stops early
/// when `visit` returns `false`; returns whether it ran to the end.
pub fn traverse_cells(a: [f32; 2], b: [f32; 2], mut visit: impl FnMut(i32, i32) -> bool) -> bool {
    let (x0, z0) = (a[0] / SQUARE_SIZE, a[1] / SQUARE_SIZE);
    let (x1, z1) = (b[0] / SQUARE_SIZE, b[1] / SQUARE_SIZE);
    let (mut cx, mut cz) = (x0.floor() as i32, z0.floor() as i32);
    let (ex, ez) = (x1.floor() as i32, z1.floor() as i32);
    if !visit(cx, cz) {
        return false;
    }
    let (dx, dz) = (x1 - x0, z1 - z0);
    let step_x = if dx > 0.0 { 1 } else { -1 };
    let step_z = if dz > 0.0 { 1 } else { -1 };
    let t_delta_x = if dx != 0.0 { 1.0 / dx.abs() } else { f32::INFINITY };
    let t_delta_z = if dz != 0.0 { 1.0 / dz.abs() } else { f32::INFINITY };
    let mut t_max_x = if dx > 0.0 {
        (x0.floor() + 1.0 - x0) * t_delta_x
    } else if dx < 0.0 {
        (x0 - x0.floor()) * t_delta_x
    } else {
        f32::INFINITY
    };
    let mut t_max_z = if dz > 0.0 {
        (z0.floor() + 1.0 - z0) * t_delta_z
    } else if dz < 0.0 {
        (z0 - z0.floor()) * t_delta_z
    } else {
        f32::INFINITY
    };
    // Advance while the next boundary crossing lies strictly inside the
    // segment (t < 1): an endpoint exactly on a boundary does not enter
    // the next cell. Bounded by the cell count so float noise can never
    // loop forever.
    let max_steps = ((ex - cx).abs() + (ez - cz).abs()) as usize + 2;
    for _ in 0..max_steps {
        let t_next = t_max_x.min(t_max_z);
        if t_next >= 1.0 {
            break;
        }
        let diff = t_max_x - t_max_z;
        if diff.abs() < 1e-6 {
            // Exactly through a corner: the two side cells are touched.
            if !visit(cx + step_x, cz) || !visit(cx, cz + step_z) {
                return false;
            }
            cx += step_x;
            cz += step_z;
            t_max_x += t_delta_x;
            t_max_z += t_delta_z;
        } else if diff < 0.0 {
            cx += step_x;
            t_max_x += t_delta_x;
        } else {
            cz += step_z;
            t_max_z += t_delta_z;
        }
        if !visit(cx, cz) {
            return false;
        }
    }
    true
}

/// Is the straight segment `a → b` walkable: every cell it passes
/// through (exact traversal, see [`traverse_cells`]) open for the mover
/// — terrain passable, not closed by `mask` — and, with a heat overlay,
/// not hot enough to defeat a shortcut.
pub fn line_clear(
    a: [f32; 2],
    b: [f32; 2],
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
    heat: Option<&crate::heat::HeatMap>,
) -> bool {
    traverse_cells(a, b, |x, z| {
        if x < 0 || z < 0 || !open(speed_map, mask, x as u32, z as u32) {
            return false;
        }
        let centre = [(x as f32 + 0.5) * SQUARE_SIZE, (z as f32 + 0.5) * SQUARE_SIZE];
        !heat.is_some_and(|hm| hm.get_with_neighbors(centre) * HEAT_COST_SOFTNESS > HEAT_SMOOTH_TOLERANCE)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32) -> SpeedMap {
        SpeedMap::uniform(w, h, 1.0)
    }

    /// A reused scratch answers every search exactly like a fresh one,
    /// including after an unreachable (flooding) search.
    #[test]
    fn reused_scratch_matches_fresh_searches() {
        let mut map = flat(40, 40);
        for z in 0..30 {
            map.speeds[(z * 40 + 20) as usize] = 0.0;
            map.speeds[(z * 40 + 7) as usize] = 0.5;
        }
        // A sealed pocket makes one goal unreachable.
        for i in 0..5 {
            for (x, z) in [(30 + i, 30), (30 + i, 34), (30, 30 + i), (34, 30 + i)] {
                map.speeds[(z * 40 + x) as usize] = 0.0;
            }
        }
        let mut scratch = SearchScratch::default();
        let queries = [
            ([20.0, 20.0], [300.0, 40.0]),
            ([20.0, 300.0], [260.0, 260.0]),
            ([300.0, 20.0], [20.0, 200.0]),
            ([20.0, 20.0], [300.0, 40.0]),
        ];
        for (src, dst) in queries {
            let fresh = find_path_masked(&map, None, None, src, dst).expect("path");
            let reused = find_path_masked_in(&mut scratch, &map, None, None, src, dst).expect("path");
            assert_eq!(fresh.points, reused.points);
            assert_eq!(fresh.reached_goal, reused.reached_goal);
        }
    }

    /// Expanding a search a few nodes at a time (the host's per-frame
    /// budget) ends in exactly the one-shot path.
    #[test]
    fn stepped_search_matches_one_shot() {
        let mut map = flat(40, 40);
        for z in 0..30 {
            map.speeds[(z * 40 + 20) as usize] = 0.0;
        }
        for i in 0..5 {
            for (x, z) in [(30 + i, 30), (30 + i, 34), (30, 30 + i), (34, 30 + i)] {
                map.speeds[(z * 40 + x) as usize] = 0.0;
            }
        }
        let mut scratch = SearchScratch::default();
        for (src, dst) in [([20.0, 20.0], [300.0, 40.0]), ([20.0, 300.0], [260.0, 260.0])] {
            let one_shot = find_path_masked(&map, None, None, src, dst).expect("path");
            let mut search = scratch.begin_search(&map, None, src, dst).ok().expect("needs a search");
            let mut steps = 0;
            let stepped = loop {
                steps += 1;
                if let SearchStatus::Done(path) = scratch.step(&mut search, &map, None, None, 7) {
                    break path.expect("path");
                }
            };
            assert!(steps > 3, "resumed across steps");
            assert_eq!(one_shot.points, stepped.points);
            assert_eq!(one_shot.reached_goal, stepped.reached_goal);
        }
    }

    /// With component labels an unreachable goal skips the flood and
    /// still yields the flood's exact path (to the same closest cell),
    /// whichever side of the wall the mover starts on.
    #[test]
    fn labelled_search_matches_flood_for_unreachable_goals() {
        let mut map = flat(48, 40);
        // A wall with one gap, and a sealed pocket.
        for z in 0..40 {
            if z != 12 {
                map.speeds[(z * 48 + 24) as usize] = 0.0;
            }
        }
        for z in 5..15 {
            map.speeds[(z * 48 + 24) as usize] = 0.0;
        }
        for i in 0..6 {
            for (x, z) in [(30 + i, 30), (30 + i, 35), (30, 30 + i), (35, 30 + i)] {
                map.speeds[(z * 48 + x) as usize] = 0.0;
            }
        }
        let labels = ComponentLabels::build(&map, None);
        assert!(labels.count() >= 3, "left, right, pocket: {}", labels.count());
        let mut plain = SearchScratch::default();
        let mut labelled = SearchScratch::default();
        let cases = [
            ([20.0, 20.0], [260.0, 260.0]),   // into the pocket
            ([260.0, 260.0], [20.0, 20.0]),   // out of the pocket
            ([20.0, 300.0], [380.0, 300.0]),  // across the wall (gap sealed)
            ([380.0, 20.0], [60.0, 200.0]),   // across, other way
            ([20.0, 20.0], [196.0, 100.0]),   // reachable: no change
            ([20.0, 20.0], [196.0, 44.0]),    // goal on the wall itself
        ];
        for (src, dst) in cases {
            let flood = find_path_masked_in(&mut plain, &map, None, None, src, dst).expect("path");
            let mut search = labelled
                .begin_search_labelled(&map, None, Some(&labels), src, dst)
                .ok()
                .expect("needs a search");
            let SearchStatus::Done(Some(fast)) = labelled.step(&mut search, &map, None, None, usize::MAX) else {
                panic!("labelled search failed");
            };
            assert_eq!(flood.reached_goal, fast.reached_goal, "{src:?} → {dst:?}");
            assert_eq!(flood.points, fast.points, "{src:?} → {dst:?}");
        }
    }

    /// Same-component relation of `a` and `b`, as canonical arrays.
    fn partition(l: &ComponentLabels) -> Vec<u32> {
        let mut canon = std::collections::HashMap::new();
        let mut out = Vec::with_capacity((l.width * l.height) as usize);
        for z in 0..l.height {
            for x in 0..l.width {
                let root = l.at(x, z);
                if root == 0 {
                    out.push(0);
                } else {
                    let n = canon.len() as u32 + 1;
                    out.push(*canon.entry(root).or_insert(n));
                }
            }
        }
        out
    }

    /// Blocking and opening rectangles keeps the incremental labels
    /// equal to a rebuild: a wall that splits, a gap that merges, a
    /// pocket that seals, and an isolated opening.
    #[test]
    fn incremental_labels_match_rebuild() {
        let mut map = flat(40, 40);
        let mut labels = ComponentLabels::build(&map, None);
        let set = |map: &mut SpeedMap, x0: u32, z0: u32, x1: u32, z1: u32, v: f32| {
            for z in z0..=z1 {
                for x in x0..=x1 {
                    map.speeds[(z * 40 + x) as usize] = v;
                }
            }
            [x0 as i32, z0 as i32, x1 as i32, z1 as i32]
        };
        let steps: [(u32, u32, u32, u32, f32); 6] = [
            (20, 0, 20, 30, 0.0),  // long wall, still connected round the end
            (20, 31, 20, 39, 0.0), // sealed: split → rebuild
            (5, 5, 8, 8, 0.0),     // a block inside one half
            (20, 12, 20, 12, 1.0), // a gap: merge
            (30, 30, 34, 34, 0.0), // pocket walls…
            (31, 31, 33, 33, 1.0), // …stay blocked here, so this reopens a pocket interior
        ];
        for (x0, z0, x1, z1, v) in steps {
            let bbox = set(&mut map, x0, z0, x1, z1, v);
            map.refresh_max_speed();
            labels.update_region(&map, None, bbox);
            let fresh = ComponentLabels::build(&map, None);
            assert_eq!(partition(&labels), partition(&fresh), "after {bbox:?} = {v}");
        }
    }

    #[test]
    fn open_terrain_is_nearly_straight() {
        let map = flat(32, 32);
        let path = find_path(&map, [20.0, 20.0], [240.0, 240.0]).expect("path");
        assert!(path.reached_goal);
        assert!(
            path.len() <= 3,
            "LOS smoothing should collapse open-field runs, got {} waypoints",
            path.len()
        );
        // Ends at the exact destination.
        let last = path.points.last().unwrap();
        assert!((last[0] - 240.0).abs() < 0.01 && (last[1] - 240.0).abs() < 0.01);
    }

    #[test]
    fn path_routes_around_wall() {
        let mut map = flat(32, 32);
        for z in 4..28 {
            map.speeds[(z * 32 + 16) as usize] = 0.0;
        }
        let src = [40.0, 128.0];
        let dst = [240.0, 128.0];
        let path = find_path(&map, src, dst).expect("path around wall");
        assert!(
            path.reached_goal,
            "wall is detourable — goal must be reached"
        );
        assert!(path.total_length() > 200.0, "must detour around the wall");
        // No waypoint sits on a wall cell (column 16, rows 4..28).
        for p in &path.points {
            let cx = (p[0] / SQUARE_SIZE) as u32;
            let cz = (p[1] / SQUARE_SIZE) as u32;
            assert!(
                !(cx == 16 && (4..28).contains(&cz)),
                "waypoint {p:?} inside the wall"
            );
        }
        // And the path is smooth enough: few waypoints.
        assert!(path.len() <= 5, "got {} waypoints", path.len());
    }

    #[test]
    fn unreachable_goal_walks_to_closest_reachable_point() {
        let mut map = flat(32, 32);
        // Sealed box in the top-right corner: walls on all four sides.
        for x in 20..28 {
            for z in 20..28 {
                let border = x == 20 || x == 27 || z == 20 || z == 27;
                if border {
                    map.speeds[(z * 32 + x) as usize] = 0.0;
                }
            }
        }
        let src = [40.0, 40.0];
        let dst = [188.0, 188.0]; // inside the sealed box
        let path = find_path(&map, src, dst).expect("partial path");
        assert!(!path.reached_goal, "sealed-box goal must not be reached");
        // Path ends adjacent to the wall, not inside the box, and not
        // at the source.
        let last = path.points.last().unwrap();
        let cx = (last[0] / SQUARE_SIZE) as u32;
        let cz = (last[1] / SQUARE_SIZE) as u32;
        assert!(!(20..=27).contains(&cx) || !(20..=27).contains(&cz));
        assert!(path.total_length() > 1.0);
        // Never crosses a blocked cell.
        for p in &path.points {
            assert!(map.get((p[0] / SQUARE_SIZE) as u32, (p[1] / SQUARE_SIZE) as u32) > 0.0);
        }
    }

    /// A unit standing on a closed cell (pushed onto a steep patch,
    /// standing in a structure's shadow) walks out via the nearest open
    /// cell instead of being refused a path.
    #[test]
    fn closed_source_starts_from_nearest_open_cell() {
        let mut map = flat(8, 8);
        map.speeds[0] = 0.0;
        let path = find_path(&map, [1.0, 1.0], [60.0, 60.0]).expect("escape path");
        assert!(path.reached_goal);
        assert_eq!(path.points[0], [1.0, 1.0]);
        // Fully enclosed in closed cells: nothing to escape to.
        let sealed = SpeedMap::uniform(20, 20, 0.0);
        assert!(find_path(&sealed, [80.0, 80.0], [100.0, 100.0]).is_none());
    }

    /// Structure masks close cells for the search and the smoother.
    #[test]
    fn mask_blocks_the_search() {
        let map = flat(32, 32);
        let mut mask = BlockMask::new(32, 32);
        for z in 4..28 {
            mask.cells[(z * 32 + 16) as usize] = true;
        }
        let path = find_path_masked(&map, Some(&mask), None, [40.0, 128.0], [240.0, 128.0])
            .expect("path around the masked wall");
        assert!(path.reached_goal);
        for w in path.points.windows(2) {
            assert!(line_clear(w[0], w[1], &map, Some(&mask), None), "segment {w:?} crosses the mask");
        }
        assert!(path.total_length() > 220.0);
    }

    /// Exact traversal: a segment that grazes a blocked cell's corner
    /// region is refused, one that stays clear is accepted.
    #[test]
    fn line_clear_is_exact() {
        let mut map = flat(8, 8);
        map.speeds[(3 * 8 + 3) as usize] = 0.0; // cell (3,3) = [24,32)²
        // Passes through (3,3) near its corner: 4-elmo sampling missed it.
        assert!(!line_clear([4.0, 4.0], [33.0, 31.5], &map, None, None));
        // Clear of the cell.
        assert!(line_clear([4.0, 4.0], [60.0, 20.0], &map, None, None));
        // Diagonal exactly through the corner between (2,3) and (3,2)
        // when both are blocked must be refused.
        let mut map = flat(8, 8);
        map.speeds[(3 * 8 + 2) as usize] = 0.0;
        map.speeds[(2 * 8 + 3) as usize] = 0.0;
        assert!(!line_clear([12.0, 12.0], [36.0, 36.0], &map, None, None));
    }

    #[test]
    fn diagonal_does_not_cut_wall_corners() {
        let mut map = flat(8, 8);
        // Wall along row z=3, columns x=0..5.
        for x in 0..5 {
            map.speeds[(3 * 8 + x) as usize] = 0.0;
        }
        // Start just above the wall, goal just below it: the only way
        // past is around the right end (x ≥ 5), and the search must not
        // squeeze diagonally through the blocked corner at (4,3).
        let path = find_path(&map, [20.0, 20.0], [20.0, 36.0]).expect("path");
        for w in path.points.windows(2) {
            let a = [
                (w[0][0] / SQUARE_SIZE) as i32,
                (w[0][1] / SQUARE_SIZE) as i32,
            ];
            let b = [
                (w[1][0] / SQUARE_SIZE) as i32,
                (w[1][1] / SQUARE_SIZE) as i32,
            ];
            let (dx, dz) = ((a[0] - b[0]).abs(), (a[1] - b[1]).abs());
            if dx == 1 && dz == 1 {
                // Both orthogonal neighbours of the diagonal must be open.
                let orth1 = map.get((a[0].min(b[0])) as u32, (a[1].min(b[1])) as u32);
                let orth2 = map.get((a[0].max(b[0])) as u32, (a[1].max(b[1])) as u32);
                assert!(orth1 > 0.0 && orth2 > 0.0, "corner cut at {a:?}->{b:?}");
            }
        }
    }

    /// A column of heat down the middle bends the route: the path
    /// prefers a detour over eating the congestion penalty, but heat
    /// never blocks — the goal is still reached.
    #[test]
    fn heat_diverts_the_path_without_blocking_it() {
        use crate::heat::HeatMap;
        let map = flat(32, 32);
        let mut heat = HeatMap::new(32, 32);
        // Hot band ACROSS the route (rows 15-17, columns 6..28):
        // walking it means eating ~24 hot cells; stepping out of the
        // band costs a couple of diagonal steps. Columns 6.. so the
        // source cell (5,16) itself stands outside the band.
        for z in 15..=17 {
            for x in 6..28 {
                heat.heat[(z * 32 + x) as usize] = 300.0;
            }
        }
        let src = [40.0, 128.0];
        let dst = [240.0, 128.0];

        let cold = find_path(&map, src, dst).expect("cold path");
        let hot = find_path_with_heat(&map, Some(&heat), src, dst).expect("hot path");

        assert!(hot.reached_goal, "heat must never seal a route");
        // The cold path runs straight through the strip; the hot path
        // pays a detour instead.
        assert!(
            hot.total_length() > cold.total_length(),
            "hot {:?} must pay at least a little over cold {:?}",
            hot.total_length(),
            cold.total_length(),
        );
        // No hot waypoint sits inside the band — that's the actual
        // fidelity claim: routing goes around hot cells.
        for p in &hot.points {
            let cx = (p[0] / SQUARE_SIZE) as u32;
            let cz = (p[1] / SQUARE_SIZE) as u32;
            assert!(
                !((15..=17).contains(&cz) && (6..28).contains(&cx)),
                "waypoint {p:?} runs straight through the hot band"
            );
        }
    }

    /// Mild heat on the direct line is cheaper than a detour: with a
    /// short low-heat strip the path stays straight — avoidance has to
    /// be worth its way around.
    #[test]
    fn mild_heat_is_preferred_over_a_detour() {
        use crate::heat::HeatMap;
        let map = flat(32, 32);
        let mut heat = HeatMap::new(32, 32);
        for z in 14..18 {
            heat.heat[(z * 32 + 16) as usize] = 5.0;
        }
        let cold = find_path(&map, [40.0, 128.0], [240.0, 128.0]).expect("cold");
        let hot =
            find_path_with_heat(&map, Some(&heat), [40.0, 128.0], [240.0, 128.0]).expect("hot");
        assert!(
            (hot.total_length() - cold.total_length()).abs() < 20.0,
            "a 5-heat sliver must not trigger a detour: hot {:?} vs cold {:?}",
            hot.total_length(),
            cold.total_length(),
        );
    }

    #[test]
    fn long_path_performance() {
        // Whole-map diagonal on a full-size KP grid.
        let map = flat(512, 384);
        let t = std::time::Instant::now();
        let path = find_path(&map, [8.0, 8.0], [4088.0, 3064.0]).expect("path");
        let elapsed = t.elapsed();
        assert!(!path.is_empty());
        assert!(elapsed.as_millis() < 500, "search took {elapsed:?}");
    }
}
