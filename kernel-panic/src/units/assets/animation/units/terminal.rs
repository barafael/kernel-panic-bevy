//! terminal.bos — the System SIGTERM air-strike building. During its
//! emerge, two lasers sweep a rectangle and spark; once built, it sits
//! inert until it dies.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// MoveLaser(): rectangle ±[32] on x, [−64] on z, at [64] elmos/s.
const SWEEP_X: f32 = 32.0;
const SWEEP_Z: f32 = 64.0;
const SWEEP_SPEED: f32 = 64.0;
/// EmitLaser(): spark per `sleep 30` (throttled for particle budget).
const EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct TerminalAnim {
    sweep_leg: usize,
    sweep_timer: f32,
    emit_timer: f32,
}

impl TerminalAnim {
    /// MoveLaser() leg sequence: x out, z out, x home, z snap home.
    fn run_leg(&mut self, rig: &mut AnimRig, leg: usize) {
        match leg {
            0 => {
                rig.move_to("bl0", Axis::X, SWEEP_X, SWEEP_SPEED);
                rig.move_to("bl1", Axis::X, -SWEEP_X, SWEEP_SPEED);
            }
            1 => {
                rig.move_to("bl0", Axis::Z, -SWEEP_Z, SWEEP_SPEED);
                rig.move_to("bl1", Axis::Z, -SWEEP_Z, SWEEP_SPEED);
            }
            2 => {
                rig.move_to("bl0", Axis::X, 0.0, SWEEP_SPEED);
                rig.move_to("bl1", Axis::X, 0.0, SWEEP_SPEED);
            }
            _ => {
                rig.move_to("bl0", Axis::Z, 0.0, 0.0);
                rig.move_to("bl1", Axis::Z, 0.0, 0.0);
            }
        }
    }
}

impl UnitAnim for TerminalAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // MoveLaser(): bl0/bl1 to x <90> now (pitched up at the ground).
        rig.turn_deg("bl0", Axis::X, 90.0, 0.0);
        rig.turn_deg("bl1", Axis::X, 90.0, 0.0);
        self.sweep_timer = SWEEP_X / SWEEP_SPEED;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Both loops only run while the terminal is still emerging
        // (`while(get BUILD_PERCENT_LEFT)`).
        if ctx.build_percent <= 0 {
            return;
        }

        // Create()'s emerge: `move body to y [-.75]·pct` — sink rises
        // with the build percentage directly.
        if ctx.emerging {
            super::emerge_lift(rig, "body", 0.75, ctx.build_percent);
        }

        self.sweep_timer -= ctx.dt;
        if self.sweep_timer <= 0.0 {
            self.sweep_timer = SWEEP_X / SWEEP_SPEED;
            self.sweep_leg = (self.sweep_leg + 1) % 4;
            self.run_leg(rig, self.sweep_leg);
        }

        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            rig.emit("bl0", 2048);
            rig.emit("bl1", 2048);
        }
    }
}
