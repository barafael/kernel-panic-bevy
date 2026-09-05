//! logic_bomb.bos — the cloaked suicide mine. The mine bulb sits sunk
//! [-4] and rises with the build; no other visible choreography (the
//! survivor-scan auto-detonation is host gameplay, not animation).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct LogicBombAnim;

impl UnitAnim for LogicBombAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): move mine to y-axis [-4] now.
        rig.move_to("mine", Axis::Y, -4.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge: mine rises from [-4]·pct/100.
        super::emerge_lift(rig, "mine", 4.0, ctx.build_percent);
    }

    fn aim(&mut self, _rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // logic_bomb.bos AimWeapon1: `return 0` — the mine kills through
        // its detonation, never through the weapon pipeline.
        false
    }
}
