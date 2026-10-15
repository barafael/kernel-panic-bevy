# M3 (G): speed — sim cost per platform, Firefox, and Spring's speed control

Scope (`docs/multiplayer-plan.md`, M3, part G): (1) what a sim tick costs at 4v4 and in an
8-player FFA, natively and in the browsers, and why Firefox was ~6× slower than the other
engines; (2) Spring-style speed control: every peer reports its sim load, the host slows the game
for the slowest player and speeds it back up, every peer changing speed on the same tick, shown
in the player list. Code: `kernel-panic/src/net/speed.rs` (new), `session.rs`, `driver.rs`,
`wire.rs`, `control.rs`, `apply.rs`, `ui/hud/net_overlay.rs`, `ui/hud/match_status.rs`; tests
in `net/speed.rs` and `net/speed_tests.rs`.

## Summary

- **Firefox is not slow; Playwright's Firefox is.** Mozilla's own Firefox runs the wasm build
  at V8's speed (FFA, 3 000 ticks, same machine and page: Firefox 157 2.9 ms/tick, Mozilla's
  Firefox 142.0.1 3.2, Node 2.3; Playwright's patched Firefox 142.0.1 build 12.5–16.2). The same
  version from Mozilla's archive is fast, so it is that build, not Firefox and not our code; a
  10-line wasm loop shows it (the generated wasm code itself is 3–9× slower there, JS is not).
  Nothing to fix on our side. The CI Firefox row measures Playwright's build.
- **Cost per tick** (CI, today's main): 4v4 0.54 ms native (Linux x86_64), 0.40–0.60 ms in
  Node / Chromium / WebKit; 8-player FFA (~220 units) 1.26 ms native, 0.95–1.46 ms in the
  browsers. That is a load of 2–4 % of one core at 1×: speed control only matters for far bigger
  games, slow machines, or a tab that loses its CPU.
- **Speed control** (Spring's `LagProtection`): every peer measures the share of a core its sim
  needs at the current speed and sends it with its backlog once a second (`Msg::Load`); every
  2 s the host applies Spring's rule (`internal / ref × target`, clamped to [0.1, players'
  speed], averaged with the current speed) and, when the speed changes, sequences
  `SimCommand::InternalSpeed { percent }` (appended, index 20) in the name of the server seat
  (`SERVER_PLAYER` = 255, Spring's); every peer applies it on the same tick to
  `MatchControl::internal_percent`, which paces the tick driver.
- **Policy: the maximum** (Spring's `SpeedControl` 2, aim 75 %), not Spring's default median
  (aim 60 %): under the median a slow 1v1 opponent or the one slow player of a team game is never
  waited for and falls behind for good (§3.3).
- **Shown**: a CPU column in the player list (Spring's roster: green → red at 75 %, blinking for
  the player who holds the game down), the speed in the list's heading and in the status line
  ("Speed 0.50x (slowed from 1.0x: a player can't keep up)").
- **Formats**: wire version 3 (`Msg::Load`, `Msg::Loads`), `SIM_VERSION` 6, goldens re-recorded
  (same final hashes; only the stamp changed). Single player: no change (no speed control).

## 1. What a tick costs

Wall time inside `FixedUpdate` per tick ("sim") and of the whole headless frame, from the
`determinism.yml` run of 2026-10-15 (`38776888514`, the golden replays on GitHub's runners; same
method as `m0-golden-replay.md`, wasm = `wasm-release` + `wasm-opt -O2` as shipped):

| platform | 4v4 sim / frame (ms) | FFA sim / frame (ms) | FFA load at 1× |
|---|---|---|---|
| linux-x86_64-gnu | 0.54 / 2.58 | 1.26 / 3.79 | 3.8 % |
| macos-arm64 | 0.52 / 2.00 | 1.21 / 3.35 | 3.6 % |
| wasm, Node 22 | 0.40 / 0.94 | 0.95 / 1.82 | 2.8 % |
| wasm, Chromium 141 | 0.60 / 1.42 | 1.46 / 2.83 | 4.4 % |
| wasm, WebKit (Safari 26) | 0.53 / 1.23 | 1.35 / 2.56 | 4.1 % |
| wasm, Playwright Firefox 142 | 4.16 / 9.44 | 9.32 / 17.05 | 28 % |

(The 4v4 ends with 56 units, the FFA with 220; the sim has grown since M0.4, which measured
0.20 / 0.66 ms natively.) The load is the speed control's measure (§3.1): tick cost × 30 ticks.
This machine (Ryzen 7 7840HS) was shared with other jobs at a load average of 15–26 during
these measurements, so local numbers below compare runs made back to back, not absolute speed.

## 2. Why "Firefox" was 6× slower

The FFA log, 3 000 ticks, the same `pkg/` and page on this machine, one after the other
(`kp-replay`'s page, run either through Playwright or as a page that runs by itself and posts
its result back, in a browser started by hand):

| runtime | sim ms/tick | frame ms/tick | wall |
|---|---|---|---|
| native release | 1.76 | 7.10 | — |
| Node 26 | 2.34 | 6.99 | 23.5 s |
| Firefox 157 (Arch package), headless, no automation | 2.30–2.87 | 7.1–8.5 | 22.5–26.7 s |
| Firefox 142.0.1 (Mozilla's release archive), no automation | 3.25 | 10.4 | 32.9 s |
| Playwright Firefox 142.0.1 (build 1495), no automation | 12.50 | 41.2 | 131.8 s |
| Playwright Firefox 142.0.1, driven by Playwright | 16.25 | 40.3 | 125.7 s |

All trails identical. So real Firefox runs the module as fast as V8; Playwright's patched build
of the same version is 4–5× slower without any automation attached, a bit more with it.

A micro-benchmark narrows it down (`m3-speed-bench/`: n× a 16-byte copy as a wasm byte loop, and
as one `memory.copy` / `memory.fill`; ns per copy):

| | loop copy 16 B | `memory.copy` 16 B | JS loop (ns/iter) |
|---|---|---|---|
| Node 26 | 29 | 15 | — |
| Firefox 157 | 7.7 | 8.8 | 4.5 |
| Mozilla Firefox 142.0.1 | 24 | — | 4.5–15 |
| Mozilla Firefox 142.0.1, `wasm_optimizingjit` off | 48 | — | — |
| Playwright Firefox 142.0.1, no automation | 62–173 | 7.6 | 5.1–5.3 |
| Playwright Firefox, code run through `page.evaluate` | 240 | 18.6 | — |

- The calls out of wasm (`memory.copy`, `memory.fill`: 27 575 and 1 205 sites in our module)
  cost the same everywhere; JS runs at the same speed in both 142 builds.
- The wasm code itself is slow in Playwright's build: slower than Mozilla's 142 even with
  Mozilla's optimizing tier switched off; turning `javascript.options.wasm_optimizingjit` on
  in a `user.js` changes nothing. Whatever their build does to SpiderMonkey's wasm compilers,
  it is theirs.
- Code run through `page.evaluate` (as `kp-replay.mjs` and CI do) is slower again: Juggler
  evaluates through the debugger API, and SpiderMonkey compiles wasm instantiated under a
  debugger in debug mode.

Nothing in our code to fix. The CI Firefox row stays useful for determinism (the trails are
identical) but not as a speed figure; a Firefox speed row would need Mozilla's build started by
hand, as above.

## 3. Speed control

### 3.1 Upstream

- Clients report CPU once a second (`CGame::SendClientProcUsage`, `rts/Net/NetCommands.cpp:71-117`,
  `NETMSG_CPU_USAGE`): the share of wall time spent in the sim (plus some of the draw time once
  the sim passes ~65 %).
- Every 2 s (`playerInfoTime`, `rts/Net/GameServer.cpp:99`, `:811`) the server's
  `CGameServer::LagProtection` (`:881-967`) broadcasts every player's CPU and ping
  (`NETMSG_PLAYERINFO`), then takes the reference load: **median** (`SpeedControl` 1, the
  default, `:71`, `:914-926`; an even count averages the middle two) or **maximum**
  (`SpeedControl` 2) of the playing players' CPU (spectators out, `:902`; the server's own client
  in; a reconnecting player out of the maximum until it is less than 2 s behind, `:900-904`).
  The target load is 60 % for the median, 75 % for the maximum (`:942`). New speed =
  `internalSpeed / ref × target`, clamped to `[0.1, userSpeedFactor]`, averaged with the
  current speed (`:948-952`); a non-dedicated host also caps it at what its own client can
  simulate (`:955-962`). Paused: nothing (`:929`).
- `InternalSpeedChange` broadcasts `NETMSG_INTERNAL_SPEED` (`:3076`); a player's speed request
  (`UserSpeedChange`, `:3084-3093`) moves the speed at once only downwards, or when nothing
  slowed the game; otherwise lag protection raises it. Clients show the CPU per player
  (`PlayerRosterDrawer.cpp:87-109`: green → red up to 75 %, blinking while the game is slowed)
  and the speed in red while below the wanted one (`Game/Game.cpp:1615`).

### 3.2 Here

- **Measure** (`speed::LoadMeter`, in the tick driver): the mean wall time of a tick over a
  second × the ticks per second the current speed asks for (30 × speed): the share of one core
  the sim *needs*. Spring measures the share it *took*, which stops near 100 % once a peer can't
  keep up; the need keeps growing, so one step lands near a speed the peer can hold. Ticks run to
  catch up don't inflate it (the cost per tick is the same). Spring's draw adjustment is left
  out (the frame's rest is hard to measure in Bevy: vsync waits look like work).
- **Report**: a guest sends `Msg::Load { permille, behind }` (unreliable; `behind` = turns it
  knows were cut and hasn't applied) once a second; the host keeps its own.
- **Decide** (`speed::SpeedControl`, `Session::protect_speed`, host): every 2 s, over its own
  load and the connected seated guests' (spectators out; reports older than 5 s ignored), with
  Spring's formula and constants. A load above 10 is treated as 10 (a broken report can't do
  more than halve the speed per step anyway).
- **Sequence**: the new speed, in whole percent, goes into the next turn as
  `SimCommand::InternalSpeed { percent }` from `SERVER_PLAYER` (255). Guests can't send it (the
  host strips it from their batches, and every peer refuses it from anyone but the server seat,
  and refuses anything else from that seat: `Refusal::ServerOnly`).
- **Apply** (`MatchControl`): `internal_percent` (Spring's `speedFactor`) next to
  `speed_percent` (the players' speed, `wantedSpeedFactor`). `InternalSpeed` sets it, clamped to
  [10 %, players' speed]; `SetSpeed` follows `UserSpeedChange` (lower at once, or when nothing
  slowed the game). The driver paces at `internal_percent`, so every peer changes pace on the
  same tick, the host's clock included.
- **Show**: the host sends everyone's load and backlog with the pings (`Msg::Loads`, roster
  order); the player list's CPU column, its heading ("· speed 0.50x") and the status line.
- **Tests**: `Pacer::cost` stands in for a machine of a given speed: each tick costs that much
  (measured as such) and a frame runs only the ticks that fit in its length. The full-sim tests
  give every peer 1 ms (so the test machine's speed never matters; also in
  `headless::netplay`'s real-WebRTC peers).

### 3.3 Median or maximum

Spring defaults to the median and tolerates the laggard: its client simply plays behind the
server and catches up when it can. Here a guest more than a minute behind can no longer be
served gap fills, and its commands land later and later. And with the median a 1v1 never waits
for the slower player (the median of two is their average: a peer needing 150 % against one
needing 3 % settles where it still needs 100 %). `the_median_leaves_a_slow_guest_behind_full_sim`
shows it: one of three peers able to run two thirds of 1× is 120 ticks behind after 12 s and
falling 10 ticks a second further. So the default is the maximum; `Session::set_speed_policy`
switches.

### 3.4 Full-sim test

`speed_tests::a_slow_guest_slows_the_game_full_sim`: three players and an AI on Marble Madness,
30 ms one way, 30 Hz frames; one guest's machine runs 20 ticks a second (50 ms a tick: a load of
1.5 at 1×). Speed steps the host sequenced (tick, %), the same on every peer:

```text
slow:     (61, 75) (107, 56) (142, 47) (171, 49) (201, 50)
recovery: (536, 75) (582, 88) (635, 94) (692, 97) (751, 99) (812, 100)
```

Settled at 50 % (the slow guest's load 75 %) within 10 s (five steps, 2 s apart); from then on
that guest was at most 3 ticks behind the host. Made fast again (1 ms a tick), the game is back
at 1× six steps (12 s) later. Players'
speed (`speed_percent`) never changed; every peer's trail identical (1 179 ticks); the other
guest's player list showed the slow guest's load. Traffic: a load report is ~16 bytes a second up,
the host's broadcast ~10 + 4 bytes per peer a second down.

Also: `speed::tests` (Spring's formula and constants, settling and recovering, the meter),
`net_overlay` (CPU column, blinking, heading), `match_status`, `codec` (index 20 is two bytes),
the command fuzz (the server's `InternalSpeed` in the chaos stream, a seat's in the malformed
stream: all refused) and the query-order fuzz, both clean.

## 4. Single player

No change: a solo session never reports or decides (Spring's local server would slow a single
player game down too; here the frame clock simply gets fewer ticks in on a slow machine).
`MatchControl::speed()` is the internal speed, equal to the players' speed unless the speed
control lowered it. The goldens were re-recorded for `SIM_VERSION` 6; every log ends on the
same hash as before.

## 5. Open

- **Draw time** is not part of the load: a peer whose *frames* are slow (rendering, a busy
  browser tab) but whose sim is cheap falls behind without the load showing it. Its `behind`
  is reported and shown, and could feed the rule (e.g. as Spring's reconnect rule in reverse);
  careful with hidden tabs (agent I), which would slow everyone to 10 %.
- **Rejoin (agent H)**: a rejoining peer fast-forwards; its need per tick is the same, so it
  doesn't slow the game, but Spring's exclusion of a reconnecting player from the maximum
  (`isReconn`) may still be wanted if the catch-up shows up in its load.
- **The policy** is a host-side constant; a lobby option (Spring's `SpeedControl` setting) would
  need `MatchRules` to grow.
- **A Firefox speed row** in CI would need Mozilla's Firefox started by hand (the page that runs
  by itself, `m3-speed-bench/stock.mjs` style), not Playwright's build.
