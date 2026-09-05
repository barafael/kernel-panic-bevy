//! signal.bos — the Network scout. Deploys from a nose-down spawn pose:
//! the base swings level ~2.5s after creation, then the four wings
//! unfold. Dies scattering its wings and dropping any loaded bomb.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct SignalAnim {
    /// Seconds since spawn (stages the deploy timeline).
    age: f32,
    stage: u8,
    /// Bomb still aboard (dropped on death or on firing).
    loaded: bool,
}

impl UnitAnim for SignalAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn base to x <-90> now (spawned nose-down);
        // loaded=1.
        rig.turn_deg("base", Axis::X, -90.0, 0.0);
        self.loaded = true;
        self.age = 0.0;
        self.stage = 0;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.age += ctx.dt;
        match self.stage {
            0 if self.age >= 2.5 => {
                // Create(): turn base to x 0 speed <60> after sleep 2500.
                self.stage = 1;
                rig.turn_deg("base", Axis::X, 0.0, 60.0);
            }
            1 if self.age >= 3.0 => {
                // Create(): unfold the four wings at ~3.5s.
                self.stage = 2;
                rig.move_to("wingul", Axis::X, -8.0, 8.0);
                rig.move_to("wingul", Axis::Y, 4.0, 4.0);
                rig.move_to("wingur", Axis::X, 8.0, 8.0);
                rig.move_to("wingur", Axis::Y, 4.0, 4.0);
                rig.move_to("wingbl", Axis::X, -8.0, 8.0);
                rig.move_to("wingbl", Axis::Y, -4.0, 4.0);
                rig.move_to("wingbr", Axis::X, 8.0, 8.0);
                rig.move_to("wingbr", Axis::Y, -4.0, 4.0);
            }
            _ => {}
        }
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): loaded=0; hide bomb.
        self.loaded = false;
        rig.hide("bomb");
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): wings FALL off; if loaded, drop the bomb.
        for wing in ["wingul", "wingur", "wingbl", "wingbr"] {
            rig.explode(wing, 3);
            rig.hide(wing);
        }
        if self.loaded {
            rig.emit("bomb", 2048);
            rig.hide("bomb");
        }
    }
}
