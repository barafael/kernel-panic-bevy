//! bit.bos — the System swarm unit. A shell (`body`) that rolls around
//! its x-axis while driving; the gun aims instantly (`turn ... now`).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct BitAnim;

impl UnitAnim for BitAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): move gunpoint to z-axis now — bytecode -491520 =
        // -7.5 elmos (bit compiles at linear constant 163840, so the
        // .bos's [-3] is 7.5 elmos). Pulls the muzzle flush with the
        // shell surface.
        rig.move_to("gunpoint", Axis::Z, -7.5, 0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-16]·pct/100 = bytecode
        // -40 elmos while building.
        if ctx.emerging {
            super::emerge_lift(rig, "base", 40.0, ctx.build_percent);
        }
    }

    fn start_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): spin body around x-axis speed <270>
        rig.spin_dps("body", Axis::X, 270.0);
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): stop-spin; snap the seam back upright.
        rig.stop_spin("body", Axis::X);
        rig.turn_deg("body", Axis::X, 0.0, 0.0);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turn gunbase to y-axis h now; x-axis (0-p) now.
        rig.turn_rad("gunbase", Axis::Y, h, 0.0);
        rig.turn_rad("gunbase", Axis::X, -p, 0.0);
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit-sfx 1025 from gunpoint
        rig.emit("gunpoint", 1025);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide body/shell, explode body SHATTER
        rig.hide("body");
        rig.hide("shell");
        rig.explode("body", 4);
    }
}
