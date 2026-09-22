//! badblock.bos — the cheap wall. Two build lasers pitched up at
//! creation, tracing a small square forever (it's the "under
//! construction" gag — the wall is always building itself).

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::SquareSweep;

/// BuildLasers(): square half-width ±30 elmos @80 elmos/s (bytecode
/// ±1966080 @5242880 — 163840 linear constant).
const SWEEP: f32 = 30.0;
const SWEEP_SPEED: f32 = 80.0;
/// Emit cadence (script: `sleep 60`, throttled for particle budget).
const EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct BadBlockAnim {
    pieces: BadBlockPieces,
    sweep: SquareSweep,
    emit_timer: f32,
}

#[derive(Clone, Copy, Default)]
struct BadBlockPieces {
    base: usize,
    blaser: [usize; 2],
}

impl BadBlockPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            blaser: [rig.bind_piece("blaser0"), rig.bind_piece("blaser1")],
        }
    }
}

impl UnitAnim for BadBlockAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = BadBlockPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): blaser0/1 to x <90> now; base starts sunk [-8].
        for blaser in self.pieces.blaser {
            rig.turn_deg(blaser, Axis::X, 90.0, 0.0);
        }
        rig.move_to(self.pieces.base, Axis::Y, -20.0, 0.0);
        self.sweep = SquareSweep::new(SWEEP, SWEEP_SPEED);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base lifts with BUILD_PERCENT_LEFT.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 20.0, ctx.build_percent);
        }

        // BuildLasers(): square sweep, forever.
        let [blaser0, blaser1] = self.pieces.blaser;
        self.sweep
            .update(rig, blaser0, blaser1, SWEEP, SWEEP_SPEED, ctx.dt);

        // EmitBuildLasers(): both lasers spark continuously.
        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            for blaser in self.pieces.blaser {
                rig.emit(blaser, SfxKind::FireFlash);
            }
        }
    }
}
