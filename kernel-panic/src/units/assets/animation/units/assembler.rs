//! assembler.bos — the System mobile constructor. Rises while built,
//! then its `body` ring spins slowly around y.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};
use super::DeathFx;

/// assembler.bos Create(): `spin body around y-axis speed <60>`
const BODY_SPIN_DPS: f32 = 60.0;

#[derive(Clone, Copy, Default)]
struct AssemblerPieces {
    base: usize,
    body: usize,
    nozzle: usize,
}

impl AssemblerPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            body: rig.bind_piece("body"),
            nozzle: rig.bind_piece("nozzle"),
        }
    }
}

#[derive(Default)]
pub struct AssemblerAnim {
    pieces: AssemblerPieces,
    /// Spin starts post-emerge.
    spinning: bool,
    death: DeathFx,
}

impl UnitAnim for AssemblerAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = AssemblerPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create(): base sinks [-8]·pct/100 while building, then the
        // body ring starts spinning.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 20.0, ctx.build_percent);
        }
        self.death.tick(ctx.dt);
        if !self.spinning && ctx.build_percent <= 0 {
            self.spinning = true;
            rig.spin_dps(self.pieces.body, Axis::Y, BODY_SPIN_DPS);
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide body/nozzle; explode body SHATTER, nozzle FALL.
        rig.hide(self.pieces.body);
        rig.hide(self.pieces.nozzle);
        rig.explode(self.pieces.body, 4);
        rig.explode(self.pieces.nozzle, 3);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }

    fn aim(&mut self, _rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // assembler.bos AimWeapon1: `return 0` — the assembler is a
        // builder and never fires through the weapon pipeline.
        false
    }
}
