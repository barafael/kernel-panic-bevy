//! socket.bos — the System datavent factory, translated from the compiled
//! bytecode (linear constant 163840: `.bos` brackets are 2.5× the values
//! below).
//!
//! - `Create()`: the four lasers pitch down (`x <90>`); the body starts
//!   40 elmos under and rises with `BUILD_PERCENT_LEFT`. While the socket
//!   is being built, `BuildLasers()` traces a ±60 square with the outer
//!   pair (x out, z out, snap home; 1.5 s, no pause) and
//!   `EmitBuildLasers()` fires `BuildLaser` beams from them every 60 ms.
//!   `SIG_COMPLETE` kills both 120 ms after the build finishes.
//! - Once complete, `ConLasers()` bobs the inner pair out to 35 and back
//!   at 40 elmos/s forever, and `EmitConLasers()` fires beams from them
//!   every 60 ms while `building` (set by `Activate`, cleared by
//!   `Deactivate`).
//! - `Activate()` sets `INBUILDSTANCE` at once: no production gate.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};

/// BuildLasers(): outer-laser square half-width ±60 elmos @80 elmos/s
/// (bytecode ±3932160 @5242880).
const SWEEP: f32 = 60.0;
const SWEEP_SPEED: f32 = 80.0;
/// ConLasers(): inner-laser bob depth 35 @40 elmos/s (bytecode
/// 2293760 @2621440).
const BOB: f32 = 35.0;
const BOB_SPEED: f32 = 40.0;
/// EmitBuildLasers()/EmitConLasers(): `sleep 60`.
const EMIT_INTERVAL: f32 = 0.06;
/// Create(): `move body to y-axis [-16]` = −40 while building.
const BODY_SINK: f32 = 40.0;

#[derive(Default)]
pub struct SocketAnim {
    pieces: SocketPieces,
    /// BuildLasers() leg 0..3 (x out, z out, snap) and its timer.
    sweep_leg: usize,
    sweep_timer: f32,
    /// ConLasers() phase and timer; starts after the build completes.
    con_started: bool,
    bob_out: bool,
    bob_timer: f32,
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

impl SocketAnim {
    /// BuildLasers(): one leg of the square, returning how long it runs.
    fn sweep_leg(&self, rig: &mut AnimRig, leg: usize) -> f32 {
        let [b0, b1] = self.pieces.blaser;
        match leg {
            0 => {
                rig.move_to(b0, Axis::X, -SWEEP, SWEEP_SPEED);
                rig.move_to(b1, Axis::X, SWEEP, SWEEP_SPEED);
                SWEEP / SWEEP_SPEED
            }
            1 => {
                rig.move_to(b0, Axis::Z, SWEEP, SWEEP_SPEED);
                rig.move_to(b1, Axis::Z, -SWEEP, SWEEP_SPEED);
                SWEEP / SWEEP_SPEED
            }
            _ => {
                for b in [b0, b1] {
                    rig.move_to(b, Axis::X, 0.0, 0.0);
                    rig.move_to(b, Axis::Z, 0.0, 0.0);
                }
                0.0
            }
        }
    }
}

impl UnitAnim for SocketAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = SocketPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): all four lasers pitch down <90>; body starts sunk.
        for laser in self.pieces.blaser.into_iter().chain(self.pieces.claser) {
            rig.turn_deg(laser, Axis::X, 90.0, 0.0);
        }
        rig.move_to(self.pieces.body, Axis::Y, -BODY_SINK, 0.0);
        self.sweep_timer = self.sweep_leg(rig, 0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.emit_timer -= ctx.dt;
        let emit = self.emit_timer <= 0.0;
        if emit {
            self.emit_timer = EMIT_INTERVAL;
        }

        if ctx.emerging {
            // Create()'s loop: body y = −40·pct/100.
            super::emerge_lift(rig, self.pieces.body, BODY_SINK, ctx.build_percent);

            // BuildLasers(): x out, z out, snap home — 1.5 s a lap.
            self.sweep_timer -= ctx.dt;
            if self.sweep_timer <= 0.0 {
                self.sweep_leg = (self.sweep_leg + 1) % 3;
                self.sweep_timer = self.sweep_leg(rig, self.sweep_leg);
            }
            // EmitBuildLasers(): beams into the ground under the pair.
            if emit {
                for blaser in self.pieces.blaser {
                    rig.emit(blaser, SfxKind::BuildBeam);
                }
            }
            return;
        }

        if !self.con_started {
            // `signal SIG_COMPLETE` parks the outer pair; ConLasers starts.
            self.con_started = true;
            self.sweep_leg(rig, 2);
            self.bob_out = true;
            self.bob_timer = BOB / BOB_SPEED;
            for claser in self.pieces.claser {
                rig.move_to(claser, Axis::Z, BOB, BOB_SPEED);
            }
        }

        // ConLasers(): inner pair bobs out and back forever.
        self.bob_timer -= ctx.dt;
        if self.bob_timer <= 0.0 {
            self.bob_timer = BOB / BOB_SPEED;
            self.bob_out = !self.bob_out;
            let target = if self.bob_out { BOB } else { 0.0 };
            for claser in self.pieces.claser {
                rig.move_to(claser, Axis::Z, target, BOB_SPEED);
            }
        }

        // EmitConLasers(): `while (building)` — Activate..Deactivate.
        if emit && ctx.producing {
            for claser in self.pieces.claser {
                rig.emit(claser, SfxKind::BuildBeam);
            }
        }
    }
}
