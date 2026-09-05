//! byte.cob — the heavy defensive rotor, translated from the *compiled
//! bytecode* (dumped via the spring-cob parser). Values below are
//! bytecode literals converted to game units: angles = raw/65536·360
//! degrees, translations = raw/65536 elmos. Byte compiles with Scriptor
//! linear constant 163840, so its `.bos` source brackets are 2.5× the
//! bytecode values — do NOT re-translate from byte.bos.
//!
//! Fold lifecycle: folded by default; unfolds to fire (base lifts 60,
//! blades fan ±10, rotor steps once to 45°); folds back up to drive.
//! Driving is only allowed in [`FoldState::Closed`], firing only in
//! [`FoldState::Open`]. Upstream has no rotor spin (the `spin` line was
//! dropped before compiling) — with the aimer pitched horizontal, a
//! rotor spin would read as the gun rolling around its barrel axis.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, DEG2RAD};
use std::f32::consts::TAU;

/// Close(): `sleep 3000` before folding after the last aim.
const IDLE_CLOSE_DELAY: f32 = 3.0;

// Choreography phase durations (seconds), from bytecode speeds:
// Open: base →60 @120 = 0.5s, then blades →±10 @40 (0.25s) alongside
// rotor →45° @90°/s (0.5s).
const OPEN_BASE_TIME: f32 = 0.5;
const OPEN_BLADE_TIME: f32 = 0.5;
// Close: aimer → 0 @70°/s (up to ~1.3s of slew), blades → 0 @40
// (0.25s), then rotor → 0 @480°/s and base → 0 @300 together (0.25s).
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
        // Open(): move base to y-axis [3932160]=60 speed [7864320]=120.
        self.state = FoldState::Opening { t: 0.0, phase: 0 };
        rig.move_to("base", Axis::Y, 60.0, 120.0);
    }

    fn enter_closing(&mut self, rig: &mut AnimRig) {
        // Close() phase 0: aimer relaxes to rest while it still can.
        self.state = FoldState::Closing { t: 0.0, phase: 0 };
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
        // Create(): TurnNow bp0..3 x ←16380 = 90°; MoveNow launcher_arm
        // y ←9830400 = 150 elmos; launcher1..5 x ←5460 = 30°, y
        // ←-3276/1638/0/1638/3276 = -18/9/0/9/18°. Spawns folded.
        for bp in ["bp0", "bp1", "bp2", "bp3"] {
            rig.turn_deg(bp, Axis::X, 90.0, 0.0);
        }
        rig.move_to("launcher_arm", Axis::Y, 150.0, 0.0);
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
        // Create()'s emerge loop: base y ← -40·pct/100 (bytecode
        // -2621440 = -40 elmos) while still emerging.
        if ctx.emerging {
            super::emerge_lift(rig, "base", 40.0, ctx.build_percent);
        }

        // A live aim request or an explicit attack order counts as
        // "wants to fight" — a ground order may target empty terrain,
        // where no AimTarget is ever stamped.
        let wants_open = ctx.aim_active || ctx.attack_ordering;
        if wants_open {
            self.since_target = 0.0;
        } else {
            self.since_target += ctx.dt;
        }

        match self.state {
            FoldState::Closed => {
                // Unfold to fight — but only while stationary.
                rig.move_gate = 1.0;
                if wants_open && !ctx.moving {
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
                    // Open() phase 1: blades → ±[655360]=10 @40; rotor
                    // steps once to ←8190 = 45° @90°/s.
                    phase = 1;
                    rig.turn_deg("rotor", Axis::Y, 45.0, 90.0);
                    rig.move_to("blade0", Axis::Z, 10.0, 40.0);
                    rig.move_to("blade1", Axis::X, 10.0, 40.0);
                    rig.move_to("blade2", Axis::Z, -10.0, 40.0);
                    rig.move_to("blade3", Axis::X, -10.0, 40.0);
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
                    // Close(): blades → 0 @40.
                    phase = 1;
                    rig.move_to("blade0", Axis::Z, 0.0, 40.0);
                    rig.move_to("blade1", Axis::X, 0.0, 40.0);
                    rig.move_to("blade2", Axis::Z, 0.0, 40.0);
                    rig.move_to("blade3", Axis::X, 0.0, 40.0);
                }
                if phase == 1 && t >= CLOSE_AIMER_TIME + CLOSE_BLADE_TIME {
                    // Close(): rotor → 0 @480°/s, base → 0 @300.
                    phase = 2;
                    rig.turn_deg("rotor", Axis::Y, 0.0, 480.0);
                    rig.move_to("base", Axis::Y, 0.0, 300.0);
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
        // AimWeapon1: aimer x → (-16380) - p = -90° - p @270°/s, y → h
        // @270°/s. `h` arrives body-relative (Spring contract: world
        // heading minus body yaw), so the gun tracks the target
        // regardless of which way the hull ended up facing.
        rig.turn_deg("aimer", Axis::X, -90.0 - rad2deg(p), 270.0);
        rig.turn_rad("aimer", Axis::Y, h, 270.0 * DEG2RAD);
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit 1024 from bp{gp}, then cycle static[1]
        // gp 0→1→2→3→0. The host fires one shot per call; the volley
        // pacing (sleep 90/150) is the weapon cooldown here.
        let idx = self_cycle(&mut rig.muzzle, 4);
        if let Some(piece) = rig.piece(format!("bp{idx}").as_str()) {
            rig.outbox.push(super::super::FxEvent::Emit { piece, sfx: 1024 });
        }
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide blade0/2 (and 1/3 via the same pattern),
        // explode blade0..3 severity 1.
        for blade in ["blade0", "blade1", "blade2", "blade3"] {
            rig.hide(blade);
            rig.explode(blade, 1);
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
