//! kernel.cob — the System homebase, translated from the compiled
//! bytecode. Kernel compiles with Scriptor linear constant 163840, so
//! every `.bos` bracket is 2.5× the bytecode value: elmos below are
//! raw/65536 straight from the disassembly.
//!
//! Lifecycle, faithful to the script:
//!
//! - `Create()` — pillars fan out (±45°/135°), everything sinks, bases
//!   rise, then pillars/heads rise partway. `canbuild` flips true here.
//! - `Activate()` (production starts) — wait for `canbuild`, then
//!   unfold the four nano-arm pillars one per 200 ms
//!   (`GetPillarNReady`): tilt out to 30°/35° and lift to 0. The base
//!   starts producing immediately, so it spawns mid-rise and unfolds
//!   right after — the "unfold at start" look.
//! - `Deactivate()` (production stops) — fold the pillars back down one
//!   per **1800 ms** (`GetPillarNRest`): a slow, leisurely retract at
//!   2.5 elmos/s.
//! - While producing, the tips spray build sparks (`StartBuilding`).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// Activate(): `sleep 200` between pillar unfolds.
const READY_STAGGER: f32 = 0.2;
/// Deactivate(): `sleep 1800` between pillar folds.
const REST_STAGGER: f32 = 1.8;
/// StartBuilding(): emit burst per `sleep 60` (throttled 2×).
const BUILD_EMIT_INTERVAL: f32 = 0.12;

/// Which staged choreography is running (mirrors which script the host
/// triggered), and the next pillar it will move.
#[derive(Clone, Copy, PartialEq)]
enum Choreo {
    Ready,
    Rest,
}

pub struct KernelAnim {
    /// Create()'s staged rise: `Some(seconds_until_pillar_rise)` while
    /// the bases are still coming up, `None` once `canbuild` is set.
    rise_wait: Option<f32>,
    /// Active Activate/Deactivate choreography, if any.
    choreo: Option<(Choreo, usize, f32)>,
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

impl Default for KernelAnim {
    fn default() -> Self {
        Self {
            rise_wait: Some(0.4),
            choreo: None,
            build_emit_timer: 0.0,
        }
    }
}

impl UnitAnim for KernelAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): fan the pillars out (8190=45°, 24570=135°), sink
        // bases −20 / pillars −80 / heads −80 (bytecode −1310720 /
        // −5242880), rise the bases @30.
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
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s staged pillar rise after the bases come up.
        if let Some(wait) = &mut self.rise_wait {
            *wait -= ctx.dt;
            if *wait <= 0.0 {
                self.rise_wait = None;
                for i in 0..4 {
                    rig.move_to(&format!("pillar{i}"), Axis::Y, -40.0, 60.0);
                    rig.move_to(&format!("head{i}"), Axis::Y, -30.0, 40.0);
                }
            }
        }

        // Activate()/Deactivate() staged choreography. Activate waits
        // for the rise (`while(!canbuild) sleep 100`) before unfolding.
        // Activate waits for the Create rise to finish (`while(!canbuild)
        // sleep 100`) before the unfold starts.
        if let Some((choreo, next_pillar, timer)) = self.choreo {
            if choreo == Choreo::Ready && self.rise_wait.is_some() {
                return;
            }
            self.choreo = None; // take ownership while firing this step
            let mut next = next_pillar;
            let mut timer = timer;
            timer -= ctx.dt;
            if timer > 0.0 {
                self.choreo = Some((choreo, next, timer));
                return;
            }
            match choreo {
                Choreo::Ready => self.pillar_ready(rig, next),
                Choreo::Rest => self.pillar_rest(rig, next),
            }
            next += 1;
            if next < 4 {
                let stagger = match choreo {
                    Choreo::Ready => READY_STAGGER,
                    Choreo::Rest => REST_STAGGER,
                };
                self.choreo = Some((choreo, next, stagger));
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
        // Activate(): staged pillar-unfold (one per 200 ms).
        self.choreo = Some((Choreo::Ready, 0, 0.0));
    }

    fn deactivate(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): staged pillar-fold (one per 1800 ms).
        self.choreo = Some((Choreo::Rest, 0, 0.0));
    }
}
