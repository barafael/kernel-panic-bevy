//! connection.bos — the Network teleporter. Its `gp` yaws and pitches
//! onto targets (the `gp2` barrel extension needs target distance, which
//! the driver context doesn't carry — skipped); the body shatters on
//! death.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct ConnectionAnim;

impl UnitAnim for ConnectionAnim {
    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): gp y to h @<180>, x to (0-p) @<180>.
        rig.turn_rad("gp", Axis::Y, h, 180.0 * super::super::DEG2RAD);
        rig.turn_rad("gp", Axis::X, -p, 180.0 * super::super::DEG2RAD);
        true
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER; hide body.
        rig.explode("body", 4);
        rig.hide("body");
    }
}
