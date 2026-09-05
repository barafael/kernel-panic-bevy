//! packet.bos — the Network's main combat unit. A plain turret: yaw
//! toward the heading fast, fire, die in shards.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

#[derive(Default)]
pub struct PacketAnim;

impl UnitAnim for PacketAnim {
    fn aim(&mut self, rig: &mut AnimRig, h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turn turret to y-axis h speed <720>; the
        // muzzle cycles via ResetAim in the script — a single `gp` piece
        // here, so no cycling needed.
        rig.turn_rad("turret", Axis::Y, h, 720.0 * super::super::DEG2RAD);
        true
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER + turret FALL; hide both.
        rig.explode("body", 4);
        rig.explode("turret", 3);
        rig.hide("body");
        rig.hide("turret");
    }
}
