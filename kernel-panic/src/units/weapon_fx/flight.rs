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
//!
//! Everything runs in Spring's native units: one [`step`] per 30 Hz sim
//! frame, speeds in elmos/frame, accelerations in elmos/frame², turn
//! rates as the engine's `weaponDef->turnrate` (TDF `turnrate` × 2π/65536
//! / 30 — a per-frame *vector* gain, not an angle per second).
//!
//! [`step`]: MissileFlight::step

use bevy::prelude::*;

use crate::sim::{GAME_SPEED, SHORT_ANGLE_TO_RAD};

/// `CProjectile::mygravity` for these projectiles: the map's gravity per
/// frame² (negative = down). Kernel Panic maps use `gravity=50`
/// (see the ballistic solve in `spawn.rs`).
pub const MAP_GRAVITY_PER_FRAME2: f32 = -50.0 / (GAME_SPEED * GAME_SPEED);

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
        let gravity = if w.my_gravity != 0.0 {
            -w.my_gravity
        } else {
            MAP_GRAVITY_PER_FRAME2
        };
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
        let gravity = if w.my_gravity != 0.0 {
            -w.my_gravity
        } else {
            MAP_GRAVITY_PER_FRAME2
        };
        (
            Self {
                dir,
                speed: (w.start_velocity / GAME_SPEED).max(0.01),
                max_speed,
                acceleration: w.weapon_acceleration / (GAME_SPEED * GAME_SPEED),
                turn_rate,
                tracking,
                max_good_dif: (tracking * 0.6).cos(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let turn = engine_turn_rate(60000.0).atan();
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
            assert!(prev.angle_between(f.dir) < gain.atan() + 0.2);
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
