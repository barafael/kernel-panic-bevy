//! trojan.bos — the Hacker mobile builder. After emerging, its `center`
//! ring spins slowly around the z-axis; the four `piece`s are the
//! build-effect anchors.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};
use super::DeathFx;

/// trojan.bos Create(): `spin center around z-axis speed <-120>`
const CENTER_SPIN_DPS: f32 = -120.0;

#[derive(Clone, Copy, Default)]
struct TrojanPieces {
    center: usize,
    piece: [usize; 4],
}

impl TrojanPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            center: rig.bind_piece("center"),
            piece: [
                rig.bind_piece("piece0"),
                rig.bind_piece("piece1"),
                rig.bind_piece("piece2"),
                rig.bind_piece("piece3"),
            ],
        }
    }
}

#[derive(Default)]
pub struct TrojanAnim {
    pieces: TrojanPieces,
    /// The spin starts only once the emerge completes (`sleep 5000`
    /// after BUILD_PERCENT_LEFT drains in the script).
    spinning: bool,
    death: DeathFx,
}

impl UnitAnim for TrojanAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = TrojanPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps(self.pieces.center, Axis::Z, CENTER_SPIN_DPS);
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode piece0..3 FALL, then hide them.
        for piece in self.pieces.piece {
            rig.explode(piece, 3);
            rig.hide(piece);
        }
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
