//! flow.bos — the airborne Network unit. Two wings counter-spin around
//! z forever after the emerge; the body pitches/yaws onto targets and
//! the muzzle cycles gp0→gp3 between shots.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

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

#[derive(Default)]
pub struct FlowAnim {
    pieces: FlowPieces,
    /// Counter-spin starts post-emerge.
    spinning: bool,
    death: DeathFx,
}

impl UnitAnim for FlowAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = FlowPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn gp2 to y <-60> now; gp3 to y <60> now.
        rig.turn_deg(self.pieces.gunpoint[2], Axis::Y, -60.0, 0.0);
        rig.turn_deg(self.pieces.gunpoint[3], Axis::Y, 60.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            // Create(): spin wing1 around z <180>; wing2 around z <-180>.
            self.spinning = true;
            rig.spin_dps(self.pieces.wing1, Axis::Z, 180.0);
            rig.spin_dps(self.pieces.wing2, Axis::Z, -180.0);
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): base x to (0-p) @<360>, y to h @<480>.
        rig.turn_rad(self.pieces.base, Axis::X, -p, deg2rad(360.0));
        rig.turn_rad(self.pieces.base, Axis::Y, h, deg2rad(480.0));
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Shot1(): gp = gp + 1 — cycle the muzzle gp0→gp1→gp2→gp3→gp0.
        rig.muzzle = (rig.muzzle + 1) % 4;
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
