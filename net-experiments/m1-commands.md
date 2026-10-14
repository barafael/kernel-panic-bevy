# M1 (B): every input a command, validated where it applies

Scope (`docs/multiplayer-plan.md`, M1): route what was left of the sim-affecting input through
`SimCommand` (dev tools, game speed and pause, resign), validate commands when they apply as
the original validated them on every receiving client, a command fuzz CI can run, and the
encoding sizes of the new commands. Code: `kernel-panic/src/net/` (`apply.rs` validation,
`control.rs` match control, `fuzz.rs`, `audit.rs`, `codec.rs`), `bot.rs`, `showcase.rs`,
`ui/game_shots.rs`, `ui/hud/match_status.rs`.

## Summary

- **Nothing outside the fixed tick writes sim state any more.** The KP_BOT dev tool, the
  showcase director and the HUD-screenshot tool issue commands like mouse input. A test reads
  Bevy's schedule access metadata and fails when a frame-loop system writes anything the tick
  writes (`net::audit`).
- **New commands** (appended, indices 14–18): `SetSpeed { percent }`, `Pause { on }`, `Resign`,
  `Dev(DevCommand)` (spawn, kill, set health, god mode), `Take { from }`. Pause, Alt+= and Alt+- are bound
  in game (Spring's defaults), with a status line at the top of the screen; agent C's tick
  driver honours them (`MatchControl`), on every peer at the same tick.
- **Validation at apply time**: the sender (seated, not resigned), the units (live, the
  sender's), what they can do (weapon, mobility, build and factory options, abilities), the
  parameters (points on the map, vent buildings on vents, canonical lists, counts in range),
  cheats only in single player. Deterministic, logged, never a panic.
- **Command fuzz**: 600 ticks (CI: 3 000) of random and malformed frames from every seat into a
  4-seat headless match on the lockstep driver; four runs (twice, storage shuffled, malformed
  mixed in) give one trail, and every malformed command is refused. `cargo test` 8 s; CI step
  in `determinism.yml` (36 s release). It found two real bugs (§3, §5, Summary).
- **Golden replays re-recorded** (`SIM_VERSION` 3, after merging agents A and C): validation
  changes outcomes where the original refused the command too (§7). The encoding of the new
  commands needs no format change.
- **This branch is the M1 integration**: agent C's lockstep runtime and agent A's seats,
  alliances and shared defeat are merged; speed and pause run through C's tick driver, the
  alliance check through A's `Alliances::friendly`.
- **Leaving play** follows the user's rule of 2026-10-15 (§2): with an ally in play a leaver's
  units wait for a flashing TAKE (`SimCommand::Take`; AI allies take themselves); the last
  leaver of a side blows the side up within 4 s; caps never change. Resign is in the Esc menu.
- The fuzz found a crash in the death pass after merging A (a unit despawned before its `Dying`
  insert applied: a buildee dying with its factory in a resignation's game-over sweep);
  `death_system` uses `try_insert` now.

## 1. What was left, and what it does now

`x-commands.md` §1 routed the orders, builds, factories, abilities and the AI. The rest:

| path | wrote | now |
|---|---|---|
| `bot.rs` (KP_BOT): `spawn`, `kill`, `hp` | spawned units, `Dying`, `Health` | `Dev(Spawn / Kill / SetHealth)` |
| `bot.rs`: `move`, `queue`, `attackmove`, `attack`, `stop`, `build`, `nx` | orders, `AttackTargetOrder`, factory queues, `CommandFireEvent` | `Order`, `Stop`, `Factory`, `Cast` |
| `bot.rs`: commanding team 1 (`team:1`, spawned enemies) | any team's units | `Dev(GodMode)` once at the start |
| `bot.rs`: `speed`, `pause`, `resume` | `Time<Virtual>` | `SetSpeed`, `Pause`; new `resign`, `god on/off` |
| `showcase.rs` director (in the tick, as the local player) | `Producer`, `BuildAt`, `DeployEvent` | `Factory`, `Build`, `Deploy` |
| `ui/game_shots.rs` (KP_GAME_SHOTS) | `Producer` from `Update` | `Factory` through `Issue` |
| game speed / pause | none in game | `SetSpeed` / `Pause`, keys Pause, Alt+= / Alt++ / Alt+numpad+, Alt+- / Alt+numpad- |
| resign | none | `Resign` (Esc menu, the bot's `resign`) |
| taking a leaver's units | none | `Take { from }` (the TAKE button, an AI ally, the bot's `take`) |

Left alone, on purpose:
- **Frame pacing of dev tools**: `KP_TIME_SCALE` (`game_setup::dev_run_control`), the video
  recorder's warm-up (`ui/record.rs`) and the screenshot tool's fast-forward set
  `Time<Virtual>`. That is the wall-clock rate of ticks, not sim state; single-player tools.
- **"Keep playing"** (`ui/menu.rs`) sets `GameState`: since agent A's merge that is the result
  panel only, never a sim gate.
- Selection, camera, control groups, placement ghosts, the command panel page: local UI.

Everything in `ui/` and `interaction/` already went through the bus; the command panel has no
fire-state, move-state, cloak or on/off toggles (KP has none).

The bot keeps its file protocol and selectors (entity indices). Commands apply on the tick
they are read (the bot issues before the apply system); a `spawn` holds the following lines
one tick and the log line `spawned: [...]` reports the entities a tick later (the apply
system hands them over in `net::control::DevSpawned`). A paused match runs no tick, so the
bot reads no `resume` while paused; unpause with the Pause key.

## 2. The new commands

```rust
enum SimCommand {
    …                               // indices 0–13 unchanged (M0 replays still decode)
    SetSpeed { percent: u16 },      // 14
    Pause { on: bool },             // 15
    Resign,                         // 16
    Dev(DevCommand),                // 17
    Take { from: u8 },              // 18 (2026-10-15)
}
enum DevCommand {
    Spawn { kind, team: u8, at: NetPos, count: u16, spacing: u16 },
    Kill { units: IdList },
    SetHealth { units: IdList, hp: u32 },
    GodMode { on: bool },
}
```

### Speed and pause: semantics and the tick driver

Spring keeps both out of the synced frame: the server owns the frame clock, clamps a
`NETMSG_USER_SPEED` to the setup's `MinSpeed` / `MaxSpeed` (`CGameServer::UserSpeedChange`,
`Net/GameServer.cpp:3084`; defaults 0.1 / 20, `Game/GameSetup.cpp:636`) and stops sending
frames while paused; a client flips `gs->paused` when `NETMSG_PAUSE` arrives
(`Net/NetCommands.cpp:384`). Any player may pause, spectators may not
(`Net/GameServer.cpp:1071`).

Here both travel in the turn stream, so every peer learns them at the same tick, and both
land in the match-scoped `net::MatchControl` resource when they apply. The lockstep tick
driver (agent C, `net/driver.rs`, `m1-runtime.md`) reads it — single player is a solo host on
the same path:

- `SetSpeed { percent }` (10…2000, else refused): `MatchControl::speed()` is the factor the
  driver's pacer multiplies the frame's virtual time by (`drive_frame`), the **one place** game
  speed scales the clock. `Time<Virtual>` stays at 1×, so the camera and UI keep their pace (the
  dev tools' `KP_TIME_SCALE` and the recorder still scale virtual time, everything included).
- `Pause { on: true }` applies at the head of its tick; that tick runs to its end; **no further
  tick runs** on any peer (`MatchControl::paused`, read by the driver after every tick).
- While paused the driver still seals every frame, so commands reach the host; the host files
  them on the next tick and cuts that turn at once when one of them is `Pause { on: false }`
  (`driver::resumes_sim`). The unpause applies at that turn's head, together with whatever was
  ordered meanwhile, and the clocks run again. An unpause from a resigned player still resumes
  the driver for that one turn, is refused there, and the match stays paused.

Neither changes sim state: the sim is a function of the turns, so *when* a peer runs its
ticks never matters. Pause and speed are commands so that every peer agrees on them and the
HUD can show them (`paused_by`), not for determinism. `multipeer_tests::pause_and_speed_full_sim`:
three peers and an AI on Marble Madness, 40 ms links; a guest pauses (every peer stops on the
same tick and runs none for 5 s), resumes, another guest doubles the speed (60 → 120 ticks per
2 s on every peer); the trails agree.

**Host policy** (M2): the host may refuse to sequence a `SetSpeed` or `Pause` it doesn't allow
(Spring's `gamePausable`, a speed range from the lobby); apply-time validation enforces the
range and that the sender is seated and not resigned. M3's lag protection sets the speed as a
`SetSpeed` from the host.

My first version, before C's driver was merged, had its own single-player clock
(`Time<Virtual>` relative speed, a run condition on Bevy's fixed loop, a pause-time seal of the
control commands); C's driver replaced all three.

### Resign, leaving play and TAKE (the user's rule, 2026-10-15)

> "When the last player on a team resigns or gets taken out, all remaining units explode over a
> duration of 4 s, in a random fashion (each has a random ttl of 0–4 s). Same in 1v1. If other
> players are active, then anybody can hit the flashing TAKE button and the units are added to
> them. Unit cap stays untouched."

- **One way out.** Resigning (`SimCommand::Resign`, `ApplyCtx::resign`), being taken out by the
  census (no factory left, `game_over.lua`) and, for M2/M3, dropping all call
  `TeamFates::leave(owner)`. The owner is inactive from then on: its player spectates, its
  commands are refused (`Refusal::Inactive`), an AI owner's brain stops.
- **An ally still plays.** The game-over pass of that tick leaves the leaver's units where they
  are, idle (orders and factory queues cancelled), still the leaver's and friendly to the team.
  Every active ally sees a flashing **TAKE** button (`ui/hud/take_button.rs`); a click sends
  `SimCommand::Take { from }` (appended, index 18). The first valid `Take` in turn order moves
  *all* of `from`'s remaining units to the sender: `TeamId`, and the unit and small-building
  tallies with it (`bookkeeping::retally`); the old owner's selection drops them. Everything
  else keyed by owner (the AI's snapshot, cloak detection, the spatial index) reads `TeamId`
  each tick. Refused, deterministically: a `Take` from an enemy, from oneself, from an owner
  still in play, by an inactive owner, and every later one (nothing left).
- **AI allies take too** (Team Game): at its next brain tick (≤ 1 s) when no human ally is in
  play, else after `TAKE_GRACE_SECS` = 10 s so a human can press TAKE first; several AI allies
  asking on the same tick: the lowest owner wins (turn order), the rest find nothing
  (`units::ai::take_from_leavers`).
- **The ally team's last active owner leaves** (resign, taken out, a 1v1): every remaining unit
  of the ally team — the leaver's and whatever earlier leavers' nobody took — gets a `LeaveFuse`
  0–120 ticks out (`SimRng<LeaveFuseRng>`, drawn in `NetId` order), its factories stop, and it
  blows up with its regular death blast and damage (`game_over::detonate_fuses` → HP 0 →
  `death_system`). The ally team has lost (A's shared defeat); the match ends at the first
  census after the last explosion.
- **Caps never change**: A's `UpdateTeamUnitLimitsOnDeath` port is gone; a taker may go over
  its cap, which only stops its factories until it is back under.

Departures from upstream, noted in the code: Recoil makes a resigner spectate and leaves the
team leaderless with its units standing (`Net/NetCommands.cpp:1195`, `CGameServer::ResignPlayer`,
`Net/GameServer.cpp:2804`); Spring's old quit box self-destructed everything (`Game/UI/QuitBox.cpp:329`);
a `KillTeam`ed team's units die at once and its cap goes to its allies
(`CTeamHandler::UpdateTeamUnitLimitsOnDeath`); the closest thing to TAKE is `/take`, which
hands a leaderless team over whole (`TEAMMSG_GIVEAWAY`).

Resign is in the Esc overlay ("Resign", then "Really resign?") and in KP_BOT (`resign`,
`take <owner>`).

Tests (`net::leave_tests`, full headless matches on Marble Madness; `net::apply`, `game_over`):
- 1v1 resign: every unit of the human dies within 120 ticks, on several different ticks, and
  the AI wins at the next census;
- 2v2 resign with an ally in play: the units stay unfused; an enemy's `Take` is refused; the
  ally's takes them all and the tallies move; a second `Take` is refused; caps unchanged; the
  same match with the storage reshuffled every tick gives the same trail;
- the taker exceeding its cap (`take_moves_every_unit_and_its_tally`: 1 200 units, cap 1 000,
  factories at the limit);
- Team Game: the AI ally takes the human's units within ~2 s;
- the last ally resigning: both owners' units fused and gone within 4 s;
- multipeer (`take_race_full_sim`): two peers press TAKE on the same frame; every peer agrees
  who got the units, trails identical.

### Dev tools and cheats

Upstream, `/give`, `/destroy`, `/godmode` are synced actions that need cheats on
(`cheatRequired`, refused while `gs->cheatEnabled` is off, `Game/IActionExecutor.h:111`), and
only the host may turn cheats on (`/cheat` is a server command, `Net/GameServer.cpp:120`,
`:2477`). Here a match allows cheats when it has **at most one human seat**
(`net::apply::cheats_allowed`): single player and the dev tools' sandbox. The seats are part of
the setup every peer agreed to, so every peer decides alike. A lobby option "cheats" (the host
decides, the handshake carries it) slots into the same function in M2.

Per tool: the bot's spawn / kill / hp and its control of other teams are cheats
(`Dev`); its orders are ordinary commands; the showcase director and the screenshot tool only
issue ordinary commands (as the local player, in single player); the frame-pacing tools stay
local (§1).

## 3. Validation

`net/apply.rs`, applied per command at the tick head on every peer. Citations are
`upstream/RecoilEngine/rts`.

| check | upstream | refused |
|---|---|---|
| the sender has a seat | `CGame::ClientReadNet` throws on `!IsValidPlayer` for `NETMSG_COMMAND` (`Net/NetCommands.cpp:690`) | the command |
| the sender hasn't resigned | resign clears the net selection (`:1225`) | the command |
| units live, known, the sender's | `NETMSG_SELECT` keeps ids `GetUnit` knows and `CanControlTeam` allows (`:730`); `AINetOrder` (`Game/SelectedUnitsHandler.cpp:785`) | those units, silently (latency, not a fault) |
| attack: a weapon that can hit the target | `AllowedCommand` → `IsAttackCapable` (`Sim/Units/CommandAI/CommandAI.cpp:694`); `OnlyTargetCategory` as the UI filters | those units |
| attack: not a friend | the port's attack order only fires on enemies (as `SetTarget` already checked) | the command (`Friendly`) |
| fight / patrol / guard / drawn moves: mobile | `canFight` / `canPatrol` / `canGuard` / `canmove` (`:713`–`:727`) | those units |
| a plain move | accepted for the whole selection, factories too (KP's single click is the engine's; Spring factories take `CMD_MOVE`) | — |
| build option / factory roster / ability | `CBuilderCAI` / `CFactoryCAI::GiveCommandReal`, as before | those units |
| points on the map | `IsCommandInMap` (`:564`) | an order's or cast's point: the command; per-unit targets, path waypoints, build sites: that point |
| build site | `TestUnitBuildSquare`: footprint on the map; vent buildings on a vent | that site |
| unit lists sorted and unique, no id 0 | the encoder's invariant (`IdList::new`) | the command |
| per-unit lists as long as their units; factory count ≤ 100 (Ctrl+Shift); ≤ 256 build sites; speed 10–2000 % | — | the command |
| `Dev` | cheats on | the command |

- **Deterministic**: every check reads replicated state only (the setup's seats, `MatchControl`,
  sim components, the map, the unit registry). Every explicit unit list goes into the
  bus's `Units::Prev` memory, refused command or not: that memory mirrors the sender's
  seal-side one, which recorded the list when it sent it (a first version skipped refused
  commands and an AI's honest `Prev` after an off-map order then named an older group). A
  `Prev` that names a malformed list is malformed itself.
- **One alliance helper**: `ApplyCtx::friendly(a, b)`, i.e. A's `Alliances::friendly` (friendly
  attacks and manual targets are checked by the *unit's* owner against the target's). **One
  control helper**: `Rules::controls(player, owner)` (`owner == player`, or god mode): a frame's
  sender is a seat, and seat `i` is owner `i` (`TeamId`, Spring's team; A's `units::teams`).
  A sender is seated when it is below the setup's seat count.
- **Logging**: `Refusals` (count, last) for tests and diagnostics; a `warn!` for what no honest
  client sends (malformed, unseated, resigned, cheats), `debug!` for what happens in play (points
  off the map, sites, friendly attacks: the AI's scattered pushes near an edge, which Spring drops
  silently too). Dead / foreign units aren't counted.
- Malformed bytes never reach apply: `CommandFrame::decode` returns an error (postcard), and a
  unit kind index out of range is a decode error (`kind_index`).

Behaviour changes for single player: none for anything the UI sends (its filters match the
checks). The scripted golden match's attack on the enemy base no longer gives the order to the
units of its group that can't attack a building (no weapon, or a weapon whose target categories
exclude it), and the AI loses the orders of units whose scattered fight point falls off the
map — both as in the original.

## 4. The audit: no frame-loop writes to sim state

`net/audit.rs`, `frame_loop_never_writes_sim_state`: builds the whole game (the headless app:
every plugin, UI included), initialises every schedule, collects the components and resources
the tick's systems (`FixedFirst` … `FixedLast`) write, and fails on any system of any other
schedule that writes one of them, unless `FRAME_LOOP_WRITES` lists it with a reason. Exclusive
systems outside the tick and systems with unbounded access need an entry too.

Listed (each with its reason in the file): the `CommandBus` itself, render assets, gizmos,
`GlobalTransform`, `Visibility`, the model cache, the sound queue (the sim's outbox), the
interpolated `Transform` (`restore_sim_pose` restores the sim pose at `FixedFirst`), the
map spawn (match setup), the camera / health bars / previews / ghosts' own transforms,
`HexFarmView`'s draw-only fields and the minimap image.

It can't see writes through `Commands` (an `insert` from an `Update` system), observers or hooks.
Those stay a convention — input systems take `Issue` / the bus and no sim component — backed
by the pacing and selection variants of the determinism tests (`headless::tests`). Dev-tool
plugins that only register under an environment variable (bot, shots, recorder) aren't in the
headless app; they were routed by hand (§1).

## 5. Command fuzz

`net/fuzz.rs`. A headless match on Marble Madness: two human seats (frames delivered like a
remote player's), two AI seats. Each tick, from the live state (in `NetId` order):

- **S, chaos**: every seat sends a frame with probability 35 %, 1–3 commands of any variant —
  own units, sometimes with another team's or a dead / never-existing id; points near own
  units, 10 % anywhere, some off the map; kinds mostly from the units' options; `Units::Prev`
  behind an explicit list of the same frame; pause, speed; a rare resignation of seat 3 after tick 400;
  and 20 % of the ticks a frame whose encoding had 1–3 random bit flips and still decoded,
  delivered as decoded (unless the damage made up a `Units::Prev`, whose meaning would depend
  on the malformed stream).
- **M, malformed**: on 30 % of the ticks, commands that must be refused whole: frames from
  players without a seat (4, 9, 200, 255) carrying anything, resignations included; unit lists
  unsorted / duplicated / with id 0; per-unit lists of the wrong length; orders and casts off
  the map; factory counts > 100; speeds out of range; 257-site build rows; cheats (two humans:
  off); a path for unit 0; and orders naming only other teams' or unknown units. Seated
  frames come from the human seats only: the AI's own sealed commands use `Units::Prev`
  against what its seat sent last, and in a match nobody else sends in an AI seat's name (the
  host stamps the sender) — lists delivered for it would change what its next `Prev` names. The
  fuzz found exactly that on its first long runs.

The match runs on the lockstep driver like the game (a solo host): frames are delivered into
the bus for the coming tick, each frame lasts one tick of game time at the current speed, and a
chaos pause is lifted by the local seat's `Pause { on: false }` through the driver, so pause,
resume and speed run the real path.

`check` plays S twice, S with the ECS storage reshuffled every tick
(`headless::replay::shuffle_storage`), and S + M. The four trails must be identical (M changes
nothing), and the refusal count of S + M must exceed S's by exactly the number of M's
commands that count as refusals. A second test decodes 20 000 random and bit-flipped byte
strings and applies whatever decodes in a bare world.

| | |
|---|---|
| `cargo test -p kernel-panic net::fuzz` (dev profile, 600 ticks × 4 runs, plus the decode test) | 8 s |
| chaos stream, 600 ticks (seed `0xf022`) | 880 frames, 1 716 commands, 73 refused in part or whole |
| malformed stream, 600 ticks | 251 frames, 444 commands, all refused |
| CI: `kernel-panic --command-fuzz --ticks 3000` (release, linux-x86_64-gnu job of `determinism.yml`) | 36 s locally |
| chaos stream, 3 000 ticks (seeds `0xf022` / `0x1234` / `0xbeef`) | ~4 450 frames, ~8 650 commands each |
| malformed stream, 3 000 ticks | 1 115–1 230 frames, 2 028–2 227 commands, all refused |

Without baked maps (the plain `cargo test` CI job) the test skips with a message; the
determinism workflow bakes them and runs the release binary.

## 6. Encoding

Postcard, unchanged shapes; the new variants are one varint index plus their fields
(`net::codec::tests::m1_command_sizes`, a frame of player 3 at tick 18 000):

| frame | bytes | over an empty frame |
|---|---:|---:|
| empty frame | 5 | — |
| `Resign` | 6 | 1 |
| `Take` | 7 | 2 |
| `Pause` | 7 | 2 |
| `SetSpeed` 150 % / 2000 % | 8 / 8 | 3 |
| `Dev(GodMode)` | 8 | 3 |
| `Dev(Spawn)` 10 Bits at a point | 19 | 14 |
| `Dev(Kill)` 10 units (gap 17) | 19 | 14 |
| `Dev(SetHealth)` 10 units | 21 | 16 |
| unpause + speed + a 10-unit `Stop` | 24 | 19 |

Per turn: match control is rare (a key press); a turn with a pause or speed change costs 2–3
bytes on top of the 5-byte empty frame, so the per-player budget of `x-commands.md` §3
(≈ 250–430 B/s) doesn't move. Variant indices 0–13 are unchanged
(`m0_variant_indices_are_stable`), so M0 replays still decode.

## 7. Golden replays

Validation changes outcomes. On its own (against main's M0 trails, before the merges) it changed
two of the three logs:

| log | M0 → M1 | why |
|---|---|---|
| `4v4_speed_balls` | identical for all 18 000 ticks | — |
| `ffa8_quad_core` | from tick 2 915 | the AI's scattered `PerUnit` push puts a unit's fight point off the map; that unit keeps its orders (Spring: `IsCommandInMap`) |
| `commands_marble` | from tick 1 201 | the script's attack on the enemy base (tick 1 200) goes to its whole group; units that can't hit the base no longer take it (`IsAttackCapable` / `OnlyTargetCategory`) |

Every difference is a command the original would have refused too. After merging agent C (no
sim change: the goldens replayed to the same hashes) and agent A (`SIM_VERSION` 2, format 2,
and a fourth log, `2v2_dual_core`), `SIM_VERSION` went to 3; with the leaving rules
(2026-10-15) it is **4**, and all four logs are re-recorded on the integration branch (`cargo
run --release -p kernel-panic --features replay -- --record-golden`; each replay matches its
recording; the encoding of the new commands needs no format change):

| log | ticks | final units | final hash |
|---|---:|---:|---|
| `4v4_speed_balls` | 18 000 | 56 | `1dc39f68fad9601d` |
| `ffa8_quad_core` | 18 000 | 220 | `8bfcd92cc3381e2b` |
| `2v2_dual_core` | 25 200 | 0 | `939e3a8b38c8fe25` |
| `commands_marble` | 3 000 | 25 | `5f4ad3e11aafc322` |

`2v2_dual_core` is over long before its 25 200 ticks here: the losing side is out by ~12 000,
and the winners' AIs self-destruct once alone for 30 s (KPAI's `SuicideIfAlone`), so the log
ends on an empty map. `shared_defeat_2v2` (A's end-to-end check, adapted to the leaving rules):
owner 1 out at tick 10 255 (its AI ally takes its units), owner 0 at 11 845, side 1 wins at
11 875.

The query-order fuzz (every log replayed with the storage reshuffled every tick, `--shuffle 1`)
gives the same trails.

## 8. Open

- **A** (merged): alliance checks are `Alliances::friendly`; leaving play is decided
  (2026-10-15) and implemented on top of A's `TeamFates`.
- **C** (merged): the driver reads `MatchControl`; `resumes_sim` is the unpause only.
- A leaver's Network packet buffer (per owner) is not handed over with a `Take`; the
  packets are lost.
- M2/M3: a dropped peer should call `TeamFates::leave` for its owner (the same path).
- **M2**: a host-side policy (who may pause, the speed range, a lobby "cheats" option feeding
  `cheats_allowed`).
- The audit can't see `Commands`-based writes; a run-time check (sim components on `NetId`
  entities changed between ticks) would need an allowlist of its own (`Selected`, materials).
- Validation doesn't re-check slopes or structure overlap at the site (the construction start
  does the overlap; a modified client could still order a building on a slope the UI refuses).
