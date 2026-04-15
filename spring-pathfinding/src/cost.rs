//! The movement speed map: a 2D grid of speed modifiers derived from
//! terrain heightmap slopes and terrain types.
//!
//! Each cell stores a relative speed from 0.0 (impassable) to 1.0 (full speed).
//! The grid resolution matches the Spring heightmap: one cell per `SQUARE_SIZE`
//! (8 world units).

pub const SQUARE_SIZE: f32 = 8.0;
pub const NUM_SPEEDMOD_BINS: u8 = 10;

/// A precomputed speed map for one movement type.
pub struct SpeedMap {
    pub width: u32,
    pub height: u32,
    /// Relative speed modifier per cell, 0.0..=1.0. Row-major.
    pub speeds: Vec<f32>,
}

impl SpeedMap {
    /// Build a speed map from heightmap data.
    ///
    /// `heights` is row-major `(width+1) * (height+1)` (fence-post vertices).
    /// `max_slope` is the maximum traversable slope (typically 0.5–1.0 for kbots).
    /// `slope_mod` controls how much slope reduces speed (typically 40.0).
    pub fn from_heightmap(
        heights: &[f32],
        heightmap_width: u32,
        heightmap_height: u32,
        max_slope: f32,
        slope_mod: f32,
    ) -> Self {
        let width = heightmap_width - 1;
        let height = heightmap_height - 1;
        let hw = heightmap_width as usize;

        let mut speeds = Vec::with_capacity((width * height) as usize);

        for z in 0..height as usize {
            for x in 0..width as usize {
                // Compute slope from the four corners of this cell.
                let h00 = heights[z * hw + x];
                let h10 = heights[z * hw + x + 1];
                let h01 = heights[(z + 1) * hw + x];
                let h11 = heights[(z + 1) * hw + x + 1];

                let dx = ((h10 - h00).abs() + (h11 - h01).abs()) * 0.5 / SQUARE_SIZE;
                let dz = ((h01 - h00).abs() + (h11 - h10).abs()) * 0.5 / SQUARE_SIZE;
                let slope = (dx * dx + dz * dz).sqrt();

                let speed = if slope > max_slope {
                    0.0 // impassable
                } else {
                    (1.0 / (1.0 + slope * slope_mod)).clamp(0.0, 1.0)
                };

                speeds.push(speed);
            }
        }

        Self {
            width,
            height,
            speeds,
        }
    }

    /// Build a uniform speed map (all cells have the same speed).
    pub fn uniform(width: u32, height: u32, speed: f32) -> Self {
        Self {
            width,
            height,
            speeds: vec![speed; (width * height) as usize],
        }
    }

    /// Get speed at grid position, or 0.0 if out of bounds.
    pub fn get(&self, x: u32, z: u32) -> f32 {
        if x < self.width && z < self.height {
            self.speeds[(z * self.width + x) as usize]
        } else {
            0.0
        }
    }

    /// Quantize a speed value into a bin index.
    pub fn speed_to_bin(speed: f32) -> u8 {
        if speed <= 0.001 {
            NUM_SPEEDMOD_BINS // blocked bin
        } else if speed >= 0.999 {
            NUM_SPEEDMOD_BINS + 1 // unrestricted bin
        } else {
            ((NUM_SPEEDMOD_BINS as f32 * speed) as u8).min(NUM_SPEEDMOD_BINS - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_terrain_full_speed() {
        // 3x3 heightmap → 2x2 speed map, all flat.
        let heights = vec![0.0; 3 * 3];
        let map = SpeedMap::from_heightmap(&heights, 3, 3, 1.0, 40.0);
        assert_eq!(map.width, 2);
        assert_eq!(map.height, 2);
        assert!((map.get(0, 0) - 1.0).abs() < 0.01);
        assert!((map.get(1, 1) - 1.0).abs() < 0.01);
    }

    #[test]
    fn steep_slope_blocked() {
        // Create a cliff: row 0 at height 0, row 1 at height 100.
        let heights = vec![0.0, 0.0, 0.0, 100.0, 100.0, 100.0];
        let map = SpeedMap::from_heightmap(&heights, 3, 2, 0.5, 40.0);
        // The slope is 100/8 = 12.5, way above max_slope=0.5.
        assert_eq!(map.get(0, 0), 0.0);
    }

    #[test]
    fn bin_blocked() {
        assert_eq!(SpeedMap::speed_to_bin(0.0), NUM_SPEEDMOD_BINS);
    }

    #[test]
    fn bin_unrestricted() {
        assert_eq!(SpeedMap::speed_to_bin(1.0), NUM_SPEEDMOD_BINS + 1);
    }

    #[test]
    fn bin_midrange() {
        let bin = SpeedMap::speed_to_bin(0.5);
        assert!(bin >= 1 && bin <= NUM_SPEEDMOD_BINS - 1);
    }
}
