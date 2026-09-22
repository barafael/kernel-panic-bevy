//! pointer.bos — deployable artillery cube. The shell (`body`) rolls
//! while moving; deploying (`Open`) splits the side plates, extends the
//! gun and exposes the muzzle; the gunbase carries the pitch when aiming.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::DeathFx;
use crate::units::combat::DeployState;

/// pointer.bos StartMoving(): `spin body around x-axis speed <180>`
const ROLL_DPS: f32 = 180.0;

#[derive(Clone, Copy, Default)]
struct PointerPieces {
    base: usize,
    body: usize,
    left: usize,
    right: usize,
    gun: usize,
    gunbase: usize,
    gunpoint: usize,
}

impl PointerPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            body: rig.bind_piece("body"),
            left: rig.bind_piece("left"),
            right: rig.bind_piece("right"),
            gun: rig.bind_piece("gun"),
            gunbase: rig.bind_piece("gunbase"),
            gunpoint: rig.bind_piece("gunpoint"),
        }
    }
}

#[derive(Default)]
pub struct PointerAnim {
    pieces: PointerPieces,
    /// Last deploy state seen — choreography fires on transitions.
    last_state: Option<DeployState>,
    death: DeathFx,
}

impl PointerAnim {
    fn open_choreography(&mut self, rig: &mut AnimRig) {
        // Open(): show gun; move left to x [10] @20, right to x [-10] @20,
        // gun to y [20] @20. (The script staggers these with
        // wait-for-move; running them in parallel reads the same.)
        rig.show(self.pieces.gun);
        rig.move_to(self.pieces.left, Axis::X, 10.0, 20.0);
        rig.move_to(self.pieces.right, Axis::X, -10.0, 20.0);
        rig.move_to(self.pieces.gun, Axis::Y, 20.0, 20.0);
    }

    fn close_choreography(&mut self, rig: &mut AnimRig) {
        // Close(): gunbase back to rest, gun retracts, plates close,
        // hide gun.
        rig.turn_deg(self.pieces.gunbase, Axis::X, 90.0, 50.0);
        rig.turn_deg(self.pieces.gunbase, Axis::Y, 0.0, 50.0);
        rig.move_to(self.pieces.gun, Axis::Y, 0.0, 20.0);
        rig.move_to(self.pieces.left, Axis::X, 0.0, 20.0);
        rig.move_to(self.pieces.right, Axis::X, 0.0, 20.0);
        rig.hide(self.pieces.gun);
    }
}

impl UnitAnim for PointerAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = PointerPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide gun; turn gunpoint to x <-90> now; gunbase to
        // x <90> now. Stowed pose until the first Open.
        rig.hide(self.pieces.gun);
        rig.turn_deg(self.pieces.gunpoint, Axis::X, -90.0, 0.0);
        rig.turn_deg(self.pieces.gunbase, Axis::X, 90.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-32]·pct/100 while building.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 32.0, ctx.build_percent);
        }
        self.death.tick(ctx.dt);

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
        rig.spin_dps(self.pieces.body, Axis::X, ROLL_DPS);
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): turn body to x 0 now; stop-spin; Open().
        rig.stop_spin(self.pieces.body, Axis::X);
        rig.turn_deg(self.pieces.body, Axis::X, 0.0, 0.0);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, ctx: AnimCtx) -> bool {
        // AimWeapon1 returns 0 unless open. When open, the gunbase
        // elevates to (<90>-p) — heading stays on the body (host-driven).
        if ctx.deploy != Some(DeployState::Open) {
            return false;
        }
        rig.turn_deg(self.pieces.gunbase, Axis::X, 90.0 - p.to_degrees(), 50.0);
        let _ = h;
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit-sfx 1024 from gunpoint
        rig.emit(self.pieces.gunpoint, SfxKind::Puff);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide left/right/gun; explode gun FALL, plates SHATTER.
        rig.hide(self.pieces.left);
        rig.hide(self.pieces.right);
        rig.hide(self.pieces.gun);
        rig.explode(self.pieces.gun, 3);
        rig.explode(self.pieces.left, 4);
        rig.explode(self.pieces.right, 4);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.last_state == Some(DeployState::Open))
    }
}
