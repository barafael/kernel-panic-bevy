//! Optional per-map directional speed bias that "swirls" all ground
//! movement around a fixed center, favoring one rotational direction.
//! Built for the Circular Buffer map (see `map_ideas.md`): walking with
//! the current is fast, walking against it crawls.
//!
//! Activation is gated on the [`CircularFlow`] resource being present.
//! [`crate::interaction::movement::movement_system`] consults it via
//! [`CircularFlow::step_multiplier`] every frame. Absent → no cost.

use bevy::prelude::*;

#[derive(Resource, Clone, Debug)]
pub struct CircularFlow {
    /// X-Z center of the swirl.
    pub center_xz: Vec2,
    /// Bias strength in `[0, 1)`. Step length is multiplied by
    /// `1 + alignment * strength`, then floored at `0.1×` so opposing
    /// movement crawls but doesn't completely stall. With `strength=0.6`,
    /// fully aligned travel runs at `1.6×` speed, fully opposed at `0.4×`,
    /// a 4× ratio between with-flow and against-flow.
    pub strength: f32,
    /// `true` to favor clockwise travel when looking down `+Y`,
    /// `false` for counter-clockwise.
    pub clockwise: bool,
}

impl CircularFlow {
    /// Multiplier applied to the unit's per-frame translation step. Returns
    /// `1.0` near the swirl center (where the tangent direction is
    /// undefined) so close-in spawns aren't twisted by a meaningless bias.
    pub fn step_multiplier(&self, pos: Vec3, forward: Vec3) -> f32 {
        let r = Vec2::new(pos.x - self.center_xz.x, pos.z - self.center_xz.y);
        if r.length_squared() < 1.0 {
            return 1.0;
        }
        let dir = Vec2::new(forward.x, forward.z);
        if dir.length_squared() < 1e-6 {
            return 1.0;
        }
        let r_n = r / r.length();
        // (+X right, +Z forward) — a unit on the +X axis travelling
        // clockwise must move toward -Z, so CW tangent is (r.y, -r.x).
        let tangent = if self.clockwise {
            Vec2::new(r_n.y, -r_n.x)
        } else {
            Vec2::new(-r_n.y, r_n.x)
        };
        let alignment = tangent.dot(dir / dir.length());
        (1.0 + alignment * self.strength).max(0.1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> CircularFlow {
        CircularFlow {
            center_xz: Vec2::ZERO,
            strength: 0.6,
            clockwise: true,
        }
    }

    #[test]
    fn aligned_clockwise_is_boosted() {
        // Unit on +X axis moving toward -Z is travelling clockwise.
        let m = flow().step_multiplier(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
        assert!((m - 1.6).abs() < 1e-4, "got {m}");
    }

    #[test]
    fn opposed_clockwise_is_slowed() {
        // Unit on +X axis moving toward +Z is travelling counter-clockwise.
        let m = flow().step_multiplier(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        assert!((m - 0.4).abs() < 1e-4, "got {m}");
    }

    #[test]
    fn radial_motion_is_unaffected() {
        // Outbound along +X is perpendicular to the tangent → multiplier 1.
        let m = flow().step_multiplier(Vec3::new(100.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
        assert!((m - 1.0).abs() < 1e-4, "got {m}");
    }

    #[test]
    fn near_center_is_unaffected() {
        let m = flow().step_multiplier(Vec3::new(0.5, 0.0, 0.5), Vec3::new(0.0, 0.0, -1.0));
        assert!((m - 1.0).abs() < 1e-4, "got {m}");
    }

    #[test]
    fn counter_clockwise_flips_the_bias() {
        let mut f = flow();
        f.clockwise = false;
        let m = f.step_multiplier(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0));
        assert!((m - 0.4).abs() < 1e-4, "got {m}");
    }

    #[test]
    fn floors_at_one_tenth() {
        let mut f = flow();
        f.strength = 0.99;
        // Worst-case opposed: alignment = -1, raw = 0.01, floored to 0.1.
        let m = f.step_multiplier(Vec3::new(100.0, 0.0, 0.0), Vec3::new(0.0, 0.0, 1.0));
        assert!((m - 0.1).abs() < 1e-4, "got {m}");
    }
}
