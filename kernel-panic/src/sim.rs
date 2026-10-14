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
//!
//! Also the match's sim clock ([`SimTick`]) and its seeded random
//! streams ([`MatchSeed`], [`SimRng`]): everything the fixed tick may
//! read as time or chance, all match-scoped, so a match replays from
//! its setup alone (`docs/multiplayer-plan.md`, M0.2).
//!
//! **Iteration order.** A query visits its tables in the order the
//! *app* created them, which depends on everything the app ran before
//! (the attract demo, an earlier match) and on per-peer components
//! (selection, fog markers). A sim system whose result depends on the
//! order it visits units — dice drawn per unit, spawns (each draws the
//! next `NetId`), queued hits summed into health, path requests served
//! in turn, sequential pushes — iterates in unit-id order instead:
//! `query.iter().sort::<&NetId>()`, Spring's fixed unit-list order.
//! Systems that update each unit from its own state alone keep the
//! cheaper ECS order.

use bevy::math::ops;
use std::f32::consts::{PI, TAU};
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

use bevy::math::{Vec2, Vec3};
use bevy::prelude::{FromWorld, ResMut, Resource, World};

/// Float `det_max` / `det_min` / `det_clamp`: the sim's `max`, `min`
/// and `clamp`, with one answer for `max(+0, -0)` on every target
/// (`f32::max` gives x86_64's and aarch64's own; `clippy.toml` bans
/// it). Signed zeros order `-0 < +0`, a NaN operand is ignored.
pub use spring_detmath::DetFloat;

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

/// Gravity per frame² (negative = down) of the usual Kernel Panic map,
/// `gravity=50` (`mapInfo->map.gravity = -50 / GAME_SPEED²`). Not every
/// shipped map uses it (Speed Balls 40, Major Madness and pacman 100,
/// Memory Bank 120, Digital Divide the engine default 130): the loaded
/// map's value is the [`MapGravity`] resource.
pub const MAP_GRAVITY_PER_FRAME2: f32 = -50.0 / (GAME_SPEED * GAME_SPEED);

/// The loaded map's gravity per frame² (negative = down), from its map
/// info: `-max(0.001, gravity) / GAME_SPEED²` (`CMapInfo::ReadGlobal`).
/// `map_loading` replaces it at every map swap; before the first map
/// (and in tests) it is the usual [`MAP_GRAVITY_PER_FRAME2`].
#[derive(bevy::prelude::Resource, Clone, Copy, Debug, PartialEq)]
pub struct MapGravity(pub f32);

impl Default for MapGravity {
    fn default() -> Self {
        Self(MAP_GRAVITY_PER_FRAME2)
    }
}

impl MapGravity {
    /// From the map info's `gravity` (elmos/s², positive = down).
    pub fn from_map_info(gravity: f32) -> Self {
        Self(-gravity.det_max(0.001) / (GAME_SPEED * GAME_SPEED))
    }
}

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
                    Vec2::new(ops::sin(ang), ops::cos(ang))
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

    /// `GetHeadingFromFacing`: a build facing (`0` south … `3` west,
    /// `spring_map::datavent`) as the heading it fronts.
    pub fn from_facing(facing: u8) -> Self {
        Heading(match facing % 4 {
            0 => 0,
            1 => 16384,
            2 => i16::MIN,
            _ => -16384,
        })
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

/// `x * x`: the deterministic spelling of `x.powi(2)`, whose precision
/// std leaves unspecified (banned in `clippy.toml`). Bit-identical to
/// what `powi(2)` compiles to today.
#[inline]
pub const fn sq(x: f32) -> f32 {
    x * x
}

/// Unit facing (x, z) for a heading. `ops::sin`/`ops::cos` rather than
/// the std methods: the sim's trigonometry must round the same on every
/// target (`clippy.toml`).
pub fn dir_of(heading: f32) -> Vec2 {
    Vec2::new(ops::sin(heading), ops::cos(heading))
}

/// Level 3D facing (y = 0) for a heading — [`dir_of`] on the XZ plane.
pub fn dir3_of(heading: f32) -> Vec3 {
    let d = dir_of(heading);
    Vec3::new(d.x, 0.0, d.y)
}

/// Heading of an (x, z) direction (`GetHeadingFromVector`). For a 3D
/// vector pass `v.xz()`.
pub fn heading_of(v: Vec2) -> f32 {
    ops::atan2(v.x, v.y)
}

/// Shortest absolute angular distance between two angles, in `[0, π]`.
pub fn angle_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(TAU);
    d.det_min(TAU - d)
}

/// The match's sim clock: fixed ticks since the match began, Spring's
/// `gs->frameNum`. Match-scoped (0 at every match swap, so nothing that
/// ran before — the menu's attract demo, an earlier game, loading time —
/// shifts it) and advanced once at the head of every fixed tick
/// ([`advance_sim_tick`]), so the match's first tick is frame 1. Sim
/// code reads time from here, never from `Time<Fixed>::elapsed`, which
/// counts from app start.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SimTick(pub u64);

impl SimTick {
    /// The sim frame number (`gs->frameNum`).
    pub fn frame(self) -> u64 {
        self.0
    }

    /// Game seconds since the match began (`frameNum / GAME_SPEED`),
    /// for the timers upstream keeps in seconds (KPAI's).
    pub fn secs(self) -> f32 {
        (self.0 as f64 / SIMULATION_HZ) as f32
    }
}

/// `FixedFirst`: one more sim frame. Runs on every fixed tick, frozen
/// gameplay (a decided match) included, as the fixed clock it replaces
/// did.
pub fn advance_sim_tick(mut tick: ResMut<SimTick>) {
    tick.0 += 1;
}

/// The running match's seed ([`GameSetup::seed`](crate::game_setup::GameSetup::seed)),
/// set by `begin_match` before the match-scoped resources are
/// re-initialised, so their `FromWorld` (the AI's dice, every
/// [`SimRng`]) seeds from it.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MatchSeed(pub u64);

impl MatchSeed {
    /// A non-zero xorshift32 state for one random stream, decorrelated
    /// from the match's other streams by `salt`.
    pub fn stream(self, salt: u64) -> u32 {
        (crate::rng::splitmix64(self.0 ^ salt) as u32).max(1)
    }
}

/// Names one of the sim's random streams ([`SimRng`]).
pub trait RngStream: Send + Sync + 'static {
    /// Mixed into the match seed, so every stream rolls differently.
    const SALT: u64;
}

/// One of the sim's random streams: an xorshift32 state seeded from the
/// [`MatchSeed`] and the stream's salt, owned by one system (each its
/// own stream, so systems that run unordered never share a state).
/// Match-scoped (`init_match_resource`): every match restarts every
/// stream from its seed, whatever ran before. Derefs to the `u32` state
/// the [`crate::rng`] helpers take.
#[derive(Resource)]
pub struct SimRng<S: RngStream> {
    state: u32,
    _stream: PhantomData<S>,
}

impl<S: RngStream> FromWorld for SimRng<S> {
    fn from_world(world: &mut World) -> Self {
        let seed = world
            .get_resource::<MatchSeed>()
            .copied()
            .unwrap_or_default();
        Self {
            state: seed.stream(S::SALT),
            _stream: PhantomData,
        }
    }
}

impl<S: RngStream> Deref for SimRng<S> {
    type Target = u32;
    fn deref(&self) -> &u32 {
        &self.state
    }
}

impl<S: RngStream> DerefMut for SimRng<S> {
    fn deref_mut(&mut self) -> &mut u32 {
        &mut self.state
    }
}

/// Tests that run sim systems on a bare `World`: the match-level sim
/// state a `begin_match` would leave, and a tick that advances it.
#[cfg(test)]
pub(crate) mod test_support {
    use std::time::Duration;

    use bevy::prelude::{Time, World};

    use super::{MatchSeed, SimRng, SimTick};
    use crate::interaction::ground_move::ImpulseRng;
    use crate::units::combat::{BurstRng, GroundFireRng, TargetingRng};
    use crate::units::lifecycle::production::FactoryExitRng;

    /// The sim clock at frame 0, a zero match seed and every random
    /// stream the gameplay systems draw from.
    pub fn init(world: &mut World) {
        world.init_resource::<MatchSeed>();
        world.init_resource::<SimTick>();
        world.init_resource::<SimRng<TargetingRng>>();
        world.init_resource::<SimRng<GroundFireRng>>();
        world.init_resource::<SimRng<BurstRng>>();
        world.init_resource::<SimRng<FactoryExitRng>>();
        world.init_resource::<SimRng<ImpulseRng>>();
        crate::units::weapon_fx::init_test_rngs(world);
    }

    /// [`init`] in an `App` builder chain.
    pub trait InitSim {
        fn init_sim(&mut self) -> &mut Self;
    }

    impl InitSim for bevy::prelude::App {
        fn init_sim(&mut self) -> &mut Self {
            init(self.world_mut());
            self
        }
    }

    /// One 30 Hz tick: `Time` (the systems' `delta`) and the frame
    /// number (`SimTick`), as `FixedFirst` would advance them.
    pub fn next_frame(world: &mut World) {
        world
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f64(1.0 / 30.0));
        world.resource_mut::<SimTick>().0 += 1;
    }
}

/// Debug builds: assert at the start of every sim tick that the CPU's
/// floating-point mode is still the IEEE default the whole deterministic
/// math plan assumes: round-to-nearest-even, denormals neither flushed
/// to zero on output (FTZ) nor read as zero on input (DAZ). Rust never
/// changes these, but a driver, audio library or injected overlay can on
/// the thread it runs on, and a desync from that is invisible
/// otherwise. Upstream checks the same at the top of every
/// `CGame::SimFrame` (`rts/Game/Game.cpp:1684` →
/// `good_fpu_control_registers`, `rts/System/Sync/FPUCheck.cpp`, whose
/// accepted MXCSR values all have FZ, DAZ and rounding bits clear).
///
/// Checks only the thread running `FixedFirst`; one register read, so
/// it costs nothing measurable. Release builds and wasm32 (which has no
/// float modes at all) get an empty system.
pub fn assert_float_env() {
    #[cfg(all(debug_assertions, target_arch = "x86_64"))]
    {
        let mut mxcsr: u32 = 0;
        // SAFETY: `stmxcsr` only stores the 32-bit MXCSR register into
        // the pointed-to local; SSE is baseline on x86_64.
        unsafe {
            std::arch::asm!(
                "stmxcsr [{}]",
                in(reg) &mut mxcsr,
                options(nostack, preserves_flags)
            );
        }
        // Bit 15 FTZ, bits 13-14 rounding control (00 = nearest), bit 6 DAZ.
        const FTZ: u32 = 1 << 15;
        const ROUNDING: u32 = 0b11 << 13;
        const DAZ: u32 = 1 << 6;
        debug_assert_eq!(
            mxcsr & (FTZ | ROUNDING | DAZ),
            0,
            "MXCSR {mxcsr:#06x}: FTZ/DAZ or a non-nearest rounding mode is set; \
             the sim would desync from other clients"
        );
    }
    #[cfg(all(debug_assertions, target_arch = "aarch64"))]
    {
        let fpcr: u64;
        // SAFETY: `mrs` only copies FPCR into a register.
        unsafe {
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
        // Bit 24 FZ, bits 22-23 RMode (00 = nearest), bit 19 FZ16.
        const FZ: u64 = 1 << 24;
        const RMODE: u64 = 0b11 << 22;
        const FZ16: u64 = 1 << 19;
        debug_assert_eq!(
            fpcr & (FZ | RMODE | FZ16),
            0,
            "FPCR {fpcr:#x}: flush-to-zero or a non-nearest rounding mode is set; \
             the sim would desync from other clients"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build facing fronts the quarter heading `facing()` reads back.
    #[test]
    fn facing_round_trips_through_heading() {
        for facing in 0..4u8 {
            let heading = Heading::from_facing(facing);
            assert_eq!(heading.facing(), facing as u32, "facing {facing}");
            let v = heading.to_vector();
            let (fx, fz) = spring_map::datavent::facing_vector(facing);
            assert!(
                (v.x - fx).abs() < 1e-3 && (v.y - fz).abs() < 1e-3,
                "{facing}: {v:?}"
            );
        }
    }

    /// Guards the build configuration lockstep depends on: `ops` and
    /// glam really route through the pure-Rust `libm` (bevy's `libm`
    /// feature), and glam really is scalar (`scalar-math`). Dropping
    /// either feature makes this fail on glibc, where the platform
    /// `powf`/`atan2`/`sinf` differ from libm on 1-16 % of inputs.
    #[test]
    fn deterministic_math_is_configured() {
        use bevy::math::{Quat, Vec4};
        // scalar-math gives Vec4 and Quat native (4-byte) alignment
        // instead of the 16 bytes of the SSE2/NEON/simd128 versions.
        assert_eq!(std::mem::align_of::<Vec4>(), 4, "glam scalar-math is off");
        assert_eq!(std::mem::align_of::<Quat>(), 4, "glam scalar-math is off");
        let mut s = 1u32;
        for _ in 0..20_000 {
            let a = crate::rng::next_signed(&mut s) * 12.0;
            let b = crate::rng::next_signed(&mut s) * 2000.0;
            let p = crate::rng::next_f32(&mut s);
            assert_eq!(ops::sin(a).to_bits(), libm::sinf(a).to_bits());
            assert_eq!(ops::atan2(a, b).to_bits(), libm::atan2f(a, b).to_bits());
            assert_eq!(
                ops::powf(p, b.abs() * 0.03).to_bits(),
                libm::powf(p, b.abs() * 0.03).to_bits()
            );
            // glam's own trigonometry (`libm` feature): from_rotation_y
            // is (0, sin(a/2), 0, cos(a/2)).
            let q = Quat::from_rotation_y(a);
            let (sin, cos) = libm::sincosf(a * 0.5);
            assert_eq!(
                (q.y.to_bits(), q.w.to_bits()),
                (sin.to_bits(), cos.to_bits())
            );
        }
    }

    #[test]
    fn map_gravity_from_map_info() {
        assert_eq!(MapGravity::from_map_info(50.0), MapGravity::default());
        let g = MapGravity::from_map_info(120.0).0 * GAME_SPEED * GAME_SPEED;
        assert!((g + 120.0).abs() < 1e-3);
        // The engine clamps to 0.001: never zero or upward.
        assert!(MapGravity::from_map_info(-5.0).0 < 0.0);
    }

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
            let h = Heading::from_vector(ops::sin(r), ops::cos(r));
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
