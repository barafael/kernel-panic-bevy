use std::cmp::Ordering;
/// A* search on the quad-tree graph.
///
/// Discovers neighbors on-the-fly by walking the node grid along edges.
/// Uses netpoints (transition points on shared edges) for accurate cost calculation.
use std::collections::BinaryHeap;

use crate::cost::SQUARE_SIZE;
use crate::node::{INVALID_INDEX, NETPOINTS_PER_EDGE, NodeIndex};
use crate::node_layer::NodeLayer;
use crate::path::Path;

/// Maximum number of nodes to explore before giving up.
const MAX_ITERATIONS: u32 = 50_000;

/// Find a path from `src` to `dst` (world coordinates) on the given node layer.
pub fn find_path(layer: &mut NodeLayer, src: [f32; 2], dst: [f32; 2]) -> Path {
    let src_gx = (src[0] / SQUARE_SIZE).clamp(0.0, (layer.width - 1) as f32) as u32;
    let src_gz = (src[1] / SQUARE_SIZE).clamp(0.0, (layer.height - 1) as f32) as u32;
    let dst_gx = (dst[0] / SQUARE_SIZE).clamp(0.0, (layer.width - 1) as f32) as u32;
    let dst_gz = (dst[1] / SQUARE_SIZE).clamp(0.0, (layer.height - 1) as f32) as u32;

    let src_node = layer.get_node_at(src_gx, src_gz);
    let dst_node = layer.get_node_at(dst_gx, dst_gz);

    if src_node == INVALID_INDEX || dst_node == INVALID_INDEX {
        return Path::empty();
    }

    // Trivial: same node.
    if src_node == dst_node {
        return Path {
            points: vec![src, dst],
        };
    }

    // Check destination is reachable.
    if !layer.node(dst_node).is_passable() {
        return Path::empty();
    }

    // Bump search generation.
    let search_id = layer.terrain_gen * 1000 + 1;
    layer.terrain_gen += 1;

    let h_mult = 1.0 / layer.max_speed_mod.max(0.001);

    // Ensure neighbors are computed for all leaf nodes.
    build_all_neighbors(layer);

    // Initialize source node.
    {
        let node = layer.node_mut(src_node);
        node.g_cost = 0.0;
        node.h_cost = distance(src, dst) * h_mult;
        node.prev_node = INVALID_INDEX;
        node.entry_point = src;
        node.search_id = search_id;
        node.search_state = 0; // open
    }

    let mut open: BinaryHeap<OpenEntry> = BinaryHeap::new();
    open.push(OpenEntry {
        f_cost: layer.node(src_node).f_cost(),
        index: src_node,
    });

    let mut best_node = src_node;
    let mut best_h = layer.node(src_node).h_cost;
    let mut iterations = 0u32;

    while let Some(entry) = open.pop() {
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            break;
        }

        let cur = entry.index;

        // Skip stale entries.
        if layer.node(cur).search_id != search_id {
            continue;
        }
        if layer.node(cur).search_state == 1 {
            continue; // already closed
        }

        layer.node_mut(cur).search_state = 1; // close

        if cur == dst_node {
            return trace_path(layer, src, dst, dst_node);
        }

        if !layer.node(cur).is_passable() {
            continue;
        }

        // Track closest node to goal for partial paths.
        let cur_h = layer.node(cur).h_cost;
        if cur_h < best_h {
            best_h = cur_h;
            best_node = cur;
        }

        // Iterate neighbors.
        let neighbors: Vec<(NodeIndex, Vec<[f32; 2]>)> = {
            let node = &layer.nodes[cur as usize];
            let num_neighbors = node.neighbors.len();
            let mut result = Vec::with_capacity(num_neighbors);
            for (i, &ngb_idx) in node.neighbors.iter().enumerate() {
                let start = i * NETPOINTS_PER_EDGE;
                let end = (start + NETPOINTS_PER_EDGE).min(node.netpoints.len());
                let pts: Vec<[f32; 2]> = node.netpoints[start..end].to_vec();
                result.push((ngb_idx, pts));
            }
            result
        };

        let cur_entry = layer.node(cur).entry_point;
        let cur_g = layer.node(cur).g_cost;
        let cur_cost = layer.node(cur).move_cost;

        for (ngb_idx, netpts) in &neighbors {
            let ngb_idx = *ngb_idx;
            if !layer.node(ngb_idx).is_passable() {
                continue;
            }

            // Find best netpoint.
            let mut best_net_g = f32::INFINITY;
            let mut best_net_h = f32::INFINITY;
            let mut best_net_pt = netpts.first().copied().unwrap_or(dst);

            for net_pt in netpts {
                let g_dist = distance(cur_entry, *net_pt);
                let h_dist = distance(*net_pt, dst);

                let g = cur_g + cur_cost * g_dist;
                let h = h_dist * h_mult;

                if g + h < best_net_g + best_net_h {
                    best_net_g = g;
                    best_net_h = h;
                    best_net_pt = *net_pt;
                }
            }

            let ngb = &layer.nodes[ngb_idx as usize];
            let is_visited = ngb.search_id == search_id;
            let is_closed = is_visited && ngb.search_state == 1;

            if !is_visited || best_net_g < ngb.g_cost {
                if is_closed {
                    continue; // don't reopen closed nodes (for performance)
                }

                let ngb = layer.node_mut(ngb_idx);
                ngb.g_cost = best_net_g;
                ngb.h_cost = best_net_h;
                ngb.prev_node = cur;
                ngb.entry_point = best_net_pt;
                ngb.search_id = search_id;
                ngb.search_state = 0; // open

                open.push(OpenEntry {
                    f_cost: best_net_g + best_net_h,
                    index: ngb_idx,
                });
            }
        }
    }

    // Partial path to closest reachable node.
    if best_node != src_node {
        let best_center = [
            layer.node(best_node).center_x(),
            layer.node(best_node).center_z(),
        ];
        return trace_path(layer, src, best_center, best_node);
    }

    Path::empty()
}

/// Build neighbor caches for all leaf nodes.
fn build_all_neighbors(layer: &mut NodeLayer) {
    let current_gen = layer.terrain_gen;
    let leaf_indices: Vec<NodeIndex> = layer
        .nodes
        .iter()
        .filter(|n| n.is_leaf() && n.index != INVALID_INDEX)
        .map(|n| n.index)
        .collect();

    for idx in leaf_indices {
        if layer.node(idx).neighbor_gen == current_gen {
            continue;
        }
        build_neighbors_for(layer, idx);
        layer.node_mut(idx).neighbor_gen = current_gen;
    }
}

/// Build the neighbor list for a single leaf node by walking along its edges.
fn build_neighbors_for(layer: &mut NodeLayer, node_index: NodeIndex) {
    let node = &layer.nodes[node_index as usize];
    let xmin = node.xmin;
    let xmax = node.xmax;
    let zmin = node.zmin;
    let zmax = node.zmax;

    let mut neighbors: Vec<NodeIndex> = Vec::new();
    let mut netpoints: Vec<[f32; 2]> = Vec::new();

    // West edge (x = xmin - 1)
    if xmin > 0 {
        walk_edge(
            layer,
            xmin - 1,
            zmin,
            zmax,
            &mut neighbors,
            &mut netpoints,
            xmin,
            zmin,
            zmax,
        );
    }
    // East edge (x = xmax)
    if xmax < layer.width {
        walk_edge(
            layer,
            xmax,
            zmin,
            zmax,
            &mut neighbors,
            &mut netpoints,
            xmax,
            zmin,
            zmax,
        );
    }
    // North edge (z = zmin - 1)
    if zmin > 0 {
        walk_edge_h(
            layer,
            zmin - 1,
            xmin,
            xmax,
            &mut neighbors,
            &mut netpoints,
            xmin,
            xmax,
            zmin,
        );
    }
    // South edge (z = zmax)
    if zmax < layer.height {
        walk_edge_h(
            layer,
            zmax,
            xmin,
            xmax,
            &mut neighbors,
            &mut netpoints,
            xmin,
            xmax,
            zmax,
        );
    }

    let node = &mut layer.nodes[node_index as usize];
    node.neighbors = neighbors;
    node.netpoints = netpoints;
}

/// Walk along a vertical edge (fixed x, varying z) to find neighbors.
#[allow(clippy::too_many_arguments)]
fn walk_edge(
    layer: &NodeLayer,
    edge_x: u32,
    zmin: u32,
    zmax: u32,
    neighbors: &mut Vec<NodeIndex>,
    netpoints: &mut Vec<[f32; 2]>,
    boundary_x: u32,
    node_zmin: u32,
    node_zmax: u32,
) {
    let mut z = zmin;
    while z < zmax {
        let ngb_idx = layer.get_node_at(edge_x, z);
        if ngb_idx == INVALID_INDEX {
            z += 1;
            continue;
        }
        let ngb = layer.node(ngb_idx);

        // Avoid duplicate neighbors.
        if !neighbors.contains(&ngb_idx) {
            neighbors.push(ngb_idx);

            // Compute netpoints along the shared edge.
            let shared_zmin = node_zmin.max(ngb.zmin);
            let shared_zmax = node_zmax.min(ngb.zmax);
            let wx = boundary_x as f32 * SQUARE_SIZE;

            for i in 0..NETPOINTS_PER_EDGE {
                let alpha = (i as f32 + 1.0) / (NETPOINTS_PER_EDGE as f32 + 1.0);
                let wz =
                    (shared_zmin as f32 + (shared_zmax - shared_zmin) as f32 * alpha) * SQUARE_SIZE;
                netpoints.push([wx, wz]);
            }
        }

        z = ngb.zmax; // skip to end of this neighbor
    }
}

/// Walk along a horizontal edge (fixed z, varying x) to find neighbors.
#[allow(clippy::too_many_arguments)]
fn walk_edge_h(
    layer: &NodeLayer,
    edge_z: u32,
    xmin: u32,
    xmax: u32,
    neighbors: &mut Vec<NodeIndex>,
    netpoints: &mut Vec<[f32; 2]>,
    node_xmin: u32,
    node_xmax: u32,
    boundary_z: u32,
) {
    let mut x = xmin;
    while x < xmax {
        let ngb_idx = layer.get_node_at(x, edge_z);
        if ngb_idx == INVALID_INDEX {
            x += 1;
            continue;
        }
        let ngb = layer.node(ngb_idx);

        if !neighbors.contains(&ngb_idx) {
            neighbors.push(ngb_idx);

            let shared_xmin = node_xmin.max(ngb.xmin);
            let shared_xmax = node_xmax.min(ngb.xmax);
            let wz = boundary_z as f32 * SQUARE_SIZE;

            for i in 0..NETPOINTS_PER_EDGE {
                let alpha = (i as f32 + 1.0) / (NETPOINTS_PER_EDGE as f32 + 1.0);
                let wx =
                    (shared_xmin as f32 + (shared_xmax - shared_xmin) as f32 * alpha) * SQUARE_SIZE;
                netpoints.push([wx, wz]);
            }
        }

        x = ngb.xmax;
    }
}

/// Trace the path backwards from target to source via prev_node pointers.
fn trace_path(layer: &NodeLayer, src: [f32; 2], dst: [f32; 2], target_node: NodeIndex) -> Path {
    let mut points: Vec<[f32; 2]> = Vec::new();
    points.push(dst);

    let mut cur = target_node;
    while cur != INVALID_INDEX {
        let entry = layer.node(cur).entry_point;
        if points.last() != Some(&entry) {
            points.push(entry);
        }
        cur = layer.node(cur).prev_node;
    }

    // The first point should be src.
    if points.last() != Some(&src) {
        points.push(src);
    }

    points.reverse();

    // Refine: reroute segments that cross blocked cells.
    refine_path(&mut points, layer.width, layer.height, |x, z| {
        if x < layer.width && z < layer.height {
            let idx = layer.grid[(z * layer.width + x) as usize];
            if idx != INVALID_INDEX {
                return layer.nodes[idx as usize].is_passable();
            }
        }
        false
    });

    // Simple smoothing: remove collinear waypoints.
    smooth_path(&mut points);

    Path { points }
}

/// Refine a coarse path by checking each segment against the grid.
/// If a segment crosses a blocked cell, insert a detour waypoint that
/// avoids the blocked area.
fn refine_path(
    points: &mut Vec<[f32; 2]>,
    grid_width: u32,
    grid_height: u32,
    is_passable: impl Fn(u32, u32) -> bool,
) {
    let mut i = 0;
    let mut safety = 0;
    while i + 1 < points.len() && safety < 5000 {
        safety += 1;
        let p0 = points[i];
        let p1 = points[i + 1];

        if let Some(blocked_cell) =
            find_blocked_cell_on_segment(p0, p1, grid_width, grid_height, &is_passable)
        {
            // Find a passable cell adjacent to the blocked one to route around.
            if let Some(detour) = find_detour(blocked_cell, grid_width, grid_height, &is_passable) {
                let detour_world = [
                    (detour.0 as f32 + 0.5) * SQUARE_SIZE,
                    (detour.1 as f32 + 0.5) * SQUARE_SIZE,
                ];
                points.insert(i + 1, detour_world);
                // Don't advance i — re-check the segment to p0→detour.
                continue;
            }
        }
        i += 1;
    }
}

/// Walk a line from p0 to p1 at cell resolution. Return the first blocked cell, if any.
fn find_blocked_cell_on_segment(
    p0: [f32; 2],
    p1: [f32; 2],
    grid_width: u32,
    grid_height: u32,
    is_passable: &impl Fn(u32, u32) -> bool,
) -> Option<(u32, u32)> {
    let steps = ((distance(p0, p1) / SQUARE_SIZE) as u32).max(1).min(500);
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let wx = p0[0] + (p1[0] - p0[0]) * t;
        let wz = p0[1] + (p1[1] - p0[1]) * t;
        let gx = (wx / SQUARE_SIZE) as u32;
        let gz = (wz / SQUARE_SIZE) as u32;
        if gx < grid_width && gz < grid_height && !is_passable(gx, gz) {
            return Some((gx, gz));
        }
    }
    None
}

/// Find a passable cell adjacent to a blocked cell (simple 8-directional search).
fn find_detour(
    blocked: (u32, u32),
    grid_width: u32,
    grid_height: u32,
    is_passable: &impl Fn(u32, u32) -> bool,
) -> Option<(u32, u32)> {
    let offsets: [(i32, i32); 8] = [
        (-1, 0),
        (1, 0),
        (0, -1),
        (0, 1),
        (-1, -1),
        (1, -1),
        (-1, 1),
        (1, 1),
    ];
    for (dx, dz) in &offsets {
        let nx = blocked.0 as i32 + dx;
        let nz = blocked.1 as i32 + dz;
        if nx >= 0 && nx < grid_width as i32 && nz >= 0 && nz < grid_height as i32 {
            let nx = nx as u32;
            let nz = nz as u32;
            if is_passable(nx, nz) {
                return Some((nx, nz));
            }
        }
    }
    None
}

/// Remove waypoints that are nearly collinear with their neighbors.
fn smooth_path(points: &mut Vec<[f32; 2]>) {
    if points.len() < 3 {
        return;
    }

    for _ in 0..8 {
        let mut changed = false;
        let mut i = 1;
        while i < points.len().saturating_sub(1) {
            let p0 = points[i - 1];
            let p1 = points[i];
            let p2 = points[i + 1];

            let d01 = normalize_2d(sub_2d(p1, p0));
            let d12 = normalize_2d(sub_2d(p2, p1));
            let dot = d01[0] * d12[0] + d01[1] * d12[1];

            if dot > 0.98 {
                points.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
        if !changed {
            break;
        }
    }
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let dx = b[0] - a[0];
    let dz = b[1] - a[1];
    (dx * dx + dz * dz).sqrt()
}

fn sub_2d(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [a[0] - b[0], a[1] - b[1]]
}

fn normalize_2d(v: [f32; 2]) -> [f32; 2] {
    let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if len > 0.0001 {
        [v[0] / len, v[1] / len]
    } else {
        [0.0, 0.0]
    }
}

/// Binary heap entry for A* open set.
#[derive(Clone, Copy)]
struct OpenEntry {
    f_cost: f32,
    index: NodeIndex,
}

impl PartialEq for OpenEntry {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl Eq for OpenEntry {}

impl PartialOrd for OpenEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OpenEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse ordering: BinaryHeap is a max-heap, we want min-heap.
        other
            .f_cost
            .partial_cmp(&self.f_cost)
            .unwrap_or(Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::SpeedMap;
    use crate::node_layer::NodeLayer;

    #[test]
    fn trivial_same_node() {
        let speed_map = SpeedMap::uniform(8, 8, 1.0);
        let mut layer = NodeLayer::new(&speed_map);
        let path = find_path(&mut layer, [10.0, 10.0], [20.0, 20.0]);
        assert!(!path.is_empty());
        assert_eq!(path.len(), 2); // just src and dst
    }

    #[test]
    fn straight_line_open_terrain() {
        let speed_map = SpeedMap::uniform(32, 32, 1.0);
        let mut layer = NodeLayer::new(&speed_map);
        let path = find_path(&mut layer, [10.0, 10.0], [200.0, 200.0]);
        assert!(!path.is_empty());
        assert!(path.len() >= 2);
    }

    #[test]
    fn blocked_destination() {
        let mut speed_map = SpeedMap::uniform(16, 16, 1.0);
        // Block the destination area.
        for z in 12..16 {
            for x in 12..16 {
                speed_map.speeds[(z * 16 + x) as usize] = 0.0;
            }
        }
        let mut layer = NodeLayer::new(&speed_map);
        let path = find_path(&mut layer, [10.0, 10.0], [112.0, 112.0]);
        // Destination is blocked — should return empty.
        assert!(path.is_empty());
    }

    #[test]
    fn path_around_wall() {
        // 32x32 map with a vertical wall from z=4 to z=28 at x=16.
        let mut speed_map = SpeedMap::uniform(32, 32, 1.0);
        for z in 4..28 {
            speed_map.speeds[(z * 32 + 16) as usize] = 0.0;
        }
        let mut layer = NodeLayer::new(&speed_map);

        // Path from left side to right side — must go around the wall.
        let src = [40.0, 128.0]; // x=5, z=16 in grid
        let dst = [200.0, 128.0]; // x=25, z=16 in grid
        let path = find_path(&mut layer, src, dst);

        assert!(!path.is_empty(), "should find a path around the wall");
        assert!(path.len() > 2, "path should have intermediate waypoints");
        // Path length should be longer than straight-line distance.
        let straight = distance(src, dst);
        assert!(
            path.total_length() > straight,
            "path should be longer than straight line: {} vs {}",
            path.total_length(),
            straight,
        );
    }

    #[test]
    fn large_map_performance() {
        let speed_map = SpeedMap::uniform(256, 256, 1.0);
        let mut layer = NodeLayer::new(&speed_map);
        let path = find_path(&mut layer, [10.0, 10.0], [2000.0, 2000.0]);
        assert!(!path.is_empty());
    }
}
