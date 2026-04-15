/// Quad-tree tessellation: recursively split nodes based on speed heterogeneity.
use crate::cost::SpeedMap;
use crate::node::NodeIndex;
use crate::node_layer::{MAX_DEPTH, NodeLayer};

/// Tessellate a node: compute its cost, split if heterogeneous, recurse into children.
pub fn tessellate(layer: &mut NodeLayer, node_index: NodeIndex, speed_map: &SpeedMap, depth: u32) {
    let (want_split, need_split) = update_move_cost(layer, node_index, speed_map);

    let can_split_normal = layer.node(node_index).can_split(MAX_DEPTH, depth, false);
    let can_split_forced = layer.node(node_index).can_split(MAX_DEPTH, depth, true);

    let do_split = (want_split && can_split_normal) || (need_split && can_split_forced);

    if do_split {
        split_node(layer, node_index);
        let child_base = layer.node(node_index).child_base;
        for i in 0..4 {
            tessellate(layer, child_base + i, speed_map, depth + 1);
        }
    } else {
        // Leaf node — register in grid.
        layer.register_leaf(node_index);
    }
}

/// Compute speed stats for a node. Returns (want_split, need_split).
///
/// - `want_split`: cells have different speed bins (heterogeneous)
/// - `need_split`: some cells are blocked but not all (partially passable)
fn update_move_cost(
    layer: &mut NodeLayer,
    node_index: NodeIndex,
    speed_map: &SpeedMap,
) -> (bool, bool) {
    let node = &layer.nodes[node_index as usize];
    let xmin = node.xmin;
    let xmax = node.xmax;
    let zmin = node.zmin;
    let zmax = node.zmax;

    let reference_speed = speed_map.get(xmin, zmin);
    let reference_bin = SpeedMap::speed_to_bin(reference_speed);

    let mut speed_sum: f32 = 0.0;
    let mut num_different_bin: u32 = 0;
    let mut num_blocked: u32 = 0;
    let area = (xmax - xmin) * (zmax - zmin);

    for z in zmin..zmax {
        for x in xmin..xmax {
            let speed = speed_map.get(x, z);
            let bin = SpeedMap::speed_to_bin(speed);

            speed_sum += speed;
            if bin != reference_bin {
                num_different_bin += 1;
            }
            if speed <= 0.001 {
                num_blocked += 1;
            }
        }
    }

    let speed_avg = if area > 0 {
        speed_sum / area as f32
    } else {
        0.0
    };
    let move_cost = if speed_avg > 0.001 {
        1.0 / speed_avg
    } else {
        f32::INFINITY
    };

    // Handle partially blocked: increase cost proportionally.
    let move_cost = if num_blocked > 0 && num_blocked < area {
        let blocked_fraction = num_blocked as f32 / area as f32;
        move_cost + (1 << 20) as f32 * blocked_fraction
    } else {
        move_cost
    };

    let node = &mut layer.nodes[node_index as usize];
    node.speed_mod_sum = speed_sum;
    node.speed_mod_avg = speed_avg;
    node.move_cost = move_cost;

    let want_split = num_different_bin > 0;
    let need_split = num_blocked > 0 && num_blocked < area;

    (want_split, need_split)
}

/// Split a node into 4 children (TL, TR, BR, BL).
fn split_node(layer: &mut NodeLayer, node_index: NodeIndex) {
    let node = &layer.nodes[node_index as usize];
    let xmin = node.xmin;
    let xmax = node.xmax;
    let zmin = node.zmin;
    let zmax = node.zmax;
    let xmid = (xmin + xmax) / 2;
    let zmid = (zmin + zmax) / 2;

    let tl = layer.alloc_node(xmin, xmid, zmin, zmid);
    let _tr = layer.alloc_node(xmid, xmax, zmin, zmid);
    let _br = layer.alloc_node(xmid, xmax, zmid, zmax);
    let _bl = layer.alloc_node(xmin, xmid, zmid, zmax);

    layer.nodes[node_index as usize].child_base = tl;
}
