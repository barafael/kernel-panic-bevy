//! flow.bos — the airborne Network unit. Two wings counter-spin around
//! z forever after the emerge; the body pitches/yaws onto targets and
//! the muzzle cycles gp0→gp3 between shots.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct FlowAnim {
    /// Counter-spin starts post-emerge.
    spinning: bool,
}

impl UnitAnim for FlowAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn gp2 to y <-60> now; gp3 to y <60> now.
        rig.turn_deg("gp2", Axis::Y, -60.0, 0.0);
        rig.turn_deg("gp3", Axis::Y, 60.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.spinning && ctx.build_percent <= 0 {
            // Create(): spin wing1 around z <180>; wing2 around z <-180>.
            self.spinning = true;
            rig.spin_dps("wing1", Axis::Z, 180.0);
            rig.spin_dps("wing2", Axis::Z, -180.0);
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): base x to (0-p) @<360>, y to h @<480>.
        rig.turn_rad("base", Axis::X, -p, 360.0 * super::super::DEG2RAD);
        rig.turn_rad("base", Axis::Y, h, 480.0 * super::super::DEG2RAD);
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Shot1(): gp = gp + 1 — cycle the muzzle gp0→gp1→gp2→gp3→gp0.
        rig.muzzle = (rig.muzzle + 1) % 4;
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode monolith FALL + wings SHATTER; hide all 3.
        rig.explode("monolith", 3);
        rig.explode("wing1", 4);
        rig.explode("wing2", 4);
        rig.hide("monolith");
        rig.hide("wing1");
        rig.hide("wing2");
    }
}
