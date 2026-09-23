//! Path heat: Spring's legacy congestion model (`MOVEINFO.TDF`
//! `HeatMapping`). Walking units deposit heat on the cells they cross
//! and the pathfinder treats hot cells as slower ground, so columns of
//! units marching the same line fan out instead of braiding into a
//! single rut that only the collision push can untangle.
//!
//! Upstream keeps per-movement-class heat params (LIGHT smears a light
//! but persistent trail, HEAVY carves a deep but fast-fading one); this
//! grid is the shared store those deposits land in. Cost integration is
//! deliberately soft — heat slows a cell, it never blocks it — so a
//! fully congested map still routes.

/// Per-cell heat, same grid resolution as [`crate::SpeedMap`] (one cell
/// per `SQUARE_SIZE`).
#[derive(Debug, Clone)]
pub struct HeatMap {
    pub width: u32,
    pub height: u32,
    /// Row-major, world units of "congestion" per cell. Zero means an
    /// untouched cell.
    pub heat: Vec<f32>,
}

impl HeatMap {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            heat: vec![0.0; (width * height) as usize],
        }
    }

    /// Deposit `amount` of heat at the cell containing world-space
    /// `world` (XZ). Out-of-bounds deposits are ignored.
    pub fn add_heat(&mut self, world: [f32; 2], amount: f32) {
        if amount <= 0.0 {
            return;
        }
        let Some((x, z)) = Self::cell_of(world, self.width, self.height) else {
            return;
        };
        let idx = (z * self.width + x) as usize;
        self.heat[idx] += amount;
    }

    /// Multiply every cell's heat by `retention` (the per-decay-step
    /// fraction that survives). Callers choose the step period; a
    /// plain multiply keeps a full-grid pass at memcpy cost.
    pub fn decay(&mut self, retention: f32) {
        if retention >= 1.0 {
            return;
        }
        for h in &mut self.heat {
            *h *= retention;
        }
    }

    /// Heat under world-space `world` (XZ), or 0.0 out of bounds.
    pub fn get(&self, world: [f32; 2]) -> f32 {
        match Self::cell_of(world, self.width, self.height) {
            Some((x, z)) => self.heat[(z * self.width + x) as usize],
            None => 0.0,
        }
    }

    /// Max heat of the up-to-four cells surrounding `world` — the
    /// conservative read for line-of-sight style sampling. Truncating
    /// to a single cell lets a shortcut that straddles a hot/cold
    /// boundary score itself on the cold side; taking the neighbours
    /// closes that loophole.
    pub fn get_with_neighbors(&self, world: [f32; 2]) -> f32 {
        let Some((x, z)) = Self::cell_of(world, self.width, self.height) else {
            return 0.0;
        };
        let mut max = 0.0_f32;
        for dz in 0..=1 {
            for dx in 0..=1 {
                let nx = (x + dx).min(self.width - 1);
                let nz = (z + dz).min(self.height - 1);
                max = max.max(self.heat[(nz * self.width + nx) as usize]);
            }
        }
        max
    }

    /// World → grid cell, mirroring the pathfinder's mapping. `None`
    /// outside the grid — negative coordinates must not wrap into
    /// cell (0, 0).
    fn cell_of(world: [f32; 2], width: u32, height: u32) -> Option<(u32, u32)> {
        let max_x = width as f32 * crate::cost::SQUARE_SIZE;
        let max_z = height as f32 * crate::cost::SQUARE_SIZE;
        if !(0.0..max_x).contains(&world[0]) || !(0.0..max_z).contains(&world[1]) {
            return None;
        }
        let x = ((world[0] / crate::cost::SQUARE_SIZE) as u32).min(width - 1);
        let z = ((world[1] / crate::cost::SQUARE_SIZE) as u32).min(height - 1);
        Some((x, z))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deposits_land_on_the_right_cell() {
        let mut hm = HeatMap::new(4, 4);
        hm.add_heat([28.0, 4.0], 5.0);
        assert!((hm.get([28.0, 4.0]) - 5.0).abs() < 1e-5);
        assert!(hm.get([4.0, 28.0]).abs() < 1e-6);
    }

    #[test]
    fn decay_scales_every_cell_and_ignores_identity() {
        let mut hm = HeatMap::new(2, 2);
        hm.add_heat([4.0, 4.0], 8.0);
        hm.decay(1.0);
        assert!((hm.get([4.0, 4.0]) - 8.0).abs() < 1e-5, "identity no-op");
        hm.decay(0.25);
        assert!((hm.get([4.0, 4.0]) - 2.0).abs() < 1e-5);
    }

    #[test]
    fn out_of_bounds_deposits_are_ignored() {
        let mut hm = HeatMap::new(2, 2);
        hm.add_heat([-50.0, 4.0], 5.0);
        hm.add_heat([4.0, 9999.0], 5.0);
        assert!(hm.heat.iter().all(|&h| h == 0.0));
    }
}
