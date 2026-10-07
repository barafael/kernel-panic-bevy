//! terminal.bos — the System SIGTERM air-strike building. During its
//! build the body rises from 75 elmos under and two lasers sweep a
//! rectangle, firing `BuildLaser` beams every 30 ms; once built, it sits
//! inert until it dies.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};

/// MoveLaser(): bl0 x → +32, bl1 x → −32 @64 (0.5 s); both z → −64 @64
/// (1.0 s); x → 0 @64 (0.5 s); z → 0 now. Period 2.0 s.
const SWEEP_X: f32 = 32.0;
const SWEEP_Z: f32 = 64.0;
const SWEEP_SPEED: f32 = 64.0;
/// EmitLaser(): `sleep 30`.
const EMIT_INTERVAL: f32 = 0.03;
/// Create(): `move body to y-axis [-.75]*BPL` — 75 elmos under at 100 %.
const BODY_SINK: f32 = 75.0;

#[derive(Clone, Copy, Default)]
struct TerminalPieces {
    body: usize,
    bl: [usize; 2],
}

impl TerminalPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            bl: [rig.bind_piece("bl0"), rig.bind_piece("bl1")],
        }
    }
}

#[derive(Default)]
pub struct TerminalAnim {
    pieces: TerminalPieces,
    sweep_leg: usize,
    sweep_timer: f32,
    emit_timer: f32,
}

impl TerminalAnim {
    /// MoveLaser() leg sequence: x out, z out, x home, z snap home.
    /// Returns the leg's duration (`wait-for-move`).
    fn run_leg(&self, rig: &mut AnimRig, leg: usize) -> f32 {
        let [bl0, bl1] = self.pieces.bl;
        match leg {
            0 => {
                rig.move_to(bl0, Axis::X, SWEEP_X, SWEEP_SPEED);
                rig.move_to(bl1, Axis::X, -SWEEP_X, SWEEP_SPEED);
                SWEEP_X / SWEEP_SPEED
            }
            1 => {
                rig.move_to(bl0, Axis::Z, -SWEEP_Z, SWEEP_SPEED);
                rig.move_to(bl1, Axis::Z, -SWEEP_Z, SWEEP_SPEED);
                SWEEP_Z / SWEEP_SPEED
            }
            2 => {
                rig.move_to(bl0, Axis::X, 0.0, SWEEP_SPEED);
                rig.move_to(bl1, Axis::X, 0.0, SWEEP_SPEED);
                SWEEP_X / SWEEP_SPEED
            }
            _ => {
                rig.move_to(bl0, Axis::Z, 0.0, 0.0);
                rig.move_to(bl1, Axis::Z, 0.0, 0.0);
                0.0
            }
        }
    }
}

impl UnitAnim for TerminalAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = TerminalPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // MoveLaser(): bl0/bl1 to x <90> now (pitched down at the ground).
        for bl in self.pieces.bl {
            rig.turn_deg(bl, Axis::X, 90.0, 0.0);
        }
        rig.move_to(self.pieces.body, Axis::Y, -BODY_SINK, 0.0);
        self.sweep_timer = self.run_leg(rig, 0);
        self.emit_timer = EMIT_INTERVAL;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Both loops only run while the terminal is still being built
        // (`while(get BUILD_PERCENT_LEFT)`).
        if !ctx.emerging {
            return;
        }

        // Create()'s emerge: `move body to y [-.75]·pct` now.
        super::emerge_lift(rig, self.pieces.body, BODY_SINK, ctx.build_percent);

        self.sweep_timer -= ctx.dt;
        if self.sweep_timer <= 0.0 {
            self.sweep_leg = (self.sweep_leg + 1) % 4;
            self.sweep_timer = self.run_leg(rig, self.sweep_leg);
        }

        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            for bl in self.pieces.bl {
                rig.emit(bl, SfxKind::BuildBeam);
            }
        }
    }
}
