//! socket.bos — the System datavent factory. Four build lasers pitch up
//! at creation; the outer pair forever traces a square while the inner
//! pair bobs — and the inner pair sparks while the factory produces.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// BuildLasers(): outer-laser square half-width [-24]/[24] @32 elmos/s.
const SWEEP: f32 = 24.0;
const SWEEP_SPEED: f32 = 32.0;
/// ConLasers(): inner-laser bob depth [14] @16 elmos/s.
const BOB: f32 = 14.0;
const BOB_SPEED: f32 = 16.0;
/// Emit cadence (script: `sleep 60`, throttled 2× for particle budget).
const EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct SocketAnim {
    /// Outer-laser sweep leg 0..4 (x-out → z-out → snap home ×2).
    sweep_leg: usize,
    sweep_timer: f32,
    /// Inner-laser bob phase.
    bob_out: bool,
    bob_timer: f32,
    /// Idle spark timer for the outer pair.
    emit_timer: f32,
}

impl UnitAnim for SocketAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): all four lasers pitch up <90>; body starts sunk.
        for laser in ["blaser0", "blaser1", "claser0", "claser1"] {
            rig.turn_deg(laser, Axis::X, 90.0, 0.0);
        }
        rig.move_to("body", Axis::Y, -16.0, 0.0);
        self.sweep_timer = SWEEP / SWEEP_SPEED;
        self.bob_timer = BOB / BOB_SPEED;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge: body lifts with BUILD_PERCENT_LEFT.
        if ctx.emerging {
            super::emerge_lift(rig, "body", 16.0, ctx.build_percent);
        }

        // BuildLasers(): outer pair traces a square forever.
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
                    // snap home (x 0 now; z 0 now) and hold one leg
                    rig.move_to("blaser0", Axis::X, 0.0, 0.0);
                    rig.move_to("blaser1", Axis::X, 0.0, 0.0);
                    rig.move_to("blaser0", Axis::Z, 0.0, 0.0);
                    rig.move_to("blaser1", Axis::Z, 0.0, 0.0);
                }
            }
        }

        // ConLasers(): inner pair bobs up and down forever.
        self.bob_timer -= ctx.dt;
        if self.bob_timer <= 0.0 {
            self.bob_timer = BOB / BOB_SPEED;
            self.bob_out = !self.bob_out;
            let target = if self.bob_out { BOB } else { 0.0 };
            rig.move_to("claser0", Axis::Z, target, BOB_SPEED);
            rig.move_to("claser1", Axis::Z, target, BOB_SPEED);
        }

        // EmitBuildLasers() / EmitConLasers(): the outer pair sparks
        // always; the inner pair only while producing (Activate sets
        // `building` in the script; ctx.producing mirrors it).
        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            rig.emit("blaser0", 2048);
            rig.emit("blaser1", 2048);
            if ctx.producing {
                rig.emit("claser0", 2051);
                rig.emit("claser1", 2051);
            }
        }
    }
}
