//! Grid pathfinding for Spring RTS engine maps.
//!
//! The classic Spring approach, kept deliberately simple:
//!
//! 1. [`SpeedMap`] — per-square movement cost from terrain slope, with
//!    slopes beyond the unit's `MaxSlope` hard-blocked.
//! 2. [`find_path`] — uniform-grid A* (8-connected) over the speed map,
//!    then line-of-sight waypoint smoothing. When the goal is
//!    unreachable, the search converges on the closest reachable cell —
//!    the upstream `pathingFailed` behaviour (units gather at the
//!    obstacle instead of walking through it).
//!
//! A previous iteration ported Spring 105's QTPFS quad-tree as an
//! acceleration layer. It was removed: at Kernel Panic map sizes the
//! plain grid is fast enough by a wide margin, and the quad-tree's
//! same-leaf straight-line shortcut was the source of units walking
//! straight over cliffs and walls.

mod cost;
mod grid_search;
mod path;

pub use cost::{SpeedMap, max_slope_from_degrees, slope_from_rise_run, slope_mod_from_max_slope};
pub use grid_search::find_path;
pub use path::Path;
