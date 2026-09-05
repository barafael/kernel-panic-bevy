//! worm.bos — the cloaked ambusher. Segments undulate in a traveling
//! wave while moving; aiming surfaces the head and yaws the body; a
//! strike lashes every segment forward before retracting.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim};

/// WALK_WAVEDIST [-8] — wave trough depth in elmos.
const WAVE_DEPTH: f32 = 8.0;
/// WALK_WAVESPEED [24] — segment chase speed, elmos/sec.
const WAVE_SPEED: f32 = 24.0;
/// WALK_INTERVAL 120 — wave step every 120ms.
const WAVE_INTERVAL: f32 = 0.12;
/// ATTACK_LENGTH [-16] — strike reach per segment (head reaches ×7).
const STRIKE_DEPTH: f32 = 16.0;
/// ATTACK_SPEED [48] — strike lash speed.
const STRIKE_SPEED: f32 = 48.0;
/// ATTACK_RETRACT [24] — retract speed.
const STRIKE_RETRACT: f32 = 24.0;

#[derive(Default)]
pub struct WormAnim {
    /// Walking-wave enabled (mirrors doMove).
    walking: bool,
    /// Current wave step (six segments per loop).
    phase: usize,
    phase_timer: f32,
    /// Strike animation in flight: seconds elapsed, `Some` while active.
    strike: Option<f32>,
    /// Head crouched (cloaked idle) vs surfaced (aiming).
    crouched: bool,
}

impl WormAnim {
    fn set_wave(&mut self, rig: &mut AnimRig) {
        // Traveling wave: each step, a band of segments sits at the
        // trough while the rest return to 0. Head crests on the first
        // half of the loop; `end` trails a step behind the last ring.
        let s = self.phase % 6;
        rig.move_to("head", Axis::Z, if s < 3 { 3.0 * WAVE_DEPTH } else { 0.0 }, WAVE_SPEED);
        for i in 0..6 {
            let active = (s + 6 - i) % 6 < 3;
            let name = format!("ring{i}");
            rig.move_to(&name, Axis::Z, if active { -WAVE_DEPTH } else { 0.0 }, WAVE_SPEED);
        }
        rig.move_to("end", Axis::Z, if (s + 5) % 6 < 3 { -WAVE_DEPTH } else { 0.0 }, WAVE_SPEED);
    }

    fn flatten(&mut self, rig: &mut AnimRig) {
        // ForceStopWalk(): every segment snaps home.
        for name in ["head", "ring0", "ring1", "ring2", "ring3", "ring4", "ring5", "end"] {
            rig.move_to(name, Axis::Z, 0.0, 0.0);
        }
    }
}

impl UnitAnim for WormAnim {
    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Cloaked idle crouch (Create()/ResetAim(): head y [-16] @12)
        // once the emerge completes.
        if !self.crouched && ctx.build_percent <= 0 && self.strike.is_none() && !self.walking {
            self.crouched = true;
            rig.move_to("head", Axis::Y, -16.0, 12.0);
        }

        if let Some(t) = &mut self.strike {
            *t += ctx.dt;
            // Two-phase strike: lash out 0.33s, then retract.
            if *t < 0.33 {
                // extension issued on strike start; hold
            } else if *t < 0.66 {
                if *t - ctx.dt < 0.33 {
                    rig.move_to("head", Axis::Z, 0.0, 7.0 * STRIKE_RETRACT);
                    for i in 0..6 {
                        rig.move_to(&format!("ring{i}"), Axis::Z, 0.0, STRIKE_RETRACT);
                    }
                    rig.move_to("end", Axis::Z, 0.0, STRIKE_RETRACT);
                }
            } else {
                self.strike = None;
                if self.walking {
                    self.set_wave(rig);
                } else {
                    self.crouched = false; // re-crouch next tick
                }
            }
        } else if self.walking {
            self.phase_timer -= ctx.dt;
            if self.phase_timer <= 0.0 {
                self.phase_timer = WAVE_INTERVAL;
                self.phase += 1;
                self.set_wave(rig);
            }
        }
    }

    fn start_moving(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // StartMoving(): doMove=1; Walkanim() unless aiming.
        if self.strike.is_none() {
            self.walking = true;
            self.phase_timer = 0.0;
        }
    }

    fn stop_moving(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // StopMoving(): doMove=0 — the wave runs out and flattens.
        self.walking = false;
        if self.strike.is_none() {
            for name in ["head", "ring0", "ring1", "ring2", "ring3", "ring4", "ring5", "end"] {
                rig.move_to(name, Axis::Z, 0.0, WAVE_SPEED);
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1: surface the head, yaw the body; a strike blocks.
        if self.strike.is_some() {
            return false;
        }
        self.walking = false;
        self.flatten(rig);
        rig.move_to("head", Axis::Y, 0.0, 12.0);
        rig.turn_rad("body", Axis::Y, h, 800.0 * super::super::DEG2RAD);
        let _ = p;
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): ForceStopWalk + the lash. Segment j goes to
        // ATTACK_LENGTH; the head goes to -7×that (7 segments' worth).
        if self.strike.is_some() {
            return;
        }
        self.strike = Some(0.0);
        self.walking = false;
        rig.move_to("head", Axis::Z, 7.0 * STRIKE_DEPTH, 7.0 * STRIKE_SPEED);
        for i in 0..6 {
            rig.move_to(&format!("ring{i}"), Axis::Z, -STRIKE_DEPTH, STRIKE_SPEED);
        }
        rig.move_to("end", Axis::Z, -STRIKE_DEPTH, STRIKE_SPEED);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): explode head + rings + end, all FALL.
        for name in ["head", "ring0", "ring1", "ring2", "ring3", "ring4", "ring5", "end"] {
            rig.explode(name, 3);
        }
    }
}
