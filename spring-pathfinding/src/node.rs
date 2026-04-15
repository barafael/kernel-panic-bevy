//! A quad-tree node representing a rectangular region of the speed map.
//!
//! Leaf nodes are used in the A* graph. Internal nodes subdivide into 4 children.

/// Index into the node pool.
pub type NodeIndex = u32;

pub const INVALID_INDEX: NodeIndex = u32::MAX;

/// Maximum transition points per neighbor edge.
pub const NETPOINTS_PER_EDGE: usize = 3;

#[derive(Clone, Debug)]
pub struct QTNode {
    pub xmin: u32,
    pub xmax: u32,
    pub zmin: u32,
    pub zmax: u32,

    /// Sum of speed modifiers over all cells in this node.
    pub speed_mod_sum: f32,
    /// Average speed modifier (speed_mod_sum / area).
    pub speed_mod_avg: f32,
    /// Movement cost: 1.0 / speed_mod_avg. INFINITY if impassable.
    pub move_cost: f32,

    /// Index of first child in the pool (children are at base..base+3).
    /// INVALID_INDEX means this is a leaf.
    pub child_base: NodeIndex,

    /// Pool index of this node.
    pub index: NodeIndex,

    // --- Neighbor cache (leaf nodes only) ---
    pub neighbors: Vec<NodeIndex>,
    /// Transition points for each neighbor. For neighbor `i`, the netpoints
    /// are at indices `i * NETPOINTS_PER_EDGE .. (i+1) * NETPOINTS_PER_EDGE`.
    pub netpoints: Vec<[f32; 2]>,

    // --- A* search state ---
    pub g_cost: f32,
    pub h_cost: f32,
    pub prev_node: NodeIndex,
    /// The world-space entry point used by the search for this node.
    pub entry_point: [f32; 2],
    /// Search generation counter. If < current search ID, this node is unvisited.
    pub search_id: u32,
    /// 0 = open, 1 = closed (within current search_id).
    pub search_state: u8,
    /// Neighbor cache generation.
    pub neighbor_gen: u32,
}

impl QTNode {
    pub fn new(xmin: u32, xmax: u32, zmin: u32, zmax: u32, index: NodeIndex) -> Self {
        Self {
            xmin,
            xmax,
            zmin,
            zmax,
            speed_mod_sum: 0.0,
            speed_mod_avg: 0.0,
            move_cost: f32::INFINITY,
            child_base: INVALID_INDEX,
            index,
            neighbors: Vec::new(),
            netpoints: Vec::new(),
            g_cost: f32::INFINITY,
            h_cost: f32::INFINITY,
            prev_node: INVALID_INDEX,
            entry_point: [0.0, 0.0],
            search_id: 0,
            search_state: 0,
            neighbor_gen: 0,
        }
    }

    pub fn is_leaf(&self) -> bool {
        self.child_base == INVALID_INDEX
    }

    pub fn area(&self) -> u32 {
        (self.xmax - self.xmin) * (self.zmax - self.zmin)
    }

    pub fn width(&self) -> u32 {
        self.xmax - self.xmin
    }

    pub fn height(&self) -> u32 {
        self.zmax - self.zmin
    }

    pub fn can_split(&self, max_depth: u32, depth: u32, forced: bool) -> bool {
        if forced {
            return self.width() > 1 && self.height() > 1;
        }
        if depth >= max_depth {
            return false;
        }
        self.width() > 1 && self.height() > 1
    }

    pub fn center_x(&self) -> f32 {
        ((self.xmin + self.xmax) as f32 * 0.5) * super::cost::SQUARE_SIZE
    }

    pub fn center_z(&self) -> f32 {
        ((self.zmin + self.zmax) as f32 * 0.5) * super::cost::SQUARE_SIZE
    }

    pub fn f_cost(&self) -> f32 {
        self.g_cost + self.h_cost
    }

    pub fn is_passable(&self) -> bool {
        self.move_cost < f32::INFINITY * 0.5
    }
}
