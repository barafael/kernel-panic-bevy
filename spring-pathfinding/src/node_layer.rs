/// NodeLayer: owns the node pool and the grid mapping cells to leaf nodes.
use crate::cost::SpeedMap;
use crate::node::{INVALID_INDEX, NodeIndex, QTNode};
use crate::tessellate;

/// Default maximum quad-tree depth.
pub const MAX_DEPTH: u32 = 16;

pub struct NodeLayer {
    pub width: u32,
    pub height: u32,

    /// Node pool (arena). Nodes are referenced by index.
    pub nodes: Vec<QTNode>,
    /// Next free index in the pool.
    next_free: NodeIndex,

    /// Grid mapping each cell (x, z) to the leaf node that covers it.
    /// Size: width * height.
    pub grid: Vec<NodeIndex>,

    /// Root node index.
    pub root: NodeIndex,

    /// Maximum relative speed mod across all cells (for admissible heuristic).
    pub max_speed_mod: f32,

    /// Terrain change counter (for neighbor cache invalidation).
    pub terrain_gen: u32,
}

impl NodeLayer {
    /// Build a new NodeLayer from a speed map.
    pub fn new(speed_map: &SpeedMap) -> Self {
        let width = speed_map.width;
        let height = speed_map.height;

        // Pre-allocate pool. Worst case: every cell is its own leaf = w*h,
        // plus internal nodes. 2 * w * h is a safe upper bound.
        let pool_capacity = (width * height * 2).max(1024) as usize;
        let mut layer = Self {
            width,
            height,
            nodes: Vec::with_capacity(pool_capacity),
            next_free: 0,
            grid: vec![INVALID_INDEX; (width * height) as usize],
            root: INVALID_INDEX,
            max_speed_mod: 0.0,
            terrain_gen: 1,
        };

        // Compute max speed mod.
        layer.max_speed_mod = speed_map
            .speeds
            .iter()
            .cloned()
            .fold(0.0f32, f32::max)
            .max(0.001);

        // Allocate root node.
        let root = layer.alloc_node(0, width, 0, height);
        layer.root = root;

        // Tessellate.
        tessellate::tessellate(&mut layer, root, speed_map, 0);

        layer
    }

    /// Allocate a new node in the pool and return its index.
    pub fn alloc_node(&mut self, xmin: u32, xmax: u32, zmin: u32, zmax: u32) -> NodeIndex {
        let index = self.next_free;
        self.next_free += 1;
        if index as usize >= self.nodes.len() {
            self.nodes.push(QTNode::new(xmin, xmax, zmin, zmax, index));
        } else {
            self.nodes[index as usize] = QTNode::new(xmin, xmax, zmin, zmax, index);
        }
        index
    }

    /// Register a leaf node: write it into the grid for all cells it covers.
    pub fn register_leaf(&mut self, node_index: NodeIndex) {
        let node = &self.nodes[node_index as usize];
        let xmin = node.xmin;
        let xmax = node.xmax;
        let zmin = node.zmin;
        let zmax = node.zmax;

        for z in zmin..zmax {
            for x in xmin..xmax {
                self.grid[(z * self.width + x) as usize] = node_index;
            }
        }
    }

    /// Get the leaf node covering grid cell (x, z).
    pub fn get_node_at(&self, x: u32, z: u32) -> NodeIndex {
        if x < self.width && z < self.height {
            self.grid[(z * self.width + x) as usize]
        } else {
            INVALID_INDEX
        }
    }

    /// Get a reference to a node by index.
    pub fn node(&self, index: NodeIndex) -> &QTNode {
        &self.nodes[index as usize]
    }

    /// Get a mutable reference to a node by index.
    pub fn node_mut(&mut self, index: NodeIndex) -> &mut QTNode {
        &mut self.nodes[index as usize]
    }

    /// Count the number of leaf nodes.
    pub fn leaf_count(&self) -> usize {
        self.nodes
            .iter()
            .take(self.next_free as usize)
            .filter(|n| n.is_leaf())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_map_single_leaf() {
        let speed_map = SpeedMap::uniform(8, 8, 1.0);
        let layer = NodeLayer::new(&speed_map);

        // Uniform map: all cells same speed → root is the only leaf (no splits).
        assert_eq!(layer.leaf_count(), 1);
        assert!(layer.node(layer.root).is_leaf());
        assert!((layer.node(layer.root).speed_mod_avg - 1.0).abs() < 0.01);
    }

    #[test]
    fn half_blocked_map_splits() {
        let mut speed_map = SpeedMap::uniform(8, 8, 1.0);
        // Block the left half.
        for z in 0..8 {
            for x in 0..4 {
                speed_map.speeds[(z * 8 + x) as usize] = 0.0;
            }
        }

        let layer = NodeLayer::new(&speed_map);

        // Should have split — more than 1 leaf.
        assert!(layer.leaf_count() > 1);

        // Check that grid cells in blocked area point to impassable nodes.
        let blocked_node = layer.node(layer.get_node_at(0, 0));
        assert!(!blocked_node.is_passable());

        // Check that grid cells in open area point to passable nodes.
        let open_node = layer.node(layer.get_node_at(7, 7));
        assert!(open_node.is_passable());
    }

    #[test]
    fn large_map_doesnt_explode() {
        let speed_map = SpeedMap::uniform(256, 256, 1.0);
        let layer = NodeLayer::new(&speed_map);
        // Uniform → single leaf.
        assert_eq!(layer.leaf_count(), 1);
    }

    #[test]
    fn checkerboard_splits_maximally() {
        // Alternating passable/blocked in a 4x4 grid.
        let mut speeds = vec![1.0; 4 * 4];
        for z in 0..4u32 {
            for x in 0..4u32 {
                if (x + z) % 2 == 0 {
                    speeds[(z * 4 + x) as usize] = 0.0;
                }
            }
        }
        let speed_map = SpeedMap {
            width: 4,
            height: 4,
            speeds,
        };
        let layer = NodeLayer::new(&speed_map);
        // Should have many leaves due to heterogeneity.
        assert!(layer.leaf_count() >= 4);
    }
}
