//! byte.bos — the heavy defensive rotor. Deploying lifts the base,
//! spreads the four blades and spins the rotor; the whole `aimer`
//! assembly (rotor + blades + barrels) swings toward the target while
//! the body stays put. Folds back up ~3s after the last aim.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};
use std::f32::consts::TAU;

/// byte.bos Close(): `sleep 3000` before folding after the last aim.
const IDLE_CLOSE_DELAY: f32 = 3.0;
/// AimWeapon1 opens before it can aim; give the choreography ~1.5s
/// (base lift + blade spread) before declaring the gun ready.
const OPEN_READY_DELAY: f32 = 1.5;

fn rad2deg(r: f32) -> f32 {
    r * 360.0 / TAU
}

#[derive(Default)]
pub struct ByteAnim {
    /// isOpen mirror.
    open: bool,
    /// Seconds since the open choreography started.
    open_timer: f32,
    /// Seconds since the last aim request.
    since_aim: f32,
}

impl ByteAnim {
    fn open_choreography(&mut self, rig: &mut AnimRig) {
        // Open(): base lifts [24] @48, rotor pre-turns <45>, blades
        // spread [±4] @16, rotor spins.
        rig.move_to("base", Axis::Y, 24.0, 48.0);
        rig.turn_deg("rotor", Axis::Y, 45.0, 90.0);
        rig.move_to("blade0", Axis::Z, 4.0, 16.0);
        rig.move_to("blade1", Axis::X, 4.0, 16.0);
        rig.move_to("blade2", Axis::Z, -4.0, 16.0);
        rig.move_to("blade3", Axis::X, -4.0, 16.0);
        rig.spin_dps("rotor", Axis::Y, 180.0);
    }

    fn close_choreography(&mut self, rig: &mut AnimRig) {
        // Close(): aimer relaxes, rotor stops and re-centers, blades
        // fold, base lowers.
        rig.turn_deg("aimer", Axis::X, 0.0, 70.0);
        rig.turn_deg("aimer", Axis::Y, 0.0, 70.0);
        rig.stop_spin("rotor", Axis::Y);
        rig.turn_deg("rotor", Axis::Y, 0.0, 480.0);
        rig.move_to("blade0", Axis::Z, 0.0, 16.0);
        rig.move_to("blade1", Axis::X, 0.0, 16.0);
        rig.move_to("blade2", Axis::Z, 0.0, 16.0);
        rig.move_to("blade3", Axis::X, 0.0, 16.0);
        rig.move_to("base", Axis::Y, 0.0, 120.0);
    }
}

impl UnitAnim for ByteAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): barrels pitch up <90>, launcher arm raised, five
        // mine tubes fanned out.
        for bp in ["bp0", "bp1", "bp2", "bp3"] {
            rig.turn_deg(bp, Axis::X, 90.0, 0.0);
        }
        rig.move_to("launcher_arm", Axis::Y, 60.0, 0.0);
        for (name, yaw) in [
            ("launcher1", -18.0),
            ("launcher2", 9.0),
            ("launcher3", 0.0),
            ("launcher4", 9.0),
            ("launcher5", 18.0),
        ] {
            rig.turn_deg(name, Axis::X, 30.0, 0.0);
            rig.turn_deg(name, Axis::Y, yaw, 0.0);
        }
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-16]·pct/100 while building.
        super::emerge_lift(rig, "base", 16.0, ctx.build_percent);

        if self.open {
            self.open_timer += ctx.dt;
            self.since_aim += ctx.dt;
            if self.since_aim > IDLE_CLOSE_DELAY && !ctx.aim_active {
                self.open = false;
                self.close_choreography(rig);
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        self.since_aim = 0.0;
        if !self.open {
            // AimWeapon1: `if (!isOpen) { start-script Open(); return 0; }`
            self.open = true;
            self.open_timer = 0.0;
            self.open_choreography(rig);
            return false;
        }
        // AimWeapon1: aimer x to (<-90>-p) @<270>, y to h @<270>.
        rig.turn_deg("aimer", Axis::X, -90.0 - rad2deg(p), 270.0);
        rig.turn_rad("aimer", Axis::Y, h, 270.0 * super::super::DEG2RAD);
        self.open_timer >= OPEN_READY_DELAY
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit from bp{gp}, then cycle gp 0→1→2→3→0.
        let idx = self_cycle(&mut rig.muzzle, 4);
        if let Some(piece) = rig.piece(format!("bp{idx}").as_str()) {
            rig.outbox.push(super::super::FxEvent::Emit { piece, sfx: 1024 });
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide + explode blade0..3 SHATTER.
        for blade in ["blade0", "blade1", "blade2", "blade3"] {
            rig.hide(blade);
            rig.explode(blade, 4);
        }
    }
}

fn self_cycle(value: &mut usize, modulus: usize) -> usize {
    let current = *value % modulus;
    *value = (current + 1) % modulus;
    current
}
