//! window.bos — the Hacker datavent factory. Bar and flap stay hidden
//! while the hourglass "under construction" scene plays; Activate
//! (production on) swings the flap open, Deactivate closes it.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct WindowAnim {
    /// Post-emerge cleanup has run (show bar/flap, hide the hourglass).
    finished: bool,
}

impl UnitAnim for WindowAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide bar; hide flap; Anim() runs the hourglass
        // rotation while building (cosmetic — skipped).
        rig.hide("bar");
        rig.hide("flap");
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.finished && ctx.build_percent <= 0 {
            // Create(), post-build: show bar/flap; hide the hourglass
            // sub-pieces.
            self.finished = true;
            rig.show("bar");
            rig.show("flap");
            for piece in ["hglass0", "hglass1", "sand0", "sand1", "sand2"] {
                rig.hide(piece);
            }
        }
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): flap to x <-90> speed <120> — the window opens.
        rig.turn_deg("flap", Axis::X, -90.0, 120.0);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): flap back to 0.
        rig.turn_deg("flap", Axis::X, 0.0, 120.0);
    }
}
