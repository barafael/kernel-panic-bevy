//! signal.bos — the Network scout. Deploys from a nose-down spawn pose:
//! the base swings level ~2.5s after creation, then the four wings
//! unfold. Dies scattering its wings and dropping any loaded bomb.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::DeathFx;

#[derive(Clone, Copy, Default)]
struct SignalPieces {
    base: usize,
    bomb: usize,
    wing: [usize; 4],
}

impl SignalPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            bomb: rig.bind_piece("bomb"),
            wing: [
                rig.bind_piece("wingul"),
                rig.bind_piece("wingur"),
                rig.bind_piece("wingbl"),
                rig.bind_piece("wingbr"),
            ],
        }
    }
}

#[derive(Default)]
pub struct SignalAnim {
    pieces: SignalPieces,
    /// Seconds since spawn (stages the deploy timeline).
    age: f32,
    stage: u8,
    /// Bomb still aboard (dropped on death or on firing).
    loaded: bool,
    death: DeathFx,
}

impl UnitAnim for SignalAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = SignalPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): turn base to x <-90> now (spawned nose-down);
        // loaded=1.
        rig.turn_deg(self.pieces.base, Axis::X, -90.0, 0.0);
        self.loaded = true;
        self.age = 0.0;
        self.stage = 0;
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        self.age += ctx.dt;
        self.death.tick(ctx.dt);
        match self.stage {
            0 if self.age >= 2.5 => {
                // Create(): turn base to x 0 speed <60> after sleep 2500.
                self.stage = 1;
                rig.turn_deg(self.pieces.base, Axis::X, 0.0, 60.0);
            }
            1 if self.age >= 3.0 => {
                // Create(): unfold the four wings at ~3.5s.
                self.stage = 2;
                let [ul, ur, bl, br] = self.pieces.wing;
                rig.move_to(ul, Axis::X, -8.0, 8.0);
                rig.move_to(ul, Axis::Y, 4.0, 4.0);
                rig.move_to(ur, Axis::X, 8.0, 8.0);
                rig.move_to(ur, Axis::Y, 4.0, 4.0);
                rig.move_to(bl, Axis::X, -8.0, 8.0);
                rig.move_to(bl, Axis::Y, -4.0, 4.0);
                rig.move_to(br, Axis::X, 8.0, 8.0);
                rig.move_to(br, Axis::Y, -4.0, 4.0);
            }
            _ => {}
        }
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): loaded=0; hide bomb.
        self.loaded = false;
        rig.hide(self.pieces.bomb);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): wings FALL off; if loaded, drop the bomb.
        for wing in self.pieces.wing {
            rig.explode(wing, 3);
            rig.hide(wing);
        }
        if self.loaded {
            rig.emit(self.pieces.bomb, SfxKind::FireFlash);
            rig.hide(self.pieces.bomb);
        }
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}
