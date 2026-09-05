//! kernel.bos — the System homebase. Four pillar assemblies rise out of
//! the pad on creation; Activate/Deactivate (production on/off) extend
//! or retract them in a stagger; while producing the tips spray build
//! sparks.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Stagger between per-pillar pose changes in Activate/Deactivate.
const PILLAR_STAGGER: f32 = 0.2;
/// StartBuilding(): emit burst per `sleep 60` (throttled 2×).
const BUILD_EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct KernelAnim {
    /// Activate/Deactivate choreography stage index (0..=4, 4 = done).
    stage: usize,
    stage_timer: f32,
    build_emit_timer: f32,
}

impl KernelAnim {
    fn pillar_ready(&self, rig: &mut AnimRig, i: usize) {
        // GetPillarNReady(): pillar x <30>, head x <35>, both lift to 0.
        let pillar = format!("pillar{i}");
        let head = format!("head{i}");
        rig.turn_deg(&pillar, Axis::X, 30.0, 30.0);
        rig.turn_deg(&head, Axis::X, 35.0, 30.0);
        rig.move_to(&pillar, Axis::Y, 0.0, 16.0);
        rig.move_to(&head, Axis::Y, 0.0, 16.0);
    }

    fn pillar_rest(&self, rig: &mut AnimRig, i: usize) {
        // GetPillarNRest(): retract to y [-6]/[-5] and straighten.
        let pillar = format!("pillar{i}");
        let head = format!("head{i}");
        rig.turn_deg(&pillar, Axis::X, 0.0, 20.0);
        rig.turn_deg(&head, Axis::X, 5.0, 20.0);
        rig.move_to(&pillar, Axis::Y, -6.0, 8.0);
        rig.move_to(&head, Axis::Y, -5.0, 6.0);
    }
}

impl UnitAnim for KernelAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): fan the pillars out, sink everything, then rise in
        // two stages (bases first, then pillars/heads).
        for (i, yaw) in [45.0, 135.0, -45.0, -135.0].iter().enumerate() {
            rig.turn_deg(&format!("pillar{i}"), Axis::Y, *yaw, 0.0);
        }
        for i in 0..4 {
            rig.move_to(&format!("base{i}"), Axis::Y, -8.0, 0.0);
            rig.move_to(&format!("pillar{i}"), Axis::Y, -32.0, 0.0);
            rig.move_to(&format!("head{i}"), Axis::Y, -32.0, 0.0);
        }
        for i in 0..4 {
            rig.move_to(&format!("base{i}"), Axis::Y, 0.0, 12.0);
        }
        self.stage = 0;
        self.stage_timer = 0.4;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s staged rise: after ~0.4s the pillars come up.
        if self.stage == 0 {
            self.stage_timer -= ctx.dt;
            if self.stage_timer <= 0.0 {
                self.stage = 4; // rise done; stage 4 = "open & idle"
                for i in 0..4 {
                    rig.move_to(&format!("pillar{i}"), Axis::Y, -16.0, 24.0);
                    rig.move_to(&format!("head{i}"), Axis::Y, -12.0, 16.0);
                }
            }
        }

        // Activate()/Deactivate() staging, one pillar per tick.
        if self.stage > 0 && self.stage < 4 {
            self.stage_timer -= ctx.dt;
            if self.stage_timer <= 0.0 {
                let i = self.stage - 1;
                if ctx.producing {
                    self.pillar_ready(rig, i);
                } else {
                    self.pillar_rest(rig, i);
                }
                self.stage += 1;
                self.stage_timer = PILLAR_STAGGER;
            }
        }

        // StartBuilding(): spray from all four tips while producing.
        if ctx.producing {
            self.build_emit_timer -= ctx.dt;
            if self.build_emit_timer <= 0.0 {
                self.build_emit_timer = BUILD_EMIT_INTERVAL;
                for i in 0..4 {
                    rig.emit(&format!("tip{i}"), 2048);
                }
            }
        }
    }

    fn activate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): start the staged pillar-ready choreography.
        self.stage = 1;
        self.stage_timer = 0.0;
    }

    fn deactivate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): staged pillar-rest choreography.
        self.stage = 1;
        self.stage_timer = 0.0;
    }
}
