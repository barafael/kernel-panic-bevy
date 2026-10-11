# Audio: bringing the original Kernel Panic sounds into the Bevy port

## 1. How the original does it

Kernel Panic ships **no music** and two kinds of audio, all public domain
(`original-game-license-info.txt`: "Sounds by Noruas and Pendrokar",
"Voices by Eva and Panda"):

| Where | What | Size |
|---|---|---|
| `upstream/Kernel-Panic/sounds/*.wav` | 26 effects, 16-bit mono 44.1 kHz PCM | 1.3 MB |
| `upstream/Kernel-Panic/sounds/tips/*.ogg` | 140 voice-over clips for the tip dispenser, Vorbis mono | 4.1 MB |

The engine (Recoil / Spring) decides *when* a sound plays; the game only
names files. Every trigger, with the engine source that fires it:

| Trigger | Upstream definition | Engine site | File |
|---|---|---|---|
| Weapon fires | `soundstart=` in `weapons/*.tdf` | `CWeapon::Fire` (`Weapon.cpp:1269`): every salvo shot, 3D at the firing unit, Battle channel | `bitfire`, `bytefire`, `pointerfire`, `packetfire`, `flowfire`, `obeliskfire`, `bugartifire`, `netelectricarc` |
| Projectile / beam explodes | `soundhit=` in `weapons/*.tdf` | `CGameHelper::Explosion` (`GameHelper.cpp:340`): every explosion, 3D at the impact, Battle channel | `pointerhit`, `bytehit`, `flowhit`, `obeliskhit`, `bugartihit`, `SIGTERMhit`, `Explosion5` |
| Unit dies | FBI `ExplodeAs=` → that weapon's `soundhit` | `CUnit::KillUnit` → `DoExplosion` → same `Explosion` path | `bitdeath` (RetroDeath, Minekiller), `bytedeath` (RetroDeathBig / VBig), `Explosion5` (logic_bomb); VirusDeath has no `soundhit` |
| Single unit selected | `gamedata/SOUND.TDF` `[COMPUTER] select1=select` | `CSelectedUnitsHandler::SelectGroup` / `SelectUnits` (`SelectedUnitsHandler.cpp:335, 386`): exactly one unit picked, 3D at the unit, UnitReply channel | `select.wav` |
| Several units selected | `gamedata/sounds.lua` `MultiSelect` (`in3d=false`) | `SelectedUnitsHandler.cpp:338`, UserInterface channel | `button9.wav` |
| Order given | `SOUND.TDF ok1=commandgiven` | `CSelectedUnitsHandler::GiveCommand` (`:264`): once per command, 3D at the *first* selected unit | `commandgiven.wav` |
| Own unit reaches its goal | `SOUND.TDF arrived1=commandgiven` | `CGroundMoveType::Arrived` (`GroundMoveType.cpp:2474`), only for `gu->myTeam`, General channel | `commandgiven.wav` |
| Firewall cast | `LuaRules/Gadgets/network_reflectorshield.lua:99` `Spring.PlaySoundFile("sounds/built.wav", 10)` | Lua, 2D | `built.wav` |
| Failed command | `sounds.lua` `FailedCommand` | `GuiHandler.cpp:94` (a command the GUI could not give) | `commandfailed.wav` |
| Map marker, chat | `sounds.lua` `MapPoint`, `IncomingChat` | `InMapDraw.cpp`, `Game.cpp` | `beep6.wav`, `beep4.wav` — the port has neither feature |
| Tips voice-over | `LuaUI/Widgets/kp_automatic_tip_dispenser.lua` | widget: a tip every 10 s, clip `<side>000 + tip#` (System 1xxx, Hacker 2xxx, Network 3xxx; Network is the fallback voice), toggled by clicking the tip box (clips 3091 / 3092 say "voice on/off") | `sounds/tips/*.ogg` |

Every unit except `mineblaster.fbi` (`SoundCategory=;`) is in the
`COMPUTER` category, so every unit answers with the same three sounds.
`activate`, `deactivate`, `build`, `underattack`, `cant` are not defined
for `COMPUTER`, so the engine's other reply hooks are silent in KP.

### How the engine mixes

`rts/System/Sound/OpenAL/SoundSource.cpp`, `Sound.cpp`:

- OpenAL `AL_INVERSE_DISTANCE_CLAMPED`, reference distance **200 elmos**,
  rolloff factor **5 × item.rolloff × heightMod**. Gain for a source `d`
  elmos from the listener: `200 / (200 + rolloff · (max(d, 200) − 200))`.
- The listener is the **camera itself** (`Game.cpp:1371`,
  `camera->GetPos()`). `heightMod = min(600 / cameraHeightAboveGround, 1)`
  (`Sound.cpp:836`) so a far zoom does not silence the battlefield.
- Listener velocity is never set (no doppler on camera moves); source
  doppler exists but no KP sound item sets it apart from the default.
- Volumes: the engine's master default is 60 %, but the KP 4.4 Windows
  installer ships a `springsettings.cfg` with `snd_volmaster=100` and
  every channel (General, Battle, UnitReply, UserInterface, BGMusic) at
  100 %. `kp_settings.lua` raises `MaxSounds` to 96 concurrent sources.
- `in3d` defaults to true; `MultiSelect` / `IncomingChat` are 2D,
  `PlaySoundFile` without a position is 2D.

## 2. What the port already has

- `spring-tdf::WeaponDef` parses `sound_start` / `sound_hit`
  (`spring-tdf/src/weapon.rs:129`) and the `WeaponRegistry` holds the
  whole `WeaponDef`, so weapon sound names are already in the unit bundle
  — nothing to re-bake.
- `DeathBlast::resolve` already turns a unit's `ExplodeAs` into a weapon
  id for `death_system`, so death sounds are one lookup away.
- Bevy 0.20 ships `bevy_audio` + Vorbis in its default features. WAV
  needs the `wav` feature (`hound` decoder). Both decode on wasm.
- The Settings page (`ui/menu.rs::settings_page`) and the persisted
  `RenderSettings` (`rendering/settings.rs`: `key=value` file natively,
  `localStorage` on the web) are where a volume setting belongs.
- `kernel-panic/assets/` is copied wholesale into the web build
  (`index.html` `copy-dir`), so new sound files need no pipeline work.

## 3. Design

### Assets

`kernel-panic/assets/sounds/<lowercase name>.ogg` — the 26 originals
re-encoded to Vorbis q4 by `scripts/bake-sounds.sh` (244 KB instead of
1.3 MB of PCM; Vorbis is Bevy's default decoder, so no `wav` feature).
File names are lower-cased because Spring's VFS is case-insensitive but
Bevy's asset paths are not (`Explosion5` in the TDF → `explosion5.ogg`,
`SIGTERMhit` → `sigtermhit.ogg`). The tips clips go to
`assets/sounds/tips/` when the tip dispenser lands.

`kernel-panic/assets/music/*.ogg` — the 72 tracks the KP 4.4 Windows
installer shipped in `Music/KP/`, copied unmodified by
`scripts/bake-music.sh` as `<artist-slug>--<title-slug>.ogg` (Bevy's web
reader does not percent-encode paths), with `catalog.json` (display
names, durations, sizes) and `credits.txt`. The folder is in Git LFS;
the deploy workflow pulls it through a cache keyed on the object list.
Native builds scan the folder at startup; `build.rs` bakes the list into
`WEB_MUSIC_CATALOG` for the web build.

### `kernel-panic/src/audio/` module

- `SoundBank` resource: every sound name → `Handle<AudioSource>`, loaded
  once at startup from a static list of the 26 names (the web build
  cannot list a directory). Lookup is by lower-cased TDF name.
- `SoundQueue` resource: the sim pushes `SoundRequest { name, pos:
  Option<Vec3>, channel }`; a `PostUpdate` system drains it, computes the
  gain with the engine's formula above from the camera position and the
  terrain height under the camera, and spawns one
  `AudioPlayer` + `PlaybackSettings::DESPAWN` entity per sound. Sim code
  never touches `bevy_audio`, so headless tests and the bot stay silent
  and dependency-free. Requests for a sound the bank does not have are
  dropped with one warning per name.
- Channels mirror the engine's: `Battle`, `UnitReply`, `General`,
  `UserInterface`, each scaled by its Audio-page knob under the master
  (Spring's `snd_volbattle`, `snd_volunitreply`, `snd_volui`; General
  follows the master).
- `audio::music` is `music.lua`: during a match, a random track from the
  catalog, a two-second gap after it ends, the next one; silent (with the
  widget's "No music found!" log line) when the folder is empty; stopped
  on leaving the match. Volume is master × the Music knob, applied live.
- The engine plays ~96 sources at most; the drain caps live sound
  entities and drops the quietest new requests past the cap, and merges
  identical requests within a frame (a 2-projectile shot = one bang).
- Stereo panning is not reproduced (rodio's spatial sink uses its own
  distance model, which would fight the engine's); everything is mixed
  as mono with the engine's gain. Can be revisited.

### Hooks (the sim side)

| Trigger | Hook in the port |
|---|---|
| fire | `combat::fire_salvo_shot` — once per salvo shot at the muzzle, `weapon_def.sound_start`; hitscan shots also push `sound_hit` at the impact point (upstream explodes beams at fire time) |
| hit | `weapon_fx::tick::trigger_delayed_hit` — traveling shots, at the impact |
| death | `combat::lifecycle::death_system` — the `DeathBlast` weapon's `sound_hit` at the corpse |
| command fire | `mechanics::command_fire` — SIGTERM / NX zone detonations push the ability weapon's `sound_hit`; the Firewall cast plays `built.wav` 2D |
| select | audio system over `Added<Selected>`: one added → `select` at that unit; more → `button9` 2D |
| ok | `SoundQueue::ok()` at every player order site (right-click orders, ability clicks, build placement, Stop / Produce / Deploy / self-destruct from the panel); the drain plays `commandgiven` once per frame at the first selected unit |
| arrived | `ground_move::movement_system` when a leg finishes with `arrived` and the mover is on the local team: `commandgiven` at the unit |
| failed command | not wired: the port's GUI never rejects an armed command the way `GuiHandler` does; left for when it does |

Systems take `Option<ResMut<SoundQueue>>` so the existing test apps keep
working without registering the resource.

### Settings

`RenderSettings` grows an `AudioSettings` (master, battle, unit replies,
interface, music, all 100 like the `springsettings.cfg` the KP 4.4
installer ships; the bare engine's master default would be 60), persisted
as `volume=` / `vol_battle=` / `vol_unitreply=` / `vol_ui=` / `vol_music=`.
A dedicated Audio page (from the Settings page, also inside the Esc
overlay) offers Off / 20 / 40 / 60 / 80 / 100 per knob. The drain reads
the knobs every frame, so changes are immediate.

### Web

Browsers create the `AudioContext` suspended until a user gesture, and
cpal only calls `resume()` when the stream starts (before any click). A
small shim in `index.html` records every `AudioContext` constructed and
resumes them on the first `pointerdown` / `keydown`, so sound starts
after the first menu click.

## 4. Phases

1. ✅ **Assets + plumbing**: the 26 effects as Vorbis, `audio` module
   (bank, queue, drain), Audio settings page, web shim.
2. ✅ **Battle**: fire / hit / death / command-fire hooks.
3. ✅ **Unit replies**: select, multi-select, ok, arrived, Firewall `built`.
4. ✅ **Music**: the `music.lua` player over `assets/music/`, with the
   original soundtrack in LFS and next-track prefetch (Bevy cannot
   stream; on the web a track is fetched whole, so the next one loads
   while the current plays).
4b. ✅ **Web player**: `kernel-panic/web/music.html`, published as
   `music.html` by Trunk's `copy-file` — the soundtrack streamed from
   `assets/music/` through an `<audio>` element, a Sound-effects tab for
   the 26 effects, and the credits.
5. **Verify in the running game** (still open): a bot-driven match
   (`KP_BOT`, script in `target/kp-audio-bot/script.txt`) to see the
   bank load and effects fire without asset errors, and a listen on the
   web build for the autoplay shim. Unit tests cover the name resolution,
   the bank ↔ TDF coverage, the gain curve and the selection replies.
6. **Later**: the tip dispenser and its voice-over, `FailedCommand`,
   marker/chat beeps, stereo panning and the verification debt are
   planned in `docs/audio-followups.md`.
