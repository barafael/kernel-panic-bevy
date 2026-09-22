//! dos.bos — the stunner. A `dot` spins inside the ring while moving
//! (with ground sparks); aiming tilts the `slash` blade and slides the
//! `center` hub forward, resetting a few seconds after the target dies.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim, deg2rad};

/// dos.bos AimWeapon1 aims over ~0.35s of waits before returning 1.
const AIM_SETTLE: f32 = 0.35;
/// dos.bos ResetAim(): `sleep 3000` before relaxing the pose.
const AIM_RESET_DELAY: f32 = 3.0;

#[derive(Clone, Copy, Default)]
struct DosPieces {
    center: usize,
    slash: usize,
    dot: usize,
    ground: usize,
}

impl DosPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            center: rig.bind_piece("center"),
            slash: rig.bind_piece("slash"),
            dot: rig.bind_piece("dot"),
            ground: rig.bind_piece("ground"),
        }
    }
}

#[derive(Default)]
pub struct DosAnim {
    pieces: DosPieces,
    /// Seconds since the last aim request (drives ResetAim).
    since_aim: f32,
    /// Seconds since aim started (fire gate).
    since_aim_start: f32,
    /// True while the slash/center pose is extended.
    aimed: bool,
    /// Ground-spark timer while moving.
    spark_timer: f32,
}

impl UnitAnim for DosAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = DosPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn ground to y-axis <180> now
        rig.turn_deg(self.pieces.ground, Axis::Y, 180.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if ctx.moving {
            // StartMoving(): emit-sfx 1024 from ground every `sleep 120`.
            self.spark_timer -= ctx.dt;
            if self.spark_timer <= 0.0 {
                self.spark_timer = 0.12;
                rig.emit(self.pieces.ground, SfxKind::Puff);
            }
        } else {
            self.spark_timer = 0.0;
        }

        if self.aimed {
            self.since_aim += ctx.dt;
            self.since_aim_start += ctx.dt;
            if self.since_aim > AIM_RESET_DELAY {
                // ResetAim(): relax the pose.
                self.aimed = false;
                rig.turn_deg(self.pieces.slash, Axis::Y, 0.0, 270.0);
                rig.turn_deg(self.pieces.slash, Axis::X, 0.0, 180.0);
                rig.move_to(self.pieces.center, Axis::Z, 0.0, 16.0);
            }
        }
    }

    fn start_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): spin dot around x-axis speed <360>
        rig.spin_dps(self.pieces.dot, Axis::X, 360.0);
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): stop-spin dot
        rig.stop_spin(self.pieces.dot, Axis::X);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): slash x to (<-67>-p) @<180>; center z to [8]
        // @16; slash y to h @<270>.
        rig.turn_deg(self.pieces.slash, Axis::X, -67.0 - p.to_degrees(), 180.0);
        rig.move_to(self.pieces.center, Axis::Z, 8.0, 16.0);
        rig.turn_rad(self.pieces.slash, Axis::Y, h, deg2rad(270.0));

        if !self.aimed {
            self.aimed = true;
            self.since_aim = 0.0;
            self.since_aim_start = 0.0;
        } else {
            // Retarget: keep the settle window honest.
            self.since_aim = 0.0;
        }
        self.since_aim_start >= AIM_SETTLE
    }
}
