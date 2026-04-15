//! QTPFS (Quad-Tree Path Finding System) for Spring RTS engine maps.
//!
//! Builds a quad-tree over the movement speed map, runs A* on the
//! quad-tree graph for coarse pathfinding, then smooths the result.
//! Faithfully mirrors the Spring engine's implementation.

mod cost;
mod node;
mod node_layer;
mod path;
mod search;
mod tessellate;

pub use cost::SpeedMap;
pub use node_layer::NodeLayer;
pub use path::Path;
pub use search::find_path;
