//! connection.bos — the Network teleporter. Its `gp` yaws and pitches
//! onto targets while `gp2` — the `QueryWeapon1` piece, hung 200 elmos
//! above `gp` in network_big.s3o — slides out along `gp`'s z to the
//! target's distance. The invisible GaussCannon beam therefore drops
//! onto the target from above it, and the first shot at a new range
//! waits for the slide. The body shatters on death.
//!
//! Upstream weapon script (the parts the engine calls):
//!
//! ```text
//! QueryWeapon1(p) { p=gp2; }
//! AimWeapon1(h,p) { signal SIG_Aim; set-signal-mask SIG_Aim;
//!                   if (lockAim) return 0;
//!                   turn gp to y-axis h speed <180>;
//!                   turn gp to x-axis 0 - p speed <180>;
//!                   if (get TARGET_ID(1))
//!                     move gp2 to z-axis (get XZ_HYPOT(get UNIT_XZ(get
//!                       TARGET_ID(1)) - get PIECE_XZ(gp))) speed [90];
//!                   wait-for-turn gp around y-axis;
//!                   wait-for-turn gp around x-axis;
//!                   wait-for-move gp2 along z-axis; return 1; }
//! FireWeapon1()   { lockAim=1; for (i=0;i<24;++i) { turn correcter to
//!                   y-axis h - get HEADING now; sleep 1; }
//!                   turn correcter to y-axis 0 now; lockAim=0; }
//! ```
//!
//! Not reproduced: `FireWeapon1`'s `correcter` counter-turn, which holds
//! `gp` still in the world while the hull turns during the 24-frame
//! lock (the driver doesn't see the unit's heading). The distance is
//! measured from the unit origin rather than `gp`, which sits right
//! above it, and is supplied for any aim target (upstream only slides
//! for unit targets; on a ground order `gp2` keeps its last extension).

use super::super::{AnimCtx, AnimRig, Axis, SHATTER, UnitAnim, deg2rad};
use super::DeathFx;
use crate::sim::DetFloat;

/// connection.bos `move gp2 ... speed [90]`.
const GP2_SPEED: f32 = 90.0;

/// `wait-for-move gp2` slack (elmos). The slide chases the target's
/// range, re-issued every tick; a target moving radially leaves `gp2` a
/// tick or two behind, which the hitscan beam doesn't care about.
const GP2_TOLERANCE: f32 = 8.0;

/// FireWeapon1's `lockAim` loop: 24 iterations of `sleep 1`, i.e. 24
/// sim frames.
const LOCK_AIM_SECS: f32 = 24.0 / 30.0;

#[derive(Clone, Copy, Default)]
struct ConnectionPieces {
    body: usize,
    gp: usize,
    gp2: usize,
}

impl ConnectionPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            body: rig.bind_piece("body"),
            gp: rig.bind_piece("gp"),
            gp2: rig.bind_piece("gp2"),
        }
    }
}

#[derive(Default)]
pub struct ConnectionAnim {
    pieces: ConnectionPieces,
    /// Seconds left of the script's `lockAim` after a shot.
    lock_aim: f32,
    death: DeathFx,
}

impl UnitAnim for ConnectionAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = ConnectionPieces::bind(rig);
    }

    fn update(&mut self, _rig: &mut AnimRig, ctx: AnimCtx) {
        self.lock_aim = (self.lock_aim - ctx.dt).det_max(0.0);
        self.death.tick(ctx.dt);
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, ctx: AnimCtx) -> bool {
        // `if (lockAim) return 0;`
        if self.lock_aim > 0.0 {
            return false;
        }
        // gp y to h @<180>, x to (0-p) @<180>; gp2 slides to the
        // target's distance @[90].
        rig.turn_rad(self.pieces.gp, Axis::Y, h, deg2rad(180.0));
        rig.turn_rad(self.pieces.gp, Axis::X, -p, deg2rad(180.0));
        if let Some(dist) = ctx.target_xz_dist {
            rig.move_to(self.pieces.gp2, Axis::Z, dist, GP2_SPEED);
        }
        // wait-for-turn gp around y, x; wait-for-move gp2 along z.
        rig.turn_settled(self.pieces.gp, Axis::Y)
            && rig.turn_settled(self.pieces.gp, Axis::X)
            && rig.move_settled(self.pieces.gp2, Axis::Z, GP2_TOLERANCE)
    }

    fn fire(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): lockAim=1 for 24 frames.
        self.lock_aim = LOCK_AIM_SECS;
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode body SHATTER; hide body.
        rig.explode(self.pieces.body, SHATTER);
        rig.hide(self.pieces.body);
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::assets::animation::{piece_names, tick_rig};
    use crate::units::content::definitions::UnitKind;

    fn aim_ctx(dist: f32) -> AnimCtx {
        AnimCtx {
            target_xz_dist: Some(dist),
            ..AnimCtx::minimal()
        }
    }

    /// The shot waits for `gp2` to slide out to the target's range at
    /// [90]/s — 300 elmos is ~3.3 s, far longer than the turns.
    #[test]
    fn aim_waits_for_the_gp2_slide() {
        let mut rig = AnimRig::for_test(piece_names(UnitKind::Connection));
        let mut anim = ConnectionAnim::default();
        anim.bind(&rig);
        let gp2 = rig.piece("gp2").unwrap();
        let mut frames = 0;
        while !anim.aim(&mut rig, 0.5, 0.1, aim_ctx(300.0)) {
            tick_rig(&mut rig, 1.0 / 30.0);
            frames += 1;
            assert!(frames < 200, "gp2 never reached the target range");
        }
        assert!((95..=100).contains(&frames), "took {frames} frames");
        assert!((rig.piece_translations[gp2][2] - 300.0).abs() <= GP2_TOLERANCE);
    }

    /// FireWeapon1's `lockAim`: no aim for 24 frames after a shot.
    #[test]
    fn fire_locks_aim_for_24_frames() {
        let mut rig = AnimRig::for_test(piece_names(UnitKind::Connection));
        let mut anim = ConnectionAnim::default();
        anim.bind(&rig);
        anim.fire(&mut rig, AnimCtx::minimal());
        let tick = AnimCtx {
            dt: 1.0 / 30.0,
            ..AnimCtx::minimal()
        };
        for _ in 0..23 {
            anim.update(&mut rig, tick);
            assert!(!anim.aim(&mut rig, 0.0, 0.0, aim_ctx(0.0)));
        }
        anim.update(&mut rig, tick);
        anim.update(&mut rig, tick);
        assert!(anim.aim(&mut rig, 0.0, 0.0, aim_ctx(0.0)));
    }
}
