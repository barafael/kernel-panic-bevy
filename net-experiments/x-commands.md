# X-commands: player input as serializable, tick-scheduled sim commands

Experiment branch `worktree-agent-a00e3ba806652b7c0` (base `main` @ efd0724).
Code: `kernel-panic/src/net/` (`mod.rs` types and bus, `apply.rs` sim side,
`codec.rs` wire shapes, `lockstep_tests.rs` the in-process lockstep tests and
the size measurements).

**Summary.** All lockstep, host and server designs need the same thing:
everything a player (or the AI) does to the simulation becomes a `SimCommand`
that names units by a stable `NetId`. Commands travel in a per-player,
per-tick `CommandFrame`, and one exclusive system at the head of the fixed
tick applies them. In this branch every gameplay input path goes through the
bus: right-click, the order modes, abilities, the command panel, the build
bar, placement, factory queues, self-destruct and the in-sim AI. The only
exceptions are dev tools. All 448 kernel-panic tests pass. Three independent
sim worlds, one per player plus a spectator, each with different `Entity`
layouts, reach identical state hashes on every tick. This holds for input
delays 1/3/6 and wire latencies 0/2/5 ticks, with and without the in-sim AI.
Getting there exposed one real determinism bug (`GroundMover` slow-update
phase keyed on `Entity` index), which the branch fixes. Typical frames are
5–70 bytes.

## 1. Inventory: who mutates sim state from input (efd0724 line numbers)

Legend: **P** player command (must travel), **L** local-only, **S** in-sim
(deterministic, runs on every peer), **D** dev tool. "Branch" says what this
branch does with it.

| # | Path (file:line @ efd0724) | What it mutates | Class | Branch |
|---|---|---|---|---|
| 1 | `interaction/selection/right_click.rs:148` `handle_right_click`, click on enemy (:312), attack-drag on enemy (:252) | `AttackTargetOrder` via `apply_ordered_command` (:416, which calls `replace_order` or `queue_command`) | P | `Order::AttackUnit` |
| 2 | same, attack-drag on ground (:269) | `AttackGroundOrder` | P | `Order::AttackGround` |
| 3 | same, plain click (:341) | `MoveTarget` and the queue for the whole selection | P | `Order::Move` |
| 4 | same, single-unit drawn path (:356, :366) | move plus queued moves | P | `SimCommand::Path` |
| 5 | same, drawn formation (:395) | one move per unit (Hungarian / no-cross assignment) | P | `SimCommand::PerUnit` |
| 6 | `interaction/ability.rs:363` attack-ground click (A) | as 2 | P | `Order::AttackGround` |
| 7 | `ability.rs:406` move click (M) | as 3 | P | `Order::Move` |
| 8 | `ability.rs:600` fight click (F) | `MoveTarget` + `AttackMoveActive` | P | `Order::AttackMove` |
| 9 | `ability.rs:509` patrol click (P), :548/:553 | patrol leg and route queue, reads unit pos and queue | P | `Order::Patrol` (route computed at apply time) |
| 10 | `ability.rs:648` guard click (G) | `GuardTarget` | P | `Order::Guard` |
| 11 | `ability.rs:454` set-target click (T), :491 | `ForcedTarget` | P | `SetTarget` |
| 12 | `ability.rs:274` D on aimed casters, `:314` ability button click, `cast_aimed_abilities` :201/:209/:211 | `CommandFireEvent`, `DispatchEvent`, `AutoDispatch` (Alt) | P | `SimCommand::Cast` |
| 13 | `ability.rs:257` D deploy, `deploy_units` :176 | `DeployEvent` (Bug↔Exploit) | P | `SimCommand::Deploy` |
| 14 | `ui/hud/command_panel/activation.rs:194` `apply_activations`: Stop :154 | `clear_orders` and drops `SelfDestructCountdown` | P | `SimCommand::Stop` |
| 15 | same, `Produce` → `edit_queue` :272 (:84–:93) | `Producer` queue (+n, front, remove) | P | `SimCommand::Factory` |
| 16 | same, Repeat :287 / AutoHold :292 | `Producer.repeat`, `AutoHold` | P | `SetRepeat` / `SetAutoHold` |
| 17 | same, Deploy/Undeploy :304, Enter :311 | `DeployEvent`, `EnterEvent` | P | `Deploy` / `Enter` |
| 18 | same, UnsetTarget (X) :317 | removes `ForcedTarget` | P | `UnsetTarget` |
| 19 | same, SelfDestruct (Ctrl+D) :328 | `SelfDestructCountdown` | P | `SelfDestruct` |
| 20 | `ui/hud/build_bar.rs:249` `bar_mouse_input` :327 | one factory's queue via `edit_queue` | P | `SimCommand::Factory` |
| 21 | `ui/hud/placement.rs:595` `place_on_release` :679/:681/:688/:694/:701 | `BuildAt` orders, Shift cancel/refuse (`queue_build`), `VentClaim` | P | `SimCommand::Build` (queueing and claim at apply time) |
| 22 | `units/ai/mod.rs:425` `run_factories` :455–:481 | homebase queues, minifac repeat | S (AI) | `Factory` / `SetRepeat` via `issue_as` |
| 23 | `ai/mod.rs:500` `run_constructors` :522/:536/:566/:572, `army.rs:146` | spread moves, build at vent and `VentClaim`, attack-move | S (AI) | `Order` / `Build` |
| 24 | `ai/mod.rs:622` `run_army` → `attack_move` | one scattered fight point per unit | S (AI) | one `PerUnit` per push |
| 25 | `ai/mod.rs:708` `run_network` :734/:760, `run_specials` :769 (:804–:879) | Dispatch, command-fire casts, deploys | S (AI) | `Cast` / `Deploy` |
| 26 | `ai/mod.rs:318` `run_late_game` :341 | AI self-destruct when alone | S (AI) | `SelfDestruct` |
| 27 | `units/lifecycle/game_over.rs:97` `check_game_over` :146 (`KillTeam`) | zeroes HP of factoryless seated teams | S | unchanged: in-sim, deterministic given seats |
| 28 | `game_over.rs:155` `next_state.set(Victory/Defeat)` from `LocalTeam` | `GameState`, which **gates the whole gameplay chain** | L, but **desync hazard** | unchanged, see §5 |
| 29 | `ui/menu.rs:697` KeepPlaying → `GameState::Playing` | resumes the local sim | L, but **desync hazard** | unchanged |
| 30 | sim-internal order edits: `command_fire.rs:621/731/1468`, `construction.rs:1026`, `production.rs:825` (bugger-off), `ground_move.rs:2705` (crush) | orders and deaths decided by the sim | S | unchanged |
| 31 | `bot.rs` (KP_BOT): spawn :330, move/queue :356, attack :374, build :395, nx :413, stop :423, kill :430, speed/pause :455/:460 | everything | D (cheats) | unchanged |
| 32 | `showcase.rs:178/183/255/287`, `ui/game_shots.rs:161` | factory queues, orders, deploys | D | unchanged |
| 33 | `game_setup.rs:244` `dev_run_control`, `record.rs:163`, `game_shots.rs:143` | `Time<Virtual>` speed | D; in MP, game speed and pause are P | unchanged |
| 34 | `interaction/selection/core.rs:211` click/box select, `selection/groups.rs:51` control groups, `rendering/camera.rs:226` camera, `ability.rs` `OrderCursorModes`, `placement.rs` `PlacementMode`/ghosts, command-panel page, markers/tori, F3 debug overlay | UI state | L | unchanged (stays local) |

Nothing in the tree is a cheat or god-mode hook besides `bot.rs`. There is no
in-game pause; the Esc menu doesn't pause.

How the old paths were timed: input ran in `Update` and its `Commands` or
messages landed on whichever `FixedUpdate` tick came next. That depends on
frame timing (0–3 ticks per frame), so even in single player the tick a
command lands on was not reproducible. The bus fixes this as a side effect.

## 2. Design

### NetId: stable unit identity
- `#[derive(Component)] struct NetId(u32)` is a **required component of
  `UnitType`**. Its `on_add` hook draws the next value from a match-scoped
  `NetIdAllocator` (starts at 1) and records it in a `NetIdIndex`
  (`HashMap<NetId, Entity>`, lookups only, never iterated). `on_remove`
  forgets it.
- Determinism: ids follow the order in which the sim spawns units, which
  every peer must reproduce anyway. Bevy applies command buffers in schedule
  order, so the hooks fire in a reproducible order. `Entity` cannot be used:
  every machine spawns a different number of render/UI entities, and the
  test proves it (peer worlds get 0/37/74 extra entities).
- A non-zero id inserted explicitly is kept, so a client/server model can
  mirror server-assigned ids.
- Ids are monotonic, never recycled. A stale command can't hit a new unit,
  and sorted delta coding keeps lists small anyway. Spring recycles 16-bit
  ids from a free list (`MaxUnits`).

### SimCommand (net/mod.rs)
```rust
enum SimCommand {
    Order { units: Units, order: Order, queue: bool },   // Move/AttackMove/Patrol/AttackGround(NetPos), AttackUnit/Guard(NetId)
    PerUnit { order: PointOrder, units: IdList, targets: PosList, queue: bool }, // formation, AI pushes
    Path { unit: NetId, waypoints: PosList, queue: bool },
    Stop { units }, Build { builders, kind, sites: PosList, queue },
    Factory { factories, kind, op: Add(n) | AddFront(n) | Remove { count, oldest } },
    SetRepeat { units, on }, SetAutoHold { units, on }, SetTarget { units, target }, UnsetTarget { units },
    SelfDestruct { units }, Deploy { units }, Enter { units }, Cast { units, target: NetPos, auto: bool },
}
enum Units { List(IdList), Prev }
```
- **Issue side vs apply side.** The issuer only *names* units, points and
  kinds. It may use local state to compute parameters (formation assignment,
  which selected units are armed, the enemy under the cursor), but the
  result goes into the command. Anything that depends on sim state is
  decided by `net::apply` at the tick it applies: Shift append vs replace
  (`Orders::busy`), the patrol route start, which queued build sites a Shift
  placement cancels, vent claims, attack/guard target positions, and the
  set-target hostility check. Apply also skips dead units, unknown ids and
  units the player doesn't own (`TeamId == frame.player`).
- `Units::Prev` reuses the player's previous explicit list. The bus
  substitutes it automatically when sealing, so Shift-queueing ten waypoints
  costs one list plus nine tiny commands. Spring gets the same effect with
  `NETMSG_SELECT`: selection is synced state and `NETMSG_COMMAND` applies to
  it.
- Positions: `NetPos` is `i32` in 1/8 elmo (a power of two, so exact in
  f32). The issuer applies the quantized value too, so quantization can't
  desync.

### CommandFrame and CommandBus
- `CommandFrame { tick: u32, player: u8, commands: Vec<SimCommand> }` is what
  a peer sends once per tick. An empty frame still says "nothing from me".
- `CommandBus` (match-scoped resource):
  - `issue(cmd)`: local player, from input systems (helper SystemParam
    `net::Issue` with the `NetId` lookup).
  - `issue_as(team, cmd)`: in-sim AI.
  - `seal`: at the head of each tick, local commands go into a frame for
    `tick + input_delay`; AI commands go into a frame for the current tick,
    not broadcast. With `broadcast_ai` the AI becomes a remote-style player
    (host-run AI).
  - `outbox` / `deliver`: transport side, with a late-frame error.
  - `ready(remote_players)`: the lockstep gate. It reports whether every
    remote frame for this tick has arrived; the first `input_delay` ticks are
    free.
  - `log`: replay and measurements.
  - Frames are applied in `(tick, player)` order, commands in issue order.
- `apply_sim_commands` is an **exclusive** system in `SimCommandSet`, ordered
  before unit motion, `ai_brain` and `GameplaySet::Produce`. It runs a cached
  `SystemState<ApplyCtx>` and flushes after **each** command, so "queue
  behind the order given one command earlier" behaves like sequential input.
- Single player keeps `input_delay = 0`: commands apply on the tick they
  would have landed on before (modulo the Update→Fixed timing noted above).
- The AI goes through the bus like a player (`issue_as`). In lockstep it
  runs on every peer, so its frames are never sent. This needed two fixes,
  see §5: `AiSeats` (which teams are AI) and the AI dice seed.

### Serialization: postcard (with hand-written delta shapes)
- **postcard**: already a dependency (the unit bundle uses it), serde, and
  `no_std`/wasm-friendly. Its wire format is specified and stable since 1.0,
  which matters when native and web builds talk to each other. Integers are
  varints, signed ones zigzag, so small numbers cost 1 byte.
- On top of that, `IdList` is sorted and gap-coded (box selections are about
  1 byte per unit), `PosList` is delta-coded from the previous point, and
  `UnitKind` is sent as its index in `VARIANTS` (1 byte). This forces a
  build/content hash check at handshake, which lockstep needs anyway.
- **bitcode** would bit-pack tighter (probably 10–30% on these frames, an
  estimate, not measured; it isn't in the offline registry here). Its format
  isn't stable across versions, it is one more dependency, and per-packet
  transport overhead (≈28 B UDP, ≈85–95 B WebRTC/SCTP/DTLS) dwarfs a
  5–70 B payload anyway. **bincode**-style fixint is the "naive" column below
  (3–11× larger).

## 3. Measured frame sizes

`cargo test -p kernel-panic net::lockstep_tests::frame_sizes -- --nocapture`.
Postcard bytes of a whole `CommandFrame` at tick 18000 (10 min), with ids
around 2000–3000 spawned interleaved with others (gap ≈ 20). "Naive" means
fixed-width u32 ids and f32 xyz, computed.

| frame | postcard | naive |
|---|---:|---:|
| empty frame | 5 | 7 |
| 1 unit move | 20 | 27 |
| 50-unit box-select move | 69 | 223 |
| 50-unit move, ids spread over 15k (gap ≈ 300) | 105 | 223 |
| 50-unit drawn formation (per-unit targets) | 265 | 811 |
| Shift-queue 10 waypoints, 50 units (10 frames total) | 202 (67 + 9×15) | 2230 |
| build row: 3 builders, 8 sites | 50 | 119 |
| factory +5 | 13 | 15 |
| 1 unit drawn path, 12 waypoints | 68 | 159 |

AI (in-sim run, `lockstep_with_in_sim_ai`, reduced harness with no
production or combat): one AI tick produced `[Factory, Order, Order,
PerUnit(10-unit push)]` in a 94 B frame, and a later refill was a 10 B
frame.

### Bandwidth per player (30 Hz ticks)
Payload (measured sizes, estimated mix):
- Empty frames: 30 × 5 B = **150 B/s** floor.
- Commands: APM counts selection and camera clicks too, so commands/s ≤ APM/60.
  The mix used is 40% repeat-group or Shift clicks (15 B), 25% new box moves
  (70 B), 15% factory or build (25 B), 10% single unit (20 B) and 10%
  formation (265 B), averaging ≈ 56 B per command.
  - APM 100 (≤ 1.7 cmd/s): ≈ 95 B/s, so ≈ **250 B/s** in total.
  - APM 300 (≤ 5 cmd/s): ≈ 280 B/s, so ≈ **430 B/s** in total.
  - The 240-tick test streams averaged 4.5 B/tick (134 B/s) per player.
- **Transport overhead dominates.** One packet per tick costs 30 × 28 B =
  0.84 KB/s on raw UDP, or 30 × ~90 B ≈ 2.7 KB/s on a WebRTC data channel
  (SCTP + DTLS).
  - Full mesh at 4v4: each peer sends to 7, so ≈ 7 × (2.7 + 0.4) ≈
    **22 KB/s up and down** (≈ 180 kbit/s). Any connection handles this.
  - Batching 3 ticks per packet (a 100 ms communication turn) gives
    ≈ 8 KB/s.
  - A host/relay star that bundles all 8 players' frames per tick gives
    ≈ 3 KB/s up and ≈ 3–4 KB/s down per client.
  - Frames are tiny, so piggy-backing the last 2–3 unacked frames for loss
    tolerance on an unreliable channel costs almost nothing.
- **AI**: 0 B in lockstep, because it runs in every sim. As a broadcast
  "player" (host-run AI), estimated from the measured shapes:
  - ≈ 10 B per factory refill, ≈ 14 B per constructor order, ≈ 12 B per cast.
  - An army push is one `PerUnit` at ≈ 2 + 7.5 B/unit (scattered points →
    ~2-byte coordinate deltas), so ≈ 450 B for 60 units.
  - At 1 decision tick/s that averages roughly 50–300 B/s per AI team,
    bursty. That is below one human's empty-frame plus header overhead.

## 4. In-process lockstep test

`net/lockstep_tests.rs`. Each "machine" is a `movement_harness::Harness`
world: the real FBI registry, a flat 2048² map and the fixed-tick motion
chain. `apply_sim_commands` (and optionally `ai_brain`) runs in a
pre-schedule.

Setup:
- Peers: player 0, player 1 and a spectator.
- Each machine first spawns 0/37/74 dummy entities, so `Entity` ids differ
  (asserted) while `NetId`s agree (asserted).
- Units: team 0 has a Kernel and 24 Bits; team 1 has a Kernel, 12 Bytes and
  6 Pointers.
- Script: move; fight; factory +5; 10 Shift waypoints (`Prev`); a drawn
  formation; Stop; patrol; a drawn path; guard; queued fight; and an order
  for enemy units, which must be ignored everywhere.
- Peers issue only their own player's commands.
- Frames travel encoded through a simulated wire with latency L steps. A
  peer stalls while `ready()` is false.
- Per tick, a state hash covers every unit by `NetId`: exact pose bits,
  `MoveTarget`, queue, path, fight marker and factory queue.

Results (`cargo test -p kernel-panic net::lockstep -- --nocapture`):

| input delay | latency (steps) | per-tick hashes, 3 peers × 240 ticks | stall-steps per peer | final hash |
|---:|---:|---|---:|---|
| 1 | 0 | identical | 0 | 0e464eff… |
| 1 | 2 | identical | 478 | 0e464eff… |
| 1 | 5 | identical | 1195 | 0e464eff… |
| 3 | 0 / 2 | identical | 0 | 09cb0a4e… |
| 3 | 5 | identical | 237 | 09cb0a4e… |
| 6 | 0 / 2 / 5 | identical | 0 | de22f8aa… |

- Latency never changes the outcome, only the stall count. Stalls appear
  exactly when latency + 1 > delay. Different delays give different
  outcomes, which shows the commands mattered.
- `every_peer_applies_the_same_stream`: the apply logs of all peers are
  equal.
- `lockstep_with_in_sim_ai`: adds AI team 2 (homebase, 2 constructors,
  10 Bits, 3 vents) running `ai_brain` on every peer through the bus, never
  broadcast. 300 ticks, identical on all peers.

**What diverged first, and the fix.** With the dummy-entity offset, peers
drifted apart ~7 ticks after the first group move. The first differences
were 1e-4-elmo position deltas on colliding Bits. The cause:
`GroundMover::seeded` (`interaction/ground_move.rs:712`) sets the
`SlowUpdate` phase to `entity.index_u32() % SLOW_UPDATE_RATE`, so units ran
their slow updates on different ticks on different machines. Fix in this
branch (`net::rephase_ground_mover`, a `GroundMover` `on_add` hook plus the
`NetId` hook): the phase becomes `NetId % SLOW_UPDATE_RATE`, the same idea as
Spring staggering by unit id. With it, all offsets agree.

Scope limits: the harness has no combat, production, construction,
projectiles, terrain height or air units. The full-sim determinism audit
belongs to X-sim.

## 5. Determinism hazards found on the way (handed to X-sim / the lobby)
1. **Fixed here:** `GroundMover` slow-update phase from `Entity` index (above).
2. **Open:** `air_movement.rs:757` seeds `HoverAir` from `entity.to_bits()`.
   Same class of bug; use `NetId`.
3. **Fixed opt-in:** the AI took "every homebase team except `LocalTeam`" as
   AI (`ai/mod.rs` `snapshot`). On a networked peer, each machine would play
   the AI for every *other human*. New `AiSeats` resource; the lobby must
   insert it from the seats.
4. **Open:** `AiTicker` seeds its dice from `match_seed()`, a per-launch
   clock value. Peers must share the match seed. The test uses
   `AiTicker::with_seed`.
5. **Open:** `AutoHold::initial(team, LocalTeam)` (`worm.rs:74`) makes sim
   state depend on who is watching. Use the seat's human/AI flag.
6. **Open:** `GameState::{Victory, Defeat}` comes from `LocalTeam`
   (`game_over.rs:155`) and **gates the whole gameplay chain**. The loser's
   sim would stop, and "Keep playing" (`menu.rs:697`) restarts it. The sim
   gate has to be a shared match state; Victory/Defeat should only be a
   local overlay.
7. **Open:** cloak/fog visibility is computed from `LocalTeam`
   (`cloak.rs:196/251/314`). Fine if it only drives rendering; it must not
   drive targeting.
8. **Removed by the bus:** input `Update` → `FixedUpdate` landing tick
   depended on frame timing.
9. The sim's `HashMap`s and the multithreaded executor are X-sim's scope.
   The command layer itself never iterates a hash map and applies in
   `(tick, player, issue order)`.

## 6. Behaviour changes in this branch (single player, delay 0)
- Shift "busy?" checks, patrol route starts and Shift-placement
  cancellation are evaluated at apply time (the next tick) instead of at the
  click.
- AI orders apply on the next tick. Before, they landed in the same tick,
  unordered against motion.
- Ability and deploy messages from input land on a deterministic tick.
- Positions are quantized to 1/8 elmo.
- The AI no longer re-sends `set_repeat` every second for every minifac:
  only changes are issued.
- `group_move_slots` (identity) stays the definition of a plain click.
  `Order::Move` carries one point, and apply is where slot spreading would
  go.

## 7. Migration plan for what's left (estimates)
| item | effort |
|---|---|
| `bot.rs` move/attack/build/nx/stop through the bus; spawn/kill as `SimCommand::Dev` behind a match flag (dev/cheat channel) | 2–3 h |
| `showcase.rs`, `ui/game_shots.rs` through the bus (or keep as single-player-only dev tools) | 1 h |
| Game speed and pause as commands (`SetSpeed`, `Pause`) instead of `Time<Virtual>` pokes; the lockstep driver honours them | 2–4 h |
| Resign / leave as `SimCommand::Resign` → the existing `KillTeam` path; decouple the sim run gate from local Victory/Defeat (§5.6) | 3 h |
| Seat-derived sim state: `AiSeats` from the lobby, shared AI seed, `AutoHold` initial from the seat (§5.3–5.5) | 1–2 h |
| `HoverAir` seed from `NetId` (§5.2) | 30 min |
| Lockstep fixed-step driver: run `FixedUpdate` only while `bus.ready()`, replacing `Time<Fixed>` accumulation when networked; adaptive input delay from measured RTT | 1 day |
| Transport glue: drain `outbox`, `deliver` remote frames (matchbox / relay); handshake with build + content hash, seed, seats | 1–2 days (the transport agent's work) |
| Desync detection: a state hash every N ticks in the frame stream (Spring's sync checksum) and a log dump on mismatch | 3 h |
| Apply-side re-validation of static capabilities (can this kind move/attack/build that), for robustness against modified clients | 2 h |
| Replays: persist `CommandBus::log` + seed + seats; play back through `deliver` | 3 h |

The command layer is about **3–5 days** from "networkable", not counting the
sim determinism audit (X-sim) and the transport.
