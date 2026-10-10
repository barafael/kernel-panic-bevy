//! gateway.bos — the Network mobile builder. While building, its
//! `center` emitter steps around the compass (0/90/180/270°) spraying
//! build sparks from each facing, and every other 250 ms step draws the
//! white `lua_BuildArc` strand to the unit it is building.

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
    /// `static-var doing`: set by StartBuilding(), flipped after every
    /// `sleep 250` of BuildFX(); the strand is drawn when it was set.
    doing: bool,
    /// BuildFX() has slept at least once since StartBuilding(), so the
    /// next step is preceded by its `if (doing) lua_BuildArc` check.
    slept: bool,
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
                // `sleep 250; if (doing) call-script lua_BuildArc(center);
                // doing = !doing;` — at the end of each loop pass, i.e.
                // before the next pass's emits.
                if self.building && self.slept {
                    if self.doing {
                        rig.build_arc(self.pieces.center);
                    }
                    self.doing = !self.doing;
                }
                self.slept = true;
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
        // StartBuilding(h, p): building = 1; doing = 1; BuildFX() (whose
        // `signal SIG_BUILDFX` restarts a running loop); INBUILDSTANCE = 1.
        self.building = true;
        self.doing = true;
        self.slept = false;
        self.step_timer = 0.0;
    }

    fn stop_building(&mut self, _rig: &mut AnimRig) {
        // StopBuilding(): building = 0; doing = 0; INBUILDSTANCE = 0.
        self.building = false;
        self.doing = false;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::assets::animation::FxEvent;

    fn arcs(rig: &mut AnimRig) -> usize {
        rig.outbox
            .drain(..)
            .filter(|e| matches!(e, FxEvent::BuildArc { .. }))
            .count()
    }

    /// gateway.bos BuildFX(): the first pass only sprays; after each
    /// `sleep 250` the strand is drawn when `doing` is set, and `doing`
    /// flips — so 250, 750, 1250 ms… after StartBuilding, never once
    /// StopBuilding() has cleared it.
    #[test]
    fn build_arc_every_other_step_while_building() {
        let mut rig = AnimRig::for_test(&["body", "center"]);
        let mut anim = GatewayAnim::default();
        anim.bind(&rig);
        let ctx = AnimCtx {
            dt: STEP_INTERVAL,
            ..AnimCtx::minimal()
        };
        // Not building: no strand however long it idles.
        for _ in 0..4 {
            anim.update(&mut rig, ctx);
        }
        assert_eq!(arcs(&mut rig), 0);

        anim.start_building(&mut rig, 0.0, 0.0);
        let mut drawn = Vec::new();
        for _ in 0..6 {
            anim.update(&mut rig, ctx);
            drawn.push(arcs(&mut rig));
        }
        assert_eq!(drawn, vec![0, 1, 0, 1, 0, 1]);

        anim.stop_building(&mut rig);
        for _ in 0..4 {
            anim.update(&mut rig, ctx);
        }
        assert_eq!(arcs(&mut rig), 0);
    }
}
