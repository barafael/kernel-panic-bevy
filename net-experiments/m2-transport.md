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
  30 ticks/s, 0–5 late batches in 40 s. But **loss, not RTT, sets the input delay**: any loss
  drives the M1 delay controller to its 15-tick cap (500 ms) — see §4 and §6.
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
  `Channel::Unreliable` (unordered, `max_retransmits = 0`).
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
| `netem_probe` (ignored) | `KP_NETEM_TEST=…netem_probe scripts/netem-matchbox.sh` | transport-only one-way delay percentiles |
| `browser_peer` (ignored) | `webrtc.yml` browser | wasm in headless Chromium vs native |

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

(The 2 missing are the last packets in flight.) The "unreliable" channel (`max_retransmits = 0`)
**loses almost nothing**: at 1 % loss netem dropped ~24 packets, and every one of them still
arrived, 100–200 ms late — webrtc-rs retransmits them anyway, and the association's shared
congestion window holds the packets behind them (X-transport §6 saw the same: "unreliable ≠
fresher"). Each such stall is an input packet or a turn packet arriving 3–6 ticks late, so the
M1 delay controller, which follows the *worst* need of the last 2 s plus one, sits at its
15-tick cap under any loss. Commands still arrive in time (0–5 late batches in 40 s), at the
price of 500 ms input latency. See §6.

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

- **Input delay under loss (M3).** With the controller taking the max of 2 s of needs, any loss
  pins the delay at 15 ticks (500 ms) on WebRTC (§4). Spring's own UDP layer resends on timers
  from the measured RTT, which webrtc-rs's SCTP does not let us do. Options: a high percentile
  (e.g. p95 of the window) instead of the max, accepting a short wait on the rare stall; a lower
  cap; or measuring stalls separately from steady lag. Needs playtesting against the 500 ms.
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
