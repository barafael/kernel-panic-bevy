# M2 — the lobby (agent E)

The room protocol on top of the `Transport` trait, the `Start` handshake that
turns a room into a networked match, and the launch menu's Multiplayer page.
Code: `kernel-panic/src/net/lobby.rs` (protocol, Bevy glue, unit tests),
`net/compat.rs` (the compatibility stamp), `net/lobby_tests.rs` (two full apps),
`ui/menu/lobby_page.rs` (the page). Plan: `docs/multiplayer-plan.md` M2.

## 1. Shape

cake's lobby (`../cake/cake-bevy/src/lobby.rs`: lowest id hosts, the host owns
the roster and broadcasts it, guests take it verbatim, `Start` from the host) with
Spring's battle room on top (seats with side and allyteam, AIs, ready flags, a
host-chosen map, archive checks).

| message | direction | when |
|---|---|---|
| `Hello { name, build, room }` | everyone → everyone | on meeting a peer (`PeerEvent::Joined`) |
| `Status { ready, map, have }` | guest → everyone | when it changes (and to newcomers) |
| `Sit { seat }` / `Stand` / `Faction { faction }` | guest → host | the guest's own seat |
| `Room(RoomState)` | host → everyone | whenever the room changes |
| `Start(StartMsg)` | host → everyone | the host presses Start |

All on the reliable channel. Every message is `(LOBBY_TAG, LOBBY_VERSION, msg)`
in postcard. `LOBBY_TAG = 0x4b4c` sits where a session `wire::Envelope` has its
format version (1), so `wire::decode` drops lobby packets and the lobby drops
session packets (test `lobby_and_session_packets_never_mix`). That is what lets
the two share one transport: a guest's last `Status` reaching the host after the
host's session took the transport is dropped there, and the session's first
unreliable turn packets reaching a guest whose lobby has not yet seen the `Start`
are drained and dropped (they repeat; the host only starts its clock once it
heard from every guest's session anyway). A tag with another version marks the
sender as another build ("runs another version of the game").

### Host election

Lowest `PeerId` among this peer and every connected peer that said hello with a
compatible build (`Build { sim_version, units_hash }`) — the session's own rule
(lowest id in the roster), so the lobby's host is the match's sequencing host
(asserted in `three_peers_meet_and_start`). Recomputed on every poll:

- **Host leaves:** the next lowest takes the room over as it last received it;
  the leaver's seat opens, members are rebuilt from who is connected, the
  revision goes up (`host_leaving_hands_the_room_on`).
- **A newcomer with a lower id** (UUIDs are random, so this is common): it would
  otherwise start a blank room and, being elected, overwrite everyone's. So every
  `Hello` carries the sender's newest `RoomState`; a peer becoming host takes over
  the highest revision it has seen. And a peer only acts as host once it is
  *settled*: every connected peer said hello, or `SETTLE` (3 s) passed since the
  last join (a peer in a running match never answers).
- An incompatible build is never elected and never seated; it hosts its own
  view, which everyone else ignores (rooms are only accepted from one's own host).

### The room

`RoomState { rev, host, map, difficulty, seats, members }`. Seats: up to 8 (4v4),
each `{ faction, ally_team, who: Open | Ai | Human(PeerId) }`; a fresh room is the
host on seat 0 (System, team 1) against an open seat (Hacker, team 2). Members:
everyone who said hello (and other-version peers), by id, with name, ready flag
and the host's `Check`: `Ok`, `Pending` (build or map hash not in yet) or
`Refused(reason)`. Readable reasons, shown to everyone:

- "runs game version 3 (this one runs 4); update the game" (`SIM_VERSION`)
- "has different unit data (…); update the game" (`units.kpu` hash)
- "runs another version of the game" (lobby protocol)
- "has no copy of map X", "has a different copy of map X" (`.kpmap` bytes hash)

Newcomers take the first open seat, otherwise they watch. The host cycles a
seat's occupant (open → AI → each member → open; picking a seated member moves
them), faction and ally team, adds and removes seats, steps the map through its
catalog and sets the AI difficulty. A guest sits down on an open seat, stands up
(watches) and cycles its own faction (Spring: each player picks its side).

**Ready** is Spring's `readyToStart` (`CGameServer::CheckForGameStart`,
`rts/Net/GameServer.cpp:2135-2177`: the game starts once every active,
non-spectating player is ready). A status names the map it is for, so a map
change clears every ready flag. `start_blocker()` says why Start is not possible
yet (shown under the buttons): not settled, fewer than two occupied seats, fewer
than two ally teams, the host's own map not read, a seated guest refused /
pending / not ready. Spectators need not be ready; one whose check is not `Ok`
is left out of the match.

### Start

`StartMsg { match_id, setup: SetupWire, roster, compat }`:

- `setup`: map, seats (faction, ally team, AI) with open seats dropped (seat `i`
  of the match is owner `i`), AI difficulty, the seed the host drew
  (`random_seed`, as single player draws one). `SetupWire` and its two
  conversions `encode_setup` / `decode_setup` are the one place a new
  `GameSetup` field (F's `MatchRules`) joins the handshake.
- `roster`: every member cleared to play → its seat, or `None` (spectator).
- `compat`: the host's `Compat { sim_version, units_hash, map_hash }`.
- `match_id`: random, so the session drops an earlier match's packets.

A guest compares the stamp with its own (build, and its hash of the map) before
anything else. On a mismatch — possible only through a race, since the host
already checked everyone — it refuses with the reason ("Your copy of map X
differs from the host's.") and leaves the room; the host's session then sees it
leave instead of waiting for it (`a_guest_refuses_a_start_it_cannot_play`). A
peer outside the roster leaves too ("The match started without you: you …").
Spring compared engine versions at the connection
(`CGameServer::BindConnection`, `GameServer.cpp:2893-2906`) and map / game
archive checksums in the start script (`CPreGame`, `rts/Game/PreGame.cpp:617-657`);
Recoil now only logs the archive mismatch. A lockstep match on different data is
a certain desync, so we refuse.

Then every peer, host included: `GameSetup` ← the setup, `NextNetMatch { config,
transport, catch_up }` ← the session and the room's transport, `AppState::InGame`
— the normal match entry: `prepare_game_entry` loads the map, the swap's
`begin_match` resets the match resources and `NetSession::from_world` takes the
`NextNetMatch`.

## 2. Into the match, not the attract demo

The menu's demo keeps loading and restarting behind the Multiplayer page, so:

- `begin_match` of a **demo** setup holds a waiting `NextNetMatch` aside while it
  resets the match resources (`NetSession` would take it) and puts it back: a
  demo load that lands between the `Start` and the networked load cannot eat it.
- The lobby's systems run in `PostUpdate`, so a `Start`'s `GameSetup` insert
  lands after anything the demo director wrote in the same frame's `Update`.
  The `InGame` entry supersedes (cancels) the demo's pending load.
- `begin_match` sets `LocalTeam` from the session's roster for a networked
  match (the seat, or the spectator team), from the swap on: before, the first
  human seat was the local team until the driver's first frame corrected it.
- Entering any other match while in a room (single player from another page)
  leaves the room.

`two_apps_meet_start_and_agree` drives this with two complete headless apps
(menu, demo, map loading, driver): the boot demo is loading when the room forms,
one app creates a room, the other joins by its code, the host sets the map and
an AI seat, the guest gets ready, the host starts; both load the match
(`ActiveMatch` = the map, not a demo; `LocalTeam` = their seat; a networked
`NetSession`, the host's role as the lobby elected it) and their per-tick hash
trails agree over 300 ticks with no desync.

## 3. The page and the launch room

Main menu → **Multiplayer** (third plate; the zigzag tightens to fit eight).
Outside a room: your name, Create room, the room code field (typed; letters,
digits, `-`, `_`; Backspace), Join, any refusal in red, Back. In a room: the
heading names the code, the map row (`<` `>` for the host), the AI difficulty
row, one row per seat (Seat n, occupant, faction, team: buttons where this player
may change them, plates otherwise), `+ seat` / `- seat` for the host, who
watches and who cannot play and why, Start (host) or Ready / Not ready (seated
guest), Leave, and a status line (the start blocker, "waiting for the host", or
the last refusal). Rows are flex rows, so long names and map names push the
next cell instead of overlapping it. `KP_MENU_SHOTS` now also shoots the door,
a fresh room and a full eight-seat room.

Launch room: `#room=<code>` in the page URL (web; creating or joining writes it
into the address bar with `history.replaceState`, so the URL is the invite;
leaving removes it), `--room <code>` or `KP_ROOM` (native). It opens the
Multiplayer page and joins. Names: `KP_NAME` (native) or a random kernel word
with two digits ("socket42").

Room codes: `RoomId` is a stand-in in `lobby.rs` until D's `net::matchbox::RoomId`
replaces it (same `Display` / `FromStr`); fresh codes are five characters of
cake's unambiguous alphabet.

Map hashes: native reads the catalog's file (≤ 2 MB `.kpmap`, FNV-1a, a few
ms, once per map); the web fetches it through the asset server like the map
loader (`BytesAsset`) and hashes it when it lands (the prefetch of the selected
map shares the request).

## 4. The transport switch

Everything opens the room through one function:

```rust
pub fn connect(room: &RoomId) -> Box<dyn Transport>
```

The game opens the room with `super::matchbox::open_room(room,
&NetConfig::from_launch())` (merged by the coordinator); in tests it joins an
in-process `SimNetwork` per room code (a process-wide registry, zero latency), so
two apps in one test process meet (the full-app test). `sim_net` is test-only. `SimEndpoint` now leaves the
network when dropped, as a closed matchbox socket does: leaving a room (Leave, a
refusal, the lobby dropped) is a `PeerEvent::Left` for the others.

## 5. Tests

| test | what |
|---|---|
| `lobby::tests::lobby_and_session_packets_never_mix` | tags: neither decoder takes the other's bytes; another lobby version is `Foreign` |
| `lobby::tests::room_codes` | `RoomId` parsing, generated codes, the URL fragment helpers |
| `lobby::tests::setup_round_trips` | `SetupWire` both ways |
| `lobby::tests::three_peers_meet_and_start` | 30–50 ms, 5 % loss: a lower-id newcomer takes the room over, seats, names, host edits, a guest sits and picks its faction, ready, start blocker texts, every peer's `Start` identical with its own seat; the roster's lowest id is the lobby's host |
| `lobby::tests::host_leaving_hands_the_room_on` | re-election keeps map and seats, opens the leaver's seat |
| `lobby::tests::mismatches_are_refused_with_reasons` | another `SIM_VERSION`: listed, never elected or seated, no block; different / missing map: blocks with the reason; map change clears ready |
| `lobby::tests::a_guest_refuses_a_start_it_cannot_play` | a stamp differing at `Start`: refused with the reason |
| `lobby::tests::late_lobby_packets_do_not_upset_the_session` | lobby traffic into a running host session: ignored |
| `lobby_tests::two_apps_meet_start_and_agree` | two full apps, see §2 |

The protocol tests run in milliseconds of wall time (seconds of simulated
network time); the full-app test loads one small map twice and plays 300 ticks.

## 6. Open

- **After the match** the transport is dropped with the session: the room is
  gone, players meet again by joining a new code (cake keeps the socket and
  returns to its lobby). Returning to the room needs the session to hand the
  transport back (F's result screen closes it today).
- **Late joiners** of a room whose match is running see silent peers ("N more in
  the room not answering") and host an empty room of their own; rejoin is M3.
- The host's room snapshot is sent whole on every change (≈ 0.5–1 KB for 8 seats
  and a dozen members; 16 KiB fits ~300 members). Fine for a lobby.
- A guest cannot rename itself in the UI (`KP_NAME` natively, random otherwise),
  and there is no chat.
- Map hashes compare the `.kpmap` bytes. A native dev build without baked maps
  hashes the `.sd7`, so it cannot play with the web build; release packages and
  the web deploy both ship baked maps, but bake separately (`release.yml` once,
  `deploy.yml` again): worth a check that the two bakes are byte-identical.
- `RoomId` and `connect` are stand-ins (D); `MatchRules` joins `SetupWire` and
  the room's options once F's lands (the coordinator).
