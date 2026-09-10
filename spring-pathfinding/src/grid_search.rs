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

use crate::cost::{SpeedMap, SQUARE_SIZE};
use crate::path::Path;

/// Expansions before giving up. A 512×384 map has ~196k cells; this
/// caps the worst case (destination on an unreachable island) at a
/// fraction of a frame.
const MAX_EXPANSIONS: u32 = 100_000;

/// A* over `speed_map` from world-space `src` to `dst` (XZ).
///
/// Returns the waypoint list in world coordinates (including both
/// endpoints), or `None` when the source itself is impassable or the
/// iteration cap was hit before reaching anywhere useful.
pub fn find_path(speed_map: &SpeedMap, src: [f32; 2], dst: [f32; 2]) -> Option<Path> {
    let width = speed_map.width;
    let height = speed_map.height;
    if width == 0 || height == 0 {
        return None;
    }

    let sx = world_to_cell(src[0], width);
    let sz = world_to_cell(src[1], height);
    let dx = world_to_cell(dst[0], width);
    let dz = world_to_cell(dst[1], height);

    // The source must be standable; the goal may be blocked — the
    // search then converges on the closest reachable cell instead.
    if speed_map.get(sx, sz) <= 0.0 {
        return None;
    }

    // Same cell: identical passability by construction, walk straight.
    if (sx, sz) == (dx, dz) {
        return Some(Path { points: vec![src, dst] });
    }

    // Admissible octile heuristic scaled by the slowest possible travel:
    // never overestimates because every cell's cost-per-elmo is
    // `1/speed ≥ 1/max_speed`.
    let max_speed = speed_map
        .speeds
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .max(0.001);
    let h_scale = 1.0 / max_speed;
    let heuristic = |x: u32, z: u32| octile(x, z, dx, dz) * h_scale;

    let cell_count = (width * height) as usize;
    let mut g_cost = vec![f32::INFINITY; cell_count];
    let mut came_from = vec![usize::MAX; cell_count];
    let mut closed = vec![false; cell_count];

    let mut open = BinaryHeap::with_capacity(1024);
    let start = cell_idx(sx, sz, width);
    g_cost[start] = 0.0;
    open.push(Open {
        f: heuristic(sx, sz),
        cell: start,
    });

    let goal = cell_idx(dx, dz, width);
    let mut best = start;
    let mut best_h = heuristic(sx, sz);
    let mut expansions = 0u32;

    while let Some(Open { cell, .. }) = open.pop() {
        if closed[cell] {
            continue; // stale heap entry
        }
        closed[cell] = true;
        expansions += 1;
        if expansions > MAX_EXPANSIONS {
            break;
        }

        if cell == goal {
            best = goal;
            break;
        }

        let cx = (cell as u32) % width;
        let cz = (cell as u32) / width;
        let cell_h = heuristic(cx, cz);
        if cell_h < best_h {
            best_h = cell_h;
            best = cell;
        }

        let g_here = g_cost[cell];

        for (nx, nz, step_len) in neighbors(cx, cz, width, height) {
            let n_idx = cell_idx(nx, nz, width);
            let speed = speed_map.speeds[n_idx];
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
                if speed_map.speeds[ax] <= 0.0 || speed_map.speeds[az] <= 0.0 {
                    continue;
                }
            }

            let g_new = g_here + step_len / speed;
            if g_new < g_cost[n_idx] {
                g_cost[n_idx] = g_new;
                came_from[n_idx] = cell;
                open.push(Open {
                    f: g_new + heuristic(nx, nz),
                    cell: n_idx,
                });
            }
        }
    }
    // Goal reachable → trace it; otherwise trace the closest reachable
    // cell (upstream's partial-path behaviour).
    let end = if closed[goal] { goal } else { best };
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
        cur = came_from[cur];
    }
    cells.reverse();

    let world = |cell: usize| -> [f32; 2] {
        [
            (((cell as u32) % width) as f32 + 0.5) * SQUARE_SIZE,
            (((cell as u32) / width) as f32 + 0.5) * SQUARE_SIZE,
        ]
    };

    let mut points: Vec<[f32; 2]> = Vec::with_capacity(cells.len());
    points.push(src);
    for &c in &cells[1..] {
        points.push(world(c));
    }
    // Replace the goal cell centre with the exact clicked position so
    // units arrive where the player pointed.
    if end == goal {
        let last = points.last_mut().expect("non-empty");
        *last = dst;
    }

    smooth(&mut points, speed_map);
    Some(Path { points })
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
fn neighbors(
    x: u32,
    z: u32,
    width: u32,
    height: u32,
) -> impl Iterator<Item = (u32, u32, f32)> {
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
/// to the furthest visible successor crosses only passable cells
/// (supercover walk so corners can't be clipped diagonally).
fn smooth(points: &mut Vec<[f32; 2]>, speed_map: &SpeedMap) {
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
        while j + 1 < points.len()
            && line_clear(points[i], points[j + 1], speed_map)
        {
            j += 1;
        }
        out.push(points[j]);
        i = j;
    }
    *points = out;
}

/// Sample the straight segment every half-square and require every
/// touched cell to be passable.
fn line_clear(a: [f32; 2], b: [f32; 2], speed_map: &SpeedMap) -> bool {
    let dist = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    let steps = (dist / (SQUARE_SIZE * 0.5)).ceil() as usize;
    for s in 1..steps {
        let t = s as f32 / steps as f32;
        let x = a[0] + (b[0] - a[0]) * t;
        let z = a[1] + (b[1] - a[1]) * t;
        if speed_map.get(world_to_cell(x, speed_map.width), world_to_cell(z, speed_map.height))
            <= 0.0
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(w: u32, h: u32) -> SpeedMap {
        SpeedMap::uniform(w, h, 1.0)
    }

    #[test]
    fn open_terrain_is_nearly_straight() {
        let map = flat(32, 32);
        let path = find_path(&map, [20.0, 20.0], [240.0, 240.0]).expect("path");
        assert!(path.len() <= 3, "LOS smoothing should collapse open-field runs, got {} waypoints", path.len());
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

    #[test]
    fn impassable_source_returns_none() {
        let mut map = flat(8, 8);
        map.speeds[0] = 0.0;
        assert!(find_path(&map, [1.0, 1.0], [60.0, 60.0]).is_none());
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
            let a = [(w[0][0] / SQUARE_SIZE) as i32, (w[0][1] / SQUARE_SIZE) as i32];
            let b = [(w[1][0] / SQUARE_SIZE) as i32, (w[1][1] / SQUARE_SIZE) as i32];
            let (dx, dz) = ((a[0] - b[0]).abs(), (a[1] - b[1]).abs());
            if dx == 1 && dz == 1 {
                // Both orthogonal neighbours of the diagonal must be open.
                let orth1 = map.get((a[0].min(b[0])) as u32, (a[1].min(b[1])) as u32);
                let orth2 = map.get((a[0].max(b[0])) as u32, (a[1].max(b[1])) as u32);
                assert!(orth1 > 0.0 && orth2 > 0.0, "corner cut at {a:?}->{b:?}");
            }
        }
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
