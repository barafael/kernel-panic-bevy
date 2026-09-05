//! badblock.bos — the cheap wall. Two build lasers pitched up at
//! creation, tracing a small square forever (it's the "under
//! construction" gag — the wall is always building itself).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// BuildLasers(): square half-width [-12]/[12] @32 elmos/s.
const SWEEP: f32 = 12.0;
const SWEEP_SPEED: f32 = 32.0;
/// Emit cadence (script: `sleep 60`, throttled for particle budget).
const EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct BadBlockAnim {
    sweep_leg: usize,
    sweep_timer: f32,
    emit_timer: f32,
}

impl UnitAnim for BadBlockAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): blaser0/1 to x <90> now; base starts sunk [-8].
        rig.turn_deg("blaser0", Axis::X, 90.0, 0.0);
        rig.turn_deg("blaser1", Axis::X, 90.0, 0.0);
        rig.move_to("base", Axis::Y, -8.0, 0.0);
        self.sweep_timer = SWEEP / SWEEP_SPEED;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base lifts with BUILD_PERCENT_LEFT.
        super::emerge_lift(rig, "base", 8.0, ctx.build_percent);

        // BuildLasers(): square sweep, forever.
        self.sweep_timer -= ctx.dt;
        if self.sweep_timer <= 0.0 {
            self.sweep_timer = SWEEP / SWEEP_SPEED;
            self.sweep_leg = (self.sweep_leg + 1) % 4;
            match self.sweep_leg {
                0 => {
                    rig.move_to("blaser0", Axis::X, -SWEEP, SWEEP_SPEED);
                    rig.move_to("blaser1", Axis::X, SWEEP, SWEEP_SPEED);
                }
                1 => {
                    rig.move_to("blaser0", Axis::Z, SWEEP, SWEEP_SPEED);
                    rig.move_to("blaser1", Axis::Z, -SWEEP, SWEEP_SPEED);
                }
                _ => {
                    rig.move_to("blaser0", Axis::X, 0.0, 0.0);
                    rig.move_to("blaser1", Axis::X, 0.0, 0.0);
                    rig.move_to("blaser0", Axis::Z, 0.0, 0.0);
                    rig.move_to("blaser1", Axis::Z, 0.0, 0.0);
                }
            }
        }

        // EmitBuildLasers(): both lasers spark continuously.
        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            rig.emit("blaser0", 2048);
            rig.emit("blaser1", 2048);
        }
    }
}
