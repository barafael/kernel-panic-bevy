# M2 transport: lockstep over matchbox WebRTC

Goal (`docs/multiplayer-plan.md`, M2, part D): the M1 lockstep session on a real network.
A `Transport` over WebRTC data channels through barafael's matchbox fork, on native and on the
web, with room codes, a configurable signaling server and ICE servers, bounded send queues with
our own flow control, and tests over real WebRTC: 2 and 8 full-game peers on localhost, a match
under emulated loss / latency / jitter, and a browser peer against a native one.

Code: `kernel-panic/src/net/matchbox.rs` (transport, `RoomId`, `NetConfig`, `open_room`),
`net/matchbox_tests.rs` (in-process signaling server and the tests),
`headless/netplay.rs` (networked headless peers, shared by the native tests and the browser
peer), `headless/web.rs` (`kp_net_peer`), `.github/scripts/kp-netpeer.mjs`,
`scripts/netem-matchbox.sh`, `.github/workflows/webrtc.yml`.

## 1. Result

- **Real WebRTC carries the session unchanged.** 2 peers + an AI (20 s) and 8 peers (15 s),
  every peer a full headless game, over localhost WebRTC: per-tick state hash trails identical
  to the host's on every tick, no desync, 30 ticks/s held.
- **Cross-play works.** The wasm build in headless Chromium (154) and a native peer, through
  the local signaling server, played 30 s of a 2-human + AI match: **identical trails over all
  897 ticks** the browser played. First connection between webrtc-rs and Chromium's WebRTC
  in this project; nothing needed but showing Chromium's real host addresses (no mDNS names).
- **Under netem** (4 and 8 peers, RTT 40–100 ms, 0.5–3 % loss, jitter): trails identical,
  30 ticks/s, 0–5 late batches in 40 s.
- **The "unreliable" channel was reliable** (§7): webrtc-rs never applied a negotiated data
  channel's `ordered: false, max_retransmits: 0`, so every lost packet held up everything behind
  it (p99 160 ms at 1 % loss, 1 s retransmission timeouts at 2 %), and the input delay sat at its
  15-tick cap. Fixed by a one-line patch of `webrtc-data` (`vendor/webrtc-data`),
  `max_retransmits` 2 instead of 0 (0 abandons chunks still in flight, in webrtc-rs and in
  Chromium) and a percentile-based delay controller: at RTT 100 ms and 2 % loss the delay is
  now 6 ticks instead of 14 and guest stalls ≥ 100 ms 0 instead of 60 in 40 s.
- **Flow control holds.** A 1.5 MiB reliable burst: 1 MiB queued, the rest refused and
  counted, drained in order at 271 KiB/s (cap 256 KiB/s + 64 KiB burst). In every match the
  queues never held anything back (lockstep needs ~0.3–4 KB/s per link).
- **Single player is untouched** (§5).

## 2. Design

### API

```rust
pub fn open_room(room: &RoomId, cfg: &NetConfig) -> Box<dyn Transport>;   // the entry point

pub struct RoomId(String);          // 1–24 of a-z 0-9 - _, lower-cased; FromStr / Display
impl RoomId { fn parse(&str) -> Result<Self, RoomIdError>; fn random() -> Self; fn as_str() }

#[derive(Resource)]
pub struct NetConfig { pub signaling: String, pub ice: Vec<IceServer> }
pub struct IceServer { pub urls: Vec<String>, pub username: Option<String>, pub credential: Option<String> }
impl NetConfig { fn default() /* build settings */; fn from_launch() /* + native overrides */;
                 fn room_url(&RoomId) -> String /* <signaling>/kp-<room> */ }

pub struct MatchboxTransport;       // open(room_url, cfg), stats() -> Arc<Mutex<TransportStats>>
```

`Transport` gained one defaulted method, `failure(&self) -> Option<String>`: the room
connection is gone for good, and why (signaling server unreachable, socket closed) — for the
lobby to show. `sim_net` keeps the default.

- **Room codes.** `RoomId::random()` draws 6 characters from 31 that do not read alike
  (no `0 o 1 l i`): ~9 × 10⁸ codes. Typed codes are lower-cased, so a code read aloud works in
  either case. The path segment is `kp-<code>`, namespaced like cake's `cake-<room>`.
- **Signaling.** Default `wss://omdurman-matchbox.fly.dev` (omdurman's `matchbox_server`, cake
  uses it too), `MATCHBOX_SERVER` at build time (cake's and omdurman's variable), natively
  `--signaling <url>` or `KP_SIGNALING`. 5 attempts 3 s apart (fly.io wakes the idle server in a
  few seconds), then `failure()` says so.
- **ICE.** Default Google STUN (`stun.l.google.com`, `stun1`, as cake / omdurman / matchbox).
  `KP_ICE` at build time or natively `--ice` / `KP_ICE`: servers separated by spaces, each
  `url[,url…][|username|credential]`, `none` for host candidates only; TURN entries must carry
  credentials. matchbox takes **one** `RTCIceServer`, so all URLs go into it with the first
  credentials found (STUN ignores credentials in browsers and webrtc-rs); a second TURN entry
  with other credentials cannot be expressed and is left out with a warning. A TURN relay is
  configuration, not code. On the web an empty list falls back to Google STUN: browsers refuse
  an ICE server without URLs, and matchbox always passes one.
- **Ids.** matchbox's UUID is `PeerId(u128)` both ways (`Uuid::as_u128` / `from_u128`), so the
  lowest-id host election is plain integer order on every peer.
- **Channels.** matchbox channel 0 = `Channel::Reliable` (`ChannelConfig::reliable()`), 1 =
  `Channel::Unreliable` (unordered, `max_retransmits` 2, §7; needs `vendor/webrtc-data` natively).
- **The 16 KiB cap is asserted** in `send` (`assert!`, also release): the session never builds
  more (`wire::encode`), and 64 KiB exactly would kill the connection, larger would vanish
  (X-transport §5).
- **The socket's loop** runs on a thread of its own natively (`bevy::tasks::block_on`;
  independent of Bevy's task pools, so tests and the headless harness need no executor ticking)
  and on the event loop on the web (`wasm_bindgen_futures::spawn_local`). Dropping the transport
  closes the channels and wakes a stop future raced against the loop (`futures_lite::or`), so the
  thread ends even while matchbox is still retrying the signaling server.

### Flow control

matchbox queues without limit and exposes no `bufferedAmount`; X-transport §6 saw an overloaded
sender's backlog starve every later message for seconds. So every peer link has, in front of
matchbox:

| | value | why |
|---|---|---|
| token bucket | 256 KiB/s, burst 64 KiB | ~ what one association sustains at 0.5 % / 40 ms (500 KB/s, X-transport §6); lockstep needs ~1 KB/s, so only gap-fill bursts ever meet it |
| reliable queue | 1 MiB, then refused (counted, warned once) | gap fills are asked for again; a link that far behind is dead anyway |
| unreliable queue | 8 packets, oldest dropped | each session packet repeats what is unacknowledged, the newest supersedes |
| unconnected peer | unreliable dropped, reliable held (bounded) | like `sim_net`'s dead link |
| departed peer | everything dropped | |

Reliable goes first when both wait. Pumping happens on every `send` and `poll_peers`, so held
packets wait at most a frame. `TransportStats` counts bytes / messages per channel, drops,
throttled sends and the peak queue.

### Leaving

matchbox (the fork) reports a peer `Disconnected` when the signaling server says it left — its
**websocket** dropped — even though its data channels, all a match needs, live on; the fork
deliberately keeps sockets alive across signaling loss (omdurman's finding 2). And it reports a
disconnect only once per peer (`try_update_peers` drops unchanged states). So a disconnected
link is only *suspect*: it is reported `Left` once it has been silent for `LEAVE_GRACE` = 3 s
(X-transport §7: app-level timeout of 3–5 s; the fork's ICE failure takes ~30 s), however long it
kept talking first. The session sends to every peer it talks to at least every 100 ms, so 3 s of
silence is unambiguous. A real departure costs ≤ 3 s of detection after the last packet.

`signaling_loss_keeps_links` checks it: the server announces B gone and then dies; B keeps
talking for 6 s and is not reported, reliable traffic still flows both ways, and 2.95 s after B
falls silent A reports it left.

### Signaling server for tests

`matchbox_signaling`'s own full-mesh topology reads a room and inserts the newcomer under
separate locks, so peers that join at the same moment may never be introduced (X-transport §2:
3 of 20 rooms of 8 simultaneous joiners complete). The tests run `RoomMesh`, a 70-line
`SignalingTopology` with `matchbox_server`'s semantics (the deployed server: rooms by path,
NewPeer to everyone present, PeerLeft on disconnect) under **one** lock. All 8 peers join at
once and connect every time.

### Networked headless peers (`headless/netplay.rs`)

Without a lobby (agent E), peers agree on a match from what all of them see: once a peer sees
the expected number of others (`Rendezvous`, peer events only — data arriving meanwhile stays
queued for the session), the roster is every id ascending, seat `i` for the `i`-th, spectators
past the human seats; match id = seed. `NetPeer` wraps a `SimHarness` with a networked
`NetSession` and plays its seat with the golden script (`golden::script_for`, now compiled on
wasm too; only `golden::record` stays native). The browser peer (`kp_net_peer`) and the native
tests build the very same match from `(map, humans, ai, seed)`.

The tests run on wall-clock time: each round of frames is as long as the previous round took
(≤ 100 ms), all peers updated in turn on one thread, the network on its own threads.

## 3. Tests and CI

| test | where | what |
|---|---|---|
| `matchbox::tests::{room_codes, ice_servers}` | `cargo test` | parsing, URLs, ICE merge, launch config |
| `transport_basics` | `cargo test` | meet, both channels 1 B–16 KiB, reliable order, burst flow control, leave |
| `signaling_loss_keeps_links` | `cargo test` | §2 Leaving |
| `unreachable_signaling_fails` | `cargo test` | 5 tries, then `failure()` (~15 s) |
| `two_peers_webrtc_full_sim` / `eight_peers_webrtc_full_sim` | `cargo test` | full sim over localhost WebRTC |
| `netem_match` (ignored) | `webrtc.yml` netem (2 profiles), `scripts/netem-matchbox.sh` | 4 peers under netem |
| `netem_probe` (ignored) | `webrtc.yml` netem (`KP_NET_PROBE_SPREAD=60`), `KP_NETEM_TEST=…netem_probe scripts/netem-matchbox.sh` | transport-only: numbered packets, one-way and newest-known delay, losses (§7) |
| `browser_peer` (ignored) | `webrtc.yml` browser | wasm in headless Chromium vs native |
| `browser_probe` (ignored) | `KP_NETEM_TEST=…browser_probe KP_NETPEER_PKG=… scripts/netem-matchbox.sh` | Chromium's unreliable channel into a native peer (§7) |

The real-network tests share `multipeer_tests::SERIAL` (one full-app test at a time). The
whole `matchbox` filter takes ~60 s single-threaded.

CI (`.github/workflows/webrtc.yml`, called from `ci.yml` like `determinism.yml`):

- **netem** (matrix `delay 20ms 5ms loss 0.5%`, `delay 50ms 10ms loss 2%`; 4 peers, 40 s):
  `scripts/netem-matchbox.sh --sudo`. Ubuntu runners refuse unprivileged user namespaces
  (AppArmor), so CI shapes its own `lo` with `sudo tc`; locally the script uses `unshare -rn`
  plus a dummy interface (webrtc-rs skips loopback candidates), X-transport's setup.
- **browser**: bake maps, build `--features replay` for wasm (wasm-release, wasm-bindgen,
  wasm-opt -O2, as determinism.yml), install Playwright Chromium, run `browser_peer`
  (`KP_NETPEER_PKG=pkg`). Locally: `NODE_PATH=<dir with playwright>`,
  `KP_BROWSER_PATH=/usr/bin/chromium` (the script's override for a system browser).

Not run locally: the two CI jobs as such (no Actions runner here); their commands were run
locally as above, except `--sudo`.

## 4. Numbers

Debug test profile (workspace opt-level 1), 16-core machine, all peers in one process.

**Localhost WebRTC** (`cargo test`):

| case | setup | ticks | delay | late | RTT (host-measured) | host up | guest up |
|---|---|---|---|---|---|---|---|
| 2 peers + AI, Marble Madness, 20 s | 21 ms | 599 (29.9/s) | 2 | 0 | 9.3 ms | 545 B/s, 30 msg/s | 435 B/s, 31 msg/s |
| 8 peers, Quad Core, 15 s | 526 ms | 442 (29.4/s) | 5 | 0 | 65 ms | 2.8 KB/s, 137 msg/s | 0.3 KB/s, 20 msg/s |

Payload only (add ~90 B per message of SCTP/DTLS/UDP/IP, m1-runtime §4). The RTTs are mostly
frame time: eight full games share one thread, so a round of frames takes ~60 ms, and the
session measures round trips in frames.

**Under netem** (`scripts/netem-matchbox.sh`, 40 s each, `unshare -rn`):

| peers | netem | setup | ticks/s | delay | late batches | RTT |
|---|---|---|---|---|---|---|
| 2 + AI | delay 20ms | 1.5 s | 30.0 | **3** | 0 | 57 |
| 2 + AI | delay 20ms 5ms | 1.5 s | 30.0 | 4 | 0 | 59 |
| 2 + AI | delay 50ms | 1.3 s | 29.9 | 5 | 1 | 119 |
| 2 + AI | delay 20ms loss 1% | 1.5 s | 30.0 | **15** | 0 | 58 |
| 4 | delay 20ms 5ms loss 0.5% | 0.7 s | 30.0 | 7 | 0 | 52–59 |
| 4 | delay 50ms 10ms loss 2% | 2.6 s | 29.9 | 15 | 4 | 129–149 |
| 4 | delay 40ms loss 3% | 1.3 s | 29.9 | 15 | 1 | 106–209 |
| 8 | delay 20ms 5ms loss 1% | 3.7 s | 29.9 | 15 | 5 | 77–140 |

Trails identical in every run. The 2-peer runs are 30 s.

**The transport alone** (`netem_probe`: a 64 B packet every 33 ms each way on the unreliable
channel, 40 s, one-way delay on one clock):

| netem | arrived | p50 | p95 | p99 | max |
|---|---|---|---|---|---|
| delay 20ms | 2424 / 2426 | 21.5 | 22.6 | 24.2 | 25.3 ms |
| delay 20ms 5ms | 2424 / 2426 | 21.5 | 25.8 | 26.8 | 29.2 ms |
| delay 20ms loss 0.5% | 2424 / 2426 | 22.0 | 23.8 | **128.8** | 195.1 ms |
| delay 20ms loss 1% | 2424 / 2426 | 21.7 | 32.9 | **162.0** | 197.3 ms |
| delay 50ms loss 2% | 2422 / 2426 | 51.6 | 198.8 | 316.7 | **1122.8 ms** |

(The 2 missing are the last packets in flight.) These tables are **before the fix in §7**: the
"unreliable" channel lost nothing and stalled behind every loss, which pinned the delay at 15
under any loss. §7 has the cause and the numbers after.

## 5. What changed for single player

Nothing at run time: no socket is opened unless something calls `open_room` (nothing does until
the lobby lands), the session stays the solo host, `NetConfig` is not even inserted. The golden
replays, the query-order fuzz and the command fuzz are unaffected (no sim change; no
`SIM_VERSION`, wire or replay format bump).

Build: `matchbox_socket` (+ `uuid`) is a dependency on every target. Natively that links
webrtc-rs (and with it tokio, rustls/ring, ~160 new crates in `Cargo.lock`); on the web
matchbox uses the browser's WebRTC and `ws_stream_wasm`. `trunk build --release` (the deploy)
works: 66.7 MB `_bg.wasm` after wasm-opt. Dev-dependencies for the tests: `matchbox_signaling`,
`matchbox_protocol` (json), `axum` (ws), `tokio`. `golden.rs` now compiles on wasm too (its
recorder stays native).

## 6. Open

- **Upstream the webrtc-data fix** (§7) to webrtc-rs and drop `vendor/webrtc-data` once a
  release has it; the probe in `webrtc.yml` fails if the channel turns reliable again. Also worth
  reporting: `max_retransmits: 0` abandons chunks at their first send (webrtc-sctp, pion).
- **Setup under heavy loss:** at 2–3 % loss, 3 of ~20 four-peer rooms had one pair that never
  connected within 60 s (every transport variant; X-transport saw the same). The lobby
  must notice a stuck room and reopen it.
- **Lobby wiring.** Agent E's `lobby::connect` switches to `matchbox::open_room(room,
  &NetConfig::from_launch())` (or a `NetConfig` resource). The lobby should show
  `Transport::failure()` (unreachable server after ~15 s) and treat a peer count stuck below the
  roster as a reason to reopen the room (X-transport §2).
- **Deployed server.** The tests use an in-process server with `matchbox_server`'s room
  semantics, not the fly.io deployment itself; nothing here contacted it. A first manual
  two-machine test through `wss://omdurman-matchbox.fly.dev` (and through real NATs, STUN only)
  is still to do.
- **TURN** is configurable, not deployed; one credential set per socket (matchbox).
- **Firefox / WebKit** browser peers untested (Playwright's Firefox and WebKit builds have
  WebRTC; `kp-netpeer.mjs` takes the runtime, CI runs Chromium only).
- **Chromium's mDNS host candidates**: the CI test disables them
  (`--disable-features=WebRtcHideLocalIpsWithMdns`). Real browser players present `.local`
  names; across the internet the STUN (srflx) candidates carry the connection, on one LAN
  webrtc-rs must resolve mDNS (it has a resolver; untested here).
- **Hidden browser tabs** throttle the web peer (M3, plan).
- Native-only `NetConfig::from_launch` reads arguments itself; if the game grows a real CLI
  parser it should move there.

## 7. Follow-up: why the unreliable channel stalled, and the delay rule

### Cause 1: negotiated channels never got their reliability parameters

The probe (`netem_probe`, now numbering its packets) showed what §4 only hinted at: under loss
**no packet ever arrived after a later one and none went missing** — the "unreliable" channel
(`ordered: false, max_retransmits: Some(0)`) behaved as reliable *and ordered*. Not the SCTP
timers as such, and not our queue (its counters showed nothing held back):

- matchbox creates every data channel **negotiated** (`RTCDataChannelInit { negotiated:
  Some(index), ordered, max_retransmits }`, `matchbox_socket/src/webrtc_socket/native.rs:557`), so
  no DCEP `DATA_CHANNEL_OPEN` / `ACK` is exchanged.
- `webrtc` maps the init to a `ChannelType` and calls `webrtc_data::DataChannel::dial` →
  `client()` (`webrtc-0.17.2/src/data_channel/mod.rs:136-187`). `client()` sends the DCEP open for
  a non-negotiated channel and returns; the parameters reach the SCTP stream only through
  `commit_reliability_params()`, which runs in `server()` (after a DCEP open) and in `handle_dcep`
  (when a DCEP ACK arrives) (`webrtc-data-0.17.2/src/data_channel/mod.rs:108-155, 240-262`). A
  negotiated channel gets neither, so its `webrtc_sctp::Stream` keeps the defaults: ordered,
  `ReliabilityType::Reliable`.
- So each lost chunk was retransmitted, and in-order delivery held every later message on the
  stream until it came: a fast retransmit after three SACKs reported the gap (~100 ms at 30
  packets/s plus the one-way delay: the p99 of 130–160 ms at 0.5–1 % loss), or, when the
  retransmission was lost too or fast recovery was already running (in recovery,
  `process_fast_retransmission` counts misses only when the cumulative TSN advances), the T3
  timer: `RTO_MIN = 1000` ms (`webrtc-sctp-0.17.2/src/timer/rtx_timer.rs:10`) — the 1.0–1.1 s
  maxima at 2 %.

No knob reaches it: `RTO_MIN` / `RTO_MAX` are `pub(crate)` constants, `webrtc_sctp::Config` has
none, the data channel's stream is private. webrtc-data 0.17.2 is the newest release (webrtc-rs
master has moved to a new sans-IO stack), and the barafael/matchbox fork does not touch it.

**Fix:** `vendor/webrtc-data` — the crates.io 0.17.2 source with one change in `client()`: a
negotiated channel commits its parameters at once (both sides of a negotiated channel create it
with the same ones). Wired with `[patch.crates-io]` in the workspace `Cargo.toml` (excluded from
the workspace members, so our lints and rustfmt leave it alone). Native only by nature: the web
build uses the browser's WebRTC.

### Cause 2: `max_retransmits: 0` abandons chunks the moment they are sent

With the channel really unreliable, a second effect showed under **jitter** (netem's jitter
reorders packets): 9.5 % of packets missing at 0.5 % loss with 5 ms jitter, 17 % at 2 % loss with
10 ms jitter. An instrumented webrtc-sctp (not committed) found every chunk marked abandoned at its
*first* transmission: `move_pending_data_chunk_to_inflight_queue` sets `nsent = 1` and calls
`check_partial_reliability_status`, which abandons when `nsent >= max_retransmits`
(`association_internal.rs:1877-1894, 2042`) — pion's SCTP has the same check. The next SACK then
moves the Advanced.Peer.Ack.Point over everything in flight and sends a FORWARD-TSN; when that
overtakes the data it skips, the receiver advances past the chunks and drops them on arrival as
duplicates (115 such drops for 118 missing packets in the instrumented run). Without reordering
the FORWARD-TSN trails its data and nothing is lost, which is why the first probes missed it.

Chromium showed its own variant: at `maxRetransmits = 0` it lost 3.6–37 % of its messages
into a native peer under 1–2 % netem loss, and only 34 of 243 missing messages had ever got a TSN
(the native receiver saw 34 FORWARD-TSN skips) — the rest were dropped inside the browser.

**Fix:** the unreliable channel uses `max_retransmits = 2`
(`matchbox::UNRELIABLE_RETRANSMITS`): webrtc-rs sends a chunk at most twice, a browser at most
three times. The original is never abandoned in flight; a lost packet comes again about a round
trip later, and since the channel is unordered it holds nothing up. Both directions, native and
Chromium, now lose (almost) nothing and never stall:

| transport probe, 40 s, both ways | sent | never arrived | arrived late (resent) | newest-known p50 / p99 / max |
|---|---|---|---|---|
| **delay 20ms loss 1%** | | | | |
| crates.io (reliable, ordered) | 2364 | 0 | 0 | 21.7 / 162.0 / 197.3 ms |
| patched, `max_retransmits` 0 | 2364 | 20 | 0 | 21.7 / 25.0 / 56.4 ms |
| **patched, 2 (final)** | 2364 | 0 | 31 | 21.1 / 53.8 / 228.8 ms |
| **delay 50ms loss 2%** | | | | |
| crates.io | 2364 | 0 | 0 | 51.6 / 1055.5 / 1288.1 ms |
| patched, 0 | 2364 | 45 | 0 | 51.3 / 84.3 / 86.5 ms |
| **patched, 2** | 2364 | 1 | 40 | 50.9 / 83.8 / 85.6 ms |
| **delay 20ms 5ms loss 0.5%** (jitter) | | | | |
| patched, 0 | 2364 | 224 | 0 | 21.4 / 59.2 / 225.7 ms |
| **patched, 2** | 1758 | 0 | 12 | 21.5 / 25.9 / 59.3 ms |
| **delay 50ms 10ms loss 2%** (jitter) | | | | |
| patched, 0 | 2364 | 394 | 0 | 51.7 / 119.3 / 160.1 ms |
| **patched, 2** | 2364 | 2 | 29 | 51.3 / 78.4 / 92.3 ms |

| Chromium 154 → native (`browser_probe`), 30 s | `maxRetransmits` | sent | never arrived | newest-in-hand gap p50 / p99 / max |
|---|---|---|---|---|
| delay 20ms loss 1% | 0 | 908–1212 | 333 / 311 / 452 (3 runs) | 64 / 70 / 130–135 ms |
| delay 20ms loss 1% | **2** | 910 | 0 | 34.4 / 43.2 / 71.2 ms |
| delay 20ms 5ms loss 1% | 0 | 909 | 337 | 60.2 / 75.5 / 137.2 ms |
| delay 20ms 5ms loss 1% | **2** | 910 | 0 | 33.4 / 60.3 / 73.2 ms |
| delay 50ms 10ms loss 2% | 0 | 908 | 33 | 33.5 / 71.0 / 91.8 ms |
| delay 50ms 10ms loss 2% | **2** | 910 | 0 | 33.4 / 60.2 / 102.1 ms |

"Newest-known": the time until any packet numbered ≥ k is in — what a stream that repeats its
unacknowledged content (turns, batches) actually waits; the browser probe measures the same as
the gap between arrivals of a new newest packet (one per 33 ms is perfect). Chromium ran inside
the same `unshare -rn` namespace (`KP_BROWSER_PATH=/usr/bin/chromium`). CI guards the native
side: `webrtc.yml` runs the probe under both netem profiles with `KP_NET_PROBE_SPREAD=60`
(newest-known p99 within 60 ms of its median; the crates.io channel failed by 100–1000 ms, the
`max_retransmits: 0` one by 70 ms with jitter).

(A smaller webrtc-sctp deviation seen on the way, harmless here: a chunk that reaches its
retransmit limit during a fast retransmit is marked abandoned but still sent once more,
`association_internal.rs:452-500`.)

### The delay rule

`DelayController` (the only change in `session.rs`): the target is the **95th percentile of the
needs of the last 4 s** (all guests' samples pooled) plus one tick, instead of the maximum of the
last 2 s. Rising at once and falling one tick per 2 s stay. A rare late packet no longer sets the
delay for the window; its command is filed one turn late instead (a late batch, applied alike
everywhere). A guest that lags all the time is 1/7 of the samples at 4v4, above the 5 % left
out, and moves the delay within about a second (unit test `the_delay_follows_the_typical_need`).
Solo play has no samples and keeps delay 0; the golden trails do not involve the controller.

With the final transport a lost packet's content comes with the next packet (33 ms) or its own
resend (~1 RTT): rare, short needs, exactly what the old maximum turned into the delay.
Measured with `scripts/netem-matchbox.sh` (40 s each, debug test profile, all peers in one
process; stalls and catch-up ticks counted after the first second, which is the start):

| profile | transport | rule | delay mean / max (ticks) | guest stalls ≥ 100 ms (longest) | catch-up ticks | late batches |
|---|---|---|---|---|---|---|
| 2 + AI, delay 20ms | crates.io | old | 2.9 / 3 | 0 | 0 | 0 |
| | final | old | 3.1 / 4 | 0 | 0 | 0 |
| | final | **new** | **3.0 / 3** | **0** | **0** | **0** |
| 2 + AI, delay 20ms loss 1% | crates.io | old | 7.1 / 9 | 3 (123 ms) | 22 | 0 |
| | final | old | 7.2 / 9 | 0 | 0 | 0 |
| | final | **new** | **3.6 / 4** | **0** | **0** | **0** |
| 4, delay 20ms 5ms loss 0.5% | crates.io | old | 7.3 / 8 | 11 (140 ms) | 47 | 2 |
| | final | old | 8.5 / 10 | 0 | 0 | 2 |
| | final | **new** | **5.0 / 7** | **0** | **1** | **3** |
| 4, delay 50ms 10ms loss 2% | crates.io | old | 13.9 / 15 | 60 (1027 ms) | 312 | 3 |
| | final | old | 12.8 / 15 | 0 | 1 | 3 |
| | final | **new** | **6.4 / 10** | **0** | **1** | **3** |
| 4, delay 40ms loss 3% | crates.io | old | 13.9 / 15 | 103 (1003 ms) | 480 | 3 |
| | final | old | 12.1 / 15 | 0 | 0 | 3 |
| | final | **new** | **5.5 / 6** | **0** | **0** | **3** |

"crates.io" = before (reliable ordered channel); "final" = patched `webrtc-data` +
`max_retransmits` 2. Reading:

- **The transport fixes end the stalls**: from 60–100 guest stalls of ~150 ms (and 1 s
  timeouts) in 40 s at 2–3 % loss to none, and catch-up ticks from 300–500 to 0–1.
- **The rule then halves the delay under loss**: with resent packets arriving a round trip
  late, the old maximum still followed them up to the cap (12–13 ticks at 2–3 %); the 95th
  percentile follows the typical lag (5.5–6.4 ticks ≈ RTT/33 ms + 2), at no cost: late batches
  1–3 in 40 s either way, the commands concerned applied one tick late.
- Earlier runs with the intermediate transport (patched, `max_retransmits` 0) gave the same
  picture (rule: 0.4–0.7 ticks less; that transport's own losses are in the probe tables).
  Run-to-run spread is about ±1 tick of mean delay.
- Setup under heavy loss: at 2–3 % loss 3 of ~20 four-peer rooms had one pair that never
  connected within 60 s, with every transport variant (X-transport saw the same).
