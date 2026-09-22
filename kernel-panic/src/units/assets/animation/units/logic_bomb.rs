//! logic_bomb.bos — the cloaked suicide mine. The mine bulb sits sunk
//! [-4] and rises with the build; no other visible choreography (the
//! survivor-scan auto-detonation is host gameplay, not animation).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Clone, Copy, Default)]
struct LogicBombPieces {
    mine: usize,
}

impl LogicBombPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            mine: rig.bind_piece("mine"),
        }
    }
}

#[derive(Default)]
pub struct LogicBombAnim {
    pieces: LogicBombPieces,
}

impl UnitAnim for LogicBombAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = LogicBombPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): move mine to y-axis now — bytecode -655360 = -10
        // elmos (163840 linear constant; the .bos's [-4] is 10 elmos).
        rig.move_to(self.pieces.mine, Axis::Y, -10.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge: mine rises from [-4]·pct/100.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.mine, 10.0, ctx.build_percent);
        }
    }

    fn aim(&mut self, _rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // logic_bomb.bos AimWeapon1: `return 0` — the mine kills through
        // its detonation, never through the weapon pipeline.
        false
    }
}
