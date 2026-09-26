//! flow.bos — the airborne Network unit. Two wings counter-spin around
//! z forever after the emerge; the whole body (`base`) yaws/pitches onto
//! the target, and each projectile leaves from the next of gp0..gp3.
//!
//! Upstream weapon script (the parts the engine calls):
//!
//! ```text
//! AimFromWeapon1(p) { p=monolith; }
//! QueryWeapon1(p)   { p=gp0 + gp; }
//! ResetAim()        { sleep 1000; turn base to x-axis 0 speed <360>;
//!                     turn base to y-axis 0 speed <480>; }
//! AimWeapon1(h,p)   { signal SIG_Aim; set-signal-mask SIG_Aim;
//!                     start-script ResetAim();
//!                     turn base to x-axis 0-p speed <360>;
//!                     turn base to y-axis h speed <480>;
//!                     wait-for-turn base around y-axis;
//!                     wait-for-turn base around x-axis; return 1; }
//! Shot1(zero)       { gp = gp + 1; }
//! EndBurst1()       { gp = -1; }
//! ```
//!
//! FlowMissile is `burst=2`, `projectiles=2`, so one salvo is
//! Shot→gp0, Shot→gp1, (9 frames), Shot→gp2, Shot→gp3, EndBurst. gp0/gp1
//! hang off `wing1`, gp2/gp3 off `wing2` (turned ±60° about y in
//! `Create`), and the wings spin — the muzzles and, for the
//! `fixedLauncher` missile, the launch directions sweep with them.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

/// `ResetAim()`'s `sleep 1000` before the base swings back to rest.
const RESET_AIM_DELAY: f32 = 1.0;

/// `wait-for-turn` tolerance. The script's wait completes once the
/// turn lands and `AimWeapon1` isn't re-run for a while; the host
/// re-issues it every tick with a target that has drifted a little
/// (a hovering Flow circling a moving target), so accept the base as
/// "there" within 2° — a fraction of one tick's turn (12°–16°) and well
/// inside what the homing missiles correct in their first frames.
const WAIT_FOR_TURN_TOLERANCE: f32 = 2.0 * std::f32::consts::PI / 180.0;

#[derive(Clone, Copy, Default)]
struct FlowPieces {
    base: usize,
    monolith: usize,
    wing1: usize,
    wing2: usize,
    gunpoint: [usize; 4],
}

impl FlowPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            monolith: rig.bind_piece("monolith"),
            wing1: rig.bind_piece("wing1"),
            wing2: rig.bind_piece("wing2"),
            gunpoint: [
                rig.bind_piece("gp0"),
                rig.bind_piece("gp1"),
                rig.bind_piece("gp2"),
                rig.bind_piece("gp3"),
            ],
        }
    }
}

pub struct FlowAnim {
    pieces: FlowPieces,
    /// Counter-spin starts post-emerge.
    spinning: bool,
    /// `static-var gp` — `-1` between bursts (`Create` / `EndBurst1`),
    /// incremented by every `Shot1`.
    gp: i32,
    /// Seconds since the last `AimWeapon1` (the `ResetAim` clock);
    /// `None` once the reset has run.
    since_aim: Option<f32>,
    death: DeathFx,
}

impl Default for FlowAnim {
    fn default() -> Self {
        Self {
            pieces: FlowPieces::default(),
            spinning: false,
            gp: -1,
            since_aim: None,
            death: DeathFx::default(),
        }
    }
}

impl FlowAnim {
    /// `QueryWeapon1`: `gp0 + gp`. Outside a burst (`gp == -1`) the COB
    /// arithmetic would name the piece before gp0 (`wing2`); nothing
    /// fires from there, so keep gp0 as the resting muzzle.
    fn query_weapon(&self) -> usize {
        self.pieces.gunpoint[self.gp.clamp(0, 3) as usize]
    }
}

impl UnitAnim for FlowAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = FlowPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn gp2 to y <-60> now; gp3 to y <60> now; gp = -1.
        rig.turn_deg(self.pieces.gunpoint[2], Axis::Y, -60.0, 0.0);
        rig.turn_deg(self.pieces.gunpoint[3], Axis::Y, 60.0, 0.0);
        self.gp = -1;
        rig.muzzle = self.query_weapon();
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            // Create(): spin wing1 around z <180>; wing2 around z <-180>.
            self.spinning = true;
            rig.spin_dps(self.pieces.wing1, Axis::Z, 180.0);
            rig.spin_dps(self.pieces.wing2, Axis::Z, -180.0);
        }
        // ResetAim(): one second after the last AimWeapon1 the base
        // swings back to rest (x @<360>, y @<480>).
        if let Some(t) = self.since_aim.as_mut() {
            *t += ctx.dt;
            if *t >= RESET_AIM_DELAY {
                self.since_aim = None;
                rig.turn_deg(self.pieces.base, Axis::X, 0.0, 360.0);
                rig.turn_deg(self.pieces.base, Axis::Y, 0.0, 480.0);
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): restart ResetAim; base x to (0-p) @<360>,
        // y to h @<480>; return 1 only once both turns have landed.
        self.since_aim = Some(0.0);
        rig.turn_rad(self.pieces.base, Axis::X, -p, deg2rad(360.0));
        rig.turn_rad(self.pieces.base, Axis::Y, h, deg2rad(480.0));
        let Some(r) = rig.piece_rotations.get(self.pieces.base) else {
            return true;
        };
        angle_delta(r[1], h) <= WAIT_FOR_TURN_TOLERANCE
            && angle_delta(r[0], -p) <= WAIT_FOR_TURN_TOLERANCE
    }

    fn shot(&mut self, rig: &mut AnimRig) {
        // Shot1(): gp = gp + 1; then the engine asks QueryWeapon1.
        self.gp += 1;
        rig.muzzle = self.query_weapon();
    }

    fn end_burst(&mut self, rig: &mut AnimRig) {
        // EndBurst1(): gp = -1.
        self.gp = -1;
        rig.muzzle = self.query_weapon();
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode monolith FALL + wings SHATTER; hide all 3.
        rig.explode(self.pieces.monolith, 3);
        rig.explode(self.pieces.wing1, 4);
        rig.explode(self.pieces.wing2, 4);
        rig.hide(self.pieces.monolith);
        rig.hide(self.pieces.wing1);
        rig.hide(self.pieces.wing2);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}

/// Shortest absolute angular distance between two angles (radians).
fn angle_delta(a: f32, b: f32) -> f32 {
    let d = (a - b).rem_euclid(std::f32::consts::TAU);
    d.min(std::f32::consts::TAU - d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rig() -> AnimRig {
        AnimRig::for_test(&[
            "base", "monolith", "wing1", "wing2", "gp0", "gp1", "gp2", "gp3",
        ])
    }

    /// One FlowMissile salvo is burst=2 × projectiles=2: Shot1 before
    /// each projectile walks gp0→gp1→gp2→gp3 (as *piece indices*, not
    /// 0..3 — the old driver wrote the slot number into the piece index
    /// and fired from base/monolith/wings), EndBurst1 rewinds.
    #[test]
    fn shot_walks_gp0_to_gp3_and_end_burst_rewinds() {
        let mut rig = rig();
        let mut anim = FlowAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        assert_eq!(rig.muzzle, 4, "rests on gp0");
        let mut seen = Vec::new();
        for _ in 0..4 {
            anim.shot(&mut rig);
            seen.push(rig.muzzle);
        }
        assert_eq!(seen, vec![4, 5, 6, 7]);
        anim.end_burst(&mut rig);
        anim.shot(&mut rig);
        assert_eq!(rig.muzzle, 4, "next salvo starts at gp0 again");
    }

    /// AimWeapon1 only returns 1 after `wait-for-turn base` — a fresh
    /// 90° heading change needs 90/480 s of turning before firing.
    #[test]
    fn aim_waits_for_the_base_turn() {
        let mut rig = rig();
        let mut anim = FlowAnim::default();
        anim.bind(&rig);
        let h = std::f32::consts::FRAC_PI_2;
        assert!(!anim.aim(&mut rig, h, 0.0, AnimCtx::minimal()));
        let dt = 1.0 / 30.0;
        let mut frames = 0;
        loop {
            crate::units::assets::animation::tick_rig(&mut rig, dt);
            frames += 1;
            if anim.aim(&mut rig, h, 0.0, AnimCtx::minimal()) {
                break;
            }
            assert!(frames < 30, "base never reached the aim heading");
        }
        // 90° at 480°/s = 5.6 frames.
        assert!((5..=7).contains(&frames), "took {frames} frames");
    }
}
