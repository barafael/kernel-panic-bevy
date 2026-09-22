//! window.bos — the Hacker datavent factory. Bar and flap stay hidden
//! while the hourglass "under construction" scene plays; Activate
//! (production on) swings the flap open, Deactivate closes it.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Clone, Copy, Default)]
struct WindowPieces {
    bar: usize,
    flap: usize,
    hourglass: [usize; 5],
}

impl WindowPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            bar: rig.bind_piece("bar"),
            flap: rig.bind_piece("flap"),
            hourglass: [
                rig.bind_piece("hglass0"),
                rig.bind_piece("hglass1"),
                rig.bind_piece("sand0"),
                rig.bind_piece("sand1"),
                rig.bind_piece("sand2"),
            ],
        }
    }
}

#[derive(Default)]
pub struct WindowAnim {
    pieces: WindowPieces,
    /// Post-emerge cleanup has run (show bar/flap, hide the hourglass).
    finished: bool,
}

impl UnitAnim for WindowAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = WindowPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide bar; hide flap; Anim() runs the hourglass
        // rotation while building (cosmetic — skipped).
        rig.hide(self.pieces.bar);
        rig.hide(self.pieces.flap);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.finished && ctx.build_percent <= 0 {
            // Create(), post-build: show bar/flap; hide the hourglass
            // sub-pieces.
            self.finished = true;
            rig.show(self.pieces.bar);
            rig.show(self.pieces.flap);
            for piece in self.pieces.hourglass {
                rig.hide(piece);
            }
        }
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): flap to x <-90> speed <120> — the window opens.
        rig.turn_deg(self.pieces.flap, Axis::X, -90.0, 120.0);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): flap back to 0.
        rig.turn_deg(self.pieces.flap, Axis::X, 0.0, 120.0);
    }
}
