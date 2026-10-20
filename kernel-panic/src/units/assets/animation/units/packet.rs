//! packet.bos — the Network's main combat unit. A plain turret: yaw
//! toward the heading fast, fire, die in shards.

use super::super::{AnimCtx, AnimRig, Axis, FALL, SHATTER, UnitAnim, deg2rad};
use super::DeathFx;

#[derive(Clone, Copy, Default)]
struct PacketPieces {
    body: usize,
    turret: usize,
}

impl PacketPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            turret: rig.bind_piece("turret"),
        }
    }
}

#[derive(Default)]
pub struct PacketAnim {
    pieces: PacketPieces,
    death: DeathFx,
}

impl UnitAnim for PacketAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = PacketPieces::bind(rig);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, _p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turn turret to y-axis h speed <720>; the
        // muzzle cycles via ResetAim in the script — a single `gp` piece
        // here, so no cycling needed.
        rig.turn_rad(self.pieces.turret, Axis::Y, h, deg2rad(720.0));
        // `wait-for-turn turret around y-axis; return 1`.
        rig.turn_settled(self.pieces.turret, Axis::Y)
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER + turret FALL; hide both.
        rig.explode(self.pieces.body, SHATTER);
        rig.explode(self.pieces.turret, FALL);
        rig.hide(self.pieces.body);
        rig.hide(self.pieces.turret);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
