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
    ///
    /// `max_slope` and `slope_mod` are in **Spring's encoding**:
    ///
    /// - `slope` is `1 - normal.y` (= `1 - cos(angle)` for a surface
    ///   tilted by `angle` from horizontal). Range `[0, 1]`.
    /// - `max_slope` is the same `1 - cos(...)` value beyond which a
    ///   square is impassable. Spring derives it from FBI MaxSlope via
    ///   `1 - cos(deg * 1.5)` — so an FBI value of `36` allows
    ///   geometric slopes up to **54°**, not 36°. (See
    ///   [`DegreesToMaxSlope`][move_def] in upstream.)
    /// - `slope_mod` is the slope penalty in
    ///   `speed = 1 / (1 + slope * slope_mod)`. Spring's default is
    ///   `4 / (max_slope + 0.001)` — so steeper-tolerant units get a
    ///   gentler penalty curve to compensate.
    ///
    /// Use [`max_slope_from_degrees`] and [`slope_mod_from_max_slope`]
    /// to compute these from FBI values; that keeps every consumer on
    /// the same encoding upstream uses.
    ///
    /// [move_def]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/Sim/MoveTypes/MoveDefHandler.cpp
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
                // tan(angle) of the steepest slope across the cell.
                let tan_slope = (dx * dx + dz * dz).sqrt();
                // Spring's encoding: `slope = 1 - cos(angle)` where
                // `cos(angle) = 1 / sqrt(1 + tan²(angle))`. Matches
                // `1.0 - faceNormal.y` in `ReadMap::UpdateSlopemap`.
                let slope = 1.0 - 1.0 / (1.0 + tan_slope * tan_slope).sqrt();

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

/// Convert FBI `MaxSlope` (degrees) to Spring's internal max-slope
/// encoding `1 - cos(clamp(deg, 0, 60) * 1.5 * π/180)`. Why: matches
/// upstream's `DegreesToMaxSlope` in `MoveDefHandler.cpp`, including
/// the 1.5× pre-multiplier — an FBI value of 36 permits geometric
/// slopes up to 54°, not 36°. Without that, the port rejects ramps
/// the original game accepts.
pub fn max_slope_from_degrees(degrees: f32) -> f32 {
    let deg = degrees.clamp(0.0, 60.0) * 1.5;
    let rad = deg.to_radians();
    1.0 - rad.cos()
}

/// Encode a single rise/run step in Spring's `1 - cos(angle)` form
/// — the same encoding [`SpeedMap::from_heightmap`] and
/// [`max_slope_from_degrees`] use, so callers can compare step slopes
/// against [`max_slope_from_degrees`] cap values directly. Returns
/// `0.0` for zero or negative `run`.
pub fn slope_from_rise_run(rise: f32, run: f32) -> f32 {
    if run <= 0.0 {
        return 0.0;
    }
    1.0 - run / (rise * rise + run * run).sqrt()
}

/// Default `slope_mod` for a given `max_slope`, mirroring upstream's
/// `slopeMod = 4 / (maxSlope + 0.001)` in `MoveDefHandler.cpp`. Steeper-
/// tolerant units (large `max_slope`) get a gentler penalty curve so
/// the engine doesn't make them crawl on every gentle hill.
pub fn slope_mod_from_max_slope(max_slope: f32) -> f32 {
    4.0 / (max_slope + 0.001)
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
        // Geometric angle ~85.4°; Spring-encoded slope ≈ 0.92.
        let heights = vec![0.0, 0.0, 0.0, 100.0, 100.0, 100.0];
        let map = SpeedMap::from_heightmap(&heights, 3, 2, 0.5, 40.0);
        assert_eq!(map.get(0, 0), 0.0);
    }

    /// FBI MaxSlope=36 (KP's LIGHT/MEDIUM/HEAVY classes) should
    /// produce Spring's internal value of `1 - cos(54°)` ≈ 0.412 —
    /// the 1.5× pre-multiplier is the upstream behaviour our
    /// pathfinder must mirror.
    #[test]
    fn fbi_max_slope_36_matches_upstream_encoding() {
        let s = max_slope_from_degrees(36.0);
        let expected = 1.0 - (54.0_f32.to_radians()).cos();
        assert!(
            (s - expected).abs() < 1e-5,
            "max_slope_from_degrees(36) = {s}, expected {expected}",
        );
    }

    /// 50° geometric ramp is comfortably under FBI MaxSlope=36's
    /// effective cap of 54°. Spring would route units onto this; the
    /// port's old `tan(angle)` cap of 1.0 (= 45°) wouldn't, leaving
    /// units stuck on plateaus. Verify the new encoding lets it
    /// through with a sensible (slow but non-zero) speed.
    #[test]
    fn ramp_under_effective_cap_is_passable() {
        let dh = 8.0 * 50.0_f32.to_radians().tan();
        let heights = vec![0.0, 0.0, 0.0, dh, dh, dh];
        let cap = max_slope_from_degrees(36.0);
        let mod_ = slope_mod_from_max_slope(cap);
        let map = SpeedMap::from_heightmap(&heights, 3, 2, cap, mod_);
        let s = map.get(0, 0);
        assert!(
            s > 0.0,
            "50° ramp must be passable under FBI MaxSlope=36 (got speed {s})",
        );
        assert!(
            s < 0.4,
            "50° ramp speed should be heavily penalised, got {s}"
        );
    }

    /// At a 60° geometric ramp the slope exceeds the FBI MaxSlope=36
    /// effective cap (54°) and the cell is hard-blocked.
    #[test]
    fn ramp_above_effective_cap_is_blocked() {
        let dh = 8.0 * 60.0_f32.to_radians().tan();
        let heights = vec![0.0, 0.0, 0.0, dh, dh, dh];
        let cap = max_slope_from_degrees(36.0);
        let mod_ = slope_mod_from_max_slope(cap);
        let map = SpeedMap::from_heightmap(&heights, 3, 2, cap, mod_);
        assert_eq!(map.get(0, 0), 0.0, "60° ramp should be blocked");
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
        assert!((1..=NUM_SPEEDMOD_BINS - 1).contains(&bin));
    }
}
