//! hole.bos — the Hacker homebase (linear constant 65536).
//!
//! - `Create()`: the nano-arm assembly (`nanoarm`, `nanomover`) is hidden.
//! - `Activate()`: show both, fade in for 2.3 s (an alpha-threshold
//!   effect the engine no longer draws; the delay is real), set
//!   `INBUILDSTANCE`, then loop: for i = 16, 32, 48, 64 the arm steps out
//!   to −i @64 while the mover returns to 0 @256 (wait both), the mover
//!   strokes out to −55 @128 while `EmitFX()` fires the `corruption_const`
//!   CEG from `nanoemitter` every 30 ms, then the next step. After the
//!   fourth stroke the mover returns and the arm snaps back to −16 @128.
//!   One lap is about 3.1 s.
//! - `Deactivate()`: kill the loop (pieces finish their current moves),
//!   clear `INBUILDSTANCE`, fade out for 2.3 s, then hide and park.

use super::super::{AnimCtx, AnimRig, Axis, SfxKind, UnitAnim};
use super::move_time;

const ARM_STEP: f32 = 16.0;
const ARM_STEP_SPEED: f32 = 64.0;
const ARM_HOME_SPEED: f32 = 128.0;
const STROKE: f32 = 55.0;
const STROKE_SPEED: f32 = 128.0;
const RETURN_SPEED: f32 = 256.0;
/// Activate()/Deactivate(): 23 × `sleep 100` alpha steps.
const FADE_TIME: f32 = 2.3;
/// EmitFX(): `sleep 30`.
const EMIT_INTERVAL: f32 = 0.03;
/// Deactivate() parks the hidden arm @64 and mover @100.
const PARK_ARM_SPEED: f32 = 64.0;
const PARK_MOVER_SPEED: f32 = 100.0;
const WAIT_LATENCY: f32 = 1.0 / 30.0;
/// `explosiongenerator1` (hole.fbi): `emit-sfx 1025`.
const CONST_CEG: &str = "custom:corruption_const";

#[derive(Clone, Copy, Default)]
struct HolePieces {
    nanoarm: usize,
    nanomover: usize,
    nanoemitter: usize,
}

impl HolePieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            nanoarm: rig.bind_piece("nanoarm"),
            nanomover: rig.bind_piece("nanomover"),
            nanoemitter: rig.bind_piece("nanoemitter"),
        }
    }
}

/// Where the Activate() thread is.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Fading in; `INBUILDSTANCE` follows.
    FadeIn,
    /// Arm stepping out to −16·step and mover returning (waited).
    Reach(usize),
    /// Mover stroking out, emitting.
    Stroke(usize),
    /// Mover home @128, arm back to −16 @128 (arm waited).
    Return,
    /// Deactivate(): fading out before hide + park.
    FadeOut,
}

#[derive(Default)]
pub struct HoleAnim {
    pieces: HolePieces,
    phase: Option<Phase>,
    /// Seconds left in the current phase.
    timer: f32,
    /// `INBUILDSTANCE`.
    in_stance: bool,
    emit_timer: f32,
}

impl HoleAnim {
    fn enter(&mut self, rig: &mut AnimRig, phase: Phase) {
        let p = self.pieces;
        self.timer = match phase {
            Phase::FadeIn | Phase::FadeOut => FADE_TIME,
            Phase::Reach(i) => {
                let arm = -(i as f32) * ARM_STEP;
                rig.move_to(p.nanoarm, Axis::X, arm, ARM_STEP_SPEED);
                rig.move_to(p.nanomover, Axis::Z, 0.0, RETURN_SPEED);
                move_time(rig, p.nanoarm, Axis::X, arm, ARM_STEP_SPEED).max(move_time(
                    rig,
                    p.nanomover,
                    Axis::Z,
                    0.0,
                    RETURN_SPEED,
                )) + WAIT_LATENCY
            }
            Phase::Stroke(_) => {
                rig.move_to(p.nanomover, Axis::Z, -STROKE, STROKE_SPEED);
                move_time(rig, p.nanomover, Axis::Z, -STROKE, STROKE_SPEED) + WAIT_LATENCY
            }
            Phase::Return => {
                rig.move_to(p.nanomover, Axis::Z, 0.0, STROKE_SPEED);
                rig.move_to(p.nanoarm, Axis::X, -ARM_STEP, ARM_HOME_SPEED);
                move_time(rig, p.nanoarm, Axis::X, -ARM_STEP, ARM_HOME_SPEED) + WAIT_LATENCY
            }
        };
        self.phase = Some(phase);
    }
}

impl UnitAnim for HoleAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = HolePieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): hide nanoarm; hide nanomover.
        rig.hide(self.pieces.nanoarm);
        rig.hide(self.pieces.nanomover);
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        let Some(phase) = self.phase else {
            return;
        };
        self.timer -= ctx.dt;
        if let Phase::Stroke(_) = phase {
            // EmitFX(): `if (doEmit) emit-sfx 1025 from nanoemitter`.
            self.emit_timer -= ctx.dt;
            if self.emit_timer <= 0.0 {
                self.emit_timer = EMIT_INTERVAL;
                rig.emit(self.pieces.nanoemitter, SfxKind::Ceg(CONST_CEG));
            }
        }
        if self.timer > 0.0 {
            return;
        }
        match phase {
            Phase::FadeIn => {
                self.in_stance = true;
                self.enter(rig, Phase::Reach(1));
            }
            Phase::Reach(i) => self.enter(rig, Phase::Stroke(i)),
            Phase::Stroke(i) if i < 4 => self.enter(rig, Phase::Reach(i + 1)),
            Phase::Stroke(_) => self.enter(rig, Phase::Return),
            Phase::Return => self.enter(rig, Phase::Reach(1)),
            Phase::FadeOut => {
                rig.hide(self.pieces.nanoarm);
                rig.hide(self.pieces.nanomover);
                rig.move_to(self.pieces.nanoarm, Axis::X, 0.0, PARK_ARM_SPEED);
                rig.move_to(self.pieces.nanomover, Axis::Z, 0.0, PARK_MOVER_SPEED);
                self.phase = None;
            }
        }
    }

    fn activate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Activate(): `signal SIG_BUILD`; show both; fade in; INBUILDSTANCE.
        rig.show(self.pieces.nanoarm);
        rig.show(self.pieces.nanomover);
        self.enter(rig, Phase::FadeIn);
    }

    fn deactivate(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Deactivate(): `signal SIG_BUILD` kills the loop; INBUILDSTANCE
        // off; fade out; then hide and park.
        self.in_stance = false;
        self.enter(rig, Phase::FadeOut);
    }

    /// `INBUILDSTANCE`: production waits for the 2.3 s fade-in.
    fn is_open(&self) -> Option<bool> {
        Some(self.in_stance)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIECES: &[&str] = &[
        "whole",
        "body",
        "nanoarm",
        "nanomover",
        "nanoemitter",
        "pad",
    ];

    #[test]
    fn build_stance_follows_the_fade_and_the_arm_steps_out() {
        let mut rig = AnimRig::for_test(PIECES);
        let mut anim = HoleAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        anim.activate(&mut rig, AnimCtx::minimal());
        let step = |anim: &mut HoleAnim, rig: &mut AnimRig, frames: usize| {
            for _ in 0..frames {
                anim.update(
                    rig,
                    AnimCtx {
                        dt: 1.0 / 30.0,
                        ..AnimCtx::minimal()
                    },
                );
                super::super::super::tick_rig(rig, 1.0 / 30.0);
            }
        };
        step(&mut anim, &mut rig, 60);
        assert_eq!(anim.is_open(), Some(false));
        step(&mut anim, &mut rig, 10);
        assert_eq!(anim.is_open(), Some(true));
        assert_eq!(anim.phase, Some(Phase::Reach(1)));
        // 16 elmos @64 = 0.25 s, then the first stroke.
        step(&mut anim, &mut rig, 9);
        assert_eq!(anim.phase, Some(Phase::Stroke(1)));
        // Stroke 55 @128 ≈ 0.43 s, then the arm steps to −32.
        step(&mut anim, &mut rig, 15);
        assert_eq!(anim.phase, Some(Phase::Reach(2)));
        let arm = rig.piece("nanoarm").unwrap();
        assert!((rig.target_translations[arm][0] - 32.0).abs() < 1e-3);
    }
}
