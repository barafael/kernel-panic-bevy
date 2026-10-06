//! Structure blocking for the pathfinder and the exact line test the
//! engine's raw search and `CanSetNextWayPoint` corner cutting use.

use crate::cost::{SQUARE_SIZE, SpeedMap};

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
    let t_delta_x = if dx != 0.0 {
        1.0 / dx.abs()
    } else {
        f32::INFINITY
    };
    let t_delta_z = if dz != 0.0 {
        1.0 / dz.abs()
    } else {
        f32::INFINITY
    };
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
/// — terrain passable, not closed by `mask` (`MoveDef::DoRawSearch`).
pub fn line_clear(
    a: [f32; 2],
    b: [f32; 2],
    speed_map: &SpeedMap,
    mask: Option<&BlockMask>,
) -> bool {
    traverse_cells(a, b, |x, z| {
        x >= 0 && z >= 0 && open(speed_map, mask, x as u32, z as u32)
    })
}
