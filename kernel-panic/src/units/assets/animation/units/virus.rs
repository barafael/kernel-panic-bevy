//! virus.bos — the Hacker's infection spawn. A turret yaws/pitches onto
//! the target, the two barrels alternate shot by shot, and body and
//! turret break apart on death.
//!
//! Upstream weapon script (the parts the engine calls):
//!
//! ```text
//! AimFromWeapon1(p) { p=turret; }
//! QueryWeapon1(p)   { if(gun) p=gun0; else p=gun1; }
//! AimWeapon1(h,p)   { signal SIG_AIM; set-signal-mask SIG_AIM;
//!                     turn turret to y-axis h speed <480>;
//!                     turn turret to x-axis 0-p speed <240>;
//!                     wait-for-turn turret around y-axis;
//!                     wait-for-turn turret around x-axis; return 1; }
//! Shot1(zero)       { gun = !gun; }
//! ```

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

#[derive(Clone, Copy, Default)]
struct VirusPieces {
    body: usize,
    turret: usize,
    gun0: usize,
    gun1: usize,
}

impl VirusPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            turret: rig.bind_piece("turret"),
            gun0: rig.bind_piece("gun0"),
            gun1: rig.bind_piece("gun1"),
        }
    }
}

#[derive(Default)]
pub struct VirusAnim {
    pieces: VirusPieces,
    /// The script's `gun` static (0 at Create).
    gun: bool,
    death: DeathFx,
}

impl UnitAnim for VirusAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = VirusPieces::bind(rig);
    }

    fn update(&mut self, _rig: &mut AnimRig, ctx: AnimCtx) {
        self.death.tick(ctx.dt);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1(h,p): turret y to h @<480>, x to (0-p) @<240>;
        // returns 1 once both turns have landed.
        rig.turn_rad(self.pieces.turret, Axis::Y, h, deg2rad(480.0));
        rig.turn_rad(self.pieces.turret, Axis::X, -p, deg2rad(240.0));
        rig.turn_settled(self.pieces.turret, Axis::Y)
            && rig.turn_settled(self.pieces.turret, Axis::X)
    }

    fn shot(&mut self, rig: &mut AnimRig) {
        // Shot1(): gun = !gun; then QueryWeapon1: gun ? gun0 : gun1.
        self.gun = !self.gun;
        rig.muzzle = if self.gun {
            self.pieces.gun0
        } else {
            self.pieces.gun1
        };
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER + turret FALL; hide both.
        rig.explode(self.pieces.body, 4);
        rig.explode(self.pieces.turret, 3);
        rig.hide(self.pieces.body);
        rig.hide(self.pieces.turret);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::assets::animation::{FxEvent, piece_names, tick_rig};
    use crate::units::content::definitions::UnitKind;

    fn rig() -> (AnimRig, VirusAnim) {
        let rig = AnimRig::for_test(piece_names(UnitKind::Virus));
        let mut anim = VirusAnim::default();
        anim.bind(&rig);
        (rig, anim)
    }

    /// `wait-for-turn turret`: no shot until the turret faces the
    /// target — 90° at <480>/s is ~6 ticks.
    #[test]
    fn aim_waits_for_the_turret() {
        let (mut rig, mut anim) = rig();
        let h = std::f32::consts::FRAC_PI_2;
        assert!(!anim.aim(&mut rig, h, 0.0, AnimCtx::minimal()));
        let mut frames = 0;
        while !anim.aim(&mut rig, h, 0.0, AnimCtx::minimal()) {
            tick_rig(&mut rig, 1.0 / 30.0);
            frames += 1;
            assert!(frames < 30, "turret never reached the aim heading");
        }
        assert!((4..=6).contains(&frames), "took {frames} frames");
    }

    /// `Shot1` flips `gun` before `QueryWeapon1` reads it: the first
    /// shot leaves gun0, then gun1, gun0, ...
    #[test]
    fn shots_alternate_barrels_starting_at_gun0() {
        let (mut rig, mut anim) = rig();
        let gun0 = rig.piece("gun0").unwrap();
        let gun1 = rig.piece("gun1").unwrap();
        let muzzles: Vec<usize> = (0..4)
            .map(|_| {
                anim.shot(&mut rig);
                rig.muzzle
            })
            .collect();
        assert_eq!(muzzles, [gun0, gun1, gun0, gun1]);
    }

    #[test]
    fn killed_breaks_body_and_turret() {
        let (mut rig, mut anim) = rig();
        anim.killed(&mut rig, AnimCtx::minimal());
        let explodes = rig
            .outbox
            .iter()
            .filter(|e| matches!(e, FxEvent::Explode { .. }))
            .count();
        assert_eq!(explodes, 2);
        assert!(anim.busy());
    }
}
