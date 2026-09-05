//! obelisk.bos — the Hacker infection-gas artillery. Four segments
//! part when aiming, snap back while idle, and the tip smoulders
//! between the 40-second reloads.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// obelisk.bos FireWeapon1(): `sleep 40000` reload.
const RELOAD_SECS: f32 = 40.0;

#[derive(Default)]
pub struct ObeliskAnim {
    /// Mirrors the script's `reloading` static.
    reloading: bool,
    reload_timer: f32,
    /// Idle smoulder emit timer.
    charge_timer: f32,
    /// Create() splays the four segments ±16 elmos and closes them once
    /// the emerge completes (`move ... speed [8]` after the build loop).
    post_emerge_done: bool,
}

impl UnitAnim for ObeliskAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): segs splayed out ±16 elmos (bytecode ±1048576)
        // while the obelisk builds.
        rig.move_to("segf", Axis::Z, 16.0, 0.0);
        rig.move_to("segb", Axis::Z, -16.0, 0.0);
        rig.move_to("segr", Axis::X, 16.0, 0.0);
        rig.move_to("segl", Axis::X, -16.0, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if !self.post_emerge_done && !ctx.emerging {
            // Create(), post-build: segments close to rest @8 elmos/s.
            self.post_emerge_done = true;
            rig.move_to("segf", Axis::Z, 0.0, 8.0);
            rig.move_to("segb", Axis::Z, 0.0, 8.0);
            rig.move_to("segr", Axis::X, 0.0, 8.0);
            rig.move_to("segl", Axis::X, 0.0, 8.0);
        }

        if self.reloading {
            self.reload_timer -= ctx.dt;
            if self.reload_timer <= 0.0 {
                // ResetAim()/ChargeFX(): segments home, tip smoulders.
                self.reloading = false;
                rig.move_to("segf", Axis::Z, 0.0, 8.0);
                rig.move_to("segb", Axis::Z, 0.0, 8.0);
                rig.move_to("segr", Axis::X, 0.0, 8.0);
                rig.move_to("segl", Axis::X, 0.0, 8.0);
            }
        } else {
            // ChargeFX(): emit-sfx 1024 from tip while !reloading
            // (script: every 50ms; throttled here for particle budget).
            self.charge_timer -= ctx.dt;
            if self.charge_timer <= 0.0 {
                self.charge_timer = 0.25;
                rig.emit("tip", 1024);
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, _h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1: `if(reloading) return 0`; otherwise part the four
        // segments and return 1 once segf arrives.
        if self.reloading {
            return false;
        }
        rig.move_to("segf", Axis::Z, 8.0, 8.0);
        rig.move_to("segb", Axis::Z, -8.0, 8.0);
        rig.move_to("segr", Axis::X, 8.0, 8.0);
        rig.move_to("segl", Axis::X, -8.0, 8.0);
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): reloading=1; sleep 40000.
        self.reloading = true;
        self.reload_timer = RELOAD_SECS;
        let _ = rig;
    }
}
