# M1 runtime: host-sequenced lockstep, tick driver, desync detection

Goal (`docs/multiplayer-plan.md`, M1, the runtime third): the fixed sim advances only by
applying the host's turn stream, with an adaptive input delay, catch-up for peers that fall
behind, stall handling, and desync detection that stops the match and dumps the state. All of it
transport-agnostic behind a small trait; M2 plugs matchbox in. Single player runs the same code
as a solo host.

Code: `kernel-panic/src/net/` — `transport.rs` (the trait), `wire.rs` (envelope and messages),
`session.rs` (the protocol: sequencer, turn buffer, delay controller, hash check), `driver.rs`
(the tick driver replacing Bevy's fixed loop, `GameSpeed`, `NetStatus`, the dump, the banner),
`sim_net.rs` (the simulated network, tests only), `state.rs` (the state hash, moved from
`headless/`), `session_tests.rs` and `multipeer_tests.rs`.

## 1. Result

- **Every peer computes the same thing under every network tested.** 2 and 8 peers, full sim
  (several headless game apps in one process), links from 0 to 200 ms one way with jitter,
  1–5 % loss, duplication and reordering, 60 Hz and 30 Hz frames: per-tick state hash trails
  identical to the host's on every tick (`multipeer_tests`). The protocol-level tests sweep the
  same conditions with a stand-in sim in milliseconds (`session_tests`).
- **No host advantage.** The host's commands wait the same input delay as everyone's: measured
  latency host 9.8 / guest 9.8 ticks at 60 ± 30 ms one way with 5 % loss, 12.6 / 12.6 at 100 ms.
- **A forced desync is found at the next hash frame** (a unit nudged on one guest after tick 200
  was reported at frame 210); every peer stops after the same tick (211) and writes a dump.
- **A dead link stalls only that guest.** It shows "Waiting for peer-0001…", asks for a gap fill
  on the reliable channel when the link returns, catches up at ≤ 8 extra ticks a frame, and ends
  within a few ticks of the host; the other peers never notice.
- **Single player is unchanged.** It runs through the same driver as a solo host with delay 0:
  the three golden replays give byte-identical trails before and after (`--replay`, all 39 000
  ticks), the query-order fuzz passes, and a command applies on the tick after it was issued.
- **Cheap.** About 3–4 KB/s per guest and 24 KB/s host upload at 8 players including WebRTC
  overhead; the state hash costs 0.15–0.9 ms every 30 ticks at 4v4 / FFA scale (≈ 0.5 % of sim
  time).

## 2. Design

### Shape

Spring's frame clock in cake's form (`docs/multiplayer-plan.md`, Decision):

1. **Seal.** At the head of every tick `t` a peer seals its local commands into a frame for
   `t + delay` (`CommandBus::seal`, now once per tick and routed to the outbox). The driver
   numbers each non-empty frame as a `Batch { seq, target, commands }`.
2. **Sequence.** Guests send their unacknowledged batches to the host (unreliable, every packet
   repeating all of them until acked). The host files each batch exactly once, in seq order
   per player, at `max(target, last target of that player, next tick to cut)` — a late batch
   goes into the next turn, never dropped (omdurman's finding 3: input must not get lost; its
   identity is the `(player, seq)` pair). The host's own batches take the same path.
3. **Cut.** The host is the clock: on every tick of game time it cuts the next `Turn { tick,
   delay, frames }` — every batch filed for that tick, by player — and applies it itself. It
   waits for nobody once every guest has sent its first packet (Spring's server likewise:
   `CGameServer::CreateNewFrame`, `rts/Net/GameServer.cpp:2578`). The player a command counts
   for is the roster's seat of the peer that sent it (cake), not what the packet claims.
4. **Apply.** Every peer, the host included, applies turns in tick order through
   `CommandBus::deliver_turn`; nothing local is applied directly (`CommandBus::routed`).
   The in-sim AI still issues into the bus on every peer and is never sent.

The lowest `PeerId` of the roster is the host (cake). The host leaving ends the match
(`Ended::HostLeft`, no migration); a guest leaving makes the host forget its link, its units stay
and stay idle (Spring: `CPlayerHandler::PlayerLeft`, `rts/Game/Players/PlayerHandler.cpp:66`,
only marks the player inactive).

### Transport trait (`transport.rs`)

```rust
pub struct PeerId(pub u128);                  // matchbox's UUID; lowest = host
pub enum Channel { Reliable, Unreliable }
pub enum PeerEvent { Joined(PeerId), Left(PeerId) }
pub trait Transport: Send + Sync + 'static {
    fn local_id(&self) -> Option<PeerId>;
    fn poll_peers(&mut self) -> Vec<PeerEvent>;          // matchbox: try_update_peers
    fn send(&mut self, peer: PeerId, channel: Channel, bytes: Vec<u8>); // best effort, ≤ 16 KiB
    fn receive(&mut self, channel: Channel) -> Vec<(PeerId, Vec<u8>)>;
}
```

It mirrors matchbox's socket API so the M2 adapter is thin: channel 0 reliable, 1 unreliable
(`max_retransmits = 0`, unordered), `try_update_peers` only (`update_peers` panics after the loop
ends, X-transport), messages never empty and never over `MAX_MESSAGE` = 16 KiB (65 536 bytes
exactly kills a webrtc-rs connection). The session builds every message under that bound: turn
packets drop their oldest turns, input packets halve their batch list, gap fills go out in
chunks.

### Wire format (`wire.rs`)

Every message is `Envelope { version: WIRE_VERSION, match_id, msg }` in postcard; another
version or another match's packets are dropped (a stale packet from the previous match in the
same room).

| message | direction | channel | content |
|---|---|---|---|
| `Turns` | host → guest | unreliable, one per tick (+ 10 Hz keepalive) | the newest ≤ `REDUNDANCY` = 5 turns the guest has not acked (ticks implied from `first`), the guest's batch ack, newest tick, clock stamp + echo |
| `Input` | guest → host | unreliable, ≤ one per frame (+ keepalive) | turn ack, the tick it sealed at, its unacked batches, stamp + echo |
| `Resend { from }` / `Fill(TurnRun)` | guest ↔ host | reliable | gap fills, ≤ 900 turns a request |
| `Hash { frame, hash }` | guest → host | reliable | every 30 frames |
| `Desync { frame, stop_at, peers }` | host → all | reliable | |
| `Left { peer }` | host → all | reliable | |

Turns ride the unreliable channel and repeat the last five, because a reliable channel freezes
lockstep for a retransmission timeout (≥ 1 s in webrtc-rs) on every loss (X-transport §4). A
five-turn packet with one order in it is under 48 bytes.

### Tick driver (`driver.rs`)

`LockstepPlugin` disables Bevy's `RunFixedMainLoopSystems::FixedMainLoop` set and runs
`drive_fixed_main` in its place; per frame (`drive_frame`):

1. take in the network (`Session::poll`);
2. add the frame's virtual delta × `GameSpeed::factor` to the clock (nothing while paused or
   before the start);
3. **host:** while a tick's worth of clock is banked, seal → file → cut → apply → run
   `FixedMain`; **guest:** run one held turn per tick of clock; if more than
   `CatchUp::target_backlog` = 2 turns are left, run extra ticks (cake's `TARGET_BACKLOG`; Spring's
   client eats its queue faster the longer it is, aiming to stay two frames behind,
   `CGame::UpdateNumQueuedSimFrames`, `rts/Net/NetCommands.cpp:161-216`), at most
   `max_extra` = 8 a frame and 11 ms of wall time (Spring balances sim and draw time the same
   way, `GetNetMessageProcessingTimeLimit`, :248). A guest the clock owes a tick but holding no
   turn is **starved**: it banks at most one tick (else the burst on arrival plays in
   fast-forward) and reports whom it waits for;
4. every 30th frame (networked only) hash the state and report it;
5. send (`Session::flush`); set `Time<Fixed>`'s overstep to the banked clock, so the render
   blend (`SimPose`) reads the same fraction as under Bevy's loop.

Per tick it does exactly what `run_fixed_main_schedule` did (advance `Time<Fixed>`, make it the
generic `Time`, run `FixedMain`), so every fixed system is untouched.

**Single player** is `Session::solo()`: a host with no guests, delay bounds 0/0/0, no
transport, no hashing, no history. One tick per 1/30 s of virtual time, at most three a frame
(the existing 100 ms `Time<Virtual>` cap), local commands sealed at the tick head for that same
tick: the very tick they landed on before. `uneven_frames_stay_smooth` drives frames of 0–120 ms
through it and gets Bevy's tick count; the goldens stay byte-identical.

**Interpolation with bunched ticks.** The drawn pose is `lerp(prev, curr, overstep)`, where
`prev`/`curr` are the last two ticks run. With 0 ticks in a frame the fraction grows toward 1
(the pose approaches the newest tick and holds there when starved: the bank is capped at one
tick); with 2–3 ticks `prev` jumps to the second-to-last tick, never behind the pose drawn
before. Checked for 0/1/2/3 ticks per frame: the drawn position never steps back and trails the
sim by at most one tick. Catch-up frames (up to 8 extra ticks) do show as fast-forward — the
same as Spring rejoining.

### Adaptive input delay (`DelayController`)

Every guest packet says which tick the guest sealed at; on arrival the host's clock is at
`next_cut`. `next_cut − tick` is the delay a command sealed at that moment needed to arrive in
time: the round trip, the guest's backlog and that packet's jitter, measured in one number. The
delay is the worst need of the last 2 s + 1 tick, clamped to [1, 15] ticks (start 3 = 100 ms),
rising at once, falling one tick per 2 s of a lower target (hysteresis). Samples from a guest
that is catching up (turns missing beyond 30 ticks, or more than 6 in hand) are ignored: its lag
is not the network's, and would hold everyone at the maximum for half a minute. Each turn carries
the delay (one byte), so all peers switch together; the host uses the same one (no host
advantage). The round trip itself (RFC 6298 SRTT from stamps echoed in both directions) is
measured too, for the status and the gap-fill timers.

### Stalls and gap fills

A guest holding no turn for its next tick waits (`NetStatus::waiting`, the banner after 300 ms:
"Waiting for peer-0001…"; it never touches the sim). Redundancy repairs single losses. When a
turn is known to exist (the host's newest tick in any packet) but missing for 100 ms + RTT, the
guest sends `Resend { from }` on the reliable channel, again every max(2 RTT, 250 ms); the host
answers from its 60 s history in ≤ 16 KiB chunks. A guest further behind than the history can
only rejoin (M3). The host waits only before the first tick, until every guest has sent a packet
("Waiting for …" too).

### Desync detection

Spring's dictatorship (`CGameServer::CheckSync`, `GameServer.cpp:588-618`: with a local client,
every player's checksum must equal the local client's; `NETMSG_SYNCRESPONSE`, :1354): every
networked peer hashes the state after each 30th frame (`net::state::capture`, the golden-trail
hash) and sends it to the host on the reliable channel; the host keeps its last 64 and compares
(reports for frames it has not hashed yet wait). On a mismatch it announces `Desync { frame,
stop_at, peers }` with `stop_at` = the ticks it has applied — guests are never ahead of the host,
so every peer can reach exactly that tick. Every peer then runs to `stop_at`, stops (the session
is `halted`; `GameState` is untouched), shows "Desync at frame N — the game is stopped. State
dump: …", and writes `state::describe` (the headless dump: every unit's pose, health, orders,
mover state and path, query order, census, archetypes) with a header to
`desync-dumps/desync-f<frame>-t<tick>-p<peer>.txt` (native, `DumpDir`) or to the console plus a
download (web). Same tick everywhere, so the dumps diff line by line. Spring prints the error and
plays on (`GameServer.cpp:719-760`); we stop.

### Game speed and pause

The driver reads `GameSpeed { factor, paused }` (a match resource) after every tick; the
speed / pause commands set it when they apply, so every peer changes pace at the same tick.
While paused no tick runs, but the driver still seals each frame, so commands reach the host;
the host files them for the next tick and cuts it as soon as one of them resumes
(`resumes_sim`), so the unpause applies on the very next tick and nothing else advances. See §6
for the agreement with the commands work.

## 3. Tests (`cargo test -p kernel-panic net::`)

Protocol, stand-in sim (`session_tests.rs`, < 1 s in all):

| test | what |
|---|---|
| `solo_applies_on_the_next_tick` | single player: 30 ticks/s, ≤ 1 tick a frame, command applied on the tick after issue |
| `two_peers_agree_under_loss_and_jitter` | 0 / 50 / 60 ± 30 ms 5 % loss dup reorder / 100 ± 20 ms 1 %: identical trails, every command once, host latency = guest latency |
| `eight_peers_agree_on_mixed_links` | 8 peers, host links 28–196 ms one way, jitter, 1–5 % loss |
| `delay_follows_the_round_trip` | the table in §4 |
| `a_dead_link_stalls_one_guest_then_it_catches_up` | 3 s outage: starves, gap fill, ≤ 8 extra ticks a frame, back in step, delay not dragged up |
| `a_desync_stops_everyone_at_the_same_tick` | detected at the next hash frame, same stop, one dump each |
| `leaving_peers` | guest leaves: the rest play on; host leaves: `HostLeft`, no more ticks |
| `pause_and_resume` | pause holds every clock; a command while paused resumes |
| `bandwidth_at_eight_players` | §4 |

Full sim (`multipeer_tests.rs`, one headless app per peer, ~35 s together, also without baked
maps): `two_peers_full_sim` (2 humans + AI on Marble Madness, 60 ± 20 ms, 3 % loss, dup,
reorder, 60 Hz), `eight_peers_full_sim` (8 humans on Quad Core, 0–196 ms, 1–5 % loss),
`forced_desync_full_sim`, `dead_link_full_sim` (4 s outage), `peers_leaving_full_sim`. Every
peer plays its seat with the golden command script (`golden::script_for`). Behind `#[ignore]`:
`long_match` (`KP_MP_PEERS`, `KP_MP_TICKS`, `KP_MP_LATENCY`, `KP_MP_JITTER`, `KP_MP_LOSS`,
`KP_MP_MAP`) and `hash_cost` (`KP_HASH_TICKS`, `KP_HASH_FFA`).

`net::driver::tests::uneven_frames_stay_smooth`: the interpolation check of §2.

## 4. Measurements

**Input delay against the round trip** (2 peers, one-way latency L with jitter L/5, 60 Hz
frames; RTT as the host measures it, frame quantisation included):

| one way | RTT | delay (ticks) | latency (ticks) |
|---:|---:|---:|---:|
| 0 ms | 17 ms | 2 | 2.2 |
| 25 ms | 68 ms | 4 | 3.9 |
| 50 ms | 128 ms | 6 | 5.9 |
| 100 ms | 239 ms | 9 | 8.9 |
| 150 ms | 343 ms | 13 | 12.8 |
| 200 ms | 454 ms | 15 (max) | 14.9 |

Roughly `delay ≈ RTT / 33 ms + 2` (the guest's turn backlog plus the margin). Single player:
delay 0, applied on the next tick, as before. In the 8-peer mix the 196 ms link sets everyone's
delay to the maximum (15); its batches then arrive a little late (3 of ~500 over 20 s) and apply
one tick later than issued + delay.

**A long match** (`long_match`, release): 8 peers on Quad Core, 80 ± 20 ms one way, 3 % loss,
1 % duplicates, 2 % reordering, 3 000 ticks: every tick identical on all eight, delay 11, 7 late
batches, host up 6.5 KB/s payload in 211 msg/s, a guest up 0.54 / down 0.91 KB/s; 136 s of wall
time for the eight apps in one process.

**Catch-up.** A 3 s outage at 30 ms one way: 221 starved frames, 96 extra ticks at ≤ 8 a frame,
one gap request; at the end within 10 ticks of the host; host delay 6 afterwards (the
catching-up samples ignored). Full sim, 4 s outage at 40 ms: same picture, every tick identical,
the cut-off guest within 10 ticks of the host at the end.

**Bandwidth per peer at 8 players** (payload; "with overhead" adds ~90 bytes per message for
SCTP + DTLS + UDP + IP, X-commands §3):

| case | host up | each guest up | each guest down |
|---|---|---|---|
| full sim, 8 humans running the golden script (few commands), 0–196 ms, 1–5 % loss | 5.8 KB/s, 207 msg/s → **24.4 KB/s** | 0.44 KB/s, 30 msg/s → **3.1 KB/s** | 0.69 KB/s → **3.4 KB/s** |
| stand-in sim, every player ordering every 5–12 ticks (far above human APM), 50 ms, 2 % loss | 12.8 KB/s, 219 msg/s → 32.5 KB/s | 0.8–1.1 KB/s, 40–50 msg/s → 4.4–5.6 KB/s | 1.8 KB/s → 4.5 KB/s |

The plan's estimate (~3–4 KB/s per client through the host) holds; packet overhead, not payload,
dominates. The host's upload grows with the number of guests (one packet each per tick).

**State hash cost** (release, `hash_cost`; per call, and as a share of sim time when run every 30
ticks):

| match | tick | units | entities | hash | tick | share |
|---|---:|---:|---:|---:|---:|---:|
| 8-AI FFA, Quad Core | 1 500 | 61 | 3 258 | 153 µs | 1.43 ms | 0.36 % |
| | 4 500 | 304 | 5 206 | 314 µs | 3.77 ms | 0.28 % |
| | 6 000 | 284 | 4 703 | 872 µs | 3.14 ms | 0.93 % |
| | 12 000 | 297 | 5 112 | 552 µs | 3.39 ms | 0.54 % |
| 4v4 AI, Speed Balls | 4 500 | 85 | 1 970 | 189 µs | 1.51 ms | 0.42 % |
| | 9 000 | 65 | 1 640 | 194 µs | 0.88 ms | 0.74 % |

Roughly 1–2 µs per unit plus the archetype census; the component-name scan the census used to
redo on every call is now cached (`CategoryCache`). Writing the desync dump
(`describe`) takes 12–28 ms for 1 500–2 300 lines, once.

## 5. What changed for single player

- `FixedUpdate` is driven by `net::driver::drive_fixed_main` instead of Bevy's
  `run_fixed_main_schedule` (set disabled). Same ticks per frame, same `Time<Fixed>` clock and
  overstep, same render blend.
- Local commands go seal → own sequencer → turn → bus (`CommandBus::routed`), applied on the
  next tick as before. Golden trails identical (39 000 ticks over three logs).
- `GameSpeed::factor` scales the clock (1.0 until the speed command sets it); the dev tools'
  `Time<Virtual>` scaling and pause still work through the virtual delta.
- No state hashing, no network, no history in single player.
- `headless::state` moved to `net::state` (re-exported under the old path); `SimPose::sim` is no
  longer test-only.

## 6. Coordination

- **Commands (agent B).** B's branch adds `SimCommand::{SetSpeed { percent }, Pause { on },
  Resign, Dev(..)}` and a match resource `MatchControl { speed_percent, paused, paused_by,
  resigned, god_mode }` with `speed()`, plus a single-player clock (`drive_local_clock`:
  `Time<Virtual>` relative speed, and a run condition on `FixedMainLoop` that holds ticks while
  paused). Agreed shape for the merge: **the driver reads `MatchControl`** — replace
  `GameSpeed::factor` with `MatchControl::speed()` and `GameSpeed::paused` with
  `MatchControl::paused` in `driver.rs` (`WorldRunner::speed`), and narrow `resumes_sim` to
  `matches!(cmd, SimCommand::Pause { on: false })` (optionally skipping resigned players).
  `drive_local_clock`, `local_clock_runs` and `CommandBus::seal_control` then go: single player
  is the solo host, whose driver seals everything each paused frame and cuts the resume turn at
  once (no "ticks between the resume and its tick"). Keep only one of the two ways of scaling
  speed (the driver's multiplication), or speed would apply twice.
- **Seats and defeat (agent A).** The driver never reads `GameState`; until A's change lands
  (local defeat no longer gates the gameplay chain), the harness's `GameOverDismissed(true)`
  stays, and so would a networked loser's sim freeze — a desync the hash check would catch.
- **The bus.** `CommandBus` gained `routed`, `reseal` and `deliver_turn`; `seal` runs once per
  tick (the driver seals first, the apply system's call is then a no-op). `apply.rs` is
  untouched. The peer-to-peer gate (`ready`, `received`) stays for `lockstep_tests` only.

## 7. Open

- **M2:** the matchbox `Transport` (UUID → `PeerId`, channel 0 reliable / 1 unreliable), the
  lobby and `Start` handshake inserting `NextNetMatch { config, transport, catch_up }` (taken at
  the next match swap — it must not be swapped into the menu's attract demo), and the setting of
  the local seat (the driver currently makes the roster's seat the `LocalTeam`).
- **Slow simulators (M3):** a guest that cannot sim 30 ticks/s falls behind for good (catch-up
  only helps with network stalls); Spring's speed control slows the host for the slowest
  client.
- **Delay ceiling:** at RTT > ~450 ms the 15-tick cap is reached and that player's commands
  land late (applied one turn after arrival). Raise the cap or let such a player feel it.
- **Long stalls:** the host keeps 60 s of turns; a guest stalled longer needs rejoin (M3).
- **Hash scale:** ~2 µs per unit; at 1 600 units ≈ 3 ms every second. Fine; if not, hash
  incrementally or every 60 frames.
- The host sends each guest one packet per tick (7 × 30/s at 4v4); batching two ticks per packet
  would halve the overhead at +33 ms latency.
- `cargo clippy --target wasm32-unknown-unknown` fails on main already (`DevOptions::no_bindless`
  never read on wasm), unrelated to this work.
