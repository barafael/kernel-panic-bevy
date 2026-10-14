# M1 (agent A): sim rules — float max/min, seats and alliances, shared defeat, the sim never stops

Scope (`docs/multiplayer-plan.md`, M1): the simulation rules lockstep needs before the tick
driver and the transport. Four parts: target-independent float `max`/`min`/`clamp`; per-seat
owners and ally teams (upstream's team / allyteam); shared victory and defeat; local defeat
decoupled from the sim.

Machine: AMD Ryzen 7 7840HS, Linux, shared with other sessions' builds (timings noisy).

## 1. `f32::max` / `min` on signed zeros

M0.4's float probe found `f32::max`/`min` returning the other zero for 31 % of its ±0 pairs on
both arm64 targets (x86_64 `maxss`/`minss` return the second operand; aarch64
`fmaxnm`/`fminnm` order `-0 < +0`; IEEE 754-2008's `maxNum` leaves the sign open).

**New crate `spring-detmath`** (no dependencies, `no_std`): `DetFloat::{det_max, det_min,
det_clamp}` for `f32` and `f64`, compare-and-select with the open cases pinned:

| case | `det_max` | `det_min` | `f32::max` / `min` |
|---|---|---|---|
| `+0` vs `-0`, either order | `+0` | `-0` | target-dependent |
| one NaN operand | the other operand | the other operand | the other operand |
| both NaN | NaN | NaN | NaN |
| `det_clamp(x, lo, hi)` | `x.det_max(lo).det_min(hi)`: NaN → `lo`, `-0` in `[0, 1]` → `+0` | | `f32::clamp`: NaN → NaN |

`lo <= hi` is a `debug_assert` (std's `clamp` always panics). Signed zeros follow IEEE
754-2019's `maximumNumber`/`minimumNumber`. Cost: two compares and a select more than `maxss`;
no measurable sim change.

- `kernel-panic` re-exports it as `crate::sim::DetFloat`.
- `clippy.toml` bans `f32::{max, min, clamp}` and the `f64` versions (reason text names the
  replacement). All 378 call sites in the workspace were converted mechanically from clippy's
  JSON spans (`.max(` → `.det_max(`, `f32::max` paths → `f32::det_max`): the sim,
  `spring-pathfinding`, `spring-map` (runtime and bake), `spring-tdf` (runtime weapon values),
  `spring-unit-mesh` (model bounds feed `hit_radius`), and the presentation code too — one rule
  everywhere, no opt-outs needed. Offline tools that already opted out of the transcendental
  bans (`spring-map-gen` binaries, the float probe) keep their file-level allow.
- **glam**: with `scalar-math`, `Vec2/Vec3/Vec3A/Vec4::max/min/clamp/max_element/min_element`
  are written as `if a > b { a } else { b }` (glam 0.33.12 `src/f32/vec3.rs:279-345`,
  `scalar/vec3a.rs`, `scalar/vec4.rs`), and `clamp_length*` compare squared lengths; none goes
  through `f32::max`. Compare-and-select is exact on every target, so they stay allowed. (Their
  tie rule differs from `DetFloat` — `max(+0, -0)` returns the second operand — but it is the
  same on every target, which is what lockstep needs.)
- **Float probe** (`net-experiments/m0-floatprobe`, appended rows, so the CI matrix runs them on
  every target including both arm64 runners): `det: DetFloat f32 max/min/clamp` (±0, ±1e-30,
  NaN, ±1, MIN_POSITIVE pairs, `x.det_max(0.0)`, clamps to `[0, 1]` and `[-1, -0]`),
  `det: DetFloat f64 max/min/clamp`, `det: glam Vec3/Vec3A max/min/clamp`. Locally
  (`run-matrix.sh`: x86_64 gnu, musl, `target-cpu=native`, wasm32 in Node, wasm after
  `wasm-opt -O2`): 0 differing bits. The `chk:` rows stay as the control group.

## 2. Seats and alliances

Before: `TeamId` was the ally team. Every seat the menu put on one side shared it, so two
humans on one side would co-own and command each other's units, Outgunned's AI seats were one
AI brain with one unit cap driving several homebases, and an allied AI next to a human seat was
not played at all (`AiSeats` skipped teams with a human seat).

After, upstream's model (`rts/Sim/Misc/TeamHandler.cpp:28-94` `LoadFromSetup`, `AllyTeam.h`,
`CTeamHandler::Ally`, `TeamHandler.h:53`):

- **Owner** = Spring *team*, one per seat: seat `i` of `GameSetup::players` is owner `i`; its
  units carry `TeamId(i)` (`spawning::spawn_homebases`). Owners hold the units, the unit cap
  (`TeamUnitCounts`, upstream `MaxUnits` per team), the Network packet buffer (`bufferSize[team]`
  in `network_buffer.lua`), the AI brain (`AiSeats`: every AI seat, upstream one KPAI per team,
  `GetTeamLuaAI(t)`), Hex Farm towers, and the command right (`net::apply` checks
  `TeamId == player`).
- **Ally team** = Spring *allyteam*: `PlayerSpec::ally_team` (renamed from `team`).
- **`units::teams::Alliances`** (resource, set from the setup in `begin_match`):
  `friendly(owner_a, owner_b)` (one 256-entry table lookup), `ally_team(owner)`,
  `allies_of(owner)`, `owners()`, `vision_mask(owner)`. An owner without a seat (the map-event
  eruption neutrals, `ERUPTION_TEAM`) is an ally team of its own, as before (Spring's Gaia
  allyteam). The default (no seats) makes every owner its own side, so bare test worlds behave
  as before.
- **FixedAllies**: alliances are fixed for the match (`EngineOptions.lua:107` `FixedAllies`,
  default true). KP's launcher wrote `fixedallies=0` (`WriteScript.lua:147`), which only
  enabled Spring's `/ally` command; the port has none.
- `LocalTeam` is the local *owner* now (the first human seat's index; `SPECTATOR_TEAM` without
  one).

### Every friend/foe site, and what upstream compares there

Owner comparisons (`TeamId == TeamId`) stayed where upstream compares *teams*; everything
upstream decides by *allyteam* now asks `Alliances::friendly`:

| site | upstream | now |
|---|---|---|
| auto-targeting (`combat_system`) | `GenerateWeaponTargets`: enemies only | `!friendly` |
| `avoidfriendly` line of fire (`free_line_of_fire`, both callers) | `TraceRay` `NOFRIENDLY` (allyteam) | `friendly` |
| projectile mid-flight hits (`broad_phase_volume_hit`, `collidefriendly=0`) | `Collision::NOFRIENDLY` | `friendly` |
| Logic Bomb proximity (`tick_kamikaze`) | `SlowUpdateKamikaze` → `GetEnemyUnits(…, allyteam)` | `!friendly` |
| Firewall bubble (`apply_firewall`) | `network_reflectorshield.lua:100` `GetUnitAllyTeam(u) == ownTeam` | `friendly` |
| Firewall damage reflection | `network_reflectorshield.lua:126` `ateam ~= team` (team) | owner, unchanged |
| infection tagging | `infection.lua:71` `unitTeam ~= attackerTeam` (team) | owner, unchanged |
| area denial (`tick_area_denial`) | `areadenial.lua:77` `AreTeamsAllied` | `friendly` |
| area denial stand-in owner | `areadenial.lua:61` `GetTeamUnitsByDefs(w.team, …)` | owner, unchanged |
| cloak detection (`update_cloak_detection`) | decloak coverage per allyteam | a detector sets the bits of its whole ally team (`vision_mask`) in `DetectedBy`; `hidden_from` unchanged |
| bugger-off (`apply_bugger_off`) | `GameHelper.cpp:898` `BuggerOffImpl`: allied units only | `friendly` |
| idle allies pushed, not avoided (`obstacle_avoidance_dir`) | `GroundMoveType.cpp:1928` `avoidee->allyteam == avoider->allyteam` | `friendly` |
| packets entering teleporters | `network_enter.lua:65` same team | owner, unchanged |
| unit cap, Logic Bomb cap, mine cap | `CTeam::AtUnitLimit`, `GetTeamUnitsByDefs(team, …)` | owner, unchanged |
| manual target command (`net::apply` `SetTarget`) | — | `ApplyCtx::friendly` (agent B's one helper) |
| cursor / right-click / set-target / guard input | `GetDefaultCmd` allied check | `friendly` |
| local cloak visibility, cloak fade, fog | `gu->myAllyTeam` | `friendly(…, LocalTeam)`: shared vision |
| AI: fairness `Lack` | `KPAI_Fair.lua:116` allied teams subtract, enemies add | the AI's side (itself + allies) against everyone else |
| AI: `SuicideIfAlone` | `KPAI.lua:181` `not AreTeamsAllied` | `!friendly` |
| AI: enemy structures, nearest enemy, enemy counts, crowd clusters | `td.allyTeam`, `GetUnitAllyTeam` | `!friendly` |
| AI: Firewall trigger | `KPAI.lua:1249` allies near by `GetUnitAllyTeam`; `lastAllyDamage` set for every allied AI (`KPAI.lua:910`) | ally counts by `friendly`; `TeamDamageLog` records a hit for every allied owner |
| AI: own units, factories, constructors, teleporters, homes | `GetTeamUnits(t)` | owner, unchanged |

Unchanged on purpose: selection, the command panel, tooltips and build bar act on the local
*owner's* units (you command only your own, as in Spring).

### Unit cap per owner, shared on death

`TeamUnitCounts` counts per owner (it always did; owners are seats now) and holds per-owner
limits: `CTeamHandler::UpdateTeamUnitLimitsOnDeath` (`TeamHandler.cpp:188-238`) — a dead owner's
cap drops to 0 and each living ally's becomes `max * n / (n - 1)` (`n` living teams counting the
dead one, integer division).

### Single player

- **Duel**: one seat per side, owner 0 vs owner 1 — the same `TeamId`s as before; owner and ally
  team coincide. Outcomes differ only through the rules that changed for everyone (the end one
  census after the last kill, see §3, `DetFloat`, and the 0-HP fix of §5).
- **Outgunned**: the AI seats are now one owner each in one ally team (as upstream's launcher
  seated them: `GenerateGame.lua:290` `allyteam=enemy and 1 or 0` per `TEAM<p>`): N AI brains
  with N unit caps instead of one brain driving N homebases. They cooperate as allies (never
  target each other, count each other in the fairness budget, react to damage on any of their
  side's units). This is a behaviour change, toward the original.
- **Team Game** (new menu grouping, upstream grouping 2): you plus AI allies against AI enemies.
  Sizes per difficulty as `GenerateGame.lua:123-143` (`HalfMaxMapTeam` taken as 4, since the
  roster is rolled before the map is read; at most 8 seats), the player's seat parity rule and
  the interleaved sides of `GenerateGame.lua:229-290`. The roll happens when grouping or
  difficulty changes (`SkirmishConfig::set_grouping` / `set_difficulty`), so the description
  line names the match Run! starts, e.g. "Medium Team Game: 3v3 - Player is #1 in [0..5] and
  System". All AI seats use the one menu difficulty (upstream gave enemies Baczek's AI on
  Hard+; there is one AI here).
- **Demo**: FFA seats, each its own ally team; unchanged in effect.

## 3. Shared victory and defeat

Upstream `LuaRules/Gadgets/game_over.lua` (KP's gamemode 1, "Kill all factories", the default):

- `f % 30 == 25` (`:76`): if a team died at the last census (`CheckForGameOver`, `:111`), look
  for a pair of teams not allied with each other among the remaining ones (`:82-89`); none left →
  `Spring.GameOver(WinningAllies)` with the remaining teams' ally teams (`:91-103`).
- Then every remaining team with no factory (homebase or MiniFac,
  `GetTeamUnitsByDefs(team, LifeSustainer)`, so nanoframes count) is `KillTeam`ed (`:107-111`) —
  regardless of whether it still has other units.
- `KillTeam` → `CTeam::Died` (`Team.cpp:255-281`): demote the team's players to spectators,
  `UpdateTeamUnitLimitsOnDeath`, `isDead`.

The port (`units::lifecycle::game_over::check_game_over`, sim, every tick):

- **`TeamFates`** (match resource): dead owners (`CTeam::isDead`), the `CheckForGameOver` flag,
  the winners once decided. `kill_team(owner)` is the one entry point — the census uses it, and
  **agent B's Resign** calls it (`ResMut<TeamFates>` in the command apply).
- On census frames (`SimTick::frame() % 30 == 25`, not the old `Time` accumulator): end check
  first, then kill every living seated owner without a factory.
- Every tick: owners killed since the last pass (census or resign) have their units' HP zeroed
  (the death path the port always used for `KillTeam`) and their unit cap shared out; every
  census re-sweeps dead owners' late spawns.
- An ally team loses when all its owners are dead; the match ends one census after the death
  that left no two enemies alive; every owner of the remaining ally team wins, dead ones
  included.
- Guards kept: no census in showcase or sandbox; a match without any factory where nobody has
  died yet (test maps) is never judged; owners without a seat are never counted.
- Note on Resign: in Spring a resigning player only starts spectating
  (`NetCommands.cpp:1195`, `TEAMMSG_RESIGN`), and a leaderless team keeps its units until the
  game-over gadget kills it; KP's gadget does not kill leaderless teams. The M1 plan makes Resign
  kill the owner instead (the common Spring-game convention, `game_end` gadgets); agent B owns
  that choice.

## 4. Local defeat no longer stops the sim

Before: `check_game_over` (a sim system) read `LocalTeam` and set `GameState::Victory/Defeat`;
`GameplaySet::*`, `ai_brain` and `reconcile_vent_claims` ran only `in_state(GameState::Playing)`,
so the loser's sim froze (and the winner's, under its panel). The headless harness inserted
`GameOverDismissed(true)` to keep all-AI matches running.

After:

- No sim system is gated on `GameState`; the sim reads no `LocalTeam` for the outcome.
- `GameState` is the result panel's state only. `game_over::announce_outcome` (`Update`) derives
  `local_outcome(setup, LocalTeam, Alliances, TeamFates)`: Defeat once the local owner is dead
  (it watches from then on — Spring's `StartSpectating`), and at the end Victory or Defeat by
  whether the local ally team won (`EndGameBox.cpp:290-315`: "Your ally-team won!"). A fallen
  player whose allies win sees Defeat, then Victory. Nothing for spectators, the demo, showcase,
  sandbox. `AnnouncedOutcome` (match resource) remembers what was shown, so "Keep on
  playing/watching" stays dismissed until the outcome changes; the menu redraws an open panel
  when the outcome changes under it (`ui/menu.rs` `maintain_game_over`).
- Single player keeps running behind the result panel (as Spring keeps simulating after
  `GameOver`); before, Victory/Defeat froze the world until "Keep on playing".
- The harness's `GameOverDismissed(true)` hack is gone.

## 5. A sim bug the to-the-end tests found: units left standing at 0 HP

The first 2v2 played to its end failed its own invariant: a dead owner still fielded five Bugs,
at 0 HP and not dying. Not the new census: any unit whose HP dropped *after* `death_system` ran
in an earlier frame (an `ExplodeAs` death blast applied by `apply_death_blasts`, which runs right
behind `death_system`; the old and new game-over sweep, which runs at the end of `Resolve`) was
never taken — in every match since M0.2 (a Bit at -4 400 HP lingered from tick 3 251).

Cause: M0.2 made `death_system` iterate `query.iter().sort::<&NetId>()` with the filter
`(Without<Dying>, Changed<Health>)`. Bevy 0.20's `QueryIter::sort` collects its keys through a
lens query that evaluates the filter with `world.last_change_tick()` / `world.change_tick()`
(`bevy_ecs` `query/iter.rs:1016-1042`), not the system's own `last_run`, so `Changed` means
"since the last frame's `clear_trackers`": a change made after the system ran last frame is
invisible to it forever. Fixed by collecting through the plain iterator (the system's ticks) and
sorting the collected items by `NetId` — same order, no missed changes. The only other sorted
query with a change filter, `update_structure_layer`'s `Changed<Transform>` pass (Hex Farm's
moved buildings), got the same fix. Both change outcomes, so they are part of `SIM_VERSION` 2.

## 6. Format and versions

- `.kpreplay` `FORMAT_VERSION` 1 → 2: `Seat::team` → `Seat::ally_team`, and seat `i` now means
  owner `i` (same bytes, different meaning — old files are refused).
- `SIM_VERSION` 1 → 2.
- Golden logs re-recorded (see §7); new `2v2_dual_core`.

## 7. Tests and numbers

Unit tests (`cargo test --workspace`, all green):

- `spring-detmath`: signed zeros both ways, NaN operands, std agreement on ordinary values,
  `det_clamp`.
- `units::teams`: 2v2 friend/foe, Duel and mirror sides, unseated owners alone, vision masks,
  `kill_team` once.
- `units::lifecycle::game_over`: kill + Victory one census later; MiniFac keeps an owner alive;
  local Defeat shown at once; **2v2 shared defeat** (one owner falls, its ally fights on, then the
  side loses and the other side wins together); a fallen player sharing its allies' Victory
  (panel re-armed); a resign (`kill_team`) destroying the owner's units on the same tick and
  passing its cap; caps passed death by death (3 × 1000 → 2 × 1500 → 3000); upstream's skipped
  neighbour in one census; census on `f % 30 == 25` only; sandbox, unseated neutrals, demo,
  pending setup, no match.
- `game_setup`: Team Game sizes over the dice range for every difficulty, the player's seat on
  its own side, the interleaved 3v3 roster and its description line.
- `units::ai`: every AI seat an owner.

Headless, release, `#[ignore]`d (`cargo test --release -p kernel-panic -- --ignored --nocapture
headless::tests::<name>`):

| test | setup | result |
|---|---|---|
| `shared_defeat_2v2` | Dual_Core, owners 0,1 (System, Hacker) vs 2,3 (Network, System), difficulty 3, seed `0x20202` | owner 1 dies at tick 15 175, its ally fights on 9 600 ticks, owner 0 dies at 24 745, ally team 1 wins at 24 775 (13.8 game min); owner 0 shown Defeat; the run repeated: same deaths, same tick, identical trail |
| `shared_defeat_4v4` | Speed_Balls_16_Way, owners 0–3 vs 4–7, difficulty 2, seed `0x5eed` | owner 1 dies at 7 465 (its side wins anyway), 6 at 7 915, 4, 5, then 7 at 20 215; ally team 0 wins at 20 245 (11.2 game min); owner 0 shown Victory |

Both check on every tick: `SimTick` advances every step (decided or not); every dead owner's
cap is 0 and an early death raised a living ally's cap above 1000; no dead owner fields a unit
60 ticks after its death; no unit stays at 0 HP for 3 ticks (§5); one ally team wins, every owner
of the other died before the end, the end is exactly one census after the last death; the state
hash keeps changing after the first death and for 300 ticks after the end.

Determinism with alliances (`determinism`, 4v4 above, 9 000 ticks so it crosses owner 1's
death; second run with uneven pacing, entity ids shifted by 37, watching as owner 3, selection
churn): sim, positions, health and orders identical for all 9 000 ticks (the fx hash differs, as
it may: it follows the local perspective).

Golden logs re-recorded (`--record-golden`; each replays its fresh log against the recording):

| log | ticks | final hash | units |
|---|---|---|---|
| `4v4_speed_balls` | 18 000 | `34fec9e3e27c005a` | 67 |
| `ffa8_quad_core` | 18 000 | `c09ef264521be3ca` | 234 |
| `2v2_dual_core` (new: the `shared_defeat_2v2` match, to its end) | 25 200 | `7ed0c4e059dcf8f0` | 52 |
| `commands_marble` | 3 000 | `1705bcf96d6fef8d` | 25 |

Query-order fuzz: all four logs replayed in full natively and again with `--shuffle 1` (every
unit's and projectile's storage reshuffled every tick): every trail identical, every tick. CI's
determinism matrix picks up `2v2_dual_core` by its glob.

## 8. For agents B and C

- **B**: friend or foe in validation is `ApplyCtx::friendly(a, b)` (`net/apply.rs`), backed by
  `Alliances`; it replaces the `units::components::is_friendly` that is gone. Resign: call
  `TeamFates::kill_team(owner)` (`ResMut<units::teams::TeamFates>`); the owner's units die on the
  same tick, the match ends at the next census if that left no enemies. The command right stays
  `TeamId == player` (owners).
- **C**: nothing in `net/mod.rs` changed. `GameState` no longer gates anything; pause on desync
  through C's own state, as planned. `TeamFates` is sim state if the desync dump wants it.

## 9. Open

- Spring's resign does not kill the team (§3); the plan's Resign does.
- Team Game uses `HalfMaxMapTeam = 4` and one AI difficulty for every seat.
- The local result panel's Restart button is single-player only (M2's lobby decides what a
  networked match offers).
