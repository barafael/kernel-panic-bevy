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
    /// Relative speed modifier per cell, 0.0..=1.0. Row-major. A direct
    /// write that *raises* a cell above [`Self::max_speed`] must call
    /// [`Self::refresh_max_speed`] (lowering only loosens the A\*
    /// heuristic, which stays admissible).
    pub speeds: Vec<f32>,
    /// Cached `max(speeds)` (≥ 0.001) scaling the A\* heuristic.
    max_speed: f32,
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
        let slopes = slope_map(heights, heightmap_width, heightmap_height);
        Self::from_slopes(&slopes, width, height, max_slope, slope_mod)
    }

    fn new(width: u32, height: u32, speeds: Vec<f32>) -> Self {
        let mut map = Self {
            width,
            height,
            speeds,
            max_speed: 0.0,
        };
        map.refresh_max_speed();
        map
    }

    /// The fastest cell's speed (at least 0.001): the A\* heuristic's
    /// scale, cached so a search doesn't rescan the whole grid.
    pub fn max_speed(&self) -> f32 {
        self.max_speed
    }

    /// Recompute [`Self::max_speed`] after writing [`Self::speeds`]
    /// directly.
    pub fn refresh_max_speed(&mut self) {
        self.max_speed = self
            .speeds
            .iter()
            .copied()
            .fold(0.0f32, f32::max)
            .max(0.001);
    }

    /// Build a speed map from a precomputed slope field
    /// ([`slope_map`]), for the same `max_slope` / `slope_mod` encoding
    /// as [`Self::from_heightmap`]. The slope of a cell does not depend
    /// on the move class, so a game with several slope buckets computes
    /// the field once and thresholds it per bucket — cell for cell the
    /// same values `from_heightmap` produces.
    pub fn from_slopes(
        slopes: &[f32],
        width: u32,
        height: u32,
        max_slope: f32,
        slope_mod: f32,
    ) -> Self {
        assert_eq!(slopes.len(), (width * height) as usize);
        Self::new(
            width,
            height,
            slopes
                .iter()
                .map(|&slope| speed_from_slope(slope, max_slope, slope_mod))
                .collect(),
        )
    }

    /// Recompute the cells touching heightmap vertices `x0..=x1`,
    /// `z0..=z1` after the heights there changed (terrain edited at
    /// runtime, e.g. Hex Farm's towers rising and sinking), with the
    /// same `max_slope` / `slope_mod` the map was built with. Cheaper
    /// than a full [`Self::from_heightmap`] rebuild for local edits.
    #[allow(clippy::too_many_arguments)]
    pub fn update_region(
        &mut self,
        heights: &[f32],
        heightmap_width: u32,
        max_slope: f32,
        slope_mod: f32,
        x0: u32,
        z0: u32,
        x1: u32,
        z1: u32,
    ) {
        let hw = heightmap_width as usize;
        let hh = self.height as usize + 1;
        let old_max = self.max_speed;
        let mut lowered_max = false;
        // A vertex belongs to the up-to-four cells around it, and a
        // cell's slope is shared by its 2×2 half-resolution block.
        let (z_lo, z_hi) = (z0.saturating_sub(1) & !1, (z1 | 1).min(self.height - 1));
        let (x_lo, x_hi) = (x0.saturating_sub(1) & !1, (x1 | 1).min(self.width - 1));
        for z in z_lo..=z_hi {
            for x in x_lo..=x_hi {
                let cell = &mut self.speeds[(z * self.width + x) as usize];
                let speed = cell_speed(
                    heights, hw, hh, x as usize, z as usize, max_slope, slope_mod,
                );
                lowered_max |= *cell >= old_max && speed < old_max;
                self.max_speed = self.max_speed.max(speed);
                *cell = speed;
            }
        }
        // A cell at the maximum slowed down: the maximum only drops if
        // no other cell still reaches it.
        if lowered_max && self.max_speed == old_max && !self.speeds.iter().any(|&s| s >= old_max) {
            self.refresh_max_speed();
        }
    }

    /// Build a uniform speed map (all cells have the same speed).
    pub fn uniform(width: u32, height: u32, speed: f32) -> Self {
        Self::new(width, height, vec![speed; (width * height) as usize])
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

/// Slope of every cell of a `heightmap_width × heightmap_height` vertex
/// grid, row-major over the `(width-1) × (height-1)` cells, in Spring's
/// `1 - cos(angle)` encoding (see [`SpeedMap::from_heightmap`]). The
/// move-class-independent half of a speed map.
///
/// Spring's slope map is *half* resolution (`CReadMap::UpdateSlopemap`,
/// `Map/ReadMap.cpp:741`): one value per 2×2 block of squares, from the
/// eight face normals of the block's four squares — `avg` and `min` of
/// their `y`, mixed as `min + (avg − min) · (min / avg)` so one steep
/// triangle pulls the whole block most of the way toward blocked, and
/// `CMoveMath::GetPosSpeedMod` reads `slopeMap[(x >> 1) + (z >> 1) ·
/// hmapx]` for every square. The grid here stays per square; the four
/// squares of a block simply share its value.
pub fn slope_map(heights: &[f32], heightmap_width: u32, heightmap_height: u32) -> Vec<f32> {
    let width = (heightmap_width - 1) as usize;
    let height = (heightmap_height - 1) as usize;
    let hw = heightmap_width as usize;
    let hh = heightmap_height as usize;
    let mut slopes = vec![0.0; width * height];
    for bz in 0..height.div_ceil(2) {
        for bx in 0..width.div_ceil(2) {
            let slope = block_slope(heights, hw, hh, bx, bz);
            for z in (bz * 2)..(bz * 2 + 2).min(height) {
                for x in (bx * 2)..(bx * 2 + 2).min(width) {
                    slopes[z * width + x] = slope;
                }
            }
        }
    }
    slopes
}

/// Relative speed of heightmap cell `(x, z)` (see
/// [`SpeedMap::from_heightmap`]); `hh` is the vertex-grid height.
fn cell_speed(
    heights: &[f32],
    hw: usize,
    hh: usize,
    x: usize,
    z: usize,
    max_slope: f32,
    slope_mod: f32,
) -> f32 {
    speed_from_slope(
        block_slope(heights, hw, hh, x >> 1, z >> 1),
        max_slope,
        slope_mod,
    )
}

/// `1 - cos(angle)` slope of the half-resolution block `(bx, bz)` —
/// squares `2bx..2bx+1 × 2bz..2bz+1` (squares off the map are left
/// out, as the engine's clamped loops do).
fn block_slope(heights: &[f32], hw: usize, hh: usize, bx: usize, bz: usize) -> f32 {
    let (width, height) = (hw - 1, hh - 1);
    let mut sum = 0.0f32;
    let mut min = 1.0f32;
    let mut n = 0.0f32;
    for z in (bz * 2)..(bz * 2 + 2).min(height) {
        for x in (bx * 2)..(bx * 2 + 2).min(width) {
            let (tl, tr) = (heights[z * hw + x], heights[z * hw + x + 1]);
            let (bl, br) = (heights[(z + 1) * hw + x], heights[(z + 1) * hw + x + 1]);
            // `fnTL = normalize(-(hTR-hTL), 8, -(hBL-hTL))`,
            // `fnBR = normalize(hBL-hBR, 8, hTR-hBR)` (ReadMap.cpp:672).
            for ny in [
                face_normal_y(tr - tl, bl - tl),
                face_normal_y(bl - br, tr - br),
            ] {
                sum += ny;
                min = min.min(ny);
                n += 1.0;
            }
        }
    }
    if n == 0.0 {
        return 0.0;
    }
    let avg = sum / n;
    // `lerp = maxslope / avgslope; slope = mix(maxslope, avgslope, lerp)`.
    let lerp = min / avg;
    let mixed = min + (avg - min) * lerp;
    1.0 - mixed
}

/// `y` of the unit normal of a face with height differences `dx`, `dz`
/// across one `SQUARE_SIZE` step each: `8 / |(-dx, 8, -dz)|`.
fn face_normal_y(dx: f32, dz: f32) -> f32 {
    SQUARE_SIZE / (dx * dx + SQUARE_SIZE * SQUARE_SIZE + dz * dz).sqrt()
}

/// The move class's view of a cell slope: impassable past its cap, else
/// Spring's `1 / (1 + slope * slopeMod)` penalty.
fn speed_from_slope(slope: f32, max_slope: f32, slope_mod: f32) -> f32 {
    if slope > max_slope {
        0.0 // impassable
    } else {
        (1.0 / (1.0 + slope * slope_mod)).clamp(0.0, 1.0)
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

    /// Spring's half-resolution slope map: one steep square makes its
    /// whole 2×2 block steep (mixed toward the minimum), and the four
    /// squares of a block share one value.
    #[test]
    fn one_steep_square_rates_its_whole_block() {
        // 5×5 vertices → 4×4 squares → 2×2 blocks; raise one vertex so
        // only the squares touching it slope.
        let mut heights = vec![0.0; 25];
        heights[0] = 40.0; // vertex (0,0): square (0,0) alone is steep
        let slopes = slope_map(&heights, 5, 5);
        let s = |x: usize, z: usize| slopes[z * 4 + x];
        assert_eq!(s(0, 0), s(1, 0));
        assert_eq!(s(0, 0), s(0, 1));
        assert_eq!(s(0, 0), s(1, 1));
        assert_eq!(s(2, 2), 0.0);
        // Faces: TL of square (0,0) has dx = -40, dz = -40 → y = 8/√3264
        // ≈ 0.140; its BR face is flat (1.0); the other 6 are flat.
        let min = 8.0 / (40.0f32 * 40.0 * 2.0 + 64.0).sqrt();
        let avg = (min + 7.0) / 8.0;
        let expected = 1.0 - (min + (avg - min) * (min / avg));
        assert!(
            (s(0, 0) - expected).abs() < 1e-5,
            "{} vs {expected}",
            s(0, 0)
        );
        // Far steeper than the LIGHT cap: blocked.
        assert!(s(0, 0) > max_slope_from_degrees(36.0));
    }

    /// Thresholding a shared slope field per bucket is bit-identical
    /// to rebuilding each bucket from the heightmap.
    #[test]
    fn from_slopes_matches_from_heightmap() {
        let (w, h) = (17u32, 11u32);
        let heights: Vec<f32> = (0..w * h)
            .map(|i| ((i * 7919) % 97) as f32 * 1.7 - (i % 13) as f32 * 3.1)
            .collect();
        let slopes = slope_map(&heights, w, h);
        for degrees in [5.0, 20.0, 36.0, 45.0, 60.0] {
            let cap = max_slope_from_degrees(degrees);
            let mod_ = slope_mod_from_max_slope(cap);
            let full = SpeedMap::from_heightmap(&heights, w, h, cap, mod_);
            let thresholded = SpeedMap::from_slopes(&slopes, w - 1, h - 1, cap, mod_);
            assert_eq!(full.speeds, thresholded.speeds, "MaxSlope {degrees}");
        }
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

#[cfg(test)]
mod update_region_tests {
    use super::*;

    #[test]
    fn update_region_matches_full_rebuild() {
        let (w, h) = (9u32, 7u32);
        let mut heights: Vec<f32> = (0..w * h).map(|i| (i % 5) as f32).collect();
        let mut map = SpeedMap::from_heightmap(&heights, w, h, 0.4, 10.0);
        heights[(3 * w + 4) as usize] = 90.0;
        map.update_region(&heights, w, 0.4, 10.0, 4, 3, 4, 3);
        let full = SpeedMap::from_heightmap(&heights, w, h, 0.4, 10.0);
        assert_eq!(map.speeds, full.speeds);
    }
}
