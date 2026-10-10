//! badblock.bos — the cheap wall. Two build lasers pitched up at
//! creation; while the block is under construction (once
//! `BUILD_PERCENT_LEFT` drops below 99) they trace a small square and
//! fire `BuildLaser` beams every 60 ms. `SIG_COMPLETE` kills both
//! threads when the build finishes and the base settles at rest.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::SquareSweep;

/// BuildLasers(): square half-width ±30 elmos @80 elmos/s (bytecode
/// ±1966080 @5242880 — 163840 linear constant).
const SWEEP: f32 = 30.0;
const SWEEP_SPEED: f32 = 80.0;
/// EmitBuildLasers(): `sleep 60`.
const EMIT_INTERVAL: f32 = 0.06;
/// Create(): `move base to y-axis [-8] now` = −20.
const BASE_SINK: f32 = 20.0;
/// Create() waits `while (BUILD_PERCENT_LEFT >= 99)` before starting
/// `BuildLasers()`.
const LASERS_FROM_PERCENT: i32 = 99;

#[derive(Default)]
pub struct BadBlockAnim {
    pieces: BadBlockPieces,
    sweep: SquareSweep,
    emit_timer: f32,
    /// `signal SIG_COMPLETE` has fired.
    complete: bool,
}

#[derive(Clone, Copy, Default)]
struct BadBlockPieces {
    base: usize,
    blaser: [usize; 2],
}

impl BadBlockPieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            blaser: [rig.bind_piece("blaser0"), rig.bind_piece("blaser1")],
        }
    }
}

impl UnitAnim for BadBlockAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = BadBlockPieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): blaser0/1 to x <90> now; base starts sunk.
        for blaser in self.pieces.blaser {
            rig.turn_deg(blaser, Axis::X, 90.0, 0.0);
        }
        rig.move_to(self.pieces.base, Axis::Y, -BASE_SINK, 0.0);
        self.sweep = SquareSweep::new(SWEEP, SWEEP_SPEED);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        if self.complete {
            return;
        }
        if !ctx.emerging {
            // `move base to y-axis 0 now; signal SIG_COMPLETE`: the
            // lasers stop wherever they are. A block that never was
            // under construction (pre-placed) completes at once.
            rig.move_to(self.pieces.base, Axis::Y, 0.0, 0.0);
            self.complete = true;
            return;
        }

        // Create()'s emerge loop: base lifts with BUILD_PERCENT_LEFT.
        super::emerge_lift(rig, self.pieces.base, BASE_SINK, ctx.build_percent);
        if ctx.build_percent >= LASERS_FROM_PERCENT {
            return;
        }

        // BuildLasers(): square sweep.
        let [blaser0, blaser1] = self.pieces.blaser;
        self.sweep
            .update(rig, blaser0, blaser1, SWEEP, SWEEP_SPEED, ctx.dt);

        // EmitBuildLasers(): `emit-sfx 2048` from both lasers.
        self.emit_timer -= ctx.dt;
        if self.emit_timer <= 0.0 {
            self.emit_timer = EMIT_INTERVAL;
            for blaser in self.pieces.blaser {
                rig.emit(blaser, SfxKind::BuildBeam);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(emerging: bool, build_percent: i32) -> AnimCtx {
        AnimCtx {
            dt: 0.1,
            emerging,
            build_percent,
            ..AnimCtx::minimal()
        }
    }

    fn emits(rig: &mut AnimRig) -> usize {
        rig.outbox.drain(..).count()
    }

    /// Regression: the lasers used to spark forever on every finished
    /// wall (~30 effect entities a second each). Upstream they run only
    /// while the block is being built, from 99 % left down to done.
    #[test]
    fn lasers_fire_only_while_under_construction() {
        let mut rig = AnimRig::for_test(&["base", "blaser0", "blaser1"]);
        let mut anim = BadBlockAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        emits(&mut rig);

        anim.update(&mut rig, ctx(true, 100));
        assert_eq!(emits(&mut rig), 0, "Create() waits below 99 % left");
        anim.update(&mut rig, ctx(true, 50));
        assert_eq!(emits(&mut rig), 2, "both lasers beam mid-build");
        anim.update(&mut rig, ctx(false, 0));
        for _ in 0..20 {
            anim.update(&mut rig, ctx(false, 0));
        }
        assert_eq!(emits(&mut rig), 0, "SIG_COMPLETE stops them for good");
    }
}
