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

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

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

/// The byte's bound piece indices. `bp0..3` are stored as an array so
/// the muzzle-cycle fire path indexes directly.
#[derive(Clone, Copy, Default)]
struct BytePieces {
    base: usize,
    aimer: usize,
    rotor: usize,
    blade: [usize; 4],
    bp: [usize; 4],
    launcher_arm: usize,
    launcher: [usize; 5],
}

impl BytePieces {
    fn bind(rig: &AnimRig) -> Self {
        Self {
            base: rig.bind_piece("base"),
            aimer: rig.bind_piece("aimer"),
            rotor: rig.bind_piece("rotor"),
            blade: [
                rig.bind_piece("blade0"),
                rig.bind_piece("blade1"),
                rig.bind_piece("blade2"),
                rig.bind_piece("blade3"),
            ],
            bp: [
                rig.bind_piece("bp0"),
                rig.bind_piece("bp1"),
                rig.bind_piece("bp2"),
                rig.bind_piece("bp3"),
            ],
            launcher_arm: rig.bind_piece("launcher_arm"),
            launcher: [
                rig.bind_piece("launcher1"),
                rig.bind_piece("launcher2"),
                rig.bind_piece("launcher3"),
                rig.bind_piece("launcher4"),
                rig.bind_piece("launcher5"),
            ],
        }
    }
}

/// Which fold timeline a [`Sequencer`] runs. `fire(i)` issues entry
/// `i`'s `.bos` commands: entry 0 fires when the sequencer starts,
/// entries 1..N at their time thresholds, and the choreography is
/// complete once `t >= finish()`.
#[derive(Clone, Copy)]
enum FoldTimeline {
    Open,
    Close,
}

impl FoldTimeline {
    /// Elapsed-time thresholds at which entries 1..N fire.
    fn thresholds(self) -> &'static [f32] {
        match self {
            FoldTimeline::Open => &[OPEN_BASE_TIME],
            FoldTimeline::Close => &[CLOSE_AIMER_TIME, CLOSE_AIMER_TIME + CLOSE_BLADE_TIME],
        }
    }

    /// Total choreography length (the last entry's animation tail).
    fn finish(self) -> f32 {
        match self {
            FoldTimeline::Open => OPEN_BASE_TIME + OPEN_BLADE_TIME,
            FoldTimeline::Close => CLOSE_AIMER_TIME + CLOSE_BLADE_TIME + CLOSE_FINAL_TIME,
        }
    }

    fn fire(self, rig: &mut AnimRig, p: &BytePieces, i: usize) {
        match (self, i) {
            // Open() entry 0: move base to y-axis [3932160]=60 speed
            // [7864320]=120.
            (FoldTimeline::Open, 0) => {
                rig.move_to(p.base, Axis::Y, 60.0, 120.0);
            }
            // Open() entry 1: blades → ±[655360]=10 @40; rotor steps once
            // to ←8190 = 45° @90°/s.
            (FoldTimeline::Open, 1) => {
                rig.turn_deg(p.rotor, Axis::Y, 45.0, 90.0);
                rig.move_to(p.blade[0], Axis::Z, 10.0, 40.0);
                rig.move_to(p.blade[1], Axis::X, 10.0, 40.0);
                rig.move_to(p.blade[2], Axis::Z, -10.0, 40.0);
                rig.move_to(p.blade[3], Axis::X, -10.0, 40.0);
            }
            // Close() entry 0: aimer relaxes to rest while it still can.
            (FoldTimeline::Close, 0) => {
                rig.turn_deg(p.aimer, Axis::X, 0.0, 70.0);
                rig.turn_deg(p.aimer, Axis::Y, 0.0, 70.0);
            }
            // Close() entry 1: blades → 0 @40 (blade0/2 on z, blade1/3
            // on x — exactly the axes Open moved them on).
            (FoldTimeline::Close, 1) => {
                rig.move_to(p.blade[0], Axis::Z, 0.0, 40.0);
                rig.move_to(p.blade[1], Axis::X, 0.0, 40.0);
                rig.move_to(p.blade[2], Axis::Z, 0.0, 40.0);
                rig.move_to(p.blade[3], Axis::X, 0.0, 40.0);
            }
            // Close() entry 2: rotor → 0 @480°/s, base → 0 @300.
            (FoldTimeline::Close, 2) => {
                rig.turn_deg(p.rotor, Axis::Y, 0.0, 480.0);
                rig.move_to(p.base, Axis::Y, 0.0, 300.0);
            }
            _ => {}
        }
    }
}

/// A minimal time-keyed command list: entry 0 fires when the sequencer
/// starts, entries 1..N fire once their threshold passes. Replaces the
/// old hand-rolled `t + phase: u8` match ladders in the fold states.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sequencer {
    t: f32,
    /// Number of threshold entries already fired.
    fired: usize,
}

impl Sequencer {
    fn start(rig: &mut AnimRig, pieces: &BytePieces, timeline: FoldTimeline) -> Self {
        timeline.fire(rig, pieces, 0);
        Self { t: 0.0, fired: 0 }
    }

    /// Advance, firing each timeline entry whose threshold has passed.
    /// Returns true once the choreography is complete.
    fn update(
        &mut self,
        rig: &mut AnimRig,
        pieces: &BytePieces,
        timeline: FoldTimeline,
        dt: f32,
    ) -> bool {
        self.t += dt;
        let thresholds = timeline.thresholds();
        while self.fired < thresholds.len() && self.t >= thresholds[self.fired] {
            self.fired += 1;
            timeline.fire(rig, pieces, self.fired);
        }
        self.t >= timeline.finish()
    }
}

/// Fold lifecycle. Driving is only allowed in [`FoldState::Closed`];
/// firing only in [`FoldState::Open`].
#[derive(Debug, Clone, Copy, PartialEq)]
enum FoldState {
    /// Folded. May drive. Unfolds when a target appears while stationary.
    Closed,
    /// Unfold choreography in flight.
    Opening(Sequencer),
    /// Fully unfolded. May fire. Folds after `IDLE_CLOSE_DELAY` without
    /// a target, or immediately on a move order.
    Open,
    /// Fold choreography in flight.
    Closing(Sequencer),
}

pub struct ByteAnim {
    state: FoldState,
    /// Seconds since a target was last visible (drives the idle fold).
    since_target: f32,
    pieces: BytePieces,
    /// Death choreography window (the `busy()` source).
    death: DeathFx,
}

impl ByteAnim {
    fn enter_opening(&mut self, rig: &mut AnimRig) {
        let seq = Sequencer::start(rig, &self.pieces, FoldTimeline::Open);
        self.state = FoldState::Opening(seq);
    }

    fn enter_closing(&mut self, rig: &mut AnimRig) {
        let seq = Sequencer::start(rig, &self.pieces, FoldTimeline::Close);
        self.state = FoldState::Closing(seq);
    }
}

impl Default for ByteAnim {
    fn default() -> Self {
        Self {
            state: FoldState::Closed,
            since_target: f32::INFINITY,
            pieces: BytePieces::default(),
            death: DeathFx::default(),
        }
    }
}

impl UnitAnim for ByteAnim {
    fn bind(&mut self, rig: &AnimRig) {
        self.pieces = BytePieces::bind(rig);
    }

    fn create(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Create(): TurnNow bp0..3 x ←16380 = 90°; MoveNow launcher_arm
        // y ←9830400 = 150 elmos; launcher1..5 x ←5460 = 30°, y
        // ←-3276/1638/0/1638/3276 = -18/9/0/9/18°. Spawns folded.
        let p = &self.pieces;
        for bp in p.bp {
            rig.turn_deg(bp, Axis::X, 90.0, 0.0);
        }
        rig.move_to(p.launcher_arm, Axis::Y, 150.0, 0.0);
        for (i, yaw) in [-18.0, 9.0, 0.0, 9.0, 18.0].iter().enumerate() {
            rig.turn_deg(p.launcher[i], Axis::X, 30.0, 0.0);
            rig.turn_deg(p.launcher[i], Axis::Y, *yaw, 0.0);
        }
    }

    fn update(&mut self, rig: &mut AnimRig, ctx: AnimCtx) {
        // Create()'s emerge loop: base y ← -40·pct/100 (bytecode
        // -2621440 = -40 elmos) while still emerging.
        if ctx.emerging {
            super::emerge_lift(rig, self.pieces.base, 40.0, ctx.build_percent);
        }
        self.death.tick(ctx.dt);

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
            FoldState::Opening(mut seq) => {
                rig.move_gate = 0.0;
                if ctx.moving {
                    // Move order mid-unfold: fold straight back up; the
                    // rig re-targets the in-flight pieces smoothly.
                    self.enter_closing(rig);
                    return;
                }
                if seq.update(rig, &self.pieces, FoldTimeline::Open, ctx.dt) {
                    self.state = FoldState::Open;
                } else {
                    self.state = FoldState::Opening(seq);
                }
            }
            FoldState::Open => {
                rig.move_gate = 0.0;
                if ctx.moving || self.since_target > IDLE_CLOSE_DELAY {
                    self.enter_closing(rig);
                }
            }
            FoldState::Closing(mut seq) => {
                rig.move_gate = 0.0;
                if seq.update(rig, &self.pieces, FoldTimeline::Close, ctx.dt) {
                    self.state = FoldState::Closed;
                } else {
                    self.state = FoldState::Closing(seq);
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
        rig.turn_deg(self.pieces.aimer, Axis::X, -90.0 - p.to_degrees(), 270.0);
        rig.turn_rad(self.pieces.aimer, Axis::Y, h, deg2rad(270.0));
        true
    }

    fn fire(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit 1024 from bp{gp}, then cycle static[1]
        // gp 0→1→2→3→0. The host fires one shot per call; the volley
        // pacing (sleep 90/150) is the weapon cooldown here.
        let idx = self_cycle(&mut rig.muzzle, 4);
        rig.emit(self.pieces.bp[idx], super::super::SfxKind::Puff);
    }

    fn killed(&mut self, rig: &mut AnimRig, _ctx: AnimCtx) {
        // Killed(): hide blade0..3, explode blade0..3 severity 1.
        for blade in self.pieces.blade {
            rig.hide(blade);
            rig.explode(blade, 1);
        }
        self.death.start();
    }

    fn busy(&self) -> bool {
        self.death.busy()
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
