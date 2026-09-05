//! bug.bos / exploit.bos — the Hacker deploy pair. The morph itself is
//! host-side (despawn + spawn with carried HP); each side just needs its
//! authored pose and aim behaviour.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// bug.bos — the crawling form. Turret and clamps stay hidden until a
/// deploy morph would reveal them; aim is body-driven by the host (the
/// script has no `AimWeapon1`).
#[derive(Default)]
pub struct BugAnim;

impl UnitAnim for BugAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide turret; hide clamps.
        rig.hide("turret");
        rig.hide("clamps");
    }
}

/// exploit.bos — the deployed form. Spawns with the body flipped over
/// (`x <180>`, sunk `y [-12]`) and the turret exposed; the turret piece
/// carries both aim axes.
#[derive(Default)]
pub struct ExploitAnim;

impl UnitAnim for ExploitAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide feet0..2; body x <180> now; body y [-12] now.
        for foot in ["feet0", "feet1", "feet2"] {
            rig.hide(foot);
        }
        rig.turn_deg("body", Axis::X, 180.0, 0.0);
        rig.move_to("body", Axis::Y, -12.0, 0.0);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turret y to h @<180>, x to (0-p) @<180>;
        // returns 1 once the turns settle.
        rig.turn_rad("turret", Axis::Y, h, 180.0 * super::super::DEG2RAD);
        rig.turn_rad("turret", Axis::X, -p, 180.0 * super::super::DEG2RAD);
        true
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // exploit.bos declares no Killed() — the flip pose dies with it.
        let _ = rig;
    }
}
