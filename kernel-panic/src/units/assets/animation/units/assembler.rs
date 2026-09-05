//! assembler.bos — the System mobile constructor. Rises while built,
//! then its `body` ring spins slowly around y.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// assembler.bos Create(): `spin body around y-axis speed <60>`
const BODY_SPIN_DPS: f32 = 60.0;

#[derive(Default)]
pub struct AssemblerAnim {
    /// Spin starts post-emerge.
    spinning: bool,
}

impl UnitAnim for AssemblerAnim {
    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create(): base sinks [-8]·pct/100 while building, then the
        // body ring starts spinning.
        super::emerge_lift(rig, "base", 8.0, ctx.build_percent);
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps("body", Axis::Y, BODY_SPIN_DPS);
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide body/nozzle; explode body SHATTER, nozzle FALL.
        rig.hide("body");
        rig.hide("nozzle");
        rig.explode("body", 4);
        rig.explode("nozzle", 3);
    }

    fn aim(&mut self, _rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // assembler.bos AimWeapon1: `return 0` — the assembler is a
        // builder and never fires through the weapon pipeline.
        false
    }
}
