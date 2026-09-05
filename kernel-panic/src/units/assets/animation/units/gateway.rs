//! gateway.bos — the Network mobile builder. While producing, its
//! `center` emitter steps around the compass (0/90/180/270°) spraying
//! build sparks from each facing.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// BuildFX(): one compass step + emit burst per `sleep 250`.
const STEP_INTERVAL: f32 = 0.25;

#[derive(Default)]
pub struct GatewayAnim {
    /// Current compass step 0..4 (0/90/180/270 degrees).
    step: usize,
    step_timer: f32,
}

impl UnitAnim for GatewayAnim {
    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if ctx.producing {
            self.step_timer -= ctx.dt;
            if self.step_timer <= 0.0 {
                self.step_timer = STEP_INTERVAL;
                let heading = ((self.step % 4) as f32) * 90.0;
                rig.turn_deg("center", Axis::Y, heading, 0.0);
                rig.emit("center", 1024);
                // Every other step fires the brighter 1025 burst.
                if self.step % 2 == 1 {
                    rig.emit("center", 1025);
                }
                self.step += 1;
            }
        } else {
            self.step_timer = 0.0;
            rig.turn_deg("center", Axis::Y, 0.0, 90.0);
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER; hide body.
        rig.explode("body", 4);
        rig.hide("body");
    }
}
