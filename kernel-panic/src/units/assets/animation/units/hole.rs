//! hole.bos — the Hacker homebase. Activating reveals the nano-arm
//! assembly, which sweeps out and shuttles the emitter head back and
//! forth while producing; deactivating hides it again.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Activate(): emitter shuttles z 0 ↔ [-55] @128 elmos/s (each leg).
/// Plus the arm sweeps x [-16] ↔ [-64] @64.
const SHUTTLE_DEPTH: f32 = 55.0;
const SHUTTLE_SPEED: f32 = 128.0;
const ARM_NEAR: f32 = 16.0;
const ARM_FAR: f32 = 64.0;
const ARM_SPEED: f32 = 64.0;

#[derive(Default)]
pub struct HoleAnim {
    /// Production open (mirrors Activate/Deactivate).
    active: bool,
    /// True = shuttling out, false = returning.
    out: bool,
    /// Countdown to the next shuttle leg reversal (depth/speed).
    leg_timer: f32,
    /// True = arm sweeping out toward the far stop.
    arm_out: bool,
    /// Countdown to the next arm sweep reversal.
    arm_timer: f32,
}

impl UnitAnim for HoleAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide nanoarm; hide nanomover.
        rig.hide("nanoarm");
        rig.hide("nanomover");
    }

    fn update(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        if !self.active {
            return;
        }
        // Shuttle loop: reverse the emitter head at each end of its
        // travel; the nano-arm sweeps back and forth in the opposite
        // rhythm (script: `for i in 16..=64 { move nanoarm to x
        // [-1]*i }` around each stroke).
        self.leg_timer -= _ctx.dt;
        if self.leg_timer <= 0.0 {
            self.leg_timer = SHUTTLE_DEPTH / SHUTTLE_SPEED;
            self.out = !self.out;
            let target = if self.out { -SHUTTLE_DEPTH } else { 0.0 };
            rig.move_to("nanomover", Axis::Z, target, SHUTTLE_SPEED);
        }

        self.arm_timer -= _ctx.dt;
        if self.arm_timer <= 0.0 {
            self.arm_timer = (ARM_FAR - ARM_NEAR) / ARM_SPEED;
            self.arm_out = !self.arm_out;
            let target = if self.arm_out { -ARM_FAR } else { -ARM_NEAR };
            rig.move_to("nanoarm", Axis::X, target, ARM_SPEED);
        }

        // EmitFX(): `if (doEmit) emit-sfx 1025 from nanoemitter` —
        // burst while the head is parked at the far end (doEmit in the
        // script is set at the bottom of each stroke).
        if self.out {
            rig.emit("nanoemitter", 1025);
        }
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): show the arm assembly and start both loops.
        self.active = true;
        self.out = true;
        self.arm_out = true;
        self.leg_timer = SHUTTLE_DEPTH / SHUTTLE_SPEED;
        self.arm_timer = (ARM_FAR - ARM_NEAR) / ARM_SPEED;
        rig.show("nanoarm");
        rig.show("nanomover");
        rig.move_to("nanomover", Axis::Z, -SHUTTLE_DEPTH, SHUTTLE_SPEED);
        rig.move_to("nanoarm", Axis::X, -ARM_FAR, ARM_SPEED);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): hide the arm and park everything.
        self.active = false;
        rig.hide("nanoarm");
        rig.hide("nanomover");
        rig.move_to("nanoarm", Axis::X, 0.0, 64.0);
        rig.move_to("nanomover", Axis::Z, 0.0, 100.0);
    }
}
