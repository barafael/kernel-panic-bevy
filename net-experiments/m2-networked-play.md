# M2 (F): networked play — drops, host rules, TAKE's buffer, the in-match network UI

Scope (`docs/multiplayer-plan.md`, M2, part F): what a networked match needs beyond the lockstep
core, testable on the simulated network: peers leaving mid-match become a leave every peer
applies on the same tick, the host's match rules, the packet buffer going with a TAKE (open
since M1), and the in-match network UI (player list, banners, Leave match). Code:
`kernel-panic/src/net/` (`session.rs`, `driver.rs`, `wire.rs`, `apply.rs`, `control.rs`,
`sim_net.rs`), `game_setup.rs`, `headless/replay.rs`, `ui/hud/net_overlay.rs`, `ui/menu.rs`.

## Summary

- **A peer that leaves is a sequenced leave.** The host, on the transport's `Left` or after a
  timeout, puts `SimCommand::Dropped` (appended, index 19, one byte) into the turn stream in the
  seat's name, after the last batch it filed for that seat. Every peer applies it on the same
  tick as a resignation: `TeamFates::leave`, so the user's leaving rules (TAKE for allies, the
  side's last leaver fuses everything) apply unchanged.
- **Timeouts** (Spring's `CheckTimeout`): 30 s before a peer's first packet, 60 s after. The host
  drops a silent guest and tells it (`Ended::Dropped`); a guest that hears nothing from the host
  gives up (`Ended::HostLost`). The host leaving still ends the match (no migration).
- **Host rules** (`MatchRules` in `GameSetup`): pausable, speed range, cheats for everyone, and
  the host's seat (Spring lets the host pause a `nopause` game). One check,
  `net::apply::rules_admit`, used by the host before sequencing and by every peer when applying.
  The default is single player's: goldens and single-player behaviour unchanged.
- **TAKE hands over the packet buffer** with the units (Spring's `GiveEverythingTo` moves the
  team's resources; KP's buffer gadget never moves `bufferSize`).
- **UI**: a player list (seat, ally / enemy / spectator, name, ping in Spring's colours; F9), a
  banner (desync, host left / lost, dropped, leaving, waiting, who left), Esc "Leave match"
  instead of Restart (resign, wait for the host's ack, close the transport, back to the menu),
  no Restart on a networked result panel.
- **Formats**: wire version 2 (`Msg::Pings`), replay format 3 (the setup's rules), `SIM_VERSION`
  5, goldens re-recorded: all four end on the same state hash as at `SIM_VERSION` 4 (no golden
  log has a Network leaver with packets taken by an ally).

## 1. Drops

### Upstream

Spring's server notices a client leaving two ways: the connection closes (`NETMSG_QUIT`, or the
link reports it), or it times out (`UDPConnection::CheckTimeout`,
`System/Net/UDPConnection.cpp:767`: `InitialNetworkTimeout` = 30 s until data arrived,
`NetworkTimeout` = 120 s after, `System/GlobalConfig.cpp:15-21`). Either way it broadcasts
`NETMSG_PLAYERLEFT` and kills the link (`CGameServer::Update`, `Net/GameServer.cpp:1963`); every
client marks the player inactive (`CPlayerHandler::PlayerLeft`,
`Game/Players/PlayerHandler.cpp:66`) and the team stays, leaderless. The user's leaving rule
(2026-10-15, `m1-commands.md` §2) replaces "stays leaderless" with `TeamFates::leave`.

### Here

- **Host** (`Session::peer_left`): the link goes `gone`, its packets are ignored from then on,
  the other guests get `Msg::Left`. If the peer had a seat, the host files
  `(tick, seat, u32::MAX) → [Dropped]` at `max(last target of that seat, next tick to cut)`, so
  it applies after everything the seat sent before it left, in the next turn cut. A seat has one
  peer (batches are numbered per seat), so its peer leaving is the seat leaving. A spectator
  leaving sequences nothing.
- **Every peer** applies `Dropped` like `Resign` (`ApplyCtx::resign`: `MatchControl::resigned`,
  `TeamFates::leave`) and records the seat in `MatchControl::dropped` for the HUD. A drop for a
  seat that already left (resigned, then disconnected: "Leave match") is silently ignored, not a
  refusal.
- **Only the host drops.** A guest's own `Dropped` is stripped at sequencing (counted in
  `SessionStats::refused`); one that got through anyway would be a resignation of the sender,
  which it may do.
- **Timeouts** (`Session::check_timeouts`, every poll): the host drops a guest silent for
  `Timeouts::running` (60 s; `initial` 30 s before its first packet) and sends it `Msg::Left`
  naming itself, so a guest that still listens learns it (`Ended::Dropped`, "The host dropped you
  from the match"). A guest silent from the host's side for as long gives up (`Ended::HostLost`).
  Spring waits 120 s; the host keeps one minute of turns (`HISTORY`), and a guest gone longer
  could only rejoin (M3), so a minute. Keepalives flow every 100 ms both ways even while paused,
  so a paused match never times out.
- **The host leaving** ends the match for the guests (`Ended::HostLeft`, as in M1); the banner
  says so and the Esc menu's Leave match goes back to the menu.

Paused: a drop is filed for the next cut like any command and applies once the match resumes;
the session's `Left` (and the player list) show it at once.

## 2. Host rules

```rust
pub struct MatchRules {
    pub pausable: bool,      // Spring's GamePausable (/nopause)
    pub min_speed: u16,      // percent; Spring's MinSpeed / MaxSpeed mod options
    pub max_speed: u16,
    pub cheats: bool,        // Spring's /cheat, host only
    pub host: Option<u8>,    // the host's seat: may pause even when not pausable
}
```

A field of `GameSetup` (and of the replay's `MatchSetup`, format 3); `Default` is single
player's: pausable, 10–2000 %, cheats only with at most one human seat (M1's rule), no host.

| rule | Spring | here |
|---|---|---|
| pause | `gamePausable || players[a].isLocal` ("allow host to pause even if nopause is set"), spectators never (`Net/GameServer.cpp:1078`) | `rules.may_pause(player)`: pausable, or the host's seat; the Pause key does nothing for others |
| speed | clamped into `[minUserSpeed, maxUserSpeed]` (`CGameServer::UserSpeedChange`, `:3086`) | the Alt+= / Alt+- steps stop at the range's ends; a request outside it is refused (only a modified client sends one) |
| cheats | `/cheat`, a server command (`:2477`) | `cheats_allowed`: `rules.cheats` or ≤ 1 human seat |

**One check, twice** (`net::apply::rules_admit(active, player, cmd)`):
- the host, before it files a batch (`Session::set_admit`, installed by the driver when the
  networked match starts): what the rules forbid is never sequenced; the batch keeps its number
  so acks line up; `warn!` and `SessionStats::refused`;
- every peer, when the command applies (`apply_sim_commands`, after the seat check): a modified
  host can't sneak one in. Refusal `Refusal::Rules(..)` (or `NoCheats`).

**The host's seat** comes from the roster: the driver's first frame of a networked match
(`seat_networked`) fills `ActiveMatch.rules.host` unless the lobby set it. Every peer has the
same roster, so every peer fills in the same seat before the first tick.

Pause and speed don't change sim state (the sim is a function of the turns), so these rules are
about who may stop or rush everyone, not determinism; cheats do change it, and every peer decides
them from the same setup.

## 3. TAKE and the packet buffer

Upstream has no rule of its own for KP's buffer: `network_buffer.lua` keeps `bufferSize[team]`
and has no `UnitGiven` / `TeamDied` call-in, so after a `/take` the packets stayed with the dead
team, out of anyone's reach. The engine's hand-over, `CTeam::GiveEverythingTo`
(`Sim/Misc/Team.cpp:216-233`), gives the team's resources (metal, energy) along with its units;
the buffer is the Network faction's resource. So a valid `Take` moves the whole buffer of `from`
to the taker with its units (`ApplyCtx::take`), the first `Take` in turn order as before. An AI
ally's take goes the same way.

This changes outcomes where a Network owner leaves with packets in its buffer and an ally takes
its units: `SIM_VERSION` 5.

## 4. In-match network UI

All from `NetStatus` (the driver's view, never the sim), `ui/hud/net_overlay.rs`:

- **Player list**, top right (Spring's player roster, `Game/UI/PlayerRosterDrawer.cpp`, shown by
  default as `ShowPlayerInfo` = 1, `Game/Game.cpp:148`; F9 toggles, Spring binds nothing to its
  `info` action): a heading with the input delay in ticks and ms, then per peer in roster order:
  seat, `A` (ally) / `E` (enemy) / `S` (spectator), name (`PeerNames`, which the lobby fills; the
  short `peer-xxxx` otherwise), "(host)", and the ping in Spring's colours (green below 250 ms,
  red from 1 000 ms), "waiting" while this peer waits for it, "left" when gone, and
  "resigned" / "dropped" from the sim's `MatchControl`.
- **Pings**: the host measures every guest's round trip; once a second it sends every guest the
  list in roster order (`Msg::Pings { ms: Vec<u16> }`, unreliable, a few bytes: no ids). A guest
  shows its own measurement for itself. Spring relays each client's ping the same way
  (`NETMSG_PLAYERINFO`).
- **Banner**, top left, the most important first: a desync (frame, dump), the match over (the
  host left / was lost / dropped us, "Esc: Leave match"), "Leaving the match…", "Waiting for X…"
  after 300 ms of stall, and for 8 s who left or timed out.
- **Esc menu**: "Leave match" in place of Restart. Leave = resign (if still in play), then
  `NetSession::request_leave`: the driver keeps sealing what the player issued even if no tick
  runs, closes the transport (`Session::close`, `Ended::Left`) once the host acknowledged the
  batch, at most after 1.5 s (`LEAVE_GRACE`); the host and a peer whose match is over close at
  once. `finish_leaving` then goes to the menu. The Menu plates (Esc, result panel, the web's
  fourth plate) do the same in a networked match, so nobody leaves a connection open behind
  them. Resign stays (resign and keep watching).
- **Result panel**: no Restart in a networked match; "Go to Menu" leaves as above.
- Closing the transport is dropping it (matchbox closes the socket on drop; `SimEndpoint` now
  leaves its room on drop the same way).

## 5. Tests and numbers

Protocol level (`net::session_tests`, the stand-in sim, milliseconds each):

| test | what | result |
|---|---|---|
| `a_guest_that_leaves_is_dropped_on_one_tick` | 3 peers, 30 ms links; the player list's pings, then guest 3 disconnects at ~tick 72 | every guest's ping shown on the other guest (50–90 ms); `Dropped` for seat 2 applied at tick 73 on both remaining peers; trails agree |
| `silence_times_peers_out` | timeouts 3 s / 1.5 s; one guest's uplink dies, another's downlink | the mute guest dropped ~1.5 s after its last packet and told (`Ended::Dropped`); the deaf one gives up (`HostLost`) and is *not* dropped (the host still hears it) |
| `the_host_refuses_what_the_rules_forbid` | a guest's pause (refused by the rules) and `Dropped` in one batch with an order | neither sequenced, the order applied, `refused` = 2 (each counted once, though the batch was resent until acked) |
| `leaving_sends_the_resignation_first` | 60 ms one way; leave right after issuing | transport closed 136 ms after asking (one round trip); the command applied before the seat's drop |

Full sim (`net::multipeer_tests`, headless game apps on the simulated network):

| test | setup | result |
|---|---|---|
| `drops_full_sim` | Marble Madness, 4 humans + 1 AI (2 v 3), 35 ms links; peer 4 "Leave match" at frame 60, peer 3's connection vanishes at frame 120 | peer 4 closed at frame 65; seat 3 out at tick 61, seat 2 at tick 118, the same ticks on both remaining peers; `dropped` = {2}, `resigned` = {2, 3}; the guest's player list shows both gone; 267 ticks of identical trails |
| `host_rules_full_sim` | 2 humans + 1 AI, pausing off, 50–200 %, 40 ms | `rules.host` = seat 0 on both peers; a guest's pause and 400 % refused by the host (`refused` = 2), its 150 % applied everywhere; the host's pause holds both peers and its unpause resumes them; trails agree |

Unit tests: `apply::match_rules_bound_pause_speed_and_cheats`, `dropped_players_leave_play`,
`take_hands_over_the_packet_buffer` (17 + 3 packets → 20 for the taker); `codec`: `Dropped` is
the byte 19; `ui::hud::net_overlay` (rows, ping colours, banner priority); `ui::menu`
(Leave match replaces Restart; Leave match resigns, `finish_leaving` goes to the menu once).

The command fuzz sends `Dropped` from unseated players too (refused like everything else
they send).

Traffic: the ping list is one varint per roster peer (`Msg::Pings`, 1–3 bytes each, about 30
bytes with the envelope at 8 players), once a second to each guest: noise next to the turn
stream (`m1-runtime.md` §5).

## 6. Single player

Unchanged: the default `MatchRules` allow what M1 allowed; the solo session gets no host check;
the Esc overlay and result panel are the same; `Dropped` is never issued. The re-recorded goldens
(only the format and `SIM_VERSION` changed) end on M1's final hashes: `4v4_speed_balls`
`1dc39f68fad9601d`, `ffa8_quad_core` `8bfcd92cc3381e2b`, `2v2_dual_core` `939e3a8b38c8fe25`,
`commands_marble` `5f4ad3e11aafc322`. The one sim change, the buffer going with a `Take`, only
shows when a Network leaver has packets and an ally takes over.

## 7. Open

- **Lobby (E):** `GameSetup.rules` comes from the lobby's options; `PeerNames` from its roster;
  `rules.host` may stay `None` (the driver fills it). E's `Start` must carry `rules` (part of
  `GameSetup`).
- **Shared seats:** a seat has exactly one peer (the host numbers batches per seat). Two players
  on one team (Spring allows it) would need batches numbered per peer.
- **Rejoin** (M3): a dropped player can't come back; its seat stays out of play.
- **Host migration** (later): the host leaving still ends the match.
- **Hidden browser tabs** (M3): a tab without frames sends nothing and is dropped after 60 s.
- Pause while a seat drops: the drop applies at the resume.
