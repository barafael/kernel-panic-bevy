//! gateway.bos — the Network mobile builder. While producing, its
//! `center` emitter steps around the compass (0/90/180/270°) spraying
//! build sparks from each facing.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::DeathFx;

/// BuildFX(): one compass step + emit burst per `sleep 250`.
const STEP_INTERVAL: f32 = 0.25;

#[derive(Clone, Copy, Default)]
struct GatewayPieces {
    body: usize,
    center: usize,
}

impl GatewayPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            center: rig.bind_piece("center"),
        }
    }
}

#[derive(Default)]
pub struct GatewayAnim {
    pieces: GatewayPieces,
    /// Current compass step 0..4 (0/90/180/270 degrees).
    step: usize,
    step_timer: f32,
    death: DeathFx,
    /// `static-var building`: StartBuilding() .. StopBuilding().
    building: bool,
}

impl UnitAnim for GatewayAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = GatewayPieces::bind(rig);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
        if ctx.producing || self.building {
            self.step_timer -= ctx.dt;
            if self.step_timer <= 0.0 {
                self.step_timer = STEP_INTERVAL;
                let heading = ((self.step % 4) as f32) * 90.0;
                rig.turn_deg(self.pieces.center, Axis::Y, heading, 0.0);
                rig.emit(self.pieces.center, SfxKind::Puff);
                // Every other step fires the brighter 1025 burst.
                if self.step % 2 == 1 {
                    rig.emit(self.pieces.center, SfxKind::Puff);
                }
                self.step += 1;
            }
        } else {
            self.step_timer = 0.0;
            rig.turn_deg(self.pieces.center, Axis::Y, 0.0, 90.0);
        }
    }

    fn start_building(&mut self, _rig: &mut AnimRig, _heading: f32, _pitch: f32) {
        // StartBuilding(h, p): building = 1; BuildFX(); INBUILDSTANCE = 1.
        self.building = true;
    }

    fn stop_building(&mut self, _rig: &mut AnimRig) {
        // StopBuilding(): building = 0; INBUILDSTANCE = 0.
        self.building = false;
    }

    fn in_build_stance(&self) -> Option<bool> {
        Some(self.building)
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER; hide body.
        rig.explode(self.pieces.body, 4);
        rig.hide(self.pieces.body);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
