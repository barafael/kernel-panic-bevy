# Multiplayer plan

Up to 4v4 (8 players), native and web players in the same match.

Based on the October 2026 investigation (experiment branches listed at the end):
the original's netcode (Spring/Recoil + Kernel Panic), the sibling projects
`../cake` and `../omdurman`, a headless determinism/bandwidth harness, a lockstep
command-layer prototype and a matchbox WebRTC benchmark.

## Decision

**Deterministic lockstep, host-sequenced, over matchbox WebRTC** — the original's
model (CGameServer: relay + frame clock, every client simulates) in cake's shape
(the lowest-PeerId peer sequences every command; everyone, host included, applies
commands only from the host's turn stream).

- Only commands travel: ~250–430 B/s payload per player, ~3–4 KB/s per client
  through the host including WebRTC overhead, independent of unit count.
- The AI runs inside the sim on every peer (as KP's synced Lua AIs did): zero
  traffic, survives any player dropping.
- Replays are free (setup + seed + command log); the per-tick state hash doubles
  as desync detection and as the cross-platform determinism test.
- Rejected: host- or server-authoritative state replication. Measured 13–66 KB/s
  per client at 30 Hz (extrapolated ~340 KB/s for 1600 units), 0.16–0.6 MB/s host
  upload, plus weapon/impact events and COB piece animation the clients would have
  to reproduce; WebRTC under 0.5–1 % loss sustains only ~120–500 KB/s per client.

## Will different hardware compute the same results?

**Yes, proven by M0.4's golden-replay matrix** (`net-experiments/m0-golden-replay.md`):
three golden logs (two 18 000-tick AI matches, one scripted-command match) give
byte-identical per-tick state hashes on Linux x86_64 (glibc, musl), Linux arm64,
Windows x86_64, macOS arm64, macOS x86_64 under Rosetta, and the web build in Node,
Chromium, Firefox and WebKit. The matrix runs on every push (`determinism.yml`). What
made it so:

| Source of difference | Status | Remedy |
|---|---|---|
| `+ − × ÷ sqrt`, conversions | IEEE-754 correctly rounded on x86_64, aarch64 and wasm32 — bit-identical on every CPU | none needed |
| Fused multiply-add | rustc never contracts `a*b+c` on its own (no fast-math); only explicit `mul_add` fuses | **Done (M0.3):** `mul_add` banned by `clippy.toml`; no fast-math or contraction flags anywhere |
| Transcendentals (`sin cos tan atan2 asin acos powf exp ln hypot`) | **Differ**: measured glibc vs wasm: sin/cos 1.2 %, atan2 16 %, powf 9 %, hypot 12.5 % of inputs. Native uses each OS's libm (glibc, macOS, MSVC all different); wasm uses Rust's bundled musl-derived libm — identical in every browser on every CPU | pure-Rust `libm` everywhere: Bevy's `libm` feature (→ `bevy_math/libm` → `glam/libm`, and `bevy::math::ops`); clippy `disallowed-methods` for `f32::sin` & co. in sim code. **Done (M0.3):** probe shows 0 differing bits across glibc, musl, wasm32 (Node, wasm-opt), an MSVC build |
| SIMD code paths (glam SSE2 vs NEON vs wasm simd128 vs scalar) | **Differ**: different summation order; measured glam slerp 48 %, `Vec3A::normalize` 29 % between targets, and `target-cpu=native` alone changes 20 % of slerp results | glam `scalar-math` feature (one code path everywhere); no `target-cpu`/`target-feature` flags for release builds. **Done (M0.3):** 0 differing bits, even with `target-cpu=native` |
| NaN payload bits | nondeterministic on wasm | never hash or branch on NaN bits. **Done (M0.4):** the state hash canonicalises NaNs and counts them (0 in every golden run) |
| Denormals / FPU modes | Rust never sets flush-to-zero; a driver or library could on its thread | debug assertion of MXCSR at the start of each sim tick (x86_64) — **done (M0.3)**, plus FPCR on arm64 |
| HashMap iteration (RandomState), query order, entity ids, wall clock, multithreaded executor | found and partly fixed in X-sim / X-commands (see M0) | ordered collections, canonical sorts, sim ids, tick counter, single-threaded sim executor. **Done (M0.2):** `BTreeMap`s, `NetId`-order iteration, match tick and seed, single-threaded `FixedUpdate`. **M0.4:** wasm and native (and native peers with different devices) hold different archetype orders; a query-order fuzz test (storage reshuffled every tick) guards it in CI, and found the nav-grid stamping order |

Web-to-web is identical across hardware (same wasm, same bundled libm, strict IEEE
in every engine). Native-to-native across operating systems and native-to-web needed
the remedies above; with them, no subsystem needs fixed-point. Left to watch:
`f32::max`/`min` on ±0 differ between x86_64 and aarch64 (not hit by the golden logs;
see the M0.4 write-up).

## Milestones

Estimates are focused-work rough guesses.

### M0 — Determinism foundation and proof (gate) · ~2 weeks

0.1 **Harvest the experiments.** Bring in X-sim's headless harness, state hash,
    and six determinism fixes, and X-commands' `NetId`. Unify `SimSerial` (X-sim)
    and `NetId` (X-commands) into one sim id assigned in spawn order, used for
    slow-update phases, RNG seeds, AI ordering and network references.
    *Done:* both branches merged whole, so the command layer (all input and the
    AI through `CommandBus`) is in too. `NetId` is the one unit id (required
    component of `UnitType`, match-scoped allocator); it also keys the headless
    state hash. Throwaway scaffolding dropped (netprobe, snapshot encoders); the
    float probe stays in `net-experiments/`. Notes in `net-experiments/x-sim.md`
    and `x-commands.md`.

0.2 **Remove the remaining nondeterminism found by X-sim:**
    - the match seed comes from the match setup (host-chosen), never per-launch entropy;
    - ~10 system `Local`s acting as RNGs/frame counters → match-scoped resources
      (fixes the rematch divergence; the attract demo must not leak state into a match);
    - sim time = integer tick counter (`sim_frame()` and the AI's `elapsed_secs()`
      currently count from app start, as f32);
    - std `HashMap`s holding sim state → `BTreeMap` / fixed-hasher maps;
    - Hex Farm tower ownership out of the frame loop;
    - the remaining 8 ambiguous system pairs in `FixedUpdate`;
    - run `FixedUpdate` on the single-threaded executor (it is also faster at
      these unit counts: 1.12 vs 1.69 ms median at ~400 units), or root-cause the
      executor divergence at tick 7188;
    - `AutoHold::initial` depending on `LocalTeam`; anything else the harness finds
      when `LocalTeam` varies between peers.

    **Done** — `net-experiments/m0-nondeterminism.md`: `GameSetup::seed` →
    match-scoped `MatchSeed` (no per-launch entropy, `test_seed` gone);
    `SimTick` match tick; every sim RNG a match-scoped `SimRng<Stream>` seeded
    from the match seed, frame counters on `SimTick`; `BTreeMap`s; Hex Farm
    ownership in the fixed tick; 0 ambiguous pairs; single-threaded `FixedUpdate`
    always (`KP_SIM_EXECUTOR` removed); `AutoHold` and the AI's teams from the
    seats (`AiSeats`). Also found by the rematch: ECS query order follows the
    app's archetype history (earlier matches, the attract demo, per-peer UI
    markers), so order-dependent sim systems iterate in `NetId` order and the
    damage queue resolves canonically. Proof: 4v4 and 8-AI FFA identical for
    18 000 ticks (uneven pacing, entity-id shift, local seat varied, selection
    churn, two processes); rematch after a 4 000-tick combat warmup identical
    (main: tick 1 561). Left for M1: local defeat stops the loser's sim.

0.3 **Deterministic math.** Enable Bevy's `libm` feature and glam `scalar-math`;
    route every transcendental in sim code (kernel-panic sim modules,
    spring-pathfinding; offline bakers excepted) through `bevy::math::ops`/`libm`;
    enforce with `clippy.toml` `disallowed-methods`; audit `mul_add` and build flags
    (Trunk/wasm-opt flags, `.cargo/config.toml`). Measure the sim cost change.
    **Done** — `net-experiments/m0-deterministic-math.md`: 182 call sites converted,
    `clippy.toml` enforcement, flag audit (no risky flags; wasm-opt is float-safe),
    `net-experiments/m0-floatprobe` (0 differing bits on every target available
    locally), no measurable sim cost change.

0.4 **Golden-replay matrix.** A headless `replay` test/binary takes
    (setup, seed, command log), runs it, and prints the per-tick hash trail.
    Record golden logs from headless 4v4 and FFA games (≥18 000 ticks), then
    compare trails on:
    - Linux x86_64 (glibc and musl), Windows x86_64, macOS arm64 — GitHub Actions
      matrix (new CI workflow; Intel and AMD runners give CPU variety);
    - wasm32 in Node, and in headless Chromium, Firefox and WebKit (wasm-bindgen-test
      or Playwright).

    The workflow skeleton exists: `.github/workflows/determinism.yml` (called from
    `ci.yml`) has the matrix, the `trail-<platform>` artifacts and the comparison
    (`.github/scripts/compare-trails.sh`); the contract at its top says where the
    golden logs go and which two steps the replay fills in.

    **Gate:** identical trails everywhere → M1. Otherwise bisect with the per-tick
    hash, fix or move the subsystem to fixed-point.

    **Done — gate passed** (`net-experiments/m0-golden-replay.md`). `.kpreplay`
    (setup with seed, compat stamp, human command frames; postcard), native
    `kernel-panic --replay` and a wasm `kp_replay` export (feature `replay`), Node and
    Playwright runners, golden logs in `kernel-panic/tests/golden/` (4v4 and 8-player
    FFA, 18 000 ticks each; a 3 000-tick match through every command kind). All 11
    platform rows identical over every tick, plus the query-order fuzz row. wasm costs
    ~2× native per sim tick in V8/JSC (6× in Firefox). One more query-order dependence
    fixed: nav-grid structure stamping (`update_structure_layer`) now runs in `NetId`
    order.

### M1 — Lockstep core · ~1.5 weeks

- The command layer (`net::{SimCommand, CommandFrame, CommandBus}`, one
  apply system at the head of `FixedUpdate`, postcard with gap-coded ids and
  1/8-elmo positions) is merged since M0.1. Route the rest: dev tools, game
  speed/pause, resign.
- Validate commands at apply time (ownership, unit alive, ally), as the original
  validated on the receiving clients.

  **Done (B: commands)** — `net-experiments/m1-commands.md`: the KP_BOT dev tool,
  the showcase director and the screenshot tool issue commands; new `SetSpeed`,
  `Pause`, `Resign`, `Dev` (spawn / kill / health / god mode, single player only)
  appended to `SimCommand`; Pause and Alt+= / Alt+- in game with a status line;
  the tick driver reads speed and pause from `MatchControl` (every peer pauses,
  resumes and speeds up on the same tick; `pause_and_speed_full_sim`).
  Validation at apply time: seated and not resigned sender, live own units,
  capabilities (`AllowedCommand`), points on the map, vent sites, canonical
  lists and counts, cheats; alliance checks through `Alliances::friendly`
  (`ApplyCtx::friendly`). A schedule-access audit fails on frame-loop writes to
  sim state. Command fuzz (random and malformed frames from every seat; twice,
  shuffled, with malformed: one trail) in `cargo test` and `determinism.yml`.
  Integrates A and C; golden logs re-recorded at `SIM_VERSION` 3, then 4 with
  the leaving rules below.

  **Decision (user, 2026-10-15): leaving play.** "When the last player on a
  team resigns or gets taken out, all remaining units explode over a duration
  of 4 s, in a random fashion (each has a random ttl of 0–4 s). Same in 1v1.
  If other players are active, then anybody can hit the flashing TAKE button
  and the units are added to them. Unit cap stays untouched."
  **Done** (`net-experiments/m1-commands.md` §2): resigning, being taken out
  (no factory) and, later, dropping all go through `TeamFates::leave`. With an
  ally still active the leaver's units stay, idle and friendly, and every
  active ally sees a flashing TAKE button (`SimCommand::Take { from }`; the
  first in turn order gets all of them, later ones are refused; an AI ally
  takes at once when no human ally is left, after 10 s otherwise). The ally
  team's last leaver fuses every remaining unit of the side (0–120 ticks from
  a `SimRng` stream; the regular death blast); the match ends at the first
  census after the last explosion. Caps never change (upstream's
  `UpdateTeamUnitLimitsOnDeath` is gone; a taker may go over its cap). Resign
  is in the Esc menu (with a confirmation) and KP_BOT (`take <owner>`).
  Departs from upstream: Recoil only makes a resigner spectate and kills a
  dead team's units at once.
- **Seats and alliances.** 4v4 needs ally teams: `is_friendly` by alliance,
  shared victory/defeat, KP's FixedAllies. AI seats come from the match setup
  (`AiSeats`), not "every team except the local one".
- **Decouple the sim from local UI state.** Local defeat must not stop the sim
  (today it flips `GameState`, which gates the gameplay chain); the losing player
  keeps simulating as a spectator.
- **Tick driver.** `FixedUpdate` advances only when `CommandBus::ready()` for the
  next tick; adaptive input delay (start at 3 ticks = 100 ms, follow the measured
  RTT/jitter); the host gets the same delay as everyone (no host advantage); a
  peer that falls behind runs extra ticks to catch up (cake's TARGET_BACKLOG).
- **Desync detection.** Every peer sends its state hash every 30 ticks; the host
  compares (Spring's "dictatorship"), announces a desync, and all peers write a
  state dump for the hash bisect. Policy: pause and show the error (Spring keeps
  playing broken; we stop).
- In-process multi-peer tests (several headless apps, simulated latency/loss) —
  the X-commands lockstep test grown to the full sim.

**Runtime done** (`net-experiments/m1-runtime.md`): a `Transport` trait (reliable +
unreliable channels, peer events; matchbox-shaped) with a deterministic simulated
network (latency, jitter, loss, duplication, reordering, dead links); the host
sequences every peer's numbered command batches into turns, sent unreliably with
the last 5 repeated, gap fills on the reliable channel; the tick driver replaces
Bevy's fixed loop and runs one turn per tick of game time (host = clock, guests
catch up past a 2-turn backlog, ≤ 8 extra ticks a frame), single player being a
solo host with delay 0 (golden trails unchanged); the input delay follows the
measured lag (start 3, [1, 15] ticks, ≈ RTT/33 ms + 2), the same for the host;
state hash every 30 frames, the host compares, every peer stops at the same tick
and dumps. 2- and 8-peer full-sim tests (0–200 ms, 1–5 % loss, reordering, a dead
link, a forced desync, peers leaving) agree on every tick. Speed/pause read from
the sim (B's `MatchControl`, merged).

**Sim rules (agent A) — done** (`net-experiments/m1-sim-rules.md`):
- `f32`/`f64` `max`/`min`/`clamp` banned in `clippy.toml`; `spring-detmath`'s
  `DetFloat` (compare-and-select, `-0 < +0`, NaN operand ignored) replaces all 378
  call sites; glam's scalar `Vec*::max/min/clamp` are compare-and-select already.
  New float-probe `det:` rows cover both, 0 bits differ locally.
- Seats and alliances as upstream: seat `i` is owner `i` (`TeamId`, Spring's team:
  units, unit cap, packet buffer, AI brain, command right), `PlayerSpec::ally_team`
  is the allyteam; `units::teams::Alliances::friendly(a, b)` answers friend or foe
  everywhere upstream asks `AreTeamsAllied` / `allyteam` (targeting, line of fire,
  kamikaze, Firewall, area denial, cloak detection and shared vision, bugger-off,
  avoidance, the AI). Fixed alliances. A dead owner's unit cap goes to its allies.
  Outgunned's AI seats are one owner and one AI each now (upstream); new Team Game
  grouping (AI allies) in the skirmish menu.
- Shared victory/defeat per `game_over.lua`: an owner without a factory dies
  (`TeamFates::kill_team`, also the hook for agent B's Resign); an ally team loses
  when all its owners are dead; the end comes one census after.
- The sim is never gated on the local outcome: `GameState` is the result panel only
  (`announce_outcome`, `Update`); a dead player watches. The harness no longer
  fakes `GameOverDismissed`.
- `.kpreplay` format 2, `SIM_VERSION` 2 (3 with B's validation); golden logs
  re-recorded plus a 2v2 with mixed ally teams.

### M2 — Transport and lobby · ~1–1.5 weeks

- ~~Port cake-net's `Sequencer`, `TurnBuffer`, `HashLog`, envelope, zero-length
  guard~~ (done in M1: `net::session`, `net::wire`). Port `RoomId` and its lobby pattern (host election = lowest
  PeerId, roster, `Start`; single player is a solo host on the same code path).
- matchbox fork at rev `27b5e50` (Bevy 0.20, socket hardening), as cake/omdurman.
- **Channels** (X-transport findings):
  - turns on the **unreliable** channel, every packet repeating the last 5 turns
    (webrtc-rs SCTP has a 1 s minimum retransmission timeout, so a lost packet on
    a reliable channel freezes lockstep for seconds); a reliable channel for
    lobby/control, hashes, chat and gap fills;
  - message size ≤ 16 KiB, fragment above (65 536 bytes exactly kills the
    connection; larger messages vanish silently);
  - bounded send queues and our own flow control (matchbox has none);
  - `try_update_peers()` only (`update_peers()` panics once the loop has ended).
- **Signaling:** omdurman's server `wss://omdurman-matchbox.fly.dev`, rooms
  `kp-<room>` (namespaced like cake's `cake-<room>`); room code in the URL fragment
  on the web, an argument/env natively; overridable at build time (`MATCHBOX_SERVER`).
  That server is full-mesh: peers also connect guest↔guest, which we ignore
  (only host links carry traffic; a failed guest↔guest link is harmless). It
  auto-stops when idle, so the first join may take a few seconds.
- **Match start handshake:** map, seed, options, seats, and a compatibility hash
  (sim version + `units.kpu` hash + map hash); mismatches are refused, as Spring
  refused mismatched versions and archives.
- **TURN-ready, not deployed:** one `IceConfig` (list of `RtcIceServerConfig`:
  urls, username, credential) read from build-time env / a settings field and
  passed to `WebRtcSocketBuilder::ice_server`; default STUN only (Google, as
  cake/omdurman). Adding coturn later is configuration, not code.
- Localhost matchbox tests with netem loss/latency (X-transport's `unshare -rn`
  setup) in CI-able form.

**Transport (agent D) — done** (`net-experiments/m2-transport.md`):
`net::matchbox::open_room(&RoomId, &NetConfig) -> Box<dyn Transport>` over the fork
(`branch = "main"`, locked at `27b5e50`), native and wasm. `RoomId`: 1–24 of `a-z0-9-_`,
lower-cased, `random()` 6 unambiguous characters; rooms `kp-<code>`. `NetConfig`: signaling
(default fly.io, `MATCHBOX_SERVER` at build time, `--signaling` / `KP_SIGNALING`) and ICE
servers (Google STUN; `KP_ICE` / `--ice`, TURN with credentials; matchbox takes one
`RTCIceServer`, so the URLs merge). Channel 0 reliable / 1 unreliable, `try_update_peers` only,
the 16 KiB cap asserted, per-link token bucket (256 KiB/s) and bounded queues; a peer the
fork reports disconnected leaves only after 3 s of silence (its signaling may drop while its
data channels live on). `Transport::failure()` reports an unreachable server. Tests: 2- and
8-peer full-sim matches over localhost WebRTC (in-process signaling server with
`matchbox_server`'s race-free rooms), netem (`scripts/netem-matchbox.sh`, CI with `sudo tc`),
and a wasm peer in headless Chromium against a native peer — identical trails (897 ticks).
Finding: webrtc-rs's unreliable channel retransmits and stalls 100–300 ms on loss, which pins
M1's max-based input delay at its 15-tick cap under any loss (M3).

**Lobby (agent E) — done** (`net-experiments/m2-lobby.md`): `net::lobby` on any
`Transport`: host = lowest compatible `PeerId`, re-elected in the lobby (a new
host takes the room over as last seen; every `Hello` carries it); the host's
room (map, AI difficulty, ≤ 8 seats open / AI / peer with faction and ally
team, members with ready flags and the host's verdict); `Start` carries the
setup with the host's seed, the session roster and the `Compat` stamp (moved
to `net::compat` with `SIM_VERSION`); a guest whose stamp differs refuses with
the reason and leaves. Lobby packets start with a tag no session envelope has.
Multiplayer page in the launch menu, `#room=` / `--room` / `KP_ROOM`.
`lobby::connect` opens the room with `matchbox::open_room` (tests: an
in-process `SimNetwork` per room code); the page shows `Transport::failure`. A demo swap never takes a waiting
`NextNetMatch`; a networked match's `LocalTeam` is the roster's seat from the swap on.

**Networked play (agent F) — done** (`net-experiments/m2-networked-play.md`):
- Drops: a guest that disconnects or is silent past the timeout (30 s before its
  first packet, Spring's `InitialNetworkTimeout`; 60 s after, the host's turn
  history) is dropped by the host, which sequences `SimCommand::Dropped` (index 19)
  in that seat's name after its last batch; every peer applies it on the same tick
  as a resignation (`TeamFates::leave`). A timed-out guest is told (`Ended::Dropped`);
  a guest that hears nothing from the host gives up (`Ended::HostLost`); the host
  leaving still ends the match.
- Host policy: `MatchRules { pausable, min/max speed, cheats, host }` in `GameSetup`
  (default = single player); one check (`apply::rules_admit`) used by the host
  before sequencing and by every peer at apply time; Spring's host-may-always-pause
  exemption, the host's seat filled from the roster. Replay format 3.
- `Take` hands the leaver's packet buffer over with its units (Spring's
  `GiveEverythingTo` moves resources; KP's buffer gadget has no hand-over).
  `SIM_VERSION` 5, goldens re-recorded.
- UI: player list (seat, A/E/S, name, ping in Spring's colours; F9), banner
  (desync, host left / lost, dropped, waiting, who left), Esc "Leave match"
  (resign, wait for the host's ack ≤ 1.5 s, close the transport, menu), no Restart
  in a networked match. The host broadcasts everyone's ping once a second
  (`Msg::Pings`, wire version 2).

### M3 — Robustness · ~1.5–2 weeks

- **Speed control** (Spring's LagProtection): peers report sim load once a second;
  the host sets game speed from the median; the web build is the likely slowest
  simulator — measure wasm tick cost at 4v4 first.
- **Browser tabs:** a hidden tab throttles timers to ~1 Hz. On `visibilitychange`
  warn, then treat the peer as dropped after N seconds. Investigate running the
  sim in a Web Worker so it keeps ticking in the background.
- **Drops:** a dropped player's units stay (as in Spring); allies may take them.
  Host leaving ends the match for now (host migration later).
- **Rejoin:** replay the command log from tick 0 at max speed (Spring's approach).
  ~54 000 ticks for 30 minutes ≈ 80 s natively at ~1.5 ms/tick; snapshot rejoin is
  a later optimisation.
- Spectators (receive the turn stream, never send), replay files and a replay
  viewer.

### Later

TURN deployment, host migration, snapshot rejoin, reconnect polish (omdurman's
findings: stable player keys, retransmit until echoed, uid dedup). Lockstep cannot
prevent map hacks (every peer holds full state), as in the original.

## Experiment branches to harvest

| Branch | Contents |
|---|---|
| `worktree-agent-a54879b76d9ed827f` (X-sim) | headless harness (`kernel-panic/src/headless/`), state hash, 6 determinism fixes, float probe, `net-experiments/x-sim.md` |
| `worktree-agent-a00e3ba806652b7c0` (X-commands) | `kernel-panic/src/net/` command layer, all inputs + AI routed, in-process lockstep test, `net-experiments/x-commands.md` |
| `worktree-agent-a2b197a1c62028710` (X-transport) | `net-experiments/transport-bench/` matchbox benchmark, netem scripts, `net-experiments/x-transport.md` |

## Open questions

- ~~Determinism remedies cost sim speed (scalar glam, software libm).~~ M0.3: no
  measurable change in 4v4 and 8-AI FFA headless games (within ±10 % noise;
  `net-experiments/m0-deterministic-math.md`).
- ~~macOS and Windows libm were not testable locally.~~ M0.4: the CI float probe and
  golden trails are bit-identical on Windows (MSVC), macOS arm64 and macOS x86_64.
- No browser was in the WebRTC benchmark; M2 adds a browser↔native test.
