//! byte.cob — the heavy defensive rotor, translated from the *compiled
//! bytecode* (dumped via the spring-cob parser). Values below are
//! bytecode literals converted to game units: angles = raw/65536·360
//! degrees, translations = raw/65536 elmos. Byte compiles with Scriptor
//! linear constant 163840, so its `.bos` source brackets are 2.5× the
//! bytecode values — do NOT re-translate from byte.bos.
//!
//! Fold lifecycle, as the script has it: folded by default; `AimWeapon1`
//! on a folded Byte starts `Open()` (base lifts 60, blades fan ±10,
//! rotor steps once to 45°) and reports "not aimed" until `isOpen`;
//! `Close()` folds back up 3 s after the last aim. The script has no
//! `StartMoving`/`StopMoving` and never touches `MAX_SPEED`: the Byte
//! drives while open, opens while driving and fires on the move. The
//! octahedron simply slides and turns — no roll, no spin.
//!
//! `HitByWeaponId` returns 30 % while `!isOpen`; `isOpen` is set at the
//! end of `Open()` and cleared only once `Close()` has brought the aimer
//! back to rest, so the armour is off during that return.

use super::super::{AnimCtx, AnimRig, Axis, UnitAnim, deg2rad};
use super::DeathFx;

/// Close(): `sleep 3000` before folding after the last aim.
const IDLE_CLOSE_DELAY: f32 = 3.0;

// Bytecode speeds and targets.
/// Open(): `move base to y-axis [3932160]=60 speed [7864320]=120`.
const BASE_LIFT: f32 = 60.0;
const BASE_LIFT_SPEED: f32 = 120.0;
/// Open(): blades → ±[655360]=10 @40 (only blade0 is waited for).
const BLADE_SPREAD: f32 = 10.0;
const BLADE_SPEED: f32 = 40.0;
/// Open(): rotor steps once to ←8190 = 45° @90°/s, not waited for.
const ROTOR_OPEN_DEG: f32 = 45.0;
const ROTOR_OPEN_SPEED: f32 = 90.0;
/// Close(): aimer x/y → 0 @70°/s, both waited for.
const CLOSE_AIMER_SPEED: f32 = 70.0;
/// Close(): rotor → 0 @480°/s and base → 0 @300, not waited for.
const CLOSE_ROTOR_SPEED: f32 = 480.0;
const CLOSE_BASE_SPEED: f32 = 300.0;
/// `wait-for-move` / `wait-for-turn` resume the thread on the frame
/// after the animation finishes.
const WAIT_LATENCY: f32 = 1.0 / 30.0;

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

/// Seconds a `move` needs from the piece's current offset on `axis`
/// (the rig stores X mirrored, as `move_to` does).
fn move_time(rig: &AnimRig, piece: usize, axis: Axis, target: f32, speed: f32) -> f32 {
    let target = if axis == Axis::X { -target } else { target };
    let current = rig
        .piece_translations
        .get(piece)
        .map_or(0.0, |t| t[axis as usize]);
    (target - current).abs() / speed
}

/// Seconds a `turn` needs from the piece's current angle on `axis`
/// (degrees per second).
fn turn_time(rig: &AnimRig, piece: usize, axis: Axis, target_deg: f32, speed_dps: f32) -> f32 {
    let current = rig
        .piece_rotations
        .get(piece)
        .map_or(0.0, |r| r[axis as usize].to_degrees());
    (target_deg - current).abs() / speed_dps
}

/// Which fold choreography a [`Sequencer`] runs. `fire(i)` issues entry
/// `i`'s `.bos` commands: entry 0 when the sequencer starts, entries
/// 1..N when their `wait-for-*` completes, and the thread is done once
/// `t >= finish`.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FoldTimeline {
    Open,
    Close,
}

impl FoldTimeline {
    fn fire(self, rig: &mut AnimRig, p: &BytePieces, i: usize) {
        match (self, i) {
            // Open() entry 0: move base to y 60 speed 120; wait-for-move.
            (FoldTimeline::Open, 0) => {
                rig.move_to(p.base, Axis::Y, BASE_LIFT, BASE_LIFT_SPEED);
            }
            // Open() entry 1: rotor → 45° @90°/s (unwaited); blades →
            // ±10 @40; wait-for-move blade0.
            (FoldTimeline::Open, 1) => {
                rig.turn_deg(p.rotor, Axis::Y, ROTOR_OPEN_DEG, ROTOR_OPEN_SPEED);
                rig.move_to(p.blade[0], Axis::Z, BLADE_SPREAD, BLADE_SPEED);
                rig.move_to(p.blade[1], Axis::X, BLADE_SPREAD, BLADE_SPEED);
                rig.move_to(p.blade[2], Axis::Z, -BLADE_SPREAD, BLADE_SPEED);
                rig.move_to(p.blade[3], Axis::X, -BLADE_SPREAD, BLADE_SPEED);
            }
            // Close() entry 0 (after `sleep 3000`): aimer x/y → 0 @70°/s;
            // wait-for-turn both.
            (FoldTimeline::Close, 0) => {
                rig.turn_deg(p.aimer, Axis::X, 0.0, CLOSE_AIMER_SPEED);
                rig.turn_deg(p.aimer, Axis::Y, 0.0, CLOSE_AIMER_SPEED);
            }
            // Close() entry 1: `isOpen = 0`; blades → 0 @40 (blade0/2 on
            // z, blade1/3 on x — the axes Open moved them on);
            // wait-for-move blade0.
            (FoldTimeline::Close, 1) => {
                rig.move_to(p.blade[0], Axis::Z, 0.0, BLADE_SPEED);
                rig.move_to(p.blade[1], Axis::X, 0.0, BLADE_SPEED);
                rig.move_to(p.blade[2], Axis::Z, 0.0, BLADE_SPEED);
                rig.move_to(p.blade[3], Axis::X, 0.0, BLADE_SPEED);
            }
            // Close() entry 2: rotor → 0 @480°/s, base → 0 @300, unwaited.
            (FoldTimeline::Close, 2) => {
                rig.turn_deg(p.rotor, Axis::Y, 0.0, CLOSE_ROTOR_SPEED);
                rig.move_to(p.base, Axis::Y, 0.0, CLOSE_BASE_SPEED);
            }
            _ => {}
        }
    }
}

/// A time-keyed command list whose thresholds come from the pieces'
/// *current* poses, like the script's `wait-for-move`/`wait-for-turn`:
/// re-opening a half-folded Byte only waits for what still has to move.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sequencer {
    timeline: FoldTimeline,
    t: f32,
    /// Elapsed times at which entries 1..=N fire; `thresholds[N]` is
    /// where the thread ends.
    thresholds: [f32; 3],
    /// Number of threshold entries already fired.
    fired: usize,
}

impl Sequencer {
    fn start(rig: &mut AnimRig, p: &BytePieces, timeline: FoldTimeline) -> Self {
        let thresholds = match timeline {
            FoldTimeline::Open => {
                let base =
                    move_time(rig, p.base, Axis::Y, BASE_LIFT, BASE_LIFT_SPEED) + WAIT_LATENCY;
                let blades = base
                    + move_time(rig, p.blade[0], Axis::Z, BLADE_SPREAD, BLADE_SPEED)
                    + WAIT_LATENCY;
                [base, blades, blades]
            }
            FoldTimeline::Close => {
                let aimer = turn_time(rig, p.aimer, Axis::X, 0.0, CLOSE_AIMER_SPEED)
                    .max(turn_time(rig, p.aimer, Axis::Y, 0.0, CLOSE_AIMER_SPEED))
                    + WAIT_LATENCY;
                let blades =
                    aimer + move_time(rig, p.blade[0], Axis::Z, 0.0, BLADE_SPEED) + WAIT_LATENCY;
                let rest =
                    blades + (ROTOR_OPEN_DEG / CLOSE_ROTOR_SPEED).max(BASE_LIFT / CLOSE_BASE_SPEED);
                [aimer, blades, rest]
            }
        };
        timeline.fire(rig, p, 0);
        Self {
            timeline,
            t: 0.0,
            thresholds,
            fired: 0,
        }
    }

    /// Advance, firing each entry whose wait has completed. Returns true
    /// once the thread has run to its end.
    fn update(&mut self, rig: &mut AnimRig, p: &BytePieces, dt: f32) -> bool {
        self.t += dt;
        let entries = match self.timeline {
            FoldTimeline::Open => 1,
            FoldTimeline::Close => 2,
        };
        while self.fired < entries && self.t >= self.thresholds[self.fired] {
            self.fired += 1;
            self.timeline.fire(rig, p, self.fired);
        }
        self.t >= self.thresholds[entries]
    }

    /// Close() only: `isOpen` is still set while the aimer returns.
    fn still_open(&self) -> bool {
        self.timeline == FoldTimeline::Close && self.t < self.thresholds[0]
    }
}

/// Fold lifecycle. Firing is only allowed in [`FoldState::Open`]; driving
/// is never gated.
#[derive(Debug, Clone, Copy, PartialEq)]
enum FoldState {
    /// Folded. Unfolds when a target appears.
    Closed,
    /// `Open()` in flight.
    Opening(Sequencer),
    /// Fully unfolded (`isOpen`). May fire. `Close()` starts after
    /// `IDLE_CLOSE_DELAY` without an aim.
    Open,
    /// `Close()` in flight; a new aim kills it.
    Closing(Sequencer),
}

pub struct ByteAnim {
    state: FoldState,
    /// Seconds since a target was last visible (drives the idle fold).
    since_target: f32,
    pieces: BytePieces,
    /// `static-var gp` — the barrel `QueryWeapon1` names (bp0..bp3),
    /// advanced by `FireWeapon1`'s sleeps between the salvo's shots.
    gp: usize,
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

    /// `AimWeapon1` arriving: a folded Byte starts `Open()`; a folding one
    /// whose `isOpen` is still set has its `Close()` killed and is open
    /// again at once (the aim itself re-turns the aimer).
    fn aim_requested(&mut self, rig: &mut AnimRig) {
        match self.state {
            FoldState::Closed => self.enter_opening(rig),
            FoldState::Closing(seq) if seq.still_open() => self.state = FoldState::Open,
            FoldState::Closing(_) => self.enter_opening(rig),
            FoldState::Opening(_) | FoldState::Open => {}
        }
    }
}

impl Default for ByteAnim {
    fn default() -> Self {
        Self {
            state: FoldState::Closed,
            since_target: f32::INFINITY,
            pieces: BytePieces::default(),
            gp: 0,
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
            self.aim_requested(rig);
        } else {
            self.since_target += ctx.dt;
        }

        match self.state {
            FoldState::Closed => {}
            FoldState::Opening(mut seq) => {
                if seq.update(rig, &self.pieces, ctx.dt) {
                    self.state = FoldState::Open;
                } else {
                    self.state = FoldState::Opening(seq);
                }
            }
            FoldState::Open => {
                if self.since_target > IDLE_CLOSE_DELAY {
                    self.enter_closing(rig);
                }
            }
            FoldState::Closing(mut seq) => {
                if seq.update(rig, &self.pieces, ctx.dt) {
                    self.state = FoldState::Closed;
                } else {
                    self.state = FoldState::Closing(seq);
                }
            }
        }
    }

    fn aim(&mut self, rig: &mut AnimRig, h: f32, p: f32, _ctx: AnimCtx) -> bool {
        // AimWeapon1: `if (!isOpen) { start-script Open(); return 0; }`
        self.aim_requested(rig);
        if self.state != FoldState::Open {
            return false;
        }
        // AimWeapon1: aimer x → (-16380) - p = -90° - p @270°/s, y → h
        // @270°/s. `h` arrives body-relative (Spring contract: world
        // heading minus body yaw), so the gun tracks the target
        // regardless of which way the hull is facing.
        rig.turn_deg(self.pieces.aimer, Axis::X, -90.0 - p.to_degrees(), 270.0);
        rig.turn_rad(self.pieces.aimer, Axis::Y, h, deg2rad(270.0));
        true
    }

    fn shot(&mut self, rig: &mut AnimRig) {
        // QueryWeapon1: `if (gp==k) piecenum=bpk`. `rig.muzzle` is a
        // piece index — the old driver wrote the 0..3 slot number into
        // it, so every shot after the first left from base/aimer/rotor.
        rig.muzzle = self.pieces.bp[self.gp % 4];
    }

    fn fire(&mut self, _rig: &mut AnimRig, _ctx: AnimCtx) {
        // FireWeapon1(): emit 1024 from bp{gp}, then `gp` steps
        // 0→1→2→3→0 (sleep 90 / 150 between barrels). The host calls
        // this once per burst shot, after that shot's `QueryWeapon1`,
        // so each shot leaves — and flashes — at the next barrel.
        let idx = self.gp % 4;
        // (Rendered by combat, not here: the `emit-sfx 1024+i` CEG comes
        // out of `fire_weapon_sfx` → `ProjectileFxEvent::muzzle_ceg` at
        // the shot's resolved muzzle. Emitting it from the rig as well
        // drew a second, generic faction puff on every shot.)
        self.gp = (idx + 1) % 4;
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

    /// `isOpen`: set when `Open()` completes, cleared once `Close()` has
    /// returned the aimer to rest.
    fn is_open(&self) -> Option<bool> {
        Some(match self.state {
            FoldState::Open => true,
            FoldState::Closing(seq) => seq.still_open(),
            FoldState::Closed | FoldState::Opening(_) => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIECES: &[&str] = &[
        "base",
        "aimer",
        "rotor",
        "blade0",
        "blade1",
        "blade2",
        "blade3",
        "bp0",
        "bp1",
        "bp2",
        "bp3",
        "launcher_arm",
        "launcher1",
        "launcher2",
        "launcher3",
        "launcher4",
        "launcher5",
    ];

    fn byte() -> (ByteAnim, AnimRig) {
        let mut rig = AnimRig::for_test(PIECES);
        let mut anim = ByteAnim::default();
        anim.bind(&rig);
        anim.create(&mut rig, AnimCtx::minimal());
        (anim, rig)
    }

    fn step(anim: &mut ByteAnim, rig: &mut AnimRig, frames: usize, aiming: bool, moving: bool) {
        for _ in 0..frames {
            let ctx = AnimCtx {
                dt: 1.0 / 30.0,
                aim_active: aiming,
                moving,
                ..AnimCtx::minimal()
            };
            anim.update(rig, ctx);
            super::super::super::tick_rig(rig, 1.0 / 30.0);
        }
    }

    /// Open() takes the base lift (0.5 s) plus the blade fan (0.25 s);
    /// the Byte is `isOpen` and may aim just after 0.75 s — moving or not.
    #[test]
    fn opens_in_three_quarters_of_a_second_while_driving() {
        let (mut anim, mut rig) = byte();
        assert!(!anim.aim(&mut rig, 0.0, 0.0, AnimCtx::minimal()));
        step(&mut anim, &mut rig, 20, true, true);
        assert_eq!(anim.is_open(), Some(false));
        step(&mut anim, &mut rig, 6, true, true);
        assert_eq!(anim.is_open(), Some(true));
        assert!(anim.aim(&mut rig, 0.3, 0.1, AnimCtx::minimal()));
        assert_eq!(rig.move_gate, 1.0);
    }

    /// Close() keeps `isOpen` while the aimer swings back and clears it
    /// when the blades fold; an aim during the return reopens at once.
    #[test]
    fn close_keeps_armour_off_until_blades_fold_and_aim_cancels_it() {
        let (mut anim, mut rig) = byte();
        step(&mut anim, &mut rig, 30, true, false);
        assert!(anim.aim(&mut rig, 0.0, deg2rad(-30.0), AnimCtx::minimal()));
        step(&mut anim, &mut rig, 10, true, false);
        // 3 s idle: Close() begins; aimer x is at -60°, 70°/s ≈ 0.86 s.
        step(&mut anim, &mut rig, 92, false, false);
        assert!(matches!(anim.state, FoldState::Closing(_)));
        assert_eq!(anim.is_open(), Some(true));
        step(&mut anim, &mut rig, 30, false, false);
        assert_eq!(anim.is_open(), Some(false));
        // A new target while folding: Open() restarts from the half-fold.
        step(&mut anim, &mut rig, 1, true, false);
        assert!(matches!(anim.state, FoldState::Opening(_)));
    }
}
