# M3 (I): rooms and browsers — hidden tabs, stuck rooms, back to the room, chat

Scope (`docs/multiplayer-plan.md`, M3, part I): what keeps a room and a match together outside
the lockstep core — a browser tab hidden mid-match, a room where a pair of peers never
connected, coming back to the room after a match, the deployed signaling server, the menu
screenshots staying off it, the map bakes of release and deploy, names and chat. Code:
`kernel-panic/src/net/` (`web_tab.rs`, `chat.rs`, `lobby.rs`, `session.rs`, `driver.rs`,
`wire.rs`, `transport.rs`, `sim_net.rs`), `vendor/bevy_winit`, `ui/menu/lobby_page.rs`,
`ui/hud/net_overlay.rs`, `ui/menu.rs`, `ui/menu_shots.rs`, `scripts/signaling-smoke.sh`.

## Summary

- **Hidden tabs keep playing** (Chromium, Firefox). A hidden page gets no animation frames and
  its timers fire once a second (Chromium: about once a minute after 5 minutes); the game's
  frames are animation frames, so a hidden tab used to stop dead. A dedicated worker's timer is
  not throttled (30/s, also past 5 minutes, in both engines), so while the tab is hidden and the
  game is in a room or a networked match, a worker wakes the event loop every 33 ms, and a
  4-line `bevy_winit` patch lets a wake run the update on a hidden page. The others see "tab
  hidden" in their player lists (`Msg::Away`). A guest that falls more than the host's turn
  history (a minute) behind is dropped. Single player still stops in a hidden tab.
- **Stuck rooms reopen.** Every peer compares the room the others show it (host and members)
  with whom it is connected to; a peer missing a lower id for 10 s reopens its connection (new
  socket, new id, `Hello::was` names the old one so the host gives the seat back), up to 3 times,
  then says so. Host↔guest and guest↔guest pairs alike.
- **Back to the room** after a match: "Back to room" (Esc overlay, result panel) resigns, sends
  `Msg::Bye` (the leave, on the same tick everywhere, without closing anything) and hands the
  connection to the lobby (`Lobby::resume`): same code, the room as it was at the start, the
  seats of those still in the match kept until they are back, ready flags cleared. A rematch
  starts as usual.
- **Deployed signaling server** (`scripts/signaling-smoke.sh`): two native peers met through
  `wss://omdurman-matchbox.fly.dev` in 0.25 s, carried both channels without loss and formed a
  room with a chat line, 0.5 s in all.
- **Menu screenshots** open their rooms on a `LoneTransport` (`OfflineRooms`), never on the
  public server.
- **Map bakes**: release-style (determinism.yml's bake job, the same action and runner as
  release.yml's), deploy (the `.kpmap` files GitHub Pages serves) and a local bake of the same
  commit are byte-identical, all 18 maps. The bake action now prints the SHA-256 of every map.
- **Names and chat**: a name field on the Multiplayer page (and `/name` in the chat), a chat in
  the room (beside the seats) and in the match (Enter opens the line); the host checks the
  sender and relays every line to everyone, as Spring's server does; not part of the sim.
- **Formats**: wire version 3 (`Msg::Bye`, `Chat`, `Away`), lobby version 2 (`Hello::was`,
  `LobbyMsg::Chat`). No sim change: `SIM_VERSION`, replay format and golden trails unchanged.

## 1. Hidden browser tabs

### What a hidden tab does

A probe page (scripts under `target/tabprobe`, not committed) counts per second its
`requestAnimationFrame` callbacks, a `setTimeout(0)` chain, a `setInterval(33)`, a
`MessageChannel` chain paced to 33 ms, the messages of a dedicated worker running
`setInterval(33)` (counted on the page), and an audio clock (a `ScriptProcessorNode` behind an
oscillator). The page is hidden for real by opening a second tab in front of it, over the raw
browser protocols: Playwright keeps every page "visible" (it emulates focus), and a headless
browser reports visible unless a tab really is in the background.

- Chromium 154: headless (`--headless=new`), DevTools protocol, `Target.createTarget` +
  `Target.activateTarget`. The page sees `blur`, then `visibilitychange` 1 ms later, and no
  animation frame in between.
- Firefox 157: headless, WebDriver BiDi (`browsingContext.create` / `activate`).

Per second, after 5 s visible; hidden for 20 s or for 7 minutes:

| | rAF | `setTimeout` chain | `setInterval(33)` | `MessageChannel` | worker → page | audio |
|---|---|---|---|---|---|---|
| Chromium, visible | 60 | 230 | 30 | 30 | 30 | 47 |
| Chromium, hidden 0–5 min | 0 | 1 | 1 | 30 | 30.3 | 47 |
| Chromium, hidden after 5 min | 0 | 0 (one a minute) | 0 | 30 | 30.3 | 47 |
| … with an open `RTCPeerConnection` | 0 | 1 (after 5 min too) | 1 | 30 | 30.3 | – |
| … playing audio | 0 | 1, then 0 | 1, then 0 | 30 | 30.3 | 47 |
| Firefox, visible (headless: 13 fps) | 13 | 220 | 30 | 30 | 29.5 | 47 |
| Firefox, hidden (7 min) | 0.1–0.3 | 1 | 1 | 30 | 29–30 | – |
| … playing audio | 0.1–0.3 | 230 | 30 | 30 | 30 | 47 |

Reading:

- **Animation frames stop** in both engines (Firefox runs one every few seconds).
- **Timers drop to 1/s** at once; Chromium's intensive throttling takes them to one wake-up a
  minute after 5 minutes hidden, except while the page holds an open WebRTC connection (which a
  networked match always does): Chrome's documented exemption, confirmed.
- **Audio** exempts a page from timer throttling in Firefox, not in headless Chromium (whose
  "audible" test may need an output device it does not have; a desktop Chrome may differ).
- **A dedicated worker's timer is not throttled** (30/s, past 5 minutes, both engines), and its
  messages reach the hidden page at once. A `MessageChannel` chain is not throttled either, but
  it is a busy loop.
- Not measured: WebKit / Safari (no WebKit build ran here; Playwright's would keep its pages
  visible anyway). Safari is known to throttle hidden tabs hard; the fallback rules apply there.

### What the game did

The game's frames are winit redraws, and a winit redraw on the web is an animation frame. Bevy
runs an app update once per redraw (`bevy_winit`'s `ran_update_since_last_redraw`), unless every
window is invisible. A hidden tab therefore ran **no update at all**: no lockstep ticks, no
packets, no lobby. A guest was dropped after the minute of silence (`Timeouts::running`), a
hidden host stopped the match for everyone, the room went quiet.

### What the game does now (`net::web_tab`, `vendor/bevy_winit`)

- `visibilitychange` (an event: it reaches a hidden page at once) starts a dedicated worker
  posting every 33 ms, if the game is in a room or a networked match; each message wakes the
  winit event loop (`EventLoopProxyWrapper`, `WinitUserEvent::WakeUp`).
- The `bevy_winit` patch counts a hidden document as "every window invisible", so a wake-up runs
  the update: Bevy's own rule for invisible native windows ("redraw calls will never succeed").
  A few lines in `WinitAppRunnerState::redraw_requested`; nothing changes while the page is
  shown, or natively. Vendored like `webrtc-data` (`[patch.crates-io]`,
  `vendor/bevy_winit/README.md`).
- The woken update is a normal frame: the driver runs the ticks the clock owes (one per wake at
  30 Hz), the session sends, the lobby polls, the frame renders (wasted: the browser composites
  nothing). Shown again, the worker rests and animation frames take over.
- `TabHidden` → `Session::set_away` → `Msg::Away` to the host, which relays it: every player
  list shows "tab hidden" in place of the ping.
- Single player keeps stopping in a hidden tab, as before (the worker only runs in a room or a
  networked match).
- Should the browser stop the worker too (a frozen or discarded tab, perhaps Safari), the
  existing rules apply: a silent guest is dropped after the timeout. New: a guest that still
  talks but lacks more turns than the host keeps (`HISTORY`, a minute) is dropped as well; it can
  never catch up, and was carried forever before (`a_guest_too_far_behind_is_dropped`: dropped
  at host tick 1 830, its downlink dead from tick 30: the history's 1 800 turns later).

### The game in a hidden tab

The `trunk build --release` web build in headless Chromium (software WebGL, SwiftShader), in a
room on the deployed signaling server (so the worker is wanted), the attract demo playing behind
the menu (a solo session: its ticks are the measure); the game's own log (`net::web_tab` writes
frames and ticks every 10 s while hidden):

| build | hidden | while hidden |
|---|---|---|
| this branch | 90 s | 30.5, 30.0, 30.0, 30.0 ticks/s per 10 s; then a 34 s stall (the demo loading its next map) |
| this branch | 60 s | a 34 s map-load stall first, then 30.0 ticks/s |
| without the `bevy_winit` patch (control; worker on) | 60 s | no update at all: not one frame logged until the tab was shown |

About 50 frames a second while hidden (the worker's 30 plus wake-ups from other events), every
tick on time. The 34 s stalls are the attract demo loading its next map, which takes 30–60 s
under software rendering whether the tab is shown or not (its first match started 66 s after
the page loaded).

## 2. Stuck rooms

D measured (`m2-transport.md` §6) that at 2–3 % loss about 3 of 20 four-peer rooms had a pair
that never connected within 60 s. webrtc-rs gives up on a failed ICE check after ~30 s, and
matchbox says nothing about a peer it never connected to (`PeerState` is only `Connected` /
`Disconnected`). So the lobby works it out from what it hears:

- Every `Hello` carries the sender's room (M2), and the host's `Room` names its members. The
  newest room a peer knows (highest revision: its own, or a connected peer's hello) names
  everyone in the room and its host (`Lobby::expected`).
- A peer named there but not connected is **missing** (`Lobby::check_stuck`). Missing for
  `STUCK` = 10 s (WebRTC connects in 0.5–4 s; a fresh connection also gets 10 s), the **higher id
  of the pair reopens** its connection (`Lobby::reopen_now`, through the `Connector` the game
  gives it): the old socket closes (the others see it leave), a new one joins the same room under
  a new id and connects to everyone afresh. The lower id never acts, so a pair never reopens on
  both sides; the host has the lowest id, so a missing host link is always the guest's to fix.
- The new `Hello` names the old id (`Hello::was`): the host moves that id's seat to the new one,
  or gives back the seat it opened when the old id left (`vacated`; matchbox reports the old id
  gone after its 3 s grace, possibly before the new one says hello).
- At most `MAX_REOPENS` = 3 per room; then the page says "no connection to X: leave and join
  again, or try another network" (a NAT that needs TURN, configurable but not deployed). While a
  peer has been missing for more than 3 s the status line says "connecting to X…".
- Not seen: a room of two whose only link failed (neither hears of the other). Each sits alone;
  leaving and joining again is the remedy.

Test `net::lobby::tests::a_stuck_room_reopens`, on the simulated network with a new
`SimNetwork::sever` (a pair that never connects: neither sees the other join, nothing passes):
the host and a newcomer never connect; the newcomer has no room (its elected host, a guest,
never sends one), learns of the host from the other guest's hello, reopens after 10 s as a new
id and is seated. Then a guest pair loses each other: the higher id reopens, its seat (a fourth
seat, not the first open one) follows it, the old ids are gone from the members, the lower id
never reopened.

## 3. Back to the room

M2 dropped the connection with the match: players met again under a new code. Now the match
hands it back:

- **"Back to room"** (the Esc overlay, where single player has Restart; the result panel, where
  Restart was) resigns if still in play, then the driver hands the transport over instead of
  closing it once the host has the resignation (`leave_step(…, to_room)`, `Session::hand_back`):
  a `Msg::Bye` to the host (a guest) or to every guest (the host) first, then the transport and
  the peers it is still connected to (the session now follows peer events for this) go to
  `BackToRoom`. The **Menu** button leaves the room as well, as before.
- `Msg::Bye` is the leave the transport would report on closing: the host drops the seat on one
  tick for everyone (`Dropped`), at once instead of after the timeout; a guest that hears it from
  the host ends its match (`HostLeft`), as when the host leaves.
- The lobby picks the connection up (`Lobby::resume`): the room code and the room as it was when
  the match started (`LobbyRoom::last`), our hello to everyone still connected. A peer still in
  the match drops it (its session ignores lobby packets), so **a first hello from a peer is
  always answered** with ours: whoever comes back later hears us, and we hear them.
- Until everyone is back, a connected peer that has not said hello is **away**: it stays a
  member (not ready), its seat waits, and the start blocker says "waiting for X to come back to
  the room". The first one back hosts its own view after the settle wait (3 s from the resume);
  a lower id coming back takes the newest room over, as in M2.
- Ready flags start cleared; the host starts the rematch as usual (new seed, new match id).

Tests: `net::lobby::tests::back_in_the_room_after_a_match` (three peers start the match; ben
comes back first and hosts its own view, the others' seats kept; ann comes back and hosts again;
cat comes back; the seats as at the start; ready; a rematch may start);
`net::session_tests::back_to_room_chat_and_hidden_tabs` (the leaver's last command applies, its
seat drops 3 ticks after it asked, on every peer, and the transport comes back with both other
peers connected; the host going back ends the match for the guest); and the full-app
`net::lobby_tests::two_apps_meet_start_and_agree`, which now goes on after its 300 agreeing
ticks: the guest presses Back to room, a second later the host; both land on the Multiplayer
page in the same room code, the seats as they were, the host hosting; the guest's chat line
reaches both; it gets ready, and the host could start the rematch.

Spring has no in-engine equivalent: the engine exits after a game, and the lobby server's battle
room, which outlived it, is where the players meet again. This is that, without leaving the game.

## 4. The deployed signaling server

Nothing had contacted `wss://omdurman-matchbox.fly.dev` from this project before (the tests run
an in-process server with its semantics). `net::matchbox_tests::deployed_signaling_smoke`
(ignored; `scripts/signaling-smoke.sh`, `KP_SIGNALING` for another server): two native peers on
this machine join a fresh room `kp-smoke-<code>` with the default ICE servers (Google STUN), wait
for the server's answer, connect, bounce 20 reliable messages, send 100 unreliable ones, then
form a lobby room over the same connections (`Lobby::resume`, as after a match) and send a chat
line round. Run once, 2026-10-15:

| server answered | peers connected | reliable round trip p50 / max | unreliable | room + chat line | all |
|---|---|---|---|---|---|
| 95 ms | 158 ms later | 12 / 12 ms | 100 / 100 | 18 ms | 0.52 s |

The server was awake (no fly.io cold start this time; matchbox retries for 15 s if it is not).
Both peers on one machine connect over host candidates, so this checks the server, the room
path and the protocol end to end, not NAT traversal; the round trips are this test's 5 ms
polling. A run across two networks (STUN only, no TURN) is still to do.

## 5. Menu screenshots

`KP_MENU_SHOTS` (`ui::menu_shots`) creates a room for its Multiplayer shots. It now inserts
`OfflineRooms`, and the lobby opens a `LoneTransport` (an id of its own, no peers, every send
dropped) instead of connecting: a screenshot run never shows up as a room on the public server.
The shots look the same (a host alone in a fresh room, then eight seats with AIs).

## 6. Release and deploy map bakes

Release packages and the web deploy bake the maps separately (`release.yml`'s `maps` job, once
for every package; `deploy.yml` again), and the lobby refuses a peer whose `.kpmap` bytes
differ. Compared at commit `71ab33f` (main when M3 started), all 18 maps:

- the `.kpmap` files GitHub Pages serves (`deploy.yml`, its Ubuntu runner and cargo cache),
- the `kpmaps-determinism` artifact of CI run 38776888514 (`determinism.yml`'s `maps` job: the
  same `bake-maps` action, runner image and `rust-cache` setup as `release.yml`'s `maps` job),
- a local bake (Manjaro, this machine's toolchain),

are **byte-identical** (the same SHA-256 for every map). Nothing to fix: the bake is
deterministic (single-threaded zstd at level 19 from the `zstd-sys` in `Cargo.lock`, libm
terrain math, no timestamps). The `bake-maps` action now prints every map's SHA-256 after
baking, so the two workflows' logs can be compared at any time.

## 7. Names and chat

- **Rename**: the Multiplayer page's name field (at the door, and above the chat in a room:
  click it, type, Enter), or `/name <name>` in the room's chat. `Lobby::set_name` says hello
  again under the new name; the host's room shows it to everyone.
- **Chat in the room**: a column beside the seats with the last 8 lines and the line being
  typed. Keys go to the field last clicked (the room code at the door, the chat in a room); a
  space or Enter in an empty field is still the menu's keyboard navigation. `LobbyMsg::Chat`.
- **Chat in the match**: Enter opens the line (Spring's chat input), Enter sends, Esc drops it;
  while it is open no key reaches the game's hotkeys (`ChatInput::swallow_keys` clears the
  frame's keyboard state after Bevy's input systems). The last lines show in the lower middle
  for 20 s (all of the last 8 while typing). `Msg::Chat`.
- **One relay, one order**: a peer sends its line to the host; the host drops a line that claims
  another sender (Spring: `CGameServer::ProcessPacket`, `rts/Net/GameServer.cpp:1147-1162`) or
  is empty (`GotChatMessage`, `:3057-3063`) and sends it to everyone, the sender included, so
  every peer has the same lines in the same order. Lines are cleaned (no control characters, at
  most 127 characters: Spring's `MAX_MSG_SIZE`, `Game/ChatMessage.cpp:16`). Not part of the
  sim, not in replays. The room's lines and the match's share one `ChatLog`, so the room's talk
  is still there in the match.

Tests: `net::lobby::tests::names_and_chat` (a rename reaches the host's room; two lines arrive
everywhere in the host's order; a line claiming another sender and an empty one are dropped),
`net::session_tests::back_to_room_chat_and_hidden_tabs` (the same in a match, and "tab
hidden"), `net::chat::tests`, `ui::hud::net_overlay::tests::chat_shows_recent_lines_and_the_open_line`.

## 8. Formats and single player

- Wire version 3: `Msg::Bye`, `Msg::Chat { from, text }`, `Msg::Away { peer, away }`, appended.
- Lobby version 2: `Hello::was`, `LobbyMsg::Chat { from, text }` (appended).
- No `SimCommand`, no sim change: `SIM_VERSION`, the replay format and the golden trails are
  unchanged.
- Single player: unchanged. The `bevy_winit` patch only acts on a hidden page, and the worker
  that wakes one runs only in a room or a networked match; the chat line never opens outside a
  networked match; `TabHidden` is never set natively.

## 9. Open

- **WebKit / Safari** hidden tabs untested. If Safari stops the worker, the fallback (drop after
  the timeout, or a minute behind) applies.
- **Rendering while hidden**: a hidden tab still renders every woken frame; skipping the render
  (cameras off while hidden) would save CPU and battery.
- **Two-peer rooms** whose only link failed are not detected (neither hears of the other).
- **Stable player keys** (part H) would let the host recognise a reopened peer without trusting
  `Hello::was`; today a peer could claim another's old id to take its seat in the lobby.
- **The UI** (name field, chat column, in-match chat box, Back to room on the result panel) is
  laid out but not seen: no game window here. The coordinator's visual check should look at the
  room with eight seats and a long chat.

## After the merge with the turn log (H)

- **One identity.** `Hello::was` is gone: the lobby `Hello` carries H's `PlayerKey`, and a
  reopened connection keeps its lobby (and so its key); the host moves the seat of the
  connection with the same key (`Lobby::seat_of_key`: the seat it sits on, or the one opened
  when it left). The Open item on trusting `was` is settled as far as keys go; keys are still
  said to everyone in the room, so a peer that wanted to could still claim another's.
- **Back to room is not a rejoin.** A hello reaching the host's session while the match runs is
  a knock only from a connection new to the match under a key that did not go back to the room.
  A connection of the match (it sent `Msg::Bye`) or a key that went back to the room
  (`Door::in_room`, e.g. its page reloaded in the room) is the room's: the session ignores it,
  and the room counts the host away until it is back. A `Bye` drops the seat at once (no rejoin
  grace). Test `net::rejoin_tests::back_in_the_room_is_not_a_knock`.
- **Too far behind** is now "stalled": the host keeps the whole log, so a guest is dropped only
  when it lacks more than a minute of turns (as before) *and* has not got any further for
  `Timeouts::running` (a minute); a late joiner replaying the log, far behind but advancing,
  stays.
- **Replays**: a match left with Back to room is recorded as any other, at the next match swap
  (`net::lobby_tests::two_apps_meet_start_and_agree` checks the file).
- One combined bump for M3 (none was published): wire version 3 (`Joined`, `Bye`, `Chat`,
  `Away`), lobby version 2 (`Hello::key`, `Running`, `Chat`).
