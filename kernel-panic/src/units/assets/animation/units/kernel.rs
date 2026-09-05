//! kernel.cob — the System homebase, translated from the compiled
//! bytecode. Kernel compiles with Scriptor linear constant 163840, so
//! every `.bos` bracket is 2.5× the bytecode value: elmos below are
//! raw/65536 straight from the disassembly. Four pillar assemblies sink
//! into the pad on creation and rise staged; Activate/Deactivate
//! (production on/off) extend or retract them staggered; while
//! producing the tips spray build sparks.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Stagger between per-pillar pose changes in Activate/Deactivate (the
/// script staggers with `sleep 200`; its per-pillar choreography runs
/// concurrently from there).
const PILLAR_STAGGER: f32 = 0.2;
/// StartBuilding(): emit burst per `sleep 60` (throttled 2×).
const BUILD_EMIT_INTERVAL: f32 = 0.12;

#[derive(Default)]
pub struct KernelAnim {
    /// Create's staged rise (stage 0 = bases rising, 1 = pillars rising,
    /// 4 = done).
    stage: usize,
    stage_timer: f32,
    build_emit_timer: f32,
}

impl KernelAnim {
    /// GetPillarNReady(): pillar x → 30° @15°/s, head x → 35° @15°/s,
    /// head y → 0 @15, pillar y → 0 @40 elmos/s.
    fn pillar_ready(&self, rig: &mut AnimRig, i: usize) {
        let pillar = format!("pillar{i}");
        let head = format!("head{i}");
        rig.turn_deg(&pillar, Axis::X, 30.0, 15.0);
        rig.turn_deg(&head, Axis::X, 35.0, 15.0);
        rig.move_to(&head, Axis::Y, 0.0, 15.0);
        rig.move_to(&pillar, Axis::Y, 0.0, 40.0);
    }

    /// GetPillarNRest(): pillar y → −15 @2.5, head y → −12.5 @2.5,
    /// pillar x → 0 @5°/s, head x → 5° @20°/s (bytecode −983040 /
    /// −819200 at speed 163840 = 2.5 elmos/s — a slow, deliberate
    /// retract).
    fn pillar_rest(&self, rig: &mut AnimRig, i: usize) {
        let pillar = format!("pillar{i}");
        let head = format!("head{i}");
        rig.turn_deg(&pillar, Axis::X, 0.0, 5.0);
        rig.turn_deg(&head, Axis::X, 5.0, 20.0);
        rig.move_to(&pillar, Axis::Y, -15.0, 2.5);
        rig.move_to(&head, Axis::Y, -12.5, 2.5);
    }
}

impl UnitAnim for KernelAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): fan the pillars out (8190=45°, 24570=135°), sink
        // bases −20 / pillars −80 / heads −80 (bytecode −1310720 /
        // −5242880), rise the bases @30, then the pillars and heads
        // staged after 0.4s: pillars → −40 @60, heads → −30 @40.
        for (i, yaw) in [45.0, 135.0, -45.0, -135.0].iter().enumerate() {
            rig.turn_deg(&format!("pillar{i}"), Axis::Y, *yaw, 0.0);
        }
        for i in 0..4 {
            rig.move_to(&format!("base{i}"), Axis::Y, -20.0, 0.0);
            rig.move_to(&format!("pillar{i}"), Axis::Y, -80.0, 0.0);
            rig.move_to(&format!("head{i}"), Axis::Y, -80.0, 0.0);
        }
        for i in 0..4 {
            rig.move_to(&format!("base{i}"), Axis::Y, 0.0, 30.0);
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
                    rig.move_to(&format!("pillar{i}"), Axis::Y, -40.0, 60.0);
                    rig.move_to(&format!("head{i}"), Axis::Y, -30.0, 40.0);
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
