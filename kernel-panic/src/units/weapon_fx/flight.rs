//! Guided-projectile flight, transcribed frame-for-frame from the Recoil
//! engine so missiles fly the authored paths:
//!
//! - [`MissileFlight`] — `CMissileProjectile::Update`
//!   (`Sim/Projectiles/WeaponProjectiles/MissileProjectile.cpp:136-243`),
//!   launched by `CMissileLauncher::FireImpl` (`MissileLauncher.cpp:38-69`).
//!   Pointer's Geometric, NX Flag.
//! - [`StarburstFlight`] — `CStarburstProjectile::Update` /
//!   `UpdateTrajectory` (`StarburstProjectile.cpp:136-257`), launched by
//!   `CStarburstLauncher::FireImpl` (`StarburstLauncher.cpp:31-50`).
//!   Flow's FlowMissile.
//! - [`CannonFlight`] — `CExplosiveProjectile` (`CProjectile::Update`),
//!   launched by `CCannon::FireImpl` (`Cannon.cpp`). Exploit's BugCannon,
//!   the Obelisk's Infection shell. Aimed with the same ballistic solve
//!   ([`crate::units::combat::aim::cannon_wanted_dir`]).
//!
//! Everything runs in Spring's native units: one [`step`] per 30 Hz sim
//! frame, speeds in elmos/frame, accelerations in elmos/frame², turn
//! rates as the engine's `weaponDef->turnrate` (TDF `turnrate` × 2π/65536
//! / 30 — a per-frame *vector* gain, not an angle per second).
//!
//! [`step`]: MissileFlight::step

use bevy::math::ops;
use bevy::prelude::*;

use crate::sim::{GAME_SPEED, SHORT_ANGLE_TO_RAD};
use crate::units::combat::aim::{cannon_launch_dir, weapon_gravity};

/// `MAX_PROJECTILE_RANGE` — `distanceToTravel` for `fixedLauncher` /
/// timed starbursts, which never run out of range.
const MAX_PROJECTILE_RANGE: f32 = 1e20;

/// TDF `turnrate` (COB angle units per second) → engine
/// `weaponDef->turnrate` (`scaleValue(TAANG2RAD * INV_GAME_SPEED)`,
/// `WeaponDef.cpp:124`).
pub fn engine_turn_rate(tdf_turnrate: f32) -> f32 {
    tdf_turnrate * SHORT_ANGLE_TO_RAD / GAME_SPEED
}

/// Where the target is this frame, for `tracks=1` weapons
/// (`UpdateTargeting`: the target's aim position and its velocity).
#[derive(Clone, Copy, Debug, Default)]
pub struct TargetSample {
    pub pos: Vec3,
    /// Elmos per frame.
    pub vel: Vec3,
}

/// `CMissileProjectile` state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MissileFlight {
    pub dir: Vec3,
    /// `speed.w`, elmos/frame.
    pub speed: f32,
    pub max_speed: f32,
    pub acceleration: f32,
    /// Engine `weaponDef->turnrate`.
    pub turn_rate: f32,
    pub ttl: i32,
    pub tracks: bool,
    pub target_pos: Vec3,
    pub extra_height: f32,
    pub extra_height_decay: f32,
    pub extra_height_time: i32,
    pub gravity: f32,
}

/// Launch parameters shared by both guided launchers.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    pub weapon: &'a spring_tdf::WeaponDef,
    pub muzzle_pos: Vec3,
    /// World-space emit direction of the `QueryWeapon` piece.
    pub muzzle_dir: Vec3,
    pub target_pos: Vec3,
    /// The map's gravity, elmos/frame² (negative): what a missile falls
    /// under once out of fuel unless the TDF sets `myGravity`.
    pub map_gravity: f32,
}

impl MissileFlight {
    /// `CMissileLauncher::FireImpl` + the `CMissileProjectile` ctor.
    /// Returns the flight and the launch position.
    pub fn launch(l: Launch) -> (Self, Vec3) {
        let w = l.weapon;
        let max_speed = (w.weapon_velocity / GAME_SPEED).max(0.01);
        let start_speed = (w.start_velocity / GAME_SPEED).max(0.01);
        let to_target = l.target_pos - l.muzzle_pos;
        let target_dist = to_target.length();
        let mut dir = to_target.normalize_or(l.muzzle_dir);
        if w.fixed_launcher {
            dir = l.muzzle_dir;
        } else if w.trajectory_height > 0.0 {
            dir = (dir + Vec3::Y * w.trajectory_height).normalize_or(Vec3::Y);
        }
        // `ttl = ceil(max(targetDist, range) / projectileSpeed + 25·burnblow)`
        // (burnblow is not used by any KP missile).
        let ttl = (target_dist.max(w.range) / max_speed).ceil() as i32;
        let (extra_height, extra_height_decay, extra_height_time) = if w.trajectory_height > 0.0 {
            let eh = target_dist * w.trajectory_height;
            let time = (target_dist.max(max_speed) / max_speed) as i32;
            let time = time.max(1);
            (eh, eh / time as f32, time)
        } else {
            (0.0, 0.0, 0)
        };
        let gravity = weapon_gravity(w, l.map_gravity);
        (
            Self {
                dir,
                speed: start_speed,
                max_speed,
                acceleration: w.weapon_acceleration / (GAME_SPEED * GAME_SPEED),
                turn_rate: engine_turn_rate(w.turn_rate),
                ttl,
                tracks: w.tracks,
                target_pos: l.target_pos,
                extra_height,
                extra_height_decay,
                extra_height_time,
                gravity,
            },
            l.muzzle_pos,
        )
    }

    /// One sim frame of `CMissileProjectile::Update` (wobble / dance are
    /// not ported — no Kernel Panic missile authors them). Returns the
    /// frame's velocity (elmos/frame); the caller moves by it.
    pub fn step(&mut self, pos: Vec3, target: Option<TargetSample>) -> Vec3 {
        self.ttl -= 1;
        if self.ttl > 0 {
            if self.speed < self.max_speed {
                self.speed += self.acceleration;
            }
            // UpdateTargeting
            let mut target_vel = Vec3::ZERO;
            if self.tracks
                && let Some(t) = target
            {
                self.target_pos = t.pos;
                target_vel = t.vel;
            }
            let org_target_pos = self.target_pos;
            // `normalize_or_zero` is Spring's `SafeNormalize` (zero stays zero).
            let target_dir = (self.target_pos - pos).normalize_or_zero();
            let target_dist = pos.distance(self.target_pos) + 0.1;

            if self.extra_height_time > 0 {
                self.extra_height -= self.extra_height_decay;
                self.extra_height_time -= 1;
                self.target_pos.y += self.extra_height;

                let climbing_to_target = (target_dir.y - self.dir.y) > 0.0
                    && (self.target_pos.y - self.extra_height - pos.y) > 0.0;
                if self.dir.y <= 0.0 {
                    // Reached the apex: blend toward the target dir.
                    let hor_diff = (self.target_pos - pos).xz().length() + 0.01;
                    let ver_diff = (self.target_pos.y - pos.y) + 0.01;
                    let dir_diff = (target_dir.y - self.dir.y).abs();
                    let ratio = (ver_diff / hor_diff).abs();
                    if climbing_to_target {
                        self.dir.y += dir_diff * ratio;
                    } else {
                        self.dir.y -= dir_diff * ratio;
                    }
                } else if climbing_to_target {
                    self.dir.y += self.extra_height_decay / target_dist;
                } else {
                    self.dir.y -= self.extra_height_decay / target_dist;
                }
            }

            let target_lead = target_vel * (target_dist / self.max_speed) * 0.7;
            let lead_dir = (self.target_pos + target_lead - pos).normalize_or_zero();
            let mut dif = lead_dir - self.dir;
            if dif.length_squared() < self.turn_rate * self.turn_rate {
                self.dir = lead_dir;
            } else {
                dif = (dif - self.dir * dif.dot(self.dir)).normalize_or_zero();
                self.dir = (self.dir + dif * self.turn_rate).normalize_or_zero();
            }
            self.target_pos = org_target_pos;
            self.dir * self.speed
        } else {
            // Out of fuel: `speed = speed·0.98 + up·mygravity`.
            let v = self.dir * self.speed * 0.98 + Vec3::Y * self.gravity;
            self.speed = v.length();
            self.dir = v.normalize_or_zero();
            v
        }
    }
}

/// `CStarburstProjectile` state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StarburstFlight {
    pub dir: Vec3,
    /// `speed.w`, elmos/frame.
    pub speed: f32,
    pub max_speed: f32,
    pub acceleration: f32,
    /// Engine `weaponDef->turnrate` — the stage-2 turn gain.
    pub turn_rate: f32,
    /// `turnrate · tracks` — the stage-3 homing gain.
    pub tracking: f32,
    /// `cos(tracking · 0.6)` — snap-to-target threshold in stage 3.
    pub max_good_dif: f32,
    /// Frames of straight ascent left (`weapontimer · 30`).
    pub uptime: i32,
    pub ttl: i32,
    pub turn_to_target: bool,
    pub distance_to_travel: f32,
    pub tracks: bool,
    pub target_pos: Vec3,
    pub gravity: f32,
}

impl StarburstFlight {
    /// `CStarburstLauncher::FireImpl` + the `CStarburstProjectile` ctor.
    /// Launches along the muzzle piece's emit direction when
    /// `fixedLauncher` (else straight up) from `muzzle + (0, 2, 0)`.
    pub fn launch(l: Launch) -> (Self, Vec3) {
        let w = l.weapon;
        let max_speed = (w.weapon_velocity / GAME_SPEED).max(0.01);
        let dir = if w.fixed_launcher {
            l.muzzle_dir.normalize_or(Vec3::Y)
        } else {
            Vec3::Y
        };
        let uptime = (w.weapon_timer * GAME_SPEED) as i32;
        let turn_rate = engine_turn_rate(w.turn_rate);
        let tracking = if w.tracks { turn_rate } else { 0.0 };
        let flight_time = (w.flight_time * GAME_SPEED) as i32;
        let ttl = if flight_time > 0 {
            flight_time
        } else {
            // `min(3000, uptime + myrange / maxSpeed + 100)`
            (uptime as f32 + w.range / max_speed + 100.0).min(3000.0) as i32
        };
        let distance_to_travel = if flight_time > 0 || w.fixed_launcher {
            MAX_PROJECTILE_RANGE
        } else {
            w.range
        };
        let gravity = weapon_gravity(w, l.map_gravity);
        (
            Self {
                dir,
                speed: (w.start_velocity / GAME_SPEED).max(0.01),
                max_speed,
                acceleration: w.weapon_acceleration / (GAME_SPEED * GAME_SPEED),
                turn_rate,
                tracking,
                max_good_dif: ops::cos(tracking * 0.6),
                uptime,
                ttl,
                turn_to_target: true,
                distance_to_travel,
                tracks: w.tracks,
                target_pos: l.target_pos,
                gravity,
            },
            l.muzzle_pos + Vec3::Y * 2.0,
        )
    }

    /// One sim frame of `CStarburstProjectile::Update` →
    /// `UpdateTargeting` + `UpdateTrajectory`. Returns the frame's
    /// velocity (elmos/frame).
    pub fn step(&mut self, pos: Vec3, target: Option<TargetSample>) -> Vec3 {
        self.ttl -= 1;
        self.uptime -= 1;
        if self.tracks
            && let Some(t) = target
        {
            self.target_pos = t.pos;
        }

        if self.uptime > 0 {
            // Stage 1: straight out along the launch dir, accelerating.
            self.speed = (self.speed + self.acceleration).min(self.max_speed);
            return self.dir * self.speed;
        }

        let alive = self.ttl > 0 && self.distance_to_travel > 0.0;
        if self.turn_to_target && alive {
            // Stage 2: swing onto the target at `turnrate` — no
            // acceleration while turning.
            let target_err = (self.target_pos - pos).normalize_or_zero();
            if target_err.dot(self.dir) > 0.99 {
                self.dir = target_err;
                self.turn_to_target = false;
            } else {
                let mut e = target_err - self.dir;
                e = (e - self.dir * e.dot(self.dir)).normalize_or_zero();
                let gain = if self.turn_rate != 0.0 {
                    self.turn_rate
                } else {
                    0.06
                };
                self.dir = (self.dir + e * gain).normalize_or(self.dir);
            }
            let v = self.dir * self.speed;
            if self.distance_to_travel != MAX_PROJECTILE_RANGE {
                self.distance_to_travel -= v.xz().length();
            }
            return v;
        }

        if alive {
            // Stage 3: home (snap inside `maxGoodDif`) and accelerate.
            let target_err = (self.target_pos - pos).normalize_or_zero();
            if target_err.dot(self.dir) > self.max_good_dif {
                self.dir = target_err;
            } else {
                let mut e = target_err - self.dir;
                e = (e - self.dir * e.dot(self.dir)).normalize_or_zero();
                self.dir = (self.dir + e * self.tracking).normalize_or_zero();
            }
            self.speed = (self.speed + self.acceleration).min(self.max_speed);
            let v = self.dir * self.speed;
            if self.distance_to_travel != MAX_PROJECTILE_RANGE {
                self.distance_to_travel -= v.xz().length();
            }
            return v;
        }

        // Out of fuel: tip toward gravity.
        self.dir = (self.dir + Vec3::Y * self.gravity).normalize_or(self.dir);
        self.speed -= self.gravity;
        self.dir * self.speed
    }
}

/// `CExplosiveProjectile` state: a ballistic shell.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CannonFlight {
    /// `speed`, elmos/frame.
    pub velocity: Vec3,
    /// `mygravity`, elmos/frame² (negative is down).
    pub gravity: f32,
    /// Frames left before the shell bursts in the air.
    pub ttl: i32,
}

impl CannonFlight {
    /// `CCannon::FireImpl`: the shell leaves the muzzle at the weapon's
    /// `projectileSpeed` (`weaponvelocity / 30` — `startvelocity` is a
    /// missile tag) along `launch_dir`, the fire side's (sprayed)
    /// ballistic solution, or — when the shot comes without one — the
    /// same solve for `target_pos` here. It lives twice the frames it
    /// needs to cover the horizontal distance (`ttl = predict · 2`).
    pub fn launch(
        weapon: &spring_tdf::WeaponDef,
        muzzle_pos: Vec3,
        target_pos: Vec3,
        launch_dir: Option<Vec3>,
        map_gravity: f32,
    ) -> Self {
        let speed = weapon.projectile_speed();
        let gravity = weapon_gravity(weapon, map_gravity);
        let target_vec = target_pos - muzzle_pos;
        let dir = launch_dir
            .unwrap_or_else(|| cannon_launch_dir(target_vec, speed, gravity))
            .normalize_or(Vec3::Y);
        let sq_speed_2d = dir.xz().length_squared() * speed * speed;
        let predict = if sq_speed_2d == 0.0 {
            -2.0 * speed * dir.y / gravity
        } else {
            (target_vec.xz().length_squared() / sq_speed_2d).sqrt()
        };
        Self {
            velocity: dir * speed,
            gravity,
            ttl: predict.ceil() as i32 * 2,
        }
    }

    /// One sim frame of `CProjectile::Update`: gravity first
    /// (`speed += up · mygravity`), then the caller moves by the returned
    /// velocity (elmos/frame) — the discrete step `CCannon::
    /// HaveFreeLineOfFire`'s `gravity · 0.5` term accounts for.
    pub fn step(&mut self) -> Vec3 {
        self.ttl -= 1;
        self.velocity.y += self.gravity;
        self.velocity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: f32 = crate::sim::MAP_GRAVITY_PER_FRAME2;

    /// Fly a shell frame by frame until it comes down through `ground_y`
    /// (or bursts); returns the landing point and the apex height.
    fn fly_shell(mut f: CannonFlight, from: Vec3, ground_y: f32) -> (Vec3, f32) {
        let mut pos = from;
        let mut apex = pos.y;
        while f.ttl > 0 {
            let next = pos + f.step();
            apex = apex.max(next.y);
            if next.y <= ground_y {
                let t = (pos.y - ground_y) / (pos.y - next.y);
                return (pos.lerp(next, t), apex);
            }
            pos = next;
        }
        (pos, apex)
    }

    /// The BugCannon's shell, launched by the shared solve at a target
    /// 1000 elmos out on level ground: a real lob (≈ 24° launch, ~113
    /// elmos apex — the old model flew it under 1° almost flat) that
    /// comes down on the target, short by about one frame of travel as
    /// in the engine (the solve is continuous, the flight discrete).
    #[test]
    fn bug_cannon_shell_lobs_onto_its_target() {
        let w = crate::units::combat::aim::tests::bug_cannon();
        let target = Vec3::new(1000.0, 0.0, 0.0);
        let f = CannonFlight::launch(&w, Vec3::ZERO, target, None, G);
        assert!((f.velocity.length() - 20.0).abs() < 1e-4);
        let pitch = ops::atan2(f.velocity.y, f.velocity.x).to_degrees();
        assert!((pitch - 24.3).abs() < 0.2, "launch pitch {pitch}°");
        // predict = ceil(1000 / (20·cos θ)) = 55 → ttl 110.
        assert_eq!(f.ttl, 110);
        let (landed, apex) = fly_shell(f, Vec3::ZERO, 0.0);
        assert!(apex > 100.0 && apex < 125.0, "apex {apex}");
        let short = target.x - landed.x;
        assert!(
            (0.0..=20.0).contains(&short),
            "landed at {landed}, {short} elmos short"
        );
        assert!(landed.z.abs() < 1e-3);
    }

    /// Aim and flight are one model: the pitch the script is asked for
    /// (`AimLaunch`) is exactly the shell's launch pitch.
    #[test]
    fn cannon_aim_pitch_matches_the_launch() {
        use crate::units::combat::aim::{AimLaunch, local_aim_angles};
        let w = crate::units::combat::aim::tests::bug_cannon();
        for target in [
            Vec3::new(300.0, 40.0, -200.0),
            Vec3::new(-900.0, -150.0, 100.0),
            Vec3::new(50.0, 0.0, 1100.0),
        ] {
            let f = CannonFlight::launch(&w, Vec3::ZERO, target, None, G);
            let dir = f.velocity.normalize();
            let (_, pitch) = local_aim_angles(Quat::IDENTITY, target, AimLaunch::of(Some(&w), G));
            assert!(
                (ops::asin(dir.y) - pitch).abs() < 1e-5,
                "{target}: launch {} vs aim {pitch}",
                ops::asin(dir.y)
            );
        }
    }

    /// A fire-side launch direction (the sprayed solve) wins over the
    /// shell's own solve, and an unreachable target bursts in the air
    /// after its ttl rather than flying forever.
    #[test]
    fn cannon_launch_dir_and_ttl() {
        let w = crate::units::combat::aim::tests::bug_cannon();
        let dir = Vec3::new(1.0, 1.0, 0.0).normalize();
        let f = CannonFlight::launch(&w, Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0), Some(dir), G);
        assert!((f.velocity.normalize() - dir).length() < 1e-6);
        let mut far = CannonFlight::launch(&w, Vec3::ZERO, Vec3::new(5000.0, 0.0, 0.0), None, G);
        assert_eq!(far.velocity.y, 0.0, "out of reach: level launch");
        let ttl = far.ttl;
        for _ in 0..ttl {
            far.step();
        }
        assert_eq!(far.ttl, 0);
    }

    fn flow_missile() -> spring_tdf::WeaponDef {
        // networkweapons.tdf [FlowMissile].
        spring_tdf::WeaponDef {
            weapon_type: "StarburstLauncher".into(),
            range: 350.0,
            weapon_velocity: 2000.0,
            start_velocity: 400.0,
            weapon_acceleration: 600.0,
            fixed_launcher: true,
            tracks: true,
            weapon_timer: 0.1,
            turn_rate: 60000.0,
            flight_time: 5.0,
            ..Default::default()
        }
    }

    #[test]
    fn engine_turn_rate_is_a_per_frame_gain() {
        // 60000 COB units/s → 60000·2π/65536/30 ≈ 0.1917 per frame.
        assert!((engine_turn_rate(60000.0) - 0.19175).abs() < 1e-4);
    }

    /// FlowMissile launches along the muzzle's emit direction (not
    /// straight up), 2 elmos above the muzzle, at 400/30 elmos/frame.
    #[test]
    fn starburst_launches_along_the_fixed_muzzle_dir() {
        let w = flow_missile();
        let dir = Vec3::new(0.5, -0.2, 0.8).normalize();
        let (f, pos) = StarburstFlight::launch(Launch {
            map_gravity: G,
            weapon: &w,
            muzzle_pos: Vec3::new(10.0, 150.0, 5.0),
            muzzle_dir: dir,
            target_pos: Vec3::new(300.0, 0.0, 0.0),
        });
        assert_eq!(pos, Vec3::new(10.0, 152.0, 5.0));
        assert!((f.dir - dir).length() < 1e-6);
        assert!((f.speed - 400.0 / 30.0).abs() < 1e-4);
        assert_eq!(f.uptime, 3);
        assert_eq!(f.ttl, 150);
    }

    /// Stage timing and turn gain, frame by frame: uptime 3 → two frames
    /// of straight accelerating flight, then the turn stage rotates the
    /// dir by exactly `atan(turnrate)` per frame (≈10.85°) at constant
    /// speed until within `acos(0.99)` ≈ 8.1°, where it snaps. (The old
    /// port converted the gain to 5.75 rad/s and snapped whenever the
    /// error was under 5.75 rad — i.e. always.)
    #[test]
    fn starburst_stages_and_turn_rate_per_frame() {
        let w = flow_missile();
        let (mut f, mut pos) = StarburstFlight::launch(Launch {
            map_gravity: G,
            weapon: &w,
            muzzle_pos: Vec3::ZERO,
            muzzle_dir: Vec3::Y,
            target_pos: Vec3::new(10_000.0, 0.0, 0.0),
        });
        let accel = 600.0 / 900.0;
        let v0 = 400.0 / 30.0;
        // Frames 1-2: straight up, accelerating.
        for k in 1..=2 {
            let v = f.step(pos, None);
            pos += v;
            assert!((v.normalize() - Vec3::Y).length() < 1e-6);
            assert!((f.speed - (v0 + accel * k as f32)).abs() < 1e-4);
        }
        // Turn stage: constant speed, ~10.85° per frame toward +X.
        let turn = ops::atan(engine_turn_rate(60000.0));
        let speed = f.speed;
        let mut prev = f.dir;
        let mut frames = 0;
        while f.turn_to_target {
            let v = f.step(pos, None);
            pos += v;
            frames += 1;
            assert!(
                (f.speed - speed).abs() < 1e-5,
                "no acceleration while turning"
            );
            if f.turn_to_target {
                let step = prev.angle_between(f.dir);
                assert!((step - turn).abs() < 1e-3, "turned {step} rad");
            }
            prev = f.dir;
            assert!(frames < 20);
        }
        // ~90° at 10.85°/frame: 8 turning frames, then the snap frame.
        assert_eq!(frames, 9);
        // Stage 3 accelerates again.
        f.step(pos, None);
        assert!(f.speed > speed);
    }

    /// `tracks=1`: stage 3 follows the target's *current* position.
    #[test]
    fn starburst_tracks_a_moving_target() {
        let w = flow_missile();
        let (mut f, mut pos) = StarburstFlight::launch(Launch {
            map_gravity: G,
            weapon: &w,
            muzzle_pos: Vec3::ZERO,
            muzzle_dir: Vec3::X,
            target_pos: Vec3::new(300.0, 0.0, 0.0),
        });
        let moved = Vec3::new(300.0, 0.0, 200.0);
        for _ in 0..12 {
            pos += f.step(
                pos,
                Some(TargetSample {
                    pos: moved,
                    vel: Vec3::ZERO,
                }),
            );
        }
        assert_eq!(f.target_pos, moved);
        assert!(
            f.dir.z > 0.3,
            "missile turned toward the moved target: {}",
            f.dir
        );
    }

    /// Geometric (MissileLauncher, trajectoryheight=1, turnrate 20000):
    /// the turn per frame is the engine gain, never a snap across a
    /// large angle — and the `extraHeight` arc lifts the missile well
    /// above the direct line before it comes down on the target.
    #[test]
    fn missile_arcs_and_turns_by_the_engine_gain() {
        let w = spring_tdf::WeaponDef {
            weapon_type: "MissileLauncher".into(),
            range: 1400.0,
            weapon_velocity: 400.0,
            start_velocity: 400.0,
            trajectory_height: 1.0,
            tracks: true,
            turn_rate: 20000.0,
            ..Default::default()
        };
        let target = Vec3::new(600.0, 0.0, 0.0);
        let (mut f, mut pos) = MissileFlight::launch(Launch {
            map_gravity: G,
            weapon: &w,
            muzzle_pos: Vec3::ZERO,
            muzzle_dir: Vec3::X,
            target_pos: target,
        });
        let gain = engine_turn_rate(20000.0);
        let mut max_y = 0.0f32;
        let mut prev = f.dir;
        let mut hit = false;
        for _ in 0..200 {
            let v = f.step(pos, None);
            pos += v;
            // Heading change per frame is bounded by atan(gain) plus the
            // extraHeight pitch bias.
            assert!(prev.angle_between(f.dir) < ops::atan(gain) + 0.2);
            prev = f.dir;
            max_y = max_y.max(pos.y);
            if pos.y <= 0.0 {
                hit = true;
                break;
            }
        }
        assert!(hit, "missile must come down");
        assert!(max_y > 100.0, "trajectoryheight arc, max_y = {max_y}");
        assert!(pos.distance(target) < 40.0, "landed at {pos}");
    }
}
