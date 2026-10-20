//! The 32-elmo cell grid the port's maps are drawn on ([`crate::kpgen`])
//! and their shifting blocks live on ([`crate::shifting`]).

/// Elmos per cell (and ground-texture texels: one texel per elmo).
pub const CELL: usize = 32;
/// Map squares per cell.
pub const CELL_SQ: usize = CELL / 8;

/// A half-open rectangle of cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x0: usize,
    pub z0: usize,
    pub x1: usize,
    pub z1: usize,
}

impl Rect {
    pub const fn new(x0: usize, z0: usize, x1: usize, z1: usize) -> Self {
        Self { x0, z0, x1, z1 }
    }

    /// The rectangle turned 180° about the centre of a `cw × cd` grid.
    pub fn rot180(self, cw: usize, cd: usize) -> Self {
        Self::new(cw - self.x1, cd - self.z1, cw - self.x0, cd - self.z0)
    }

    /// The rectangle turned a quarter clockwise about the centre of an
    /// `n × n` grid: `(x, z) → (n − z, x)`.
    pub fn rot90(self, n: usize) -> Self {
        Self::new(n - self.z1, self.x0, n - self.z0, self.x1)
    }

    /// The rectangle mirrored across the vertical centre line of a grid
    /// `cw` cells wide.
    pub fn mirror_x(self, cw: usize) -> Self {
        Self::new(cw - self.x1, self.z0, cw - self.x0, self.z1)
    }

    /// The rectangle mirrored across the horizontal centre line of a
    /// grid `cd` cells down.
    pub fn mirror_z(self, cd: usize) -> Self {
        Self::new(self.x0, cd - self.z1, self.x1, cd - self.z0)
    }

    pub fn contains(&self, x: usize, z: usize) -> bool {
        (self.x0..self.x1).contains(&x) && (self.z0..self.z1).contains(&z)
    }

    /// The world-space centre (elmos).
    pub fn centre(&self) -> (f32, f32) {
        (
            (self.x0 + self.x1) as f32 * 0.5 * CELL as f32,
            (self.z0 + self.z1) as f32 * 0.5 * CELL as f32,
        )
    }
}

/// A point turned 180° about the centre of a `w × d` elmo world.
pub fn rot180(p: (f32, f32), w: f32, d: f32) -> (f32, f32) {
    (w - p.0, d - p.1)
}

/// A point turned a quarter clockwise about the centre of an `s × s`
/// elmo world: `(x, z) → (s − z, x)`.
pub fn rot90(p: (f32, f32), s: f32) -> (f32, f32) {
    (s - p.1, p.0)
}

/// A point turned by `degrees` (clockwise seen from above) about `c`.
pub fn rotate(p: (f32, f32), c: (f32, f32), degrees: f32) -> (f32, f32) {
    let (s, co) = libm::sincosf(degrees.to_radians());
    let (dx, dz) = (p.0 - c.0, p.1 - c.1);
    (c.0 + dx * co - dz * s, c.1 + dx * s + dz * co)
}
