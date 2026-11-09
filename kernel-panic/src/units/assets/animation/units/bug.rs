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

/// Frames the alpha dissolve of the morph takes (`for (i = 72; i > 5;
/// --i) { …; sleep 30; }`, a frame a step): the pieces keep their look,
/// the time passes.
const DISSOLVE_FRAMES: f32 = 67.0;
/// One frame in seconds.
const FRAME: f32 = 1.0 / 30.0;

/// Where a morph choreography stands.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Flip {
    #[default]
    Idle,
    /// Bug: the body rises 8 and turns over. Exploit: waiting out the
    /// dissolve.
    First(f32),
    /// Bug: the body sinks 12. Exploit: rising 8 and turning back.
    Second,
    /// Exploit: lowering to the ground.
    Third,
}

/// bug.bos — the crawling form. Turret and clamps stay hidden until a
/// deploy morph reveals them; aim is body-driven by the host (the
/// script has no `AimWeapon1`).
#[derive(Default)]
pub struct BugAnim {
    pieces: BugPieces,
    flip: Flip,
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

    fn morph(&mut self, rig: &mut AnimRig) {
        // Deploy(): hide the feet, face front, rise 8 and turn over.
        let p = self.pieces;
        for foot in p.foot {
            rig.hide(foot);
        }
        rig.turn_deg(p.body, Axis::Y, 0.0, 720.0);
        rig.move_to(p.body, Axis::Y, 8.0, 32.0);
        rig.turn_deg(p.body, Axis::X, 180.0, 360.0);
        self.flip = Flip::First(0.0);
    }

    fn update(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        let p = self.pieces;
        match self.flip {
            Flip::First(_) if rig.move_settled(p.body, Axis::Y, 0.01) => {
                rig.move_to(p.body, Axis::Y, -12.0, 64.0);
                self.flip = Flip::Second;
            }
            Flip::Second if rig.move_settled(p.body, Axis::Y, 0.01) => {
                // The clamps and the turret show, then the dissolve.
                rig.show(p.clamps);
                rig.show(p.turret);
                self.flip = Flip::Idle;
            }
            _ => {}
        }
    }
}

/// exploit.bos — the deployed form. Spawns with the body flipped over
/// (`x <180>`, sunk `y [-12]`) and the turret exposed; the turret piece
/// carries both aim axes.
#[derive(Default)]
pub struct ExploitAnim {
    pieces: BugPieces,
    flip: Flip,
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
        rig.turn_settled(self.pieces.turret, Axis::Y)
            && rig.turn_settled(self.pieces.turret, Axis::X)
    }

    fn morph(&mut self, _rig: &mut AnimRig) {
        // Undeploy(): the dissolve first, then the flip back.
        self.flip = Flip::First(DISSOLVE_FRAMES * FRAME);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        let p = self.pieces;
        match self.flip {
            Flip::First(left) => {
                let left = left - ctx.dt;
                if left > 0.0 {
                    self.flip = Flip::First(left);
                } else {
                    // hide clamps, turret; body up 8, over to 0.
                    rig.hide(p.clamps);
                    rig.hide(p.turret);
                    rig.move_to(p.body, Axis::Y, 8.0, 64.0);
                    rig.turn_deg(p.body, Axis::X, 0.0, 360.0);
                    self.flip = Flip::Second;
                }
            }
            Flip::Second if rig.move_settled(p.body, Axis::Y, 0.01) => {
                rig.move_to(p.body, Axis::Y, 0.0, 32.0);
                self.flip = Flip::Third;
            }
            Flip::Third if rig.move_settled(p.body, Axis::Y, 0.01) => {
                for foot in p.foot {
                    rig.show(foot);
                }
                self.flip = Flip::Idle;
            }
            _ => {}
        }
    }

    // exploit.bos declares no Killed() — the flip pose dies with it.
}

#[cfg(test)]
mod tests {
    use super::super::super::tick_rig;
    use super::*;

    const NAMES: &[&str] = &["body", "turret", "clamps", "feet0", "feet1", "feet2"];

    /// Run a driver's `update` and the rig's own motion for `secs`.
    fn run(anim: &mut dyn UnitAnim, rig: &mut AnimRig, secs: f32) {
        let ctx = AnimCtx {
            dt: FRAME,
            ..AnimCtx::minimal()
        };
        for _ in 0..(secs / FRAME).round() as usize {
            anim.update(rig, ctx);
            tick_rig(rig, FRAME);
        }
    }

    /// bug.bos `Deploy()`: the body rises 8, turns over and sinks to -12.
    #[test]
    fn the_bug_flips_over_and_sinks() {
        let mut rig = AnimRig::for_test(NAMES);
        let mut anim = BugAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        anim.morph(&mut rig);
        run(&mut anim, &mut rig, 1.0);
        let body = rig.piece_translations[0];
        assert!((body[1] - -12.0).abs() < 0.1, "sunk: {body:?}");
        assert_eq!(anim.flip, Flip::Idle);
    }

    /// exploit.bos `Undeploy()`: the dissolve passes, then the body
    /// rises 8 and comes down to 0 with the feet back.
    #[test]
    fn the_exploit_flips_back_after_the_dissolve() {
        let mut rig = AnimRig::for_test(NAMES);
        let mut anim = ExploitAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        anim.morph(&mut rig);
        run(&mut anim, &mut rig, 2.0);
        assert!(
            (rig.piece_translations[0][1] - -12.0).abs() < 0.1,
            "still lying in the dissolve"
        );
        run(&mut anim, &mut rig, 1.0);
        assert!(
            rig.piece_translations[0][1].abs() < 0.1,
            "back on the ground"
        );
        assert_eq!(anim.flip, Flip::Idle);
    }
}
