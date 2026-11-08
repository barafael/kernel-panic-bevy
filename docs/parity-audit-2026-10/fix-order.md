# Fix order for the parity gaps found by the 2026-11-08 review

Ordered to keep churn low: leaves first (no other fix touches them), then
the shared subsystems in dependency order, so nothing is written twice.
Every batch ends with `cargo check`, the unit tests of the touched crate,
and a full `cargo test` before the goldens are re-recorded
(`SIM_VERSION` bumps invalidate them; re-record once per batch that
changed the sim, not once per fix).

## Batch 0: first review pass (committed, SIM_VERSION 21)

Reload freeze while stunned, no hits past 0 HP, kamikaze/self-destruct
paused while stunned, Firewall 640 frames over a ground cylinder, Flow
starburst range, `sign(0)`, CEG particle axes, DOS flare, recharge label,
dying units unselectable, `dyn_damage_multiplier`.

## Batch 1: leaves (input, HUD, AI, small sim rules)

No dependency on each other or on the sensor work.

- Homebases start on Repeat (the AI switches it off).
- A Shift-armed order mode ends with Shift.
- Control groups: group 0, one group per unit, Ctrl+Shift add, Alt toggle,
  second press focuses the camera.
- Minimap input: left click/box select, middle pans, right-click and armed
  orders at the mapped point (`MinimapPointer` is read by `ground_hit` /
  `unit_hit`).
- Health bars on every damaged unit.
- AI runs for any team that still owns a factory.
- Explosion fronts that arrive after the attacker is gone have no attacker.
- NX zone only while its Pointer still exists.
- An infected Packet entering a Port / Bug-Exploit morph releases its Virus.
- Kamikaze scans on the unit's own slow update, strict `<`.
- Worm / Logic Bomb cloak only once built (Worm 193 frames after).
- SIGTERM release window 12 frames; Signal flies as an engine bomber.
- Obelisk Infection: high-arc shell, 200/1500 impact blast, then the gas.

## Batch 2: sensors (one subsystem, so one batch)

Order inside: per-team LOS/radar sets -> targeting gate -> attack-order
tracking -> radar contacts (error, priority) -> nanoframes in the index ->
nanoframe health and ARMORED -> minimap radar dots / ghosts.
Everything below depends on it (idle engagement needs "visible enemy",
target priority needs ARMORED nanoframes, Guard/Fight need both).

## Batch 3: command-AI behaviour (needs batch 2)

Target priority (`GenerateWeaponTargets` score), idle Maneuver engagement
(Pointer exempt), Fight diverts, Guard retaliates, `TARGET_LOST` rules for
mobile targets, right-click Guard / Repair and builders' repair.

## Batch 4: movement costs

Water price in speed and path cost, horizontal speed regulation on slopes
(both change every replay, so they share a re-record).

## Batch 5: weapons detail

DOS sub-beams with range falloff, arc line of fire against terrain,
Worm bite range to the near surface, yardmap facing, morph timing.

## Left for the end / not attempted

See the list in the final report.
