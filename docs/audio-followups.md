# Audio follow-ups: what the original still does that the port does not

Companion to `docs/audio-plan.md`, which covers what landed on 2026-10-11
(the 26 effects, the engine's attenuation, unit replies, the soundtrack
in Git LFS, the Audio settings page, `music.html`). This is the plan for
the rest, ordered by value: the tip dispenser with its voice-over is the
one players will notice; the others are small or wait on other features.

## 1. The tip dispenser and its voice-over

### What upstream does

`LuaUI/Widgets/kp_automatic_tip_dispenser.lua` (public domain, zwzsg
2009). Every 10 s it draws a new tip in a cyan box at the bottom centre
of the screen and, unless muted, plays the matching voice clip:

- `GetTipsList()` (lines 260–521) rebuilds the candidate list every time
  from game state: 94 `AddTip(weight, text, sound)` calls, 91 of them
  voiced, each guarded by conditions such as the player's side, which
  unit kinds they own (`CountMy`), the enemy owns (`CountTheirs`), a
  kind's share of the army (`Prevalence`), what is selected
  (`isSelected`), damage multipliers between kinds (`GetDamage`,
  `GetDamageBonus`), game time, and the hero / O.N.S. mod options. Unit
  names are substituted per faction (`GetFixedUnitNames`: "Bit" / "Bug"
  / "Packet" for the spam unit, and so on).
- `GetRandomTip()` draws by weight, with the tip currently shown set to
  weight 0 so it never repeats back to back.
- The clip is `sounds/tips/<side><tip>.ogg`: side 1 = System, 2 =
  Hacker, 3 = Network, 4 = Touhou, 5 = R.P.S. The 140 clips are 64 System,
  68 Network, 2 Touhou, 6 R.P.S. and **none for Hacker**; the widget
  falls back to the Network voice (`3000 + tip`) for any side lacking a
  clip. "Voices by Eva and Panda" are the two voices.
- Clicking the box toggles the voice and plays clip 3091 / 3092 ("voice
  on" / "voice off"); the box reads "Voice: ON/OFF" for one period.
  `+`/`-` on the box resize the font. The widget removes itself in
  missions and for spectators.

### Port

1. **Assets**: `scripts/bake-sounds.sh` gains the tips: copy the 140
   `.ogg` (already Vorbis, 4.1 MB) to `kernel-panic/assets/sounds/tips/`
   as-is (names are digits, no renaming). Regular git, not LFS.
2. **Data** (`kernel-panic/src/ui/hud/tips/catalog.rs`): the 94 tips as a
   static table `Tip { weight, text: TipText, clip: Option<u16>,
   when: fn(&TipContext) -> bool }`, transcribed in upstream order with
   the line number of each `AddTip` in a comment. `TipText` is a template
   with the faction unit-name placeholders upstream fills (`un.spam`,
   `un.minifac`, `un.cons`, …), rendered through a `UnitNames` table per
   faction (port `GetFixedUnitNames`).
3. **Context** (`tips/context.rs`): a `TipContext` built once per draw
   from the ECS: local faction, per-kind counts for the local team and for
   enemies (`TeamUnitCounts` already exists in
   `units::lifecycle::bookkeeping`), selection by kind, game seconds,
   the damage table from `WeaponRegistry` for `GetDamage`, mod options
   (heroes, O.N.S.) where the port has them, else `false`. Keep the
   helper names (`count_my`, `count_theirs`, `prevalence`,
   `is_selected`) so the transcription reads like the Lua.
4. **Dispenser** (`tips/mod.rs`): a resource `TipDispenser { timer,
   current: Option<usize>, voice: bool, rng }`; every `TIP_PERIOD` (10 s)
   it filters the table, draws by weight with the current tip zeroed
   (`GetRandomTip`), sets the box text, and pushes a `SoundRequest` for
   `tips/<side><clip>.ogg` on a new `Channel::Voice`, falling back to the
   Network voice exactly as upstream (clips the side lacks → 3xxx). A
   voice clip is 2D (`PlaySoundFile` without a position) and must not be
   cut by the concurrency cap: give it a priority or play it outside the
   `MAX_SOUNDS` budget.
5. **Bank**: `SoundBank` grows a second map for the tip clips keyed by
   their number; `SoundId::from_name` stays for effects. Loading 140
   small handles at startup is fine (same as the effects).
6. **UI**: the cyan box at bottom centre, text size slaved to the tooltip
   font like upstream intended, drawn with the HUD's `LabelSpec` style
   (`ui/hud/tooltip.rs` is the closest sibling). Click toggles the voice
   and plays 3091 / 3092; the Audio page gets a "Tips voice" On/Off
   switch persisted in `AudioSettings` so the choice survives restarts
   (upstream forgets it per match). Hidden in the showcase and in
   missions when those arrive.
7. **Tests**: the table has 94 entries and 91 clips; every referenced
   clip number resolves to a file for System and Network (or to the
   Network fallback); `GetRandomTip`'s weighting never returns the
   current tip when another candidate exists; a snapshot test that the
   context built from a small world (one Socket, three Bits selected)
   yields the same candidate set as the Lua would — hand-derived for two
   or three states.
8. **Credits**: "Voices by Eva and Panda" is already on the Credits page;
   add the tips to `music.html`'s Sound effects tab as a third group
   ("Tips voice-over", one row per clip with its text) once the catalog
   exists, so the page can play them too.

Effort: the transcription is the bulk (a day of careful work); the
plumbing reuses `SoundQueue` and the HUD label code.

## 2. `FailedCommand` (`commandfailed.wav`)

Upstream: `GuiHandler.cpp:1305` plays it on the UserInterface channel
when a click with an armed command yields `CMD_FAILED`, i.e. the GUI
could not turn the click into a command: a build placed on blocked
ground, an attack order on nothing attackable, a command whose target
is out of the map. The port keeps the command armed on an invalid build
site (`ui/hud/placement.rs`, "A click on an invalid site places nothing
and stays armed") but says nothing.

Port: push `SoundId::COMMAND_FAILED` (2D, `Channel::UserInterface`) at
the sites that reject an armed click: the invalid build site in
`placement.rs`, and the ability click handlers in `interaction/ability.rs`
when the armed mode finds no valid target (`armed_ground_click` returning
nothing). One helper `SoundQueue::command_failed()`. Test: an invalid
site produces exactly one request per click. Small.

## 3. Map markers and chat (`beep6.wav`, `beep4.wav`)

Upstream plays `MapPoint` (3D at the marker, no rolloff, no doppler) when
any player drops a map marker, and `IncomingChat` (2D) on a chat line.
Neither feature exists in the port today; both arrive with
`docs/multiplayer-plan.md`. When they do: markers push `beep6` at the
marker position with `rolloff = 0` (so `spatial_gain` is bypassed: a
`SoundRequest` flag `attenuate: bool`), chat pushes `beep4` 2D. Also
worth a local-only marker (the Spring `MapPoint` is a single-player
feature too: Alt+click) before multiplayer, since it is a cheap HUD
feature with an obvious sound.

## 4. Stereo panning

The engine positions every 3D sound with OpenAL, so a battle on the left
of the screen sounds left. The port mixes mono.

Port, without fighting rodio's distance model: keep the engine gain from
`spatial_gain` as the sink volume, and use Bevy's spatial audio only for
direction. rodio 0.22 (`source/spatial.rs:57-68`) computes each ear's
gain as `pan × min(1 / dist², 1)`, with `pan` in [0.5, 1] from the
difference of the ear distances over the ear gap. So: give the camera a
`SpatialListener::new(0.6)` and spawn each 3D sound with
`spatial: true` and a `Transform` at `listener + dir × 0.6`, where `dir`
is the source direction in the camera's frame projected to the ear axis
(screen x). Every ear distance then stays ≤ 0.9, the distance term is
exactly 1, and only the pan term applies. The emitter sits at a fixed
world point, so camera movement during a short sample does not matter;
for the music (2D) nothing changes. Test: a source left of the camera
yields left gain > right gain in a `SpatialPlayer` computed by hand, and
the sink volume still equals `spatial_gain`. Small, half a day with
listening.

## 5. Verification debt from the first pass

- Scripted bot match (`KP_BOT`, `target/kp-audio-bot/script.txt`): see
  "audio: loading 26 original sound effects" and "audio: first sound
  effect" in the log, no asset errors, no panic. Needs a debug build;
  build alone, the machine ran out of memory with two cargo jobs.
- Credits page: twelve lines at 22/30 of the page size; confirm the block
  clears "Spring Engine by:" at a 1280×720 window.
- Web: open the deployed game, click once, hear a track start within a
  few seconds, confirm the next track is fetched during the first
  (Network panel) and the 2 s gap holds.
- Headless Chromium screenshots of `music.html` already exist for
  desktop and 375 px widths; re-shoot after any style change.
