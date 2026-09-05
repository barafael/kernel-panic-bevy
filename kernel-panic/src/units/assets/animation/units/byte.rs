//! byte.bos — the heavy defensive rotor. Folded by default; unfolds to
//! fire (base lifts, blades spread, rotor steps out 45°) and folds back
//! up to move — with the choreography completing *before* the unit may
//! drive, and unfolding completing *before* the gun may fire.
//!
//! Upstream mechanism, reproduced: `AimWeapon1` opens if closed and
//! returns 0 until the unfold finishes; a finished unfold returns 1. The
//! aim signal also re-schedules `Close()` (sleep 3000), which is what
//! folds the byte back up ~3s after the fighting stops. Note upstream's
//! `Open()` has `spin rotor` commented out — the rotor only steps once
//! to 45°; it never spins (with the aimer pitched to horizontal, a rotor
//! spin would read as the gun assembly rolling around the barrel axis).

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, DEG2RAD};
use std::f32::consts::TAU;

/// byte.bos Close(): `sleep 3000` before folding after the last aim.
const IDLE_CLOSE_DELAY: f32 = 3.0;

// Choreography phase durations (seconds), from the script's speeds:
// Open: base [24]@48 = 0.5s, then blades [±4]@16 (0.25s) alongside
// rotor <45>@90 (0.5s).
const OPEN_BASE_TIME: f32 = 0.5;
const OPEN_BLADE_TIME: f32 = 0.5;
// Close: aimer → 0 @70 (up to ~1.3s of slew), blades → 0 @16 (0.25s),
// then rotor → 0 @480 and base → 0 @120 together (0.25s).
const CLOSE_AIMER_TIME: f32 = 1.4;
const CLOSE_BLADE_TIME: f32 = 0.3;
const CLOSE_FINAL_TIME: f32 = 0.25;

fn rad2deg(r: f32) -> f32 {
    r * 360.0 / TAU
}

/// Fold lifecycle. Driving is only allowed in [`FoldState::Closed`];
/// firing only in [`FoldState::Open`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum FoldState {
    /// Folded. May drive. Unfolds when a target appears while stationary.
    Closed,
    /// Unfold choreography in flight (`t` seconds elapsed, `phase` 0/1).
    Opening { t: f32, phase: u8 },
    /// Fully unfolded. May fire. Folds after `IDLE_CLOSE_DELAY` without
    /// a target, or immediately on a move order.
    Open,
    /// Fold choreography in flight (`t` seconds elapsed, `phase` 0/1/2).
    Closing { t: f32, phase: u8 },
}

pub struct ByteAnim {
    state: FoldState,
    /// Seconds since a target was last visible (drives the idle fold).
    since_target: f32,
}

impl ByteAnim {
    fn enter_opening(&mut self, rig: &mut AnimRig) {
        // Open() phase 0: `move base to y-axis [24] speed [48]`.
        self.state = FoldState::Opening {
            t: 0.0,
            phase: 0,
        };
        rig.move_to("base", Axis::Y, 24.0, 48.0);
    }

    fn enter_closing(&mut self, rig: &mut AnimRig) {
        // Close() phase 0: aimer relaxes to rest while it still can.
        self.state = FoldState::Closing {
            t: 0.0,
            phase: 0,
        };
        rig.turn_deg("aimer", Axis::X, 0.0, 70.0);
        rig.turn_deg("aimer", Axis::Y, 0.0, 70.0);
    }
}

impl Default for ByteAnim {
    fn default() -> Self {
        Self {
            state: FoldState::Closed,
            since_target: f32::INFINITY,
        }
    }
}

impl UnitAnim for ByteAnim {
    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): barrels pitch up <90>, launcher arm raised, five
        // mine tubes fanned out. The byte spawns folded (no auto-open).
        for bp in ["bp0", "bp1", "bp2", "bp3"] {
            rig.turn_deg(bp, Axis::X, 90.0, 0.0);
        }
        rig.move_to("launcher_arm", Axis::Y, 60.0, 0.0);
        for (name, yaw) in [
            ("launcher1", -18.0),
            ("launcher2", 9.0),
            ("launcher3", 0.0),
            ("launcher4", 9.0),
            ("launcher5", 18.0),
        ] {
            rig.turn_deg(name, Axis::X, 30.0, 0.0);
            rig.turn_deg(name, Axis::Y, yaw, 0.0);
        }
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base sinks [-16]·pct/100 while building.
        if ctx.emerging {
            super::emerge_lift(rig, "base", 16.0, ctx.build_percent);
        }

        if ctx.aim_active {
            self.since_target = 0.0;
        } else {
            self.since_target += ctx.dt;
        }

        match self.state {
            FoldState::Closed => {
                // Unfold to fight — but only while stationary.
                rig.move_gate = 1.0;
                if ctx.aim_active && !ctx.moving {
                    self.enter_opening(rig);
                }
            }
            FoldState::Opening { mut t, mut phase } => {
                rig.move_gate = 0.0;
                if ctx.moving {
                    // Move order mid-unfold: fold straight back up; the
                    // rig re-targets the in-flight pieces smoothly.
                    self.enter_closing(rig);
                    return;
                }
                t += ctx.dt;
                if phase == 0 && t >= OPEN_BASE_TIME {
                    // Open() phase 1: blades spread; rotor steps out to
                    // <45> once (upstream's spin is commented out).
                    phase = 1;
                    rig.turn_deg("rotor", Axis::Y, 45.0, 90.0);
                    rig.move_to("blade0", Axis::Z, 4.0, 16.0);
                    rig.move_to("blade1", Axis::X, 4.0, 16.0);
                    rig.move_to("blade2", Axis::Z, -4.0, 16.0);
                    rig.move_to("blade3", Axis::X, -4.0, 16.0);
                }
                if phase == 1 && t >= OPEN_BASE_TIME + OPEN_BLADE_TIME {
                    self.state = FoldState::Open;
                } else {
                    self.state = FoldState::Opening { t, phase };
                }
            }
            FoldState::Open => {
                rig.move_gate = 0.0;
                if ctx.moving || self.since_target > IDLE_CLOSE_DELAY {
                    self.enter_closing(rig);
                }
            }
            FoldState::Closing { mut t, mut phase } => {
                rig.move_gate = 0.0;
                t += ctx.dt;
                if phase == 0 && t >= CLOSE_AIMER_TIME {
                    // Close(): fold the blades back in.
                    phase = 1;
                    rig.move_to("blade0", Axis::Z, 0.0, 16.0);
                    rig.move_to("blade1", Axis::X, 0.0, 16.0);
                    rig.move_to("blade2", Axis::Z, 0.0, 16.0);
                    rig.move_to("blade3", Axis::X, 0.0, 16.0);
                }
                if phase == 1 && t >= CLOSE_AIMER_TIME + CLOSE_BLADE_TIME {
                    // Close(): rotor re-centers, base lowers.
                    phase = 2;
                    rig.turn_deg("rotor", Axis::Y, 0.0, 480.0);
                    rig.move_to("base", Axis::Y, 0.0, 120.0);
                }
                if phase == 2 && t >= CLOSE_AIMER_TIME + CLOSE_BLADE_TIME + CLOSE_FINAL_TIME {
                    self.state = FoldState::Closed;
                } else {
                    self.state = FoldState::Closing { t, phase };
                }
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, ctx: AnimCtx) -> bool {
        // AimWeapon1: `if (!isOpen) { start-script Open(); return 0; }`
        if matches!(self.state, FoldState::Closed) && !ctx.moving {
            self.enter_opening(rig);
        }
        if self.state != FoldState::Open {
            return false;
        }
        // AimWeapon1: aimer x to (<-90>-p) @<270>, y to h @<270> — lay
        // the gun assembly out lengthwise at the target and track by
        // yaw/pitch only. (The combat system additionally gates the shot
        // on the aimer piece actually arriving within tolerance.)
        rig.turn_deg("aimer", Axis::X, -90.0 - rad2deg(p), 270.0);
        rig.turn_rad("aimer", Axis::Y, h, 270.0 * DEG2RAD);
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit from bp{gp}, then cycle gp 0→1→2→3→0.
        let idx = self_cycle(&mut rig.muzzle, 4);
        if let Some(piece) = rig.piece(format!("bp{idx}").as_str()) {
            rig.outbox.push(super::super::FxEvent::Emit { piece, sfx: 1024 });
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide + explode blade0..3 SHATTER.
        for blade in ["blade0", "blade1", "blade2", "blade3"] {
            rig.hide(blade);
            rig.explode(blade, 4);
        }
    }

    fn is_open(&self) -> Option<bool> {
        Some(self.state == FoldState::Open)
    }
}

fn self_cycle(value: &mut usize, modulus: usize) -> usize {
    let current = *value % modulus;
    *value = (current + 1) % modulus;
    current
}
