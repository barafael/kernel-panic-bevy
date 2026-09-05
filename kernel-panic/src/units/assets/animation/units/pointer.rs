//! pointer.bos — deployable artillery cube. The shell (`body`) rolls
//! while moving; deploying (`Open`) splits the side plates, extends the
//! gun and exposes the muzzle; the gunbase carries the pitch when aiming.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};
use crate::units::combat::DeployState;
use std::f32::consts::TAU;

/// Radians↔degrees helper for host-computed aim pitches.
fn rad2deg(r: f32) -> f32 {
    r * 360.0 / TAU
}

/// pointer.bos StartMoving(): `spin body around x-axis speed <180>`
const ROLL_DPS: f32 = 180.0;

#[derive(Default)]
pub struct PointerAnim {
    /// Last deploy state seen — choreography fires on transitions.
    last_state: Option<DeployState>,
}

impl PointerAnim {
    fn open_choreography(&mut self, rig: &mut AnimRig) {
        // Open(): show gun; move left to x [10] @20, right to x [-10] @20,
        // gun to y [20] @20. (The script staggers these with
        // wait-for-move; running them in parallel reads the same.)
        rig.show("gun");
        rig.move_to("left", Axis::X, 10.0, 20.0);
        rig.move_to("right", Axis::X, -10.0, 20.0);
        rig.move_to("gun", Axis::Y, 20.0, 20.0);
    }

    fn close_choreography(&mut self, rig: &mut AnimRig) {
        // Close(): gunbase back to rest, gun retracts, plates close,
        // hide gun.
        rig.turn_deg("gunbase", Axis::X, 90.0, 50.0);
        rig.turn_deg("gunbase", Axis::Y, 0.0, 50.0);
        rig.move_to("gun", Axis::Y, 0.0, 20.0);
        rig.move_to("left", Axis::X, 0.0, 20.0);
        rig.move_to("right", Axis::X, 0.0, 20.0);
        rig.hide("gun");
    }
}

impl UnitAnim for PointerAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide gun; turn gunpoint to x <-90> now; gunbase to
        // x <90> now. Stowed pose until the first Open.
        rig.hide("gun");
        rig.turn_deg("gunpoint", Axis::X, -90.0, 0.0);
        rig.turn_deg("gunbase", Axis::X, 90.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-32]·pct/100 while building.
        super::emerge_lift(rig, "base", 32.0, ctx.build_percent);

        // React to deploy transitions (the host Deployable state machine
        // mirrors the script's Open/Close cycle).
        if ctx.deploy != self.last_state {
            match (self.last_state, ctx.deploy) {
                (_, Some(DeployState::Opening)) | (_, Some(DeployState::Open)) => {
                    self.open_choreography(rig);
                }
                (_, Some(DeployState::Closing)) | (_, Some(DeployState::Closed)) => {
                    self.close_choreography(rig);
                }
                _ => {}
            }
            self.last_state = ctx.deploy;
        }
    }

    fn start_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): Close(), then spin body around x-axis <180>.
        rig.spin_dps("body", Axis::X, ROLL_DPS);
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): turn body to x 0 now; stop-spin; Open().
        rig.stop_spin("body", Axis::X);
        rig.turn_deg("body", Axis::X, 0.0, 0.0);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, ctx: AnimCtx) -> bool {
        // AimWeapon1 returns 0 unless open. When open, the gunbase
        // elevates to (<90>-p) — heading stays on the body (host-driven).
        if ctx.deploy != Some(DeployState::Open) {
            return false;
        }
        rig.turn_deg("gunbase", Axis::X, 90.0 - rad2deg(p), 50.0);
        let _ = h;
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit-sfx 1024 from gunpoint
        rig.emit("gunpoint", 1024);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide left/right/gun; explode gun FALL, plates SHATTER.
        rig.hide("left");
        rig.hide("right");
        rig.hide("gun");
        rig.explode("gun", 3);
        rig.explode("left", 4);
        rig.explode("right", 4);
    }
}
