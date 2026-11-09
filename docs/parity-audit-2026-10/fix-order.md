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

## Status (2026-11-09)

Batch 0 is commit 03c1723 (SIM_VERSION 21), batches 1-4 are 6755798
(SIM_VERSION 22), and batch 5 plus the follow-up pass are 6233d1c
(SIM_VERSION 23), which adds builder repair (`Order::Repair`,
right-click on a damaged friend), the timed Bug <-> Exploit morph
(`morphable` / `idle` gates, 88 silent frames, heading and queue kept),
the Virus release on a finished infected frame, melee range to the target's
surface, DOS/Gauss sub-beams with falloff, yardmap rotation by build
facing, no datavent reservation by orders (the AI tracks its own
constructors' sites), ghosted buildings and minimap radar blips.

## Fourth pass (SIM_VERSION 24)

- A Worm's radar and seismic sense start 152 frames after it is built
  (`SensorsPending`, worm.bos `ACTIVATION`; `LosHandler.cpp:182` blocks
  every sensor of a deactivated unit).
- The Bug/Exploit morph's result is off the blocking map
  (`Unblocked`, `SetUnitBlocking(nu, false)` in `LuaCOB.lua`): nothing
  collides with, avoids or paths round it, an Exploit is never stamped on
  the structure layer, a Bug blocks again once a slow update finds it on
  another map square (`CMoveType::SlowUpdate`).
- A Worm holding fire (cloaked, AutoHold on = `STANDINGFIREORDERS 0`)
  neither engages while idle nor diverts a Fight. It is the only Fire
  State the game ever sets, so the Fire State gap is closed.
- Enter is a targeted command (`Mode::Enter`, `CMDTYPE.ICON_UNIT`): the
  `E` button / hotkey arms it and the click on an own teleporter sends the
  selected Packets in. Anything else is ignored and the mode stays armed.

## Fifth pass (SIM_VERSION 25)

- Shift-queued gadget commands: `SimCommand::Cast` has a `queue` flag; a
  busy unit queues `QueuedCommand::Cast`, which on promotion becomes a
  `QueuedCast` -> `CommandFireEvent { queued: true }` (the approach walk
  and the NX Flag keep the rest of the queue) or a Dispatch order. Idle
  units cast at once, as Spring runs a Lua command at an empty queue.
- A builder resumes its own unfinished nanoframe with Repair
  (`BuildDriven::solo_builder`, set when it started the frame); the
  order walks into build range, then becomes `Constructing` on the frame.
- Enter is refused on a stunned teleporter (`AllowCommand`).
- The morph shows the scripts' flip: the Bug hides its feet, rises 8,
  turns over and sinks 12 and shows clamps and turret; the Exploit waits
  out the 67-frame dissolve, hides them, rises, turns back and lowers
  (`UnitAnim::morph`). The alpha dissolve itself is not drawn (no
  alpha-threshold path for unit materials).
- Ghosted buildings are drawn translucent (`Ghosted`, alpha 0.6) and one
  that dies unseen leaves a `DeadGhost` copy of its model until a friendly
  unit sees the spot; the dying unit is hidden, so its death no longer
  shows through the fog. Rendering only; not seen on screen in a
  running game yet, only unit-tested.

Checked and not gaps:

- Repair on someone else's nanoframe: Kernel Panic's constructors are
  `CanAssist=0`, so a frame has a `soloBuilder` and `ExecuteRepair`
  refuses every other builder (`canRepairUnit &= !unit->soloBuilder ||
  unit->soloBuilder == owner`).
- A Packet's Enter on an enemy teleporter is refused
  (`network_enter.lua` `AllowCommand`; `apply.rs` drops it). A plain
  right-click on one is the engine's default Attack, as here.
- Minimap-drawn formations: the right-drag path samples through
  `ground_hit`, which reads the minimap pointer, so a line drawn on the
  minimap is a formation line like one drawn in the world.
- Fight resume: the engine resumes the same Fight goal; only its leash
  line (`commandPos1/2`, roaming limit) differs, which has no effect on
  the movement.

## Still open (observed in-parities)

- The morph's alpha dissolve is not drawn.
- Unlit unit/terrain materials (no sun/ambient, Bevy tonemapping): a look
  decision, not changed.
- KPAI flavour: the Fair budget runs at every difficulty (a design choice
  of the remake's slack ladder).
- The Signal's flight constants follow the engine source but were not
  compared with the original's flight time (needs the original running).
