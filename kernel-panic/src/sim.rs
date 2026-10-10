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

/// `UNIT_SLOWUPDATE_RATE` (GlobalConstants.h:60): frames between a
/// unit's `SlowUpdate`s.
pub const SLOW_UPDATE_RATE: u32 = 15;

/// The map's gravity per frame² (negative = down): Kernel Panic maps
/// all set `gravity=50` (`mapInfo->map.gravity = -50 / GAME_SPEED²`).
pub const MAP_GRAVITY_PER_FRAME2: f32 = -50.0 / (GAME_SPEED * GAME_SPEED);

/// Heightmap square edge in elmos (`SQUARE_SIZE`). Re-exported from the
/// pathfinder so the speed grid and the game can never disagree.
pub use spring_pathfinding::SQUARE_SIZE;

/// Radians per 16-bit heading unit (`TAANG2RAD`): Spring's short angles
/// (FBI `TurnRate`, weapon `turnrate`, spray angles) run 65536 per turn.
pub const SHORT_ANGLE_TO_RAD: f32 = TAU / 65536.0;

/// `SPRING_CIRCLE_DIVS` / `SPRING_MAX_HEADING`: a full turn / half a turn
/// in 16-bit heading units.
pub const SPRING_CIRCLE_DIVS: i32 = 65536;
pub const SPRING_MAX_HEADING: i32 = 32768;

/// `NUM_HEADINGS`: entries of the engine's heading → vector table.
const NUM_HEADINGS: i32 = 4096;

/// A 16-bit Spring heading (`short heading`): 65536 units per turn,
/// 0 faces +Z, 16384 faces +X, wrapping arithmetic. The ground mover
/// turns in whole units per frame exactly like `CGroundMoveType`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Heading(pub i16);

impl Heading {
    /// `GetHeadingFromVector(dx, dz)` (System/SpringMath.inl:38-82):
    /// the engine's atan2 *approximation*, truncated to an int and
    /// wrapped the way the engine does it.
    pub fn from_vector(dx: f32, dz: f32) -> Self {
        use std::f32::consts::FRAC_PI_2;
        let sign = |b: bool| if b { 1.0 } else { -1.0 };
        let h = if dz != 0.0 {
            // Keep `dz` away from 0 so `d` stays finite.
            let sz = dz * 2.0 - 1.0;
            let d = dx / (dz + 0.000001 * sz);
            let dd = d * d;
            let mut h = if d.abs() > 1.0 {
                1.0f32.copysign(d) * FRAC_PI_2 - d / (dd + 0.28)
            } else {
                d / (1.0 + 0.28 * dd)
            };
            // Add ±π when `dz < 0` (the sign follows `dx`).
            if dz < 0.0 {
                h += PI * sign(dx > 0.0);
            }
            h
        } else {
            FRAC_PI_2 * sign(dx > 0.0)
        };
        let mut ih = (h * (SPRING_MAX_HEADING as f32 / PI)) as i32;
        // Due north would wrap from -32768 to 0 (due south) below.
        ih += (ih == -SPRING_MAX_HEADING) as i32;
        ih %= SPRING_MAX_HEADING;
        Self(ih as i16)
    }

    #[cfg_attr(
        target_arch = "wasm32",
        allow(dead_code, reason = "read by the native-only bot")
    )]
    pub fn to_radians(self) -> f32 {
        self.0 as f32 * SHORT_ANGLE_TO_RAD
    }

    /// `GetVectorFromHeading` (SpringMath.inl:109): a 4096-entry
    /// `(sin, cos)` table indexed by `heading / 16 + 2048`, so facings
    /// are quantised to 1/4096 of a turn.
    pub fn to_vector(self) -> Vec2 {
        use std::sync::LazyLock;
        static TABLE: LazyLock<Vec<Vec2>> = LazyLock::new(|| {
            (0..NUM_HEADINGS)
                .map(|a| {
                    let ang = (a - NUM_HEADINGS / 2) as f32 * TAU / NUM_HEADINGS as f32;
                    Vec2::new(ang.sin(), ang.cos())
                })
                .collect()
        });
        let div = (SPRING_MAX_HEADING / NUM_HEADINGS) * 2;
        TABLE[(self.0 as i32 / div + NUM_HEADINGS / 2) as usize]
    }

    /// [`Self::to_vector`] on the XZ plane.
    pub fn to_vector3(self) -> Vec3 {
        let v = self.to_vector();
        Vec3::new(v.x, 0.0, v.y)
    }

    /// `short(self - other)`: the signed shortest turn, in units.
    pub fn wrapping_sub(self, other: Heading) -> i16 {
        self.0.wrapping_sub(other.0)
    }

    pub fn wrapping_add(self, delta: i16) -> Heading {
        Heading(self.0.wrapping_add(delta))
    }

    /// `GetFacingFromHeading`: 0 south (+Z), 1 east (+X), 2 north,
    /// 3 west — quadrants centred on the axes.
    pub fn facing(self) -> u32 {
        match self.0 {
            -8192..=8191 => 0,
            8192..=24575 => 1,
            -24576..=-8193 => 3,
            _ => 2,
        }
    }
}

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

    /// `GetHeadingFromVector` is an approximation: it is exact on the
    /// axes and within about 0.3° elsewhere, and wraps like the engine.
    #[test]
    fn heading_from_vector_matches_the_engine() {
        assert_eq!(Heading::from_vector(0.0, 1.0).0, 0);
        assert_eq!(Heading::from_vector(1.0, 0.0).0, 16384);
        assert_eq!(Heading::from_vector(-1.0, 0.0).0, -16384);
        // Due north truncates to -32768 → kept as -32767 after the fix-up.
        assert_eq!(Heading::from_vector(0.0, -1.0).0, -32767);
        for deg in (-179..180).step_by(7) {
            let r = (deg as f32).to_radians();
            let h = Heading::from_vector(r.sin(), r.cos());
            let back = h.to_radians().to_degrees();
            let err = ((back - deg as f32 + 540.0) % 360.0 - 180.0).abs();
            assert!(err < 0.35, "{deg}° → {} ({back}°)", h.0);
        }
        // The table vector of the heading round-trips to the axis.
        assert!((Heading(16384).to_vector() - Vec2::X).length() < 1e-6);
        assert_eq!(Heading(0).facing(), 0);
        assert_eq!(Heading(16384).facing(), 1);
        assert_eq!(Heading(-16384).facing(), 3);
        assert_eq!(Heading(i16::MIN).facing(), 2);
        // Boundaries as `GetFacingFromHeading` draws them.
        assert_eq!(Heading(-8192).facing(), 0);
        assert_eq!(Heading(8192).facing(), 1);
        assert_eq!(Heading(-24576).facing(), 3);
        assert_eq!(Heading(24576).facing(), 2);
    }

    #[test]
    fn wrap_and_delta_take_the_short_way() {
        assert!((angle_delta(0.1, TAU - 0.1) - 0.2).abs() < 1e-5);
        assert!((angle_delta(-PI, PI)).abs() < 1e-5);
    }
}
