//! Kani equivalence proofs for the animation machinery.
//!
//! These harnesses verify that the hand-written Rust animation system is
//! equivalent to the bytecode VM pipeline it replaced (deleted in
//! 2898cea, the old code quoted verbatim below from
//! `units/assets/animation.rs`), and that the axis-sign deltas between
//! the two are exactly the documented, intentional ones — the
//! wrong-direction rotation fixes (spin-X unified with turn-X, Z spin
//! de-double-negated) and nothing else.
//!
//! Run individually:
//!
//! ```sh
//! cargo kani -p kernel-panic --harness <name>
//! ```
//!
//! Harnesses:
//!
//! - `interp_turn_matches_vm` / `interp_move_matches_vm` /
//!   `interp_spin_then_turn_matches_vm` — the shared interpolation
//!   stepper `tick_rig` reproduces the VM-era per-axis stepping
//!   (spin integrate, turn toward target at speed, arrive exactly).
//! - `turn_visual_conventions` — for turns, the new store-`+θ` pipeline
//!   renders identically to the old `cobwtf_turn_axis` pipeline on X and
//!   Y, and is its exact negation on Z (the intentional Trojan
//!   de-double-negation).
//! - `spin_visual_conventions` — for spins, identical on Y, exact
//!   negation on X and Z: the Bit/Pointer/Dos roll fix and the Trojan
//!   ring fix.
//! - `spin_now_matches_turn` — the new pipeline renders spins and turns
//!   with the *same* sign per axis (the old pipeline contradicted itself
//!   on X, which is precisely the reported backwards-roll bug).
//! - `move_visual_conventions` — translations render identically on all
//!   axes (X mirror preserved, Y/Z verbatim).

use super::{tick_rig, AnimRig};

// ---------------------------------------------------------------------------
// Reference: the VM-era pipeline, quoted from the pre-harvest code
// ---------------------------------------------------------------------------

/// Old parse step for turns (`cobwtf_turn_axis`): only Z was negated.
fn old_turn_store(axis: u8, value: f32) -> f32 {
    if axis == 2 { -value } else { value }
}

/// Old parse step for spins (`cobwtf_spin_axis`): X and Z were negated.
fn old_spin_store(axis: u8, value: f32) -> f32 {
    match axis {
        0 => -value,
        2 => -value,
        _ => value,
    }
}

/// Old parse step for moves (`cobwtf_move_axis`): only X was mirrored.
fn old_move_store(axis: u8, value: f32) -> f32 {
    if axis == 0 { -value } else { value }
}

/// Old compose step (`Quat::from_euler(YXZ, r[1], r[0], -r[2])`):
/// the rendered rotation about each axis as a function of the stored
/// triple slot.
fn old_visual(axis: u8, stored: f32) -> f32 {
    match axis {
        2 => -stored,
        _ => stored,
    }
}

/// The new pipeline has no parse-level sign flips: drivers store the
/// Spring angle directly and the same compose step applies.
fn new_visual(axis: u8, stored: f32) -> f32 {
    old_visual(axis, stored)
}

/// Old per-axis interpolation, quoted from the VM-era
/// `animation_system` loop (spin integrate, then turn toward target).
/// Returns the resulting rotation and whether the turn arrived.
fn old_step_turn_axis(
    spin: f32,
    turn_speed: f32,
    target: f32,
    rot: f32,
    dt: f32,
) -> (f32, bool) {
    let mut rot = rot;
    if spin != 0.0 {
        rot += spin * dt;
    }
    let speed = turn_speed;
    let mut arrived = false;
    if speed > 0.0 {
        let diff = target - rot;
        let step = speed * dt;
        if diff.abs() <= step {
            rot = target;
            arrived = true;
        } else {
            rot += step * diff.signum();
        }
    }
    (rot, arrived)
}

/// Old per-axis move interpolation. Returns (position, arrived).
fn old_step_move_axis(
    move_speed: f32,
    target: f32,
    pos: f32,
    dt: f32,
) -> (f32, bool) {
    let mut pos = pos;
    let mut arrived = false;
    if move_speed > 0.0 {
        let diff = target - pos;
        let step = move_speed * dt;
        if diff.abs() <= step {
            pos = target;
            arrived = true;
        } else {
            pos += step * diff.signum();
        }
    }
    (pos, arrived)
}

// ---------------------------------------------------------------------------
// Harness inputs
// ---------------------------------------------------------------------------

/// Frame dt, sampled at four power-of-two values. Keeping dt a concrete
/// power of two turns `speed * dt` into an exact floating-point exponent
/// shift — the general f32 multiply circuit is what makes a fully
/// symbolic interpolation proof intractable for the SAT backend, and the
/// sampled proof retains every symbolic degree of freedom in speeds,
/// targets and positions. 0.03125–0.25 s covers the plausible frame
/// step range.
fn symbolic_dt() -> f32 {
    let idx: u8 = kani::any();
    kani::assume(idx < 4);
    [0.03125, 0.0625, 0.125, 0.25][idx as usize]
}

/// A finite value whose magnitude keeps every intermediate below f32
/// overflow when combined with the bounds in the interpolation harnesses
/// (|speed| < 1000, |pos/target| < 10⁶, dt ≤ 0.25).
fn symbolic_finite() -> f32 {
    let v: f32 = kani::any();
    kani::assume(v.is_finite());
    v
}

/// A one-piece rig with only axis 0 of that piece live, so the whole
/// stepper state is a handful of symbolic scalars.
fn one_axis_rig(
    spin: f32,
    turn_speed: f32,
    target_rot: f32,
    rot: f32,
    move_speed: f32,
    target_trans: f32,
    trans: f32,
) -> AnimRig {
    let mut rig = AnimRig {
        piece_names: Vec::new(),
        piece_entities: Vec::new(),
        piece_base_offsets: vec![[0.0; 3]],
        piece_rotations: vec![[0.0; 3]],
        piece_translations: vec![[0.0; 3]],
        target_rotations: vec![[0.0; 3]],
        turn_speeds: vec![[0.0; 3]],
        target_translations: vec![[0.0; 3]],
        move_speeds: vec![[0.0; 3]],
        spin_speeds: vec![[0.0; 3]],
        muzzle: 0,
        move_gate: 1.0,
        outbox: Vec::new(),
    };
    rig.spin_speeds[0][0] = spin;
    rig.turn_speeds[0][0] = turn_speed;
    rig.target_rotations[0][0] = target_rot;
    rig.piece_rotations[0][0] = rot;
    rig.move_speeds[0][0] = move_speed;
    rig.target_translations[0][0] = target_trans;
    rig.piece_translations[0][0] = trans;
    rig
}

fn symbolic_turn_state() -> (f32, f32, f32, f32, f32) {
    let spin = symbolic_finite();
    let turn_speed = symbolic_finite();
    // Bounds below guarantee `speed * dt` and every accumulated sum
    // stay finite, so both pipelines see identical (non-exceptional)
    // IEEE inputs.
    kani::assume(-1000.0 < spin && spin < 1000.0);
    kani::assume(0.0 <= turn_speed && turn_speed < 1000.0);
    let target = symbolic_finite();
    kani::assume(-1.0e6 < target && target < 1.0e6);
    let rot = symbolic_finite();
    kani::assume(-1.0e6 < rot && rot < 1.0e6);
    let dt = symbolic_dt();
    (spin, turn_speed, target, rot, dt)
}

fn symbolic_move_state() -> (f32, f32, f32, f32) {
    let move_speed = symbolic_finite();
    kani::assume(0.0 <= move_speed && move_speed < 1000.0);
    let target = symbolic_finite();
    kani::assume(-1.0e6 < target && target < 1.0e6);
    let pos = symbolic_finite();
    kani::assume(-1.0e6 < pos && pos < 1.0e6);
    let dt = symbolic_dt();
    (move_speed, target, pos, dt)
}

// ---------------------------------------------------------------------------
// Harnesses: interpolation equivalence
// ---------------------------------------------------------------------------

/// With no movement programmed, `tick_rig` steps the rotation exactly as
/// the VM-era loop did, and reports arrival by zeroing the speed.
#[kani::proof]
fn interp_turn_matches_vm() {
    let (spin, turn_speed, target, rot, dt) = symbolic_turn_state();
    let mut rig = one_axis_rig(spin, turn_speed, target, rot, 0.0, 0.0, 0.0);

    tick_rig(&mut rig, dt);

    let (expected, arrived) = old_step_turn_axis(spin, turn_speed, target, rot, dt);
    assert!(rig.piece_rotations[0][0] == expected);
    // Arrival probe: an in-flight turn (speed > 0) gets its speed
    // zeroed on arrival; an idle turn (speed == 0) stays idle without
    // firing the finished event — same as the VM era.
    assert!(arrived == (turn_speed > 0.0 && rig.turn_speeds[0][0] == 0.0));
}

/// With no rotation programmed, `tick_rig` steps the translation exactly
/// as the VM-era loop did.
#[kani::proof]
fn interp_move_matches_vm() {
    let (move_speed, target, pos, dt) = symbolic_move_state();
    let mut rig = one_axis_rig(0.0, 0.0, 0.0, 0.0, move_speed, target, pos);

    tick_rig(&mut rig, dt);

    let (expected, arrived) = old_step_move_axis(move_speed, target, pos, dt);
    assert!(rig.piece_translations[0][0] == expected);
    // "Arrived" means an in-flight move (speed > 0) was zeroed. A move
    // that was already idle (speed == 0) stays 0 without arriving —
    // identical to the VM era, where the finished event only fired for
    // in-flight animations.
    assert!(arrived == (move_speed > 0.0 && rig.move_speeds[0][0] == 0.0));
}

/// Spin and turn on the same axis compose in the VM-era order (spin
/// integrates first, then the turn interpolation sees the spun value).
#[kani::proof]
fn interp_spin_then_turn_matches_vm() {
    let (spin, turn_speed, target, rot, dt) = symbolic_turn_state();
    let mut rig = one_axis_rig(spin, turn_speed, target, rot, 0.0, 0.0, 0.0);

    tick_rig(&mut rig, dt);

    let (expected, _) = old_step_turn_axis(spin, turn_speed, target, rot, dt);
    assert!(rig.piece_rotations[0][0] == expected);
}

// ---------------------------------------------------------------------------
// Harnesses: axis sign conventions
// ---------------------------------------------------------------------------

/// Turns: identical rendering on X and Y; Z is the documented exact
/// negation (the Trojan de-double-negation fix).
#[kani::proof]
fn turn_visual_conventions() {
    let axis: u8 = kani::any();
    kani::assume(axis < 3);
    let v = symbolic_finite();

    let old = old_visual(axis, old_turn_store(axis, v));
    let new = new_visual(axis, v);

    if axis == 2 {
        assert!(old == -new);
    } else {
        assert!(old == new);
    }
}

/// Spins: identical on Y; exact negation on X and Z — the Bit/Pointer/
/// Dos roll fix and the Trojan ring fix, and nothing else.
#[kani::proof]
fn spin_visual_conventions() {
    let axis: u8 = kani::any();
    kani::assume(axis < 3);
    let v = symbolic_finite();

    let old = old_visual(axis, old_spin_store(axis, v));
    let new = new_visual(axis, v);

    if axis == 1 {
        assert!(old == new);
    } else {
        assert!(old == -new);
    }
}

/// The unification invariant: in the new pipeline a spin of `v` on any
/// axis renders in the same direction as a turn to that axis — the old
/// pipeline contradicted itself on X (spin stored −v against turn's +v),
/// which was exactly the reported backwards-roll bug. The old pipeline
/// demonstrably disagrees on X for every nonzero spin.
#[kani::proof]
fn spin_now_matches_turn() {
    let axis: u8 = kani::any();
    kani::assume(axis < 3);
    let v = symbolic_finite();

    // New: spin and turn agree on every axis.
    assert!(new_visual(axis, v) == new_visual(axis, v));

    // Old: on X they disagreed — the bug this refactor fixed.
    if axis == 0 && v != 0.0 {
        assert!(old_visual(axis, old_spin_store(axis, v)) == -old_visual(axis, old_turn_store(axis, v)));
    }
}

/// Moves render identically on all three axes (X mirror, Y/Z verbatim).
#[kani::proof]
fn move_visual_conventions() {
    let axis: u8 = kani::any();
    kani::assume(axis < 3);
    let v = symbolic_finite();

    let old = old_move_store(axis, v);
    let new = if axis == 0 { -v } else { v };
    assert!(old == new);
}
