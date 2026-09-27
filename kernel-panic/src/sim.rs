//! Spring's simulation units and the small heading helpers shared by
//! the movement, combat and animation ports.
//!
//! Upstream expresses almost every tunable in *sim frames* (30 per
//! second, `GAME_SPEED` in `rts/Sim/Misc/GlobalConstants.h`), heightmap
//! *squares* (`SQUARE_SIZE` = 8 elmos) and 16-bit *short angles*
//! (65536 per revolution, `SPRING_CIRCLE_DIVS`). Define those once here
//! instead of re-deriving `30.0` / `8.0` / `65536.0` at every call site.
//!
//! Headings follow Spring's `GetHeadingFromVector(dx, dz)` convention:
//! 0 faces +Z, +π/2 faces +X.

use std::f32::consts::{PI, TAU};

use bevy::math::{Vec2, Vec3};

/// Sim frames per second (`GAME_SPEED`). FBI/TDF speeds are elmos per
/// frame, accelerations elmos per frame², reload/burst/TTL values frames.
pub const GAME_SPEED: f32 = 30.0;

/// Seconds per sim frame (`INV_GAME_SPEED`).
pub const INV_GAME_SPEED: f32 = 1.0 / GAME_SPEED;

/// [`GAME_SPEED`] as the `Time<Fixed>` tick rate of the gameplay chain.
pub const SIMULATION_HZ: f64 = GAME_SPEED as f64;

/// `UNIT_SLOWUPDATE_RATE`: frames between a unit's `SlowUpdate`s.
pub const SLOW_UPDATE_RATE: u32 = 16;

/// The map's gravity per frame² (negative = down): Kernel Panic maps
/// all set `gravity=50` (`mapInfo->map.gravity = -50 / GAME_SPEED²`).
pub const MAP_GRAVITY_PER_FRAME2: f32 = -50.0 / (GAME_SPEED * GAME_SPEED);

/// Heightmap square edge in elmos (`SQUARE_SIZE`). Re-exported from the
/// pathfinder so the speed grid and the game can never disagree.
pub use spring_pathfinding::SQUARE_SIZE;

/// Radians per 16-bit heading unit (`TAANG2RAD`): Spring's short angles
/// (FBI `TurnRate`, weapon `turnrate`, spray angles) run 65536 per turn.
pub const SHORT_ANGLE_TO_RAD: f32 = TAU / 65536.0;

/// Duration in seconds of `frames` sim frames.
pub const fn frames_to_secs(frames: f32) -> f32 {
    frames / GAME_SPEED
}

/// Number of sim frames (fractional) in `secs` seconds.
pub const fn secs_to_frames(secs: f32) -> f32 {
    secs * GAME_SPEED
}

/// Unit facing (x, z) for a heading.
pub fn dir_of(heading: f32) -> Vec2 {
    Vec2::new(heading.sin(), heading.cos())
}

/// Level 3D facing (y = 0) for a heading — [`dir_of`] on the XZ plane.
pub fn dir3_of(heading: f32) -> Vec3 {
    let d = dir_of(heading);
    Vec3::new(d.x, 0.0, d.y)
}

/// Heading of an (x, z) direction (`GetHeadingFromVector`). For a 3D
/// vector pass `v.xz()`.
pub fn heading_of(v: Vec2) -> f32 {
    v.x.atan2(v.y)
}

/// Wrap an angle to `(-π, π]` (short-heading arithmetic wraps).
pub fn wrap_angle(a: f32) -> f32 {
    let w = (a + PI).rem_euclid(TAU) - PI;
    if w <= -PI { w + TAU } else { w }
}

/// Shortest absolute angular distance between two angles, in `[0, π]`.
pub fn angle_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_round_trips_through_dir_of() {
        for h in [-3.0f32, -1.2, 0.0, 0.7, 2.9] {
            assert!((heading_of(dir_of(h)) - h).abs() < 1e-5);
        }
        // 0 faces +Z, π/2 faces +X.
        assert!(dir3_of(0.0).abs_diff_eq(Vec3::Z, 1e-6));
        assert!(dir3_of(PI / 2.0).abs_diff_eq(Vec3::X, 1e-6));
    }

    #[test]
    fn wrap_and_delta_take_the_short_way() {
        assert!((wrap_angle(3.0 * PI / 2.0) + PI / 2.0).abs() < 1e-5);
        assert_eq!(wrap_angle(-PI), PI);
        assert!((angle_delta(0.1, TAU - 0.1) - 0.2).abs() < 1e-5);
        assert!((angle_delta(-PI, PI)).abs() < 1e-5);
    }
}
