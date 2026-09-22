//! socket.bos — the System datavent factory. Four build lasers pitch up
//! at creation; the outer pair forever traces a square while the inner
//! pair bobs — and the inner pair sparks while the factory produces.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::SquareSweep;

/// BuildLasers(): outer-laser square half-width ±60 elmos @80 elmos/s
/// (bytecode ±3932160 @5242880 — socket compiles at linear constant
/// 163840, so the .bos's ±[24]@[32] is 2.5× that).
const SWEEP: f32 = 60.0;
const SWEEP_SPEED: f32 = 80.0;
/// ConLasers(): inner-laser bob depth 35 @40 elmos/s (bytecode
/// 2293760 @2621440).
const BOB: f32 = 35.0;
const BOB_SPEED: f32 = 40.0;
/// Emit cadence (script: `sleep 60`, throttled 2× for particle budget).
const EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct SocketAnim {
    pieces: SocketPieces,
    /// Outer-laser square sweep.
    sweep: SquareSweep,
    /// Inner-laser bob phase.
    bob_out: bool,
    bob_timer: f32,
    /// Idle spark timer for the outer pair.
    emit_timer: f32,
}

#[derive(Clone, Copy, Default)]
struct SocketPieces {
    body: usize,
    blaser: [usize; 2],
    claser: [usize; 2],
}

impl SocketPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            blaser: [rig.bind_piece("blaser0"), rig.bind_piece("blaser1")],
            claser: [rig.bind_piece("claser0"), rig.bind_piece("claser1")],
        }
    }
}

impl UnitAnim for SocketAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = SocketPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): all four lasers pitch up <90>; body starts sunk.
        for laser in self.pieces.blaser.into_iter().chain(self.pieces.claser) {
            rig.turn_deg(laser, Axis::X, 90.0, 0.0);
        }
        rig.move_to(self.pieces.body, Axis::Y, -40.0, 0.0);
        self.sweep = SquareSweep::new(SWEEP, SWEEP_SPEED);
        self.bob_timer = BOB / BOB_SPEED;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge: body lifts with BUILD_PERCENT_LEFT.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.body, 40.0, ctx.build_percent);
        }

        // BuildLasers(): outer pair traces a square forever.
        let [blaser0, blaser1] = self.pieces.blaser;
        self.sweep
            .update(rig, blaser0, blaser1, SWEEP, SWEEP_SPEED, ctx.dt);

        // ConLasers(): inner pair bobs up and down forever.
        self.bob_timer -= ctx.dt;
        if self.bob_timer <= 0.0 {
            self.bob_timer = BOB / BOB_SPEED;
            self.bob_out = !self.bob_out;
            let target = if self.bob_out { BOB } else { 0.0 };
            for claser in self.pieces.claser {
                rig.move_to(claser, Axis::Z, target, BOB_SPEED);
            }
        }

        // EmitBuildLasers() / EmitConLasers(): the outer pair sparks
        // always; the inner pair only while producing (Activate sets
        // `building` in the script; ctx.producing mirrors it).
        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            for blaser in self.pieces.blaser {
                rig.emit(blaser, SfxKind::FireFlash);
            }
            if ctx.producing {
                for claser in self.pieces.claser {
                    rig.emit(claser, SfxKind::FireFlash);
                }
            }
        }
    }
}
