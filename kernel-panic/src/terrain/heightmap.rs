//! Runtime heightmap sampler.
//!
//! The map's heightmap is consumed by the map loader to build terrain meshes
//! and the pathfinding nav grid, then dropped. Everything that happens during
//! play — movement snapping units to the ground, weapons checking line of
//! sight over a ridge — needs to query heights at arbitrary world positions,
//! long after `ParsedMap` is gone.
//!
//! [`Heightmap`] owns the row-major height grid and the world→grid scale,
//! and is inserted as a Bevy resource during map load. Sampling is bilinear
//! so a unit crossing a slope sees a smooth Y instead of stair-stepping
//! between heightmap cells.

use bevy::prelude::*;

use spring_map::map_types::SQUARE_SIZE;

/// Floor so short shots still sample meaningfully; cap so pathologically
/// long shots don't burn cycles on samples finer than the terrain resolution.
const MIN_LOS_SAMPLES: usize = 4;
const MAX_LOS_SAMPLES: usize = 64;

/// World-space heightmap, queried by movement and combat for terrain Y and
/// line-of-sight checks. Lifetime matches the loaded map: inserted on load,
/// replaced on map cycle.
#[derive(Resource)]
pub struct Heightmap {
    heights: Vec<f32>,
    width: usize,
    height: usize,
    square_size: f32,
    /// Per-square centre normals, `(width-1) × (height-1)` row-major —
    /// the port of `CReadMap`'s `centerNormalsSynced`. The engine
    /// precomputes this table precisely so `GetSmoothNormal` is four
    /// table reads; recomputing per call was ~8 cross products +
    /// normalizations per tilted unit per sim tick. Refreshed in place
    /// by [`Heightmap::refresh_center_normals`] when Hex Farm rewrites
    /// heights.
    center_normals: Vec<Vec3>,
}

impl Heightmap {
    /// A heightmap from raw row-major vertex heights (8-elmo squares).
    /// Takes the map's height grid over — the loader hands it off once
    /// nothing else needs the `ParsedMap` copy.
    pub fn from_raw(heights: Vec<f32>, width: usize, height: usize) -> Self {
        assert_eq!(heights.len(), width * height);
        let mut hm = Self {
            heights,
            width,
            height,
            square_size: SQUARE_SIZE as f32,
            center_normals: Vec::new(),
        };
        hm.refresh_center_normals(0, 0, width.saturating_sub(1), height.saturating_sub(1));
        hm
    }

    /// Raw row-major heights (`width × height` vertices).
    pub fn heights(&self) -> &[f32] {
        &self.heights
    }

    /// Mutable heights, for runtime terrain edits (Hex Farm). Callers
    /// re-sync the nav grids / terrain mesh for what they touch.
    pub fn heights_mut(&mut self) -> &mut [f32] {
        &mut self.heights
    }

    /// Heightmap vertex columns / rows.
    pub fn grid_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Terrain Y at world position `(x, z)`, interpolated on the two
    /// triangles of the heightmap square exactly as the terrain mesh is
    /// triangulated (diagonal top-right → bottom-left) — Spring's
    /// `CGround::GetHeightReal` / `InterpolateCornerHeight`
    /// (`Map/Ground.cpp:18`). A bilinear blend differs from the drawn
    /// surface by up to a quarter of the diagonal's height step, so
    /// units floated or sank on ridges. Out-of-bounds queries clamp to
    /// the nearest edge.
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 0.0;
        }
        let gx = (x / self.square_size).clamp(0.0, (self.width - 1) as f32);
        let gz = (z / self.square_size).clamp(0.0, (self.height - 1) as f32);

        let x0 = (gx.floor() as usize).min(self.width.saturating_sub(2));
        let z0 = (gz.floor() as usize).min(self.height.saturating_sub(2));
        let x1 = (x0 + 1).min(self.width - 1);
        let z1 = (z0 + 1).min(self.height - 1);

        let dx = gx - x0 as f32;
        let dz = gz - z0 as f32;

        let h00 = self.heights[z0 * self.width + x0];
        let h10 = self.heights[z0 * self.width + x1];
        let h01 = self.heights[z1 * self.width + x0];
        let h11 = self.heights[z1 * self.width + x1];

        if dx + dz < 1.0 {
            // Top-left triangle.
            h00 + (h10 - h00) * dx + (h01 - h00) * dz
        } else {
            // Bottom-right triangle.
            h11 + (h01 - h11) * (1.0 - dx) + (h10 - h11) * (1.0 - dz)
        }
    }

    /// `CReadMap::centerNormals2D` of square `(sx, sz)`: the horizontal
    /// part of its centre normal, normalized — pointing downhill, or
    /// zero on a level square. `None` off the map.
    pub fn center_normal_2d(&self, sx: i32, sz: i32) -> Option<Vec2> {
        if sx < 0 || sz < 0 || sx as usize + 1 >= self.width || sz as usize + 1 >= self.height {
            return None;
        }
        let n = self.center_normal(sx as usize, sz as usize);
        Some(Vec2::new(n.x, n.z).normalize_or_zero())
    }

    /// `CReadMap::centerNormals` of square `(sx, sz)`: the normalized
    /// sum of its two triangles' face normals. Table lookup — the table
    /// is (re)built by [`Self::refresh_center_normals`].
    fn center_normal(&self, sx: usize, sz: usize) -> Vec3 {
        let sw = self.width - 1;
        self.center_normals[sz * sw + sx]
    }

    /// Recompute every square centre normal whose corners touch the
    /// vertex box `[x0..=x1] × [z0..=z1]` — the analogue of
    /// `CReadMap`'s `TerrainChange` recalc, dilated one square so
    /// neighbours sharing a changed corner come along. Call after
    /// writing heights through [`Self::heights_mut`].
    pub fn refresh_center_normals(&mut self, x0: usize, z0: usize, x1: usize, z1: usize) {
        if self.width < 2 || self.height < 2 {
            return;
        }
        let sw = self.width - 1;
        let sh = self.height - 1;
        if self.center_normals.len() != sw * sh {
            self.center_normals.resize(sw * sh, Vec3::Y);
        }
        let sx0 = x0.saturating_sub(1).min(sw - 1);
        let sx1 = x1.min(sw - 1);
        let sz0 = z0.saturating_sub(1).min(sh - 1);
        let sz1 = z1.min(sh - 1);
        for sz in sz0..=sz1 {
            for sx in sx0..=sx1 {
                self.center_normals[sz * sw + sx] =
                    compute_center_normal(&self.heights, self.width, self.square_size, sx, sz);
            }
        }
    }

    /// `CGround::GetSmoothNormal` (`Map/Ground.cpp:511`): the square
    /// centre normals around `(x, z)` blended bilinearly — the up vector
    /// ground units tilt to.
    pub fn smooth_normal(&self, x: f32, z: f32) -> Vec3 {
        if self.width < 4 || self.height < 4 {
            return Vec3::Y;
        }
        let (mx, mz) = (self.width - 1, self.height - 1);
        let fx = x / self.square_size;
        let fz = z / self.square_size;
        let sx = (fx.floor() as isize).clamp(1, mx as isize - 2) as usize;
        let sz = (fz.floor() as isize).clamp(1, mz as isize - 2) as usize;
        let dx = fx - sx as f32;
        let dz = fz - sz as f32;
        let (sx2, wx) = if dx > 0.5 {
            (sx + 1, dx - 0.5)
        } else {
            (sx - 1, 0.5 - dx)
        };
        let (sz2, wz) = if dz > 0.5 {
            (sz + 1, dz - 0.5)
        } else {
            (sz - 1, 0.5 - dz)
        };
        let (wx, wz) = (wx.clamp(0.0, 1.0), wz.clamp(0.0, 1.0));
        let n = self.center_normal(sx, sz) * (1.0 - wx) * (1.0 - wz)
            + self.center_normal(sx2, sz) * wx * (1.0 - wz)
            + self.center_normal(sx, sz2) * (1.0 - wx) * wz
            + self.center_normal(sx2, sz2) * wx * wz;
        n.normalize_or(Vec3::Y)
    }

    /// World-space position at `(x, z)` with Y snapped to the terrain.
    pub fn place(&self, x: f32, z: f32) -> Vec3 {
        Vec3::new(x, self.sample(x, z), z)
    }

    /// World-space extent of the map in elmos (`mapx * SQUARE_SIZE`,
    /// matching [`spring_map::map_types::SmfHeader::world_width`]).
    /// The height grid stores *vertex* columns/rows, so the extent is
    /// one square smaller than `grid_size() * square_size` — the extra
    /// vertex column has zero width.
    pub fn world_size(&self) -> (f32, f32) {
        (
            (self.width.saturating_sub(1)) as f32 * self.square_size,
            (self.height.saturating_sub(1)) as f32 * self.square_size,
        )
    }

    /// Upward-pointing terrain normal at `(x, z)`, derived from the heightmap
    /// gradient via central differences. Used by movement to tilt units into
    /// the slope they're traversing.
    pub fn normal(&self, x: f32, z: f32) -> Vec3 {
        // Step one heightmap cell in each direction. Central differences
        // give a smoother gradient than forward/backward differences — no
        // bias at the edges of a slope.
        let step = self.square_size;
        let dy_dx = (self.sample(x + step, z) - self.sample(x - step, z)) / (2.0 * step);
        let dy_dz = (self.sample(x, z + step) - self.sample(x, z - step)) / (2.0 * step);
        // Surface tangent vectors are (1, dy/dx, 0) and (0, dy/dz, 1); their
        // cross product is (-dy/dx, 1, -dy/dz), which is the upward normal.
        Vec3::new(-dy_dx, 1.0, -dy_dz).normalize()
    }

    /// Steepest slope sampled across an axis-aligned footprint centred on
    /// `(center.x, center.z)`, returned in Spring's `1 - cos(angle)`
    /// encoding so callers can compare directly against
    /// [`spring_pathfinding::max_slope_from_degrees`] caps. Footprint is in
    /// elmos; sampling stride is one heightmap square (8 elmos).
    pub fn max_slope_in_footprint(&self, center: Vec3, footprint: Vec2) -> f32 {
        let half_x = footprint.x * 0.5;
        let half_z = footprint.y * 0.5;
        let step = self.square_size;
        let mut max_slope = 0.0_f32;
        // Integer sample counts: a float `while x <= end; x += step`
        // never ends for a non-finite or huge `x` (the add is a no-op).
        if !(center.is_finite() && footprint.is_finite()) {
            return max_slope;
        }
        let nx = (footprint.x / step).clamp(0.0, 512.0) as i32;
        let nz = (footprint.y / step).clamp(0.0, 512.0) as i32;
        for ix in 0..=nx {
            let x = center.x - half_x + ix as f32 * step;
            for iz in 0..=nz {
                let z = center.z - half_z + iz as f32 * step;
                let n = self.normal(x, z);
                let slope = (1.0 - n.y.max(0.0)).max(0.0);
                if slope > max_slope {
                    max_slope = slope;
                }
            }
        }
        max_slope
    }

    /// Does a straight line from `from` to `to` clear the terrain?
    ///
    /// `margin` is added to each sampled terrain height — callers pass a
    /// small positive value to tolerate the shooter standing on a crest
    /// without self-blocking. Ballistic arcs (non-zero trajectory height)
    /// should skip this check entirely.
    pub fn has_line_of_sight(&self, from: Vec3, to: Vec3, margin: f32) -> bool {
        let dx = to.x - from.x;
        let dz = to.z - from.z;
        let horizontal = (dx * dx + dz * dz).sqrt();
        let step_count = ((horizontal / self.square_size).ceil() as usize)
            .clamp(MIN_LOS_SAMPLES, MAX_LOS_SAMPLES);

        // Skip endpoints — the shooter and target are by definition at (or
        // above) the terrain Y at their own positions, so sampling there
        // just invites flaky self-blocking from the margin.
        for i in 1..step_count {
            let t = i as f32 / step_count as f32;
            let x = from.x + dx * t;
            let z = from.z + dz * t;
            let beam_y = from.y + (to.y - from.y) * t;
            let terrain_y = self.sample(x, z);
            if beam_y < terrain_y + margin {
                return false;
            }
        }
        true
    }
}

/// `CReadMap::centerNormals` of square `(sx, sz)`, from raw heights:
/// the normalized sum of the square's two triangles' face normals.
fn compute_center_normal(heights: &[f32], w: usize, s: f32, sx: usize, sz: usize) -> Vec3 {
    let p = |x: usize, z: usize| Vec3::new(x as f32 * s, heights[z * w + x], z as f32 * s);
    let (tl, tr) = (p(sx, sz), p(sx + 1, sz));
    let (bl, br) = (p(sx, sz + 1), p(sx + 1, sz + 1));
    let up = |n: Vec3| if n.y < 0.0 { -n } else { n };
    let n1 = up((bl - tl).cross(tr - tl)).normalize_or_zero();
    let n2 = up((bl - tr).cross(br - tr)).normalize_or_zero();
    (n1 + n2).normalize_or(Vec3::Y)
}

#[cfg(test)]
mod triangle_tests {
    use super::*;

    /// Heights follow the mesh's two triangles (diagonal TR→BL), not a
    /// bilinear blend: with only the bottom-right corner raised, the
    /// square's centre lies on the diagonal at height 0 (bilinear: 2).
    #[test]
    fn sample_follows_the_mesh_triangles() {
        let hm = Heightmap::from_raw(vec![0.0, 0.0, 0.0, 8.0], 2, 2);
        assert!(hm.sample(4.0, 4.0).abs() < 1e-5);
        assert!((hm.sample(6.0, 6.0) - 4.0).abs() < 1e-5);
        assert!(hm.sample(2.0, 2.0).abs() < 1e-5);
    }

    /// The smooth normal is up on flat ground and tilts downhill.
    #[test]
    fn smooth_normal_tilts_downhill() {
        let w = 8;
        let flat = Heightmap::from_raw(vec![0.0; w * w], w, w);
        assert!((flat.smooth_normal(20.0, 20.0) - Vec3::Y).length() < 1e-5);
        let ramp: Vec<f32> = (0..w * w).map(|i| (i % w) as f32 * 8.0).collect();
        let n = Heightmap::from_raw(ramp, w, w).smooth_normal(20.0, 20.0);
        assert!(n.x < -0.5 && n.y > 0.5, "45° ramp rising along +X: {n}");
    }

    /// After `heights_mut` + `refresh_center_normals`, `smooth_normal`
    /// reflects the edit exactly as a freshly built heightmap would.
    #[test]
    fn refresh_center_normals_picks_up_edits() {
        let w = 8;
        let mut hm = Heightmap::from_raw(vec![0.0; w * w], w, w);
        // Raise one vertex column into a wall along +X.
        for z in 0..w {
            hm.heights_mut()[z * w + 5] = 40.0;
        }
        // Without the refresh the smoothed normal at x≈36 (square 4,
        // west face of the wall) is still flat.
        assert!((hm.smooth_normal(36.0, 24.0) - Vec3::Y).length() < 0.2);
        hm.refresh_center_normals(5, 0, 5, w - 1);
        let n = hm.smooth_normal(36.0, 24.0);
        let fresh = Heightmap::from_raw(hm.heights().to_vec(), w, w);
        assert!(
            (n - fresh.smooth_normal(36.0, 24.0)).length() < 1e-5,
            "refreshed normals match a rebuilt table: {n}"
        );
        assert!(n.x < -0.3, "wall tilts the normal toward -X: {n}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(h: f32, w: usize, d: usize) -> Heightmap {
        Heightmap::from_raw(vec![h; w * d], w, d)
    }

    #[test]
    fn sample_flat_map_returns_constant_height() {
        let hm = flat(42.0, 4, 4);
        assert_eq!(hm.sample(0.0, 0.0), 42.0);
        assert_eq!(hm.sample(12.5, 7.0), 42.0);
        assert_eq!(hm.sample(1000.0, 1000.0), 42.0); // clamped
    }

    #[test]
    fn sample_interpolates_between_corners() {
        // 2x2 grid with a ramp along +X: 0, 10 / 0, 10.
        let hm = Heightmap::from_raw(vec![0.0, 10.0, 0.0, 10.0], 2, 2);
        assert!((hm.sample(0.0, 0.0) - 0.0).abs() < 1e-4);
        assert!((hm.sample(8.0, 0.0) - 10.0).abs() < 1e-4);
        assert!((hm.sample(4.0, 0.0) - 5.0).abs() < 1e-4);
        assert!((hm.sample(4.0, 4.0) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn place_snaps_y_to_terrain() {
        let hm = flat(17.0, 4, 4);
        assert_eq!(hm.place(10.0, 20.0), Vec3::new(10.0, 17.0, 20.0));
    }

    /// The world extent is one square smaller than the vertex grid:
    /// 5 vertex columns span 4 squares × 8 elmos = 32, matching
    /// `SmfHeader::world_width` (`mapx * SQUARE_SIZE`).
    #[test]
    fn world_size_is_squares_times_square_size() {
        let hm = flat(0.0, 5, 5);
        assert_eq!(hm.world_size(), (32.0, 32.0));
        // A 1×1 grid has zero extent, not one square.
        assert_eq!(flat(0.0, 1, 1).world_size(), (0.0, 0.0));
    }

    #[test]
    fn normal_is_up_on_flat_terrain() {
        let hm = flat(42.0, 8, 8);
        let n = hm.normal(16.0, 16.0);
        assert!((n - Vec3::Y).length() < 1e-4);
    }

    #[test]
    fn normal_tilts_toward_downhill_on_ramp() {
        // 5x5 ramp along +X: height increases by 8 per column, square_size=8,
        // so slope is 1. Normal at the centre should tilt toward -X.
        let mut heights = Vec::with_capacity(25);
        for _ in 0..5 {
            for x in 0..5 {
                heights.push(x as f32 * 8.0);
            }
        }
        let hm = Heightmap::from_raw(heights, 5, 5);
        let n = hm.normal(16.0, 16.0);
        assert!(n.x < 0.0, "normal should lean away from rising terrain");
        assert!((n.z).abs() < 1e-4, "no Z gradient on an X-only ramp");
        assert!(n.y > 0.0, "normal still points up-ish");
        assert!((n.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn max_slope_in_footprint_zero_on_flat_terrain() {
        let hm = flat(42.0, 8, 8);
        let s = hm.max_slope_in_footprint(Vec3::new(32.0, 0.0, 32.0), Vec2::splat(32.0));
        assert!(s.abs() < 1e-4);
    }

    #[test]
    fn max_slope_in_footprint_picks_up_a_ramp() {
        // 5×5 ramp along +X: slope = 1 elmo rise per elmo of run (45°).
        let mut heights = Vec::with_capacity(25);
        for _ in 0..5 {
            for x in 0..5 {
                heights.push(x as f32 * 8.0);
            }
        }
        let hm = Heightmap::from_raw(heights, 5, 5);
        // 1 - cos(45°) ≈ 0.293.
        let s = hm.max_slope_in_footprint(Vec3::new(16.0, 0.0, 16.0), Vec2::splat(16.0));
        assert!((s - 0.293).abs() < 0.01, "expected ~0.293, got {s}");
    }

    #[test]
    fn line_of_sight_clear_over_flat_terrain() {
        let hm = flat(0.0, 8, 8);
        assert!(hm.has_line_of_sight(Vec3::new(0.0, 5.0, 0.0), Vec3::new(50.0, 5.0, 0.0), 0.5));
    }

    #[test]
    fn line_of_sight_blocked_by_ridge() {
        // 5x1 strip: low, low, tall ridge, low, low.
        let hm = Heightmap::from_raw(vec![0.0, 0.0, 100.0, 0.0, 0.0], 5, 1);
        // Shoot low-to-low across the ridge: beam Y stays at 5, ridge at 100.
        assert!(!hm.has_line_of_sight(Vec3::new(0.0, 5.0, 0.0), Vec3::new(32.0, 5.0, 0.0), 0.5));
        // Shoot high-to-high well above the ridge: passes.
        assert!(hm.has_line_of_sight(Vec3::new(0.0, 200.0, 0.0), Vec3::new(32.0, 200.0, 0.0), 0.5));
    }
}
