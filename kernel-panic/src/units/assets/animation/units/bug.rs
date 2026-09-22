//! bug.bos / exploit.bos — the Hacker deploy pair. The morph itself is
//! host-side (despawn + spawn with carried HP); each side just needs its
//! authored pose and aim behaviour.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};

#[derive(Clone, Copy, Default)]
struct BugPieces {
    turret: usize,
    clamps: usize,
    foot: [usize; 3],
    body: usize,
}

impl BugPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            turret: rig.bind_piece("turret"),
            clamps: rig.bind_piece("clamps"),
            foot: [
                rig.bind_piece("feet0"),
                rig.bind_piece("feet1"),
                rig.bind_piece("feet2"),
            ],
            body: rig.bind_piece("body"),
        }
    }
}

/// bug.bos — the crawling form. Turret and clamps stay hidden until a
/// deploy morph would reveal them; aim is body-driven by the host (the
/// script has no `AimWeapon1`).
#[derive(Default)]
pub struct BugAnim {
    pieces: BugPieces,
}

impl UnitAnim for BugAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = BugPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide turret; hide clamps.
        rig.hide(self.pieces.turret);
        rig.hide(self.pieces.clamps);
    }
}

/// exploit.bos — the deployed form. Spawns with the body flipped over
/// (`x <180>`, sunk `y [-12]`) and the turret exposed; the turret piece
/// carries both aim axes.
#[derive(Default)]
pub struct ExploitAnim {
    pieces: BugPieces,
}

impl UnitAnim for ExploitAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = BugPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide feet0..2; body x <180> now; body y [-12] now.
        for foot in self.pieces.foot {
            rig.hide(foot);
        }
        rig.turn_deg(self.pieces.body, Axis::X, 180.0, 0.0);
        rig.move_to(self.pieces.body, Axis::Y, -12.0, 0.0);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turret y to h @<180>, x to (0-p) @<180>;
        // returns 1 once the turns settle.
        rig.turn_rad(self.pieces.turret, Axis::Y, h, deg2rad(180.0));
        rig.turn_rad(self.pieces.turret, Axis::X, -p, deg2rad(180.0));
        true
    }

    // exploit.bos declares no Killed() — the flip pose dies with it.
}
