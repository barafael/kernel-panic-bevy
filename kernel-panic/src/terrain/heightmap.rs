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

use spring_map::map_types::{ParsedMap, SQUARE_SIZE};

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
}

impl Heightmap {
    pub fn from_parsed(parsed: &ParsedMap) -> Self {
        Self {
            heights: parsed.heights.clone(),
            width: parsed.header.heightmap_width(),
            height: parsed.header.heightmap_height(),
            square_size: SQUARE_SIZE as f32,
        }
    }

    /// Bilinearly sample the terrain Y at world position `(x, z)`. Out-of-bounds
    /// queries clamp to the nearest edge cell.
    pub fn sample(&self, x: f32, z: f32) -> f32 {
        let gx = (x / self.square_size).clamp(0.0, (self.width - 1) as f32);
        let gz = (z / self.square_size).clamp(0.0, (self.height - 1) as f32);

        let x0 = gx.floor() as usize;
        let z0 = gz.floor() as usize;
        let x1 = (x0 + 1).min(self.width - 1);
        let z1 = (z0 + 1).min(self.height - 1);

        let fx = gx - x0 as f32;
        let fz = gz - z0 as f32;

        let h00 = self.heights[z0 * self.width + x0];
        let h10 = self.heights[z0 * self.width + x1];
        let h01 = self.heights[z1 * self.width + x0];
        let h11 = self.heights[z1 * self.width + x1];

        let top = h00 + (h10 - h00) * fx;
        let bot = h01 + (h11 - h01) * fx;
        top + (bot - top) * fz
    }

    /// World-space position at `(x, z)` with Y snapped to the terrain.
    pub fn place(&self, x: f32, z: f32) -> Vec3 {
        Vec3::new(x, self.sample(x, z), z)
    }

    /// Total world-space extent of the map in elmos (width × depth).
    /// Useful for synthesising spawn positions when the map's
    /// `start_positions` doesn't supply enough.
    pub fn world_size(&self) -> (f32, f32) {
        (
            self.width as f32 * self.square_size,
            self.height as f32 * self.square_size,
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
        let mut x = center.x - half_x;
        while x <= center.x + half_x {
            let mut z = center.z - half_z;
            while z <= center.z + half_z {
                let n = self.normal(x, z);
                let slope = (1.0 - n.y.max(0.0)).max(0.0);
                if slope > max_slope {
                    max_slope = slope;
                }
                z += step;
            }
            x += step;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(h: f32, w: usize, d: usize) -> Heightmap {
        Heightmap {
            heights: vec![h; w * d],
            width: w,
            height: d,
            square_size: 8.0,
        }
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
        let hm = Heightmap {
            heights: vec![0.0, 10.0, 0.0, 10.0],
            width: 2,
            height: 2,
            square_size: 8.0,
        };
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
        let hm = Heightmap {
            heights,
            width: 5,
            height: 5,
            square_size: 8.0,
        };
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
        let hm = Heightmap {
            heights,
            width: 5,
            height: 5,
            square_size: 8.0,
        };
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
        let hm = Heightmap {
            heights: vec![0.0, 0.0, 100.0, 0.0, 0.0],
            width: 5,
            height: 1,
            square_size: 8.0,
        };
        // Shoot low-to-low across the ridge: beam Y stays at 5, ridge at 100.
        assert!(!hm.has_line_of_sight(Vec3::new(0.0, 5.0, 0.0), Vec3::new(32.0, 5.0, 0.0), 0.5));
        // Shoot high-to-high well above the ridge: passes.
        assert!(hm.has_line_of_sight(Vec3::new(0.0, 200.0, 0.0), Vec3::new(32.0, 200.0, 0.0), 0.5));
    }
}
