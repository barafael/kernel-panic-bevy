//! Pathfinding for Spring RTS engine maps, as Kernel Panic's engine
//! does it (`pathFinderSystem=1`, QTPFS):
//!
//! 1. [`SpeedMap`] — per-square movement cost from the terrain slope,
//!    with slopes beyond the unit's `MaxSlope` hard-blocked
//!    (`CMoveMath::GetPosSpeedMod`).
//! 2. [`BlockMask`] — squares a mover class may not enter because a
//!    structure lies within its footprint.
//! 3. [`qtpfs`] — the quad-tree pathfinder: tesselation of the speed
//!    map into leaves, a bidirectional A* over the edges they share,
//!    one smoothing pass, partial paths toward unreachable goals.

mod block;
mod cost;
pub mod qtpfs;

pub use block::{BlockMask, line_clear, traverse_cells};
pub use cost::{
    SQUARE_SIZE, SpeedMap, max_slope_from_degrees, slope_from_rise_run, slope_map,
    slope_mod_from_max_slope,
};
