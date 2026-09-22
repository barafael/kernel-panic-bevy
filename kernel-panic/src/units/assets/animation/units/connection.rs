//! connection.bos — the Network teleporter. Its `gp` yaws and pitches
//! onto targets (the `gp2` barrel extension needs target distance, which
//! the driver context doesn't carry — skipped); the body shatters on
//! death.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

#[derive(Clone, Copy, Default)]
struct ConnectionPieces {
    body: usize,
    gp: usize,
}

impl ConnectionPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            gp: rig.bind_piece("gp"),
        }
    }
}

#[derive(Default)]
pub struct ConnectionAnim {
    pieces: ConnectionPieces,
    death: DeathFx,
}

impl UnitAnim for ConnectionAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = ConnectionPieces::bind(rig);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): gp y to h @<180>, x to (0-p) @<180>.
        rig.turn_rad(self.pieces.gp, Axis::Y, h, deg2rad(180.0));
        rig.turn_rad(self.pieces.gp, Axis::X, -p, deg2rad(180.0));
        true
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER; hide body.
        rig.explode(self.pieces.body, 4);
        rig.hide(self.pieces.body);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
