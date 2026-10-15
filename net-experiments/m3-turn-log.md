# M3 (H): the turn log — rejoin, spectators, replay files

Scope (`docs/multiplayer-plan.md`, M3, part H): the host keeps the whole match's turn log, so a
player who dropped (or whose page reloaded) comes back to its seat, anyone else who opens the room
watches, and every match — networked or single player — is recorded as a `.kpreplay` the game can
play back. Code: `kernel-panic/src/net/rejoin.rs` (keys, the door, `Running`),
`net/session.rs` (the turn log, `knock`, `Session::late`, `Session::replay`, the rejoin grace),
`net/lobby.rs` (`Hello::key`, `LobbyMsg::Running`), `net/driver.rs` (fast-forward, the viewer's
clock), `net/apply.rs` (`SimCommand::Returned`), `net/replay_log.rs` (the file format, recording,
saving, `NextReplay`), `ui/menu/replays_page.rs`, `ui/hud/replay_bar.rs`, tests in
`net/rejoin_tests.rs`.

## Summary

- **Seats belong to keys, not connections.** Every game has a `PlayerKey` (random; the web keeps
  it per tab in `sessionStorage`, so a reload keeps it; native per process, `KP_PLAYER_KEY` to set
  it) and says it in its lobby `Hello` (`LOBBY_VERSION` 2). omdurman's finding (stable player
  keys); Spring binds a player slot to its name (`CGameServer::BindConnection`,
  `Net/GameServer.cpp:2914`).
- **The host's session is the door.** While the match runs, a newcomer's `Hello` reaches the
  host's session (lobby and session packets share the transport and never mix); the host answers
  `LobbyMsg::Running` — the `Start` it missed, the roster as it is now, the host's id, who left,
  everyone's name. The key of a seat gets that seat back (its old connection leaves the roster and
  is told, should it still be open: Spring's "terminating existing connection"); any other key
  watches (Spring's `AllowSpectatorJoin`, default on, `GameServer.cpp:73`, `:2957-2966`). Every
  guest hears of it (`Msg::Joined`, wire version 3) and updates its roster.
- **Catching up is replaying the log.** The newcomer loads the match from the `Start` like
  everyone and starts a session holding no turn (`Session::late`: the host named, not elected —
  the newcomer's id may be the lowest; no turn horizon). The host keeps every turn now (the
  one-minute `HISTORY` is gone); the newcomer's ordinary gap fills fetch the log from tick 0, a
  minute (1 800 turns) per request, in ≤ 16 KiB chunks on the reliable channel. The driver
  fast-forwards (`Pacer::fast_forward`): as many ticks per frame as 250 ms of wall time allow, a
  frame drawn in between, the banner saying "Catching up with the match: n / m s", until it holds
  no more than its normal backlog. Spring's way: the server resends its whole packet cache
  (`GameServer.cpp:3046-3047`) and the client simulates it.
- **The seat's fate (decided).** A disconnect is dropped after a grace, `Timeouts::rejoin` = 30 s
  (a timeout by silence still drops at once: it had a minute already). A player back within the
  grace finds its seat as it was: nothing is sequenced, its units did what they were told
  meanwhile (Spring's leaderless team). After the drop the user's leaving rules have acted (TAKE
  for its allies, or its side's units fused); the host then sequences `SimCommand::Returned`
  (index 21; 20 is the speed control's) after the `Dropped`, and every peer decides alike: the
  seat is back in play if it dropped, its side still has an active owner, it still owns units
  (nobody took them) and the match is not decided — else the player watches, as after a
  resignation. Spring lets a dropped player reconnect to its team and revives a dead one
  (`CTeam::AddPlayer`, `Sim/Misc/Team.cpp:283`).
- **Spectators** are roster members without a seat: they get the turn stream, send acks and
  hashes but no commands (they were already in M2's roster); late joiners with an unknown key
  become spectators through the same catch-up. Their `LocalTeam` is the spectator team.
- **Replay files.** Every peer keeps the commands of the turns it applied (`Session::log`), the
  solo host of single player too. At the swap to the next match (or when the game quits) the
  match becomes a `.kpreplay` (format 4: + `ReplayMeta` — names per seat, networked, when):
  natively `replays/<date>_<map>.kpreplay` (the newest 100 kept), on the web the page session's
  list with a Save (download) button and "Open file…". Not recorded: the menu's demo, showcases,
  the bot's sandbox, matches under a second, replays themselves.
- **The viewer.** Main menu → Replays: the recordings newest first, eight a page, Play. A replay
  session holds every turn and plays one per tick (`Session::replay`), the viewer spectating; a
  bar at the top shows the file, the game time and the speed, with slower / pause / faster
  buttons (and Pause or Space, `-` / `+`). Pause and speed act on the driver's clock only
  (`Pacer::hold`, `Pacer::rate`, 0.25×–16×); the recorded pauses and speed changes still apply.
  Esc → Restart plays it from the start. A replay recorded on another copy of the map says so.
  The golden-replay player (`headless::replay::play`) now plays through the same replay session
  (it used to deliver frames to the bus directly).
- **Formats:** `SIM_VERSION` 6, wire 3, lobby 2, replay format 4; goldens re-recorded.

## Design notes

- **Host election vs a named host.** A newcomer's matchbox id is random, so it may be lower than
  the host's; the match's host is named in `Running` and `Session::late` takes it as given (the
  lowest-id rule only ever picks the host at the `Start`). The roster is changed the same way on
  every peer (`Msg::Joined` replaces the old connection), so `Msg::Pings`' roster order holds.
- **Batch numbering.** Batches are numbered per seat; a returning connection numbers from 0
  again, so the host resets that seat's input (`Host::rejoin_seat`): what the old connection sent
  and the host had not filed is dropped, what it filed stays scheduled.
- **The log.** The host's `history` is now the whole match (`Vec<Turn>`, ~30 bytes a turn in
  memory: 1.6 MB for 30 minutes); gap fills serve up to 1 800 turns per request (was 900), packed
  up to 2 048 turns per ≤ 16 KiB chunk (a quiet minute is one chunk). The guest's `TurnBuffer`
  keeps its contiguous end (`through`) instead of counting held turns each call: a late joiner or
  a replay holds tens of thousands. A late joiner's buffer has no horizon (3 600 turns
  otherwise).
- **Fast-forward** (`Pacer::fast_forward`): the guest branch's extra ticks lose their cap (8 a
  frame) for a 250 ms wall budget; Spring's reconnecting client likewise spends up to
  `1000 / MinDrawFPS` ms simulating per drawn frame (`CGame::GetNetMessageProcessingTimeLimit`,
  `rts/Net/NetCommands.cpp:245-256`, `MinDrawFPS` 2). It ends when no more than the normal
  backlog (2) is in hand and the host has nothing newer. The delay controller already ignores a
  catching-up guest's samples; the host's hash check ignores reports older than its last 64.
- **Recording.** `Session::pop` keeps each applied turn's non-empty frames; the solo host's turns
  hold the local player's commands (and cheats), every turn of a networked match the seated
  players' (and the host's `Dropped` / `Returned`). The AI is never in it: it runs in the sim.
  `replay_log::finish_match` runs at the swap (`match_scope::begin_match`, before the resets)
  and in `Last` when `AppExit` is written. The map stamp is the hash of the file the match loaded
  (`ActiveMapHash`, set at the swap), as the lobby compares it. The headless harness saves nothing
  (`ReplayDir(None)`).
- **Playing.** `NetSession::from_replay`: every turn in hand, `CatchUp::replay()` (no extra ticks),
  the local team the spectator's. A recorded pause is followed by its resume on the next turn, so
  the replay plays straight through it. The golden player (`headless::replay::play`) uses the same
  session; the golden trails are what they were.

## Tests and numbers

Protocol level (`net::rejoin_tests`, stand-in sim, real lobby on the newcomer's side, 30 ms
links, 60 Hz frames; milliseconds of wall time):

| test | what | result |
|---|---|---|
| `a_player_back_within_the_grace_keeps_its_seat` | 3 peers; seat 2's connection breaks at 4 s, it comes back 2 s later as `PeerId(0)` (below the host's id) with its key | seat 2 again, the old id gone from every roster, the host still host; nothing sequenced; 359 ticks in step from tick 0; 19 of its orders applied after the return |
| `a_dropped_player_returns_and_a_newcomer_watches` | grace 0: seat 1 dropped at once, back 2 s later; then a newcomer with an unknown key | `Dropped` then `Returned` on the same ticks everywhere; the newcomer watches (no seat, no commands); every trail the host's |
| `the_turn_log_plays_back` | 5 s match | every peer's log identical; `Session::replay` of the host's log gives its trail, one tick a frame |
| `rejoin::tests::seats_follow_keys_not_connections` | the door | a seat's key gets the seat, a watcher's watches, unknown keys and the host's own key watch |
| `apply::tests::returned_players_come_back_if_nothing_took_their_seat` | the sim rule | back while its ally plays and its units are its own; refused after a `Take`, after a resignation, for a seat that never left |

Full sim (`net::rejoin_tests::full::rejoin_full_sim`, Marble Madness, seats 0 + 1 (humans)
against 2 (human) + an AI, 35 ms links, 30 Hz frames, every seat scripted): seat 1's connection
breaks at frame 150 and the host drops it at tick 147 (its units idle for seat 0); the player
comes back as a new connection with its key at frame 210 — its lobby gets `Running`, it loads
the map, replays 209 ticks in one frame and goes live; `Returned` at tick 209 puts seat 1 back on
every peer and its script's orders apply again; a newcomer then joins and watches. All four
trails (the returning player's and the watcher's from tick 1) are the host's over 587 ticks, no
desync. The host's recording (`replay_log::replay_of`: 12 frames, 28 commands) goes through the
file format and the replay player and gives the host's full per-tick trail over all 587 ticks.

Catch-up cost: a rejoin replays the match at sim speed — natively ~1–3.5 ms a tick at 4v4 / FFA
scale (`m1-runtime.md` §4), so a 30-minute match (54 000 ticks) takes 1–3 minutes, the web build
2–6× that (`m0-golden-replay.md`). The log transfer is not the limit: a quiet minute of turns is
one ≤ 16 KiB chunk.

The goldens were re-recorded (`SIM_VERSION` 6, format 4); the query-order fuzz and the command
fuzz (with `Returned` from unseated senders) pass.

## Single player

Unchanged in play: the solo host's turns are as before. New: the match is recorded (a copy of
each non-empty turn frame in memory, written at the swap) and saved to `replays/`. The Replays
page and the viewer are new; the main menu has one plate more (nine natively).

## Open

- **Native identity across restarts:** the key is per process (or `KP_PLAYER_KEY`); a restarted
  native game comes back as a watcher. omdurman keeps it in a locked slot file in the config
  directory; worth doing when the native game gets a profile.
- **Duplicated browser tabs** copy `sessionStorage`, so a duplicate takes the seat over (the
  original is told it was dropped), as Spring's same-name reconnect does.
- **Snapshot rejoin:** a long match costs minutes of catch-up, more on the web; a state snapshot
  (or periodic checkpoints in the host's log) would cut it to the snapshot's transfer.
- **Host migration:** the log lives on the host; with it gone the match ends (as before).
- **Back to the room after a match** (agent I): a rejoin needs the room; once the transport goes
  back to the lobby after the match, `Running` must not be answered for a finished match.
- **Replay seeking:** the viewer plays forward only (Restart starts over); seeking means
  fast-forwarding from the start or keeping snapshots.
- **Spectator chat / player list in replays:** the viewer shows no player list (names are in the
  file's meta).
