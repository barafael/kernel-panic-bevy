//! trojan.bos — the Hacker mobile builder. After emerging, its `center`
//! ring spins slowly around the z-axis; the four `piece`s are the
//! build-effect anchors.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// trojan.bos Create(): `spin center around z-axis speed <-120>`
const CENTER_SPIN_DPS: f32 = -120.0;

#[derive(Default)]
pub struct TrojanAnim {
    /// The spin starts only once the emerge completes (`sleep 5000`
    /// after BUILD_PERCENT_LEFT drains in the script).
    spinning: bool,
}

impl UnitAnim for TrojanAnim {
    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps("center", Axis::Z, CENTER_SPIN_DPS);
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode piece0..3 FALL, then hide them.
        for piece in ["piece0", "piece1", "piece2", "piece3"] {
            rig.explode(piece, 3);
            rig.hide(piece);
        }
    }
}
