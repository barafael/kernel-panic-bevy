//! dos.bos — the stunner. A `dot` spins inside the ring while moving
//! (with ground sparks); aiming tilts the `slash` blade and slides the
//! `center` hub forward, resetting a few seconds after the target dies.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim, deg2rad};

/// dos.bos ResetAim(): `sleep 3000` before relaxing the pose.
const AIM_RESET_DELAY: f32 = 3.0;

/// Where dos.bos `ResetAim()` is: started by a completed `AimWeapon1`,
/// killed (`signal SIG_AIM`) by the next one.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Reset {
    #[default]
    Idle,
    /// `sleep 3000`, seconds elapsed.
    Sleeping(f32),
    /// `turn slash to y-axis 0 speed <270>; wait-for-turn slash around
    /// y-axis`, then x home, center home, `aimed=0`.
    Unwinding,
}

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
    /// The script's `ResetAim()` thread.
    reset: Reset,
    /// The script's `aimed` static: set once the first aim's x-axis
    /// tilt has landed, cleared when `ResetAim()` finishes.
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

        match self.reset {
            Reset::Idle => {}
            Reset::Sleeping(t) if t + ctx.dt < AIM_RESET_DELAY => {
                self.reset = Reset::Sleeping(t + ctx.dt);
            }
            Reset::Sleeping(_) => {
                // ResetAim(): turn slash to y-axis 0 speed <270>.
                rig.turn_deg(self.pieces.slash, Axis::Y, 0.0, 270.0);
                self.reset = Reset::Unwinding;
            }
            Reset::Unwinding => {
                // wait-for-turn slash around y-axis; then x and the hub
                // home, aimed=0.
                if rig.at_target(self.pieces.slash, Axis::Y) {
                    rig.turn_deg(self.pieces.slash, Axis::X, 0.0, 180.0);
                    rig.move_to(self.pieces.center, Axis::Z, 0.0, 16.0);
                    self.aimed = false;
                    self.reset = Reset::Idle;
                }
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
        // AimWeapon1(h,p): its `signal SIG_AIM` kills a pending ResetAim().
        self.reset = Reset::Idle;
        // slash x to (<-67>-p) @<180>; center z to [8] @16.
        rig.turn_deg(self.pieces.slash, Axis::X, -67.0 - p.to_degrees(), 180.0);
        rig.move_to(self.pieces.center, Axis::Z, 8.0, 16.0);
        // `if (!aimed) wait-for-turn slash around x-axis; aimed=1;` — the
        // first aim tilts the blade up before it swings round.
        if !self.aimed {
            if !rig.turn_settled(self.pieces.slash, Axis::X) {
                return false;
            }
            self.aimed = true;
        }
        // slash y to h @<270>; wait for both turns; start ResetAim();
        // return 1.
        rig.turn_rad(self.pieces.slash, Axis::Y, h, deg2rad(270.0));
        let ready = rig.turn_settled(self.pieces.slash, Axis::Y)
            && rig.turn_settled(self.pieces.slash, Axis::X);
        if ready {
            self.reset = Reset::Sleeping(0.0);
        }
        ready
    }
}
